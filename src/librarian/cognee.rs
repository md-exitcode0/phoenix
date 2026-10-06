//! Cognee-backed memory engine (feature `cognee`).
//!
//! Wraps the vendored `cognee-rs` (brute-force vector store + ladybug graph +
//! sqlite, all under `~/.phoenix/cognee/`) behind a small Phoenix-shaped API:
//! `remember` a piece of text into the knowledge graph, `recall` relevant
//! context for a turn, and `visualize` the graph to HTML. This is the engine
//! the librarian's save/preload hooks route through, replacing the file-tier
//! store. The LLM used for cognify (entity/relationship extraction) and the
//! embedding endpoint are the user's own configured provider — any
//! OpenAI-compatible model, including a local one.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use cognee_bindings_common::{ops, HandleState};
use cognee_lib::config::Settings;

/// Keep Cognee's implicit operations on the same dataset used by Phoenix's
/// scoped memory API and the legacy `phoenix` -> `team` migration.
const DEFAULT_DATASET: &str = super::memory::TEAM_DATASET;
/// Must match `build_cognify_pipeline(...).with_name("cognify")` in the
/// vendored engine: this is the dataset-level qualification-cache key.
const COGNIFY_PIPELINE_NAME: &str = "cognify";

/// Everything Cognee needs from Phoenix to run: where to store data and the
/// OpenAI-compatible endpoint used for the cognify (entity/relationship
/// extraction) step. Embeddings run locally on-device via ONNX BGE-Small, so
/// there is no embedding endpoint/key — recall costs zero provider tokens and
/// works offline. Resolved from Phoenix's provider config at the call site.
#[derive(Clone, Debug)]
pub struct CogneeConfig {
    /// Storage root — normally `phoenix_cognee_root()`.
    pub root: PathBuf,
    pub llm_endpoint: String,
    pub llm_api_key: String,
    pub llm_model: String,
    /// `chat_completions` or `responses`; selected from the Phoenix provider.
    pub llm_api_style: String,
    /// Dataset the agent's memories live in (one per Phoenix "brain").
    pub dataset: String,
    /// Whether the graph-extraction step can run. False when no usable
    /// credential exists. Ingest (`add`), local-embedding recall, and memify keep
    /// working either way — upstream parity: `add` never needs the LLM.
    pub cognify_enabled: bool,
}

impl CogneeConfig {
    /// Build cognee `Settings` pinned to Phoenix's storage + provider, with the
    /// pure-Rust brute-force vector store (never LanceDB), the embedded ladybug
    /// graph, and local ONNX embeddings.
    fn to_settings(&self) -> Settings {
        let root = self.root.to_string_lossy().to_string();
        let mut s = Settings::default();
        // All storage under ~/.phoenix/cognee/ so memory lives with the config.
        s.system_root_directory = format!("{root}/system");
        s.data_root_directory = format!("{root}/data");
        s.cache_root_directory = format!("{root}/cache");
        s.logs_root_directory = format!("{root}/logs");
        // Lean local engines: brute-force vectors (no Arrow/LanceDB), embedded
        // ladybug graph, sqlite relational. The vector_db_url is the snapshot
        // file that makes brute-force vectors survive restarts — without it
        // every embedding died with the process and recall was a cold store
        // in every new one, even after a clean cognify (live 2026-07-06).
        s.vector_db_provider = "brute-force".to_string();
        s.vector_db_url = format!("{root}/system/vectors.json");
        s.graph_database_provider = "ladybug".to_string();
        s.db_provider = "sqlite".to_string();
        s.relational_db_url = format!("sqlite:{root}/cognee.db?mode=rwc");
        // Cognify LLM = the user's own OpenAI-compatible provider.
        s.llm_provider = "openai".to_string();
        s.llm_model = self.llm_model.clone();
        s.llm_endpoint = self.llm_endpoint.clone();
        s.llm_api_key = self.llm_api_key.clone();
        s.llm_api_style = self.llm_api_style.clone();
        // Embeddings run locally: ONNX BGE-Small (384-dim), auto-downloaded once
        // to the cognee root, then reused. No per-write token cost, no dependency
        // on the chat provider serving an /embeddings route (opencode Zen does
        // not). The dlopen'd libonnxruntime is a system library.
        s.embedding_provider = "onnx".to_string();
        s.embedding_model_name = "BGE-Small-v1.5".to_string();
        s.embedding_dimensions = 384;
        s.embedding_model_path = format!("{root}/models/bge-small-en-v1.5.onnx");
        s.embedding_tokenizer_path = format!("{root}/models/bge-small-tokenizer.json");
        // Memory maintenance is background work. ONNX's upstream default of
        // 32 texts can allocate multiple gigabytes of transformer activations
        // and make an otherwise idle desktop look like a runaway workload.
        // Four keeps peak memory bounded without changing embedding quality.
        s.embedding_onnx_batch_size = 4;
        s.embedding_batch_size = 4;
        s
    }

    /// Resolve a `CogneeConfig` from Phoenix's live config: storage under
    /// `~/.phoenix/cognee/`, the cognify LLM = the librarian-role provider when
    /// one is configured (cognify is librarian-tier work), else the main
    /// provider, with the librarian-tier model for the per-ingest extraction
    /// step (cheap by design). Returns an error when that provider does not
    /// expose a verified OpenAI-compatible wire; a missing credential disables
    /// indexing for credentialed providers, while keyless local providers use
    /// their normal placeholder. The caller degrades rather than failing the
    /// turn.
    pub fn from_phoenix_config(config: &crate::config::PhoenixConfig) -> Result<Self> {
        let llm = &config.profile.llm;
        let factory = crate::providers::ProviderFactory::new();
        // The memory lane is EXPLICIT when set (`memory_provider`/`memory_model`
        // in [profile.llm], "Memory graph" in configure → Model roles); only
        // without it does cognify fall back to riding the librarian. The old
        // implicit-librarian-only coupling meant changing "the model" anywhere
        // else never reached memory — invisible and infuriating.
        let resolved = match llm
            .memory_provider
            .as_deref()
            .or(llm.librarian_provider.as_deref())
            .filter(|id| !id.trim().is_empty() && *id != llm.provider)
        {
            Some(id) => factory
                .resolve_llm_profile(&llm.probe_for_provider(id))
                .with_context(|| format!("resolving Phoenix memory provider `{id}`"))?,
            None => factory
                .resolve_llm_profile(llm)
                .context("resolving provider for Phoenix memory indexing")?,
        };
        // Cognee's vendored adapter speaks only OpenAI Chat Completions or
        // Responses. A provider being present in Phoenix's catalog does not
        // imply that wire is compatible: Anthropic, Gemini, and their CLI
        // subscription lanes use different paths, headers, and payloads. Fail
        // closed instead of sending a valid credential over the wrong wire.
        let llm_api_style = cognee_api_style_for_provider(&resolved.provider_id)?;
        let provider = crate::providers::providers_data::get_provider(&resolved.provider_id)
            .with_context(|| {
                format!(
                    "memory provider `{}` disappeared from the provider catalog",
                    resolved.provider_id
                )
            })?;

        // Keyless OpenAI-compatible local providers still need cognify. The
        // adapter always emits an Authorization header, so give those lanes a
        // harmless non-empty placeholder (the same contract the runtime uses
        // for the free HF endpoint) rather than silently disabling indexing.
        let key = cognee_api_key(resolved.auth.credential.clone(), provider.auth_type);
        let cognify_enabled = key.is_some();
        if !cognify_enabled {
            tracing::debug!(
                "Phoenix Memory indexing disabled (provider `{}`, credential {}) — set the Memory lane (`phoenix configure` → Model roles, or `memory_provider` under [profile.llm]); memories are still ingested and locally recallable",
                resolved.provider_id,
                if key.is_some() { "present" } else { "missing" }
            );
        }
        let llm_endpoint = openai_compat_base(&resolved.provider_id, &resolved.base_url);
        if resolved.provider_id == "grok-cli"
            && !is_resolved_grok_cli_proxy(&resolved.provider_id, &llm_endpoint)
        {
            anyhow::bail!(
                "refusing unexpected Grok CLI cognify endpoint `{llm_endpoint}`; expected the exact cli-chat-proxy.grok.com /v1 origin"
            );
        }
        Ok(Self {
            root: crate::config::phoenix_cognee_root(),
            llm_endpoint,
            llm_api_key: key.unwrap_or_default(),
            // Explicit memory model when set, else librarian — cheap and
            // reliable is right for the mechanical extraction step.
            llm_model: llm.memory(),
            llm_api_style: llm_api_style.to_string(),
            dataset: DEFAULT_DATASET.to_string(),
            cognify_enabled,
        })
    }
}

/// Wire compatibility for Cognee's vendored OpenAI adapter. Keep this list in
/// lockstep with the OpenAI-compatible branches in `ProviderFactory::instantiate`;
/// special native clients (Anthropic/Google and their CLI lanes) are
/// intentionally absent. Returning the API style here makes it impossible for
/// a newly catalogued provider to be guessed onto the OpenAI wire.
fn cognee_api_style_for_provider(provider_id: &str) -> Result<&'static str> {
    match provider_id {
        "openai-codex" => Ok("responses"),
        "openai"
        | "groq"
        | "mistral"
        | "together"
        | "fireworks"
        | "deepinfra"
        | "moonshot"
        | "kimi-coding"
        | "tokenrouter"
        | "zai"
        | "xai"
        | "cerebras"
        | "venice"
        | "kilocode"
        | "nvidia"
        | "volcengine"
        | "byteplus"
        | "stepfun"
        | "qianfan"
        | "tencent"
        | "xiaomi"
        | "chutes"
        | "minimax-portal"
        | "github-copilot"
        | "ollama-cloud"
        | "ollama"
        | "sglang"
        | "vllm"
        | "lm_studio"
        | "litellm"
        | "huggingface"
        | "hf-deepseek-v4-free"
        | "openrouter"
        | "grok-cli"
        | "opencode"
        | "deepseek" => Ok("chat_completions"),
        other => anyhow::bail!(
            "memory provider `{other}` does not expose a compatible chat wire for Phoenix semantic memory; choose a compatible Memory provider in Phoenix Settings → Models"
        ),
    }
}

fn cognee_api_key(
    credential: Option<String>,
    auth_type: crate::providers::providers_data::AuthType,
) -> Option<String> {
    credential.or_else(|| {
        (auth_type == crate::providers::providers_data::AuthType::None)
            .then(|| "not-needed".to_string())
    })
}

/// Cognee's vendored client is OpenAI-style — it POSTs `{base}/chat/completions`.
/// Providers whose catalog base is a NATIVE api root (ollama.com serves its
/// native API there and OpenAI compat under `/v1`) need the compat prefix
/// appended, or every cognify call 404s against the provider's WEBSITE
/// (live 2026-07-06: two days of graph-indexing failures returning HTML).
fn openai_compat_base(provider_id: &str, base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    match provider_id {
        "ollama" | "ollama-cloud" if !trimmed.ends_with("/v1") => format!("{trimmed}/v1"),
        _ => trimmed.to_string(),
    }
}

/// The subscription-only Grok wire requires privileged CLI headers. Gate that
/// behavior on both the resolved Phoenix provider and the exact HTTPS proxy
/// origin/path; a model name or a hostname substring is not sufficient.
fn is_resolved_grok_cli_proxy(provider_id: &str, endpoint: &str) -> bool {
    if provider_id != "grok-cli" {
        return false;
    }
    let Ok(url) = url::Url::parse(endpoint) else {
        return false;
    };
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case("cli-chat-proxy.grok.com"))
        && url.port_or_known_default() == Some(443)
        && url.path().trim_end_matches('/') == "/v1"
        && url.query().is_none()
        && url.fragment().is_none()
}

/// A live Cognee memory engine. Cheap to clone (shares the handle).
#[derive(Clone)]
pub struct CogneeMemory {
    handle: Arc<HandleState>,
    root: PathBuf,
    dataset: String,
    cognify_enabled: bool,
}

impl CogneeMemory {
    /// Open (or create) the memory store under the configured root. Sync — no
    /// I/O happens until the first remember/recall.
    pub fn open(config: &CogneeConfig) -> Result<Self> {
        std::fs::create_dir_all(&config.root)
            .with_context(|| format!("creating Phoenix memory root {}", config.root.display()))?;
        point_ort_at_system_onnxruntime()?;
        let handle = HandleState::from_settings(config.to_settings());
        Ok(Self {
            handle: Arc::new(handle),
            root: config.root.clone(),
            dataset: config.dataset.clone(),
            cognify_enabled: config.cognify_enabled,
        })
    }

    /// Persist a piece of text with durable `add` (ingest) and re-arm its graph
    /// indexing row. No LLM is needed on the foreground save path; deferred
    /// cognify is picked up by [`CogneeMemory::cognify_pending`] on the
    /// maintenance timer.
    pub async fn remember(&self, text: &str) -> Result<()> {
        self.remember_into(text, &self.dataset.clone()).await
    }

    /// Persist into a SPECIFIC dataset (memory scope): `team` or an
    /// `agent-<role>` partition. Same ingest-first/deferred-indexing shape
    /// as [`CogneeMemory::remember`].
    pub async fn remember_into(&self, text: &str, dataset: &str) -> Result<()> {
        // Publish the re-arm intent *before* add. `add` is durable and may
        // commit even when its caller subsequently sees an error. More
        // importantly, if reset fails after a successful add, the next add is
        // a duplicate; without this durable marker that retry would return
        // early and the COMPLETED pipeline cache could strand the note forever.
        let marker = cognify_rearm_marker(&self.root, dataset);
        let rearm_was_pending = mark_cognify_rearm_pending(&marker, dataset)?;
        let inputs = serde_json::json!([{ "type": "text", "text": text }]);
        let add_result = ops::pipeline::add(&self.handle, inputs, dataset, &serde_json::json!({}))
            .await
            .map_err(|e| anyhow::anyhow!("Phoenix memory ingest failed: {e}"))?;

        // `add` returns both newly-created and deduplicated items. Only a real
        // insertion invalidates the dataset-level cognify completion cache.
        // Without this reset, the first successful cognify leaves COMPLETED as
        // the latest row forever and every later note is silently skipped.
        let added = add_result_has_new_data(&add_result)?;
        if !needs_cognify_rearm(added, rearm_was_pending) {
            clear_cognify_rearm_pending(&marker)?;
            return Ok(());
        }
        self.rearm_cognify(dataset).await?;
        // Once INITIATED is durable, normal maintenance can index the note.
        // Do not await provider-driven cognify in the foreground save: that
        // can take minutes, made a successful durable add look like a 15s save
        // timeout, and held the serialized memory writer against later saves.
        // A failure before here intentionally leaves the marker for a
        // duplicate retry.
        clear_cognify_rearm_pending(&marker)?;
        Ok(())
    }

    /// Put the dataset's cognify qualification row back into INITIATED after
    /// new input lands. Resolve the database row by name instead of deriving
    /// its UUID: the legacy `phoenix` -> `team` migration deliberately keeps
    /// the old ID, so name-based UUID generation would reset a phantom row.
    async fn rearm_cognify(&self, dataset_name: &str) -> Result<()> {
        let services =
            self.handle.services().await.map_err(|e| {
                anyhow::anyhow!("opening Phoenix memory services after ingest: {e}")
            })?;
        let owner_id = self
            .handle
            .owner_id()
            .await
            .map_err(|e| anyhow::anyhow!("resolving Phoenix memory owner after ingest: {e}"))?;
        let dataset = cognee_lib::database::ops::datasets::get_dataset_by_name(
            &services.database,
            dataset_name,
            owner_id,
            None,
        )
        .await
        .map_err(|e| {
            anyhow::anyhow!("resolving Phoenix memory dataset `{dataset_name}` after ingest: {e}")
        })?
        .with_context(|| {
            format!("Phoenix memory ingest succeeded but dataset `{dataset_name}` was not found")
        })?;

        ops::admin::run_reset_pipeline_run_status(
            &self.handle,
            &dataset.id.to_string(),
            COGNIFY_PIPELINE_NAME,
        )
        .await
        .map_err(|e| anyhow::anyhow!("re-arming Phoenix memory indexing after ingest: {e}"))?;
        Ok(())
    }

    /// Retrieve the most relevant remembered chunks for a query. Uses CHUNKS
    /// (raw retrieved text, no extra LLM synthesis) so preload injects facts
    /// cheaply; the agent does its own reasoning over them.
    pub async fn recall(&self, query: &str, top_k: usize) -> Result<Vec<String>> {
        self.search(query, "CHUNKS", top_k).await
    }

    /// Typed search over the graph — the full upstream `SearchType` surface
    /// (SUMMARIES, CHUNKS, GRAPH_COMPLETION, TRIPLET_COMPLETION, TEMPORAL, …).
    /// CHUNKS/CHUNKS_LEXICAL are local-only; the completion types use the
    /// configured cognify LLM.
    pub async fn search(
        &self,
        query: &str,
        search_type: &str,
        top_k: usize,
    ) -> Result<Vec<String>> {
        self.search_scoped(query, search_type, top_k, None).await
    }

    /// Search restricted to specific datasets (memory scopes). `None` = the
    /// whole store. An explicitly empty filter returns nothing. The engine
    /// resolves names for the current owner, searches the known subset, and
    /// rejects a filter whose names are all unknown.
    pub async fn search_scoped(
        &self,
        query: &str,
        search_type: &str,
        top_k: usize,
        datasets: Option<&[String]>,
    ) -> Result<Vec<String>> {
        // Upstream treats an empty dataset list as unscoped. Preserve the
        // caller's explicit scope before initializing any memory services.
        if datasets.is_some_and(|names| names.is_empty()) {
            return Ok(Vec::new());
        }
        let mut opts = serde_json::json!({ "searchType": search_type, "topK": top_k });
        if let Some(names) = datasets {
            if !names.is_empty() {
                opts["datasets"] = serde_json::json!(names);
            }
        }
        let value = match ops::retrieval::recall(&self.handle, query, &opts).await {
            Ok(value) => value,
            // A store that was never cognified has no vector collections yet
            // (upstream creates them at first cognify). That is a COLD store,
            // not a failure: keyless ingest is designed to defer cognify to
            // the maintenance pass, so recall before it simply knows nothing.
            Err(e) if e.to_string().contains("missing vector collection") => {
                tracing::debug!(
                    "Phoenix memory search ({search_type}): cold store (not indexed yet)"
                );
                return Ok(Vec::new());
            }
            Err(e) => anyhow::bail!("Phoenix memory search ({search_type}) failed: {e}"),
        };
        Ok(extract_texts(&value))
    }

    /// Cognify everything in the dataset that has data (the deferred backlog
    /// from saves made without a usable provider). No-op when cognify is
    /// disabled. Called from the maintenance timer before memify.
    pub async fn cognify_pending(&self) -> Result<String> {
        self.cognify_pending_in(&self.dataset.clone()).await
    }

    /// Backlog cognify for ONE named dataset (memory scope). The maintenance
    /// caller loops every dataset present in the store — a hardcoded single
    /// dataset here would leave agent-scoped partitions saved-but-unsearchable
    /// forever (the exact 2026-07-04 disease, re-armed by scoping).
    pub async fn cognify_pending_in(&self, dataset: &str) -> Result<String> {
        if !self.cognify_enabled {
            return Ok("cognify disabled (no usable librarian provider)".to_string());
        }
        let value = ops::pipeline::cognify(&self.handle, dataset, &serde_json::json!({}))
            .await
            .map_err(|e| anyhow::anyhow!("Phoenix memory indexing failed: {e}"))?;
        Ok(value
            .get("cognify")
            .and_then(|c| c.get("processedCount"))
            .and_then(|v| v.as_u64())
            .map(|n| format!("{n} item(s) cognified"))
            .unwrap_or_else(|| "completed".to_string()))
    }

    /// Run Cognee's self-improvement pipeline: triplet embeddings for every
    /// graph edge. Idempotent — the daemon's 12h maintenance timer calls this
    /// instead of the archived LLM gardening loop. Returns a short summary.
    pub async fn memify(&self) -> Result<String> {
        let value = ops::memory::run_memify_op(&self.handle, &serde_json::json!({}))
            .await
            .map_err(|e| anyhow::anyhow!("Phoenix memory maintenance failed: {e}"))?;
        Ok(summarize_memify(&value))
    }

    /// Render the whole knowledge graph to an HTML file (the `phoenix memory`
    /// viewer). Returns the path written.
    pub async fn visualize(&self, out: &Path) -> Result<PathBuf> {
        let out_str = out.to_string_lossy().to_string();
        let opts = serde_json::json!({ "path": out_str });
        ops::visualization::visualize_to_file(&self.handle, Some(&opts))
            .await
            .map_err(|e| anyhow::anyhow!("Phoenix memory visualization failed: {e}"))?;
        Ok(out.to_path_buf())
    }

    /// The REAL Cognee knowledge graph as JSON `{nodes, edges}` — the actual
    /// indexed entities and their relationship triplets, NOT the raw note
    /// files. This is what the dashboard memory graph should render: the edges
    /// are real, so the graph forms genuine clusters/hubs instead of a
    /// fabricated chain. Same `(nodes, edges)` the HTML `visualize` draws.
    pub async fn graph_data(&self) -> Result<serde_json::Value> {
        use std::borrow::Cow;
        use std::collections::HashMap;
        let svc = self
            .handle
            .services()
            .await
            .map_err(|e| anyhow::anyhow!("Phoenix memory services unavailable: {e}"))?;
        let (nodes, edges) = svc
            .graph_db
            .get_graph_data()
            .await
            .map_err(|e| anyhow::anyhow!("Phoenix memory graph export failed: {e}"))?;
        let get = |m: &HashMap<Cow<'static, str>, serde_json::Value>, k: &str| {
            m.get(k).and_then(|v| v.as_str()).map(|s| s.to_string())
        };
        let nodes_json: Vec<serde_json::Value> = nodes
            .into_iter()
            .map(|(id, props)| {
                let label = get(&props, "name")
                    .or_else(|| get(&props, "text"))
                    .or_else(|| get(&props, "content"))
                    .unwrap_or_else(|| id.clone());
                let text = get(&props, "text")
                    .or_else(|| get(&props, "content"))
                    .unwrap_or_default();
                let group = get(&props, "type").unwrap_or_else(|| "entity".to_string());
                serde_json::json!({
                    "id": id,
                    "label": label.chars().take(60).collect::<String>(),
                    "text": text,
                    "group": group,
                    "kind": group,
                })
            })
            .collect();
        let edges_json: Vec<serde_json::Value> = edges
            .into_iter()
            .map(|(source, target, relation, _props)| {
                serde_json::json!({ "source": source, "target": target, "relation": relation })
            })
            .collect();
        Ok(serde_json::json!({ "nodes": nodes_json, "edges": edges_json }))
    }
}

fn cognify_rearm_marker(root: &Path, dataset: &str) -> PathBuf {
    use sha2::{Digest, Sha256};

    let digest = Sha256::digest(dataset.as_bytes());
    root.join("pending-cognify")
        .join(format!("{:x}.pending", digest))
}

/// Returns whether an earlier add/reset attempt left a pending intent.
fn mark_cognify_rearm_pending(path: &Path, dataset: &str) -> Result<bool> {
    crate::config::private_io::read_modify_write_private(path, |current| {
        let pending = match current {
            Some(bytes) if bytes == dataset.as_bytes() => true,
            Some(_) => anyhow::bail!(
                "Phoenix memory re-arm marker {} does not match dataset `{dataset}`",
                path.display()
            ),
            None => false,
        };
        Ok((pending, dataset.as_bytes().to_vec()))
    })
    .with_context(|| format!("persisting Phoenix memory re-arm intent for `{dataset}`"))
}

fn clear_cognify_rearm_pending(path: &Path) -> Result<()> {
    crate::config::private_io::remove_private_file(path)
        .map(|_| ())
        .with_context(|| format!("clearing Phoenix memory re-arm marker {}", path.display()))
}

fn needs_cognify_rearm(added: bool, earlier_attempt_pending: bool) -> bool {
    added || earlier_attempt_pending
}

/// Interpret the documented binding result from `ops::pipeline::add`.
/// Missing or malformed counts are errors, not "probably a duplicate": a
/// permissive fallback here would reintroduce silent uncognified memories if
/// the vendored wire shape ever changes.
fn add_result_has_new_data(result: &serde_json::Value) -> Result<bool> {
    result
        .get("addedCount")
        .and_then(serde_json::Value::as_u64)
        .map(|count| count > 0)
        .context("Phoenix memory ingest result is missing numeric `addedCount`")
}

/// Render memify's result JSON (`{tripletCount, indexedCount, ...}`) as one
/// human line for the maintenance receipt log.
fn summarize_memify(value: &serde_json::Value) -> String {
    let triplets = value.get("tripletCount").and_then(|v| v.as_u64());
    let indexed = value.get("indexedCount").and_then(|v| v.as_u64());
    let already = value
        .get("alreadyCompleted")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    match (triplets, indexed) {
        _ if already => "already up to date".to_string(),
        (Some(t), Some(i)) => format!("{t} triplet(s) embedded, {i} indexed"),
        _ => "completed".to_string(),
    }
}

/// Pull human-readable strings out of Cognee's recall JSON, whatever shape it
/// returns (array of strings, `{results:[...]}`, or objects with a `text`/
/// `content` field). Defensive so a schema tweak upstream degrades to fewer
/// results, never a panic.
/// ort is built with `load-dynamic` + `api-23` (see vendor/cognee-rs/
/// Cargo.toml for the full root-cause note: the default static build and any
/// init-time failure both DEADLOCK inside ort's recursive error constructor).
/// Point it at the system libonnxruntime before the first `ort::init()`
/// unless the user already chose one via ORT_DYLIB_PATH — and REFUSE to open
/// memory when no library exists, because letting ort discover that itself
/// parks every thread forever instead of erroring.
// The Ubuntu 1.23 package is intentionally absent here.  It links the split
// `libonnx.so.1` alongside ONNX Runtime and registers the same schemas twice
// (hundreds of errors for the BGE model).  Prefer the known-good monolithic
// Handy build, then an explicitly app-owned/local install.
const ORT_DYLIB_CANDIDATES: [&str; 3] = [
    "/usr/lib/Handy/libonnxruntime.so.1",
    "/usr/local/lib/libonnxruntime.so.1",
    "/usr/local/lib/libonnxruntime.so",
];

fn select_ort_dylib_path<'a>(
    explicitly_configured: bool,
    candidates: &'a [&'a str],
    mut exists: impl FnMut(&str) -> bool,
) -> Option<&'a str> {
    if explicitly_configured {
        return None;
    }
    candidates.iter().copied().find(|path| exists(path))
}

fn validate_explicit_ort_dylib_path(
    configured: Option<std::ffi::OsString>,
    mut is_file: impl FnMut(&std::path::Path) -> bool,
) -> Result<Option<std::path::PathBuf>> {
    let Some(configured) = configured else {
        return Ok(None);
    };
    let path = std::path::PathBuf::from(configured);
    if path.as_os_str().is_empty() || !is_file(&path) {
        anyhow::bail!(
            "ORT_DYLIB_PATH points at `{}`, but that is not a readable regular file; refusing to initialize ONNX Runtime",
            path.display()
        );
    }
    Ok(Some(path))
}

fn point_ort_at_system_onnxruntime() -> Result<()> {
    if validate_explicit_ort_dylib_path(std::env::var_os("ORT_DYLIB_PATH"), |path| path.is_file())?
        .is_some()
    {
        return Ok(());
    }
    match select_ort_dylib_path(
        false,
        &ORT_DYLIB_CANDIDATES,
        |path| std::path::Path::new(path).is_file(),
    ) {
        Some(path) => {
            std::env::set_var("ORT_DYLIB_PATH", path);
            Ok(())
        }
        None => anyhow::bail!(
            "no compatible monolithic libonnxruntime found (looked at {ORT_DYLIB_CANDIDATES:?}) — memory stays disabled. Install an app-owned ONNX Runtime build or set ORT_DYLIB_PATH to a verified library."
        ),
    }
}

/// Pull the human-readable texts out of a recall/search result.
///
/// The REAL shape (vendored `ops::retrieval::recall`, upstream-parity): a
/// top-level `items` array of RecallItems `{ source, content, score }`, where
/// `content` is a plain string (Text/Texts outputs) or, for graph CHUNKS, the
/// serialized search item `{ id, score, payload: { text, … } }`. This parser
/// previously only read a `results` key that the adapter never emits — every
/// recall parsed to EMPTY, the receipts claimed "cold or empty store", and the
/// memory read side was silently dead for days while saves kept landing (the
/// 2026-07-06 OpenClaw turn dropped 8 relevant retrieved chunks on the floor).
fn extract_texts(value: &serde_json::Value) -> Vec<String> {
    fn from_item(item: &serde_json::Value, out: &mut Vec<String>) {
        match item {
            serde_json::Value::String(s) => out.push(s.clone()),
            serde_json::Value::Object(map) => {
                for key in ["text", "content", "chunk", "value"] {
                    if let Some(serde_json::Value::String(s)) = map.get(key) {
                        out.push(s.clone());
                        return;
                    }
                }
                // Graph CHUNKS item: { id, score, payload: { text, … } }.
                if let Some(serde_json::Value::Object(payload)) = map.get("payload") {
                    for key in ["text", "content", "chunk", "value"] {
                        if let Some(serde_json::Value::String(s)) = payload.get(key) {
                            out.push(s.clone());
                            return;
                        }
                    }
                }
                // RecallItem wrapper: { source, content: <object>, score } —
                // unwrap and retry (a string `content` was caught above).
                if let Some(inner @ serde_json::Value::Object(_)) = map.get("content") {
                    from_item(inner, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    match value {
        serde_json::Value::Array(items) => items.iter().for_each(|i| from_item(i, &mut out)),
        serde_json::Value::Object(map) => {
            // `items` is what the adapter emits; `results` kept for any older
            // callers/shapes.
            for key in ["items", "results"] {
                if let Some(serde_json::Value::Array(items)) = map.get(key) {
                    items.iter().for_each(|i| from_item(i, &mut out));
                    if !out.is_empty() {
                        break;
                    }
                }
            }
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_compat_base_appends_v1_for_ollama_lanes() {
        assert_eq!(
            openai_compat_base("ollama-cloud", "https://ollama.com"),
            "https://ollama.com/v1"
        );
        assert_eq!(
            openai_compat_base("ollama", "http://localhost:11434/v1"),
            "http://localhost:11434/v1"
        );
        // Already-compat providers pass through untouched.
        assert_eq!(
            openai_compat_base("openrouter", "https://openrouter.ai/api/v1"),
            "https://openrouter.ai/api/v1"
        );
    }

    #[test]
    fn grok_cli_cognify_gate_requires_provider_and_exact_proxy_origin() {
        let canonical = "https://cli-chat-proxy.grok.com/v1";
        assert!(is_resolved_grok_cli_proxy("grok-cli", canonical));
        assert!(is_resolved_grok_cli_proxy(
            "grok-cli",
            "https://CLI-CHAT-PROXY.GROK.COM:443/v1/"
        ));
        assert!(!is_resolved_grok_cli_proxy("xai", canonical));

        for endpoint in [
            "http://cli-chat-proxy.grok.com/v1",
            "https://cli-chat-proxy.grok.com.evil.example/v1",
            "https://evil.example/cli-chat-proxy.grok.com/v1",
            "https://user@cli-chat-proxy.grok.com/v1",
            "https://cli-chat-proxy.grok.com:444/v1",
            "https://cli-chat-proxy.grok.com/v1/chat/completions",
            "https://cli-chat-proxy.grok.com/v1?redirect=evil",
        ] {
            assert!(
                !is_resolved_grok_cli_proxy("grok-cli", endpoint),
                "accepted unsafe endpoint: {endpoint}"
            );
        }
    }

    #[test]
    fn cognee_accepts_only_providers_with_a_verified_openai_wire() {
        for provider in [
            "openai",
            "openai-codex",
            "openrouter",
            "grok-cli",
            "deepseek",
            "opencode",
            "ollama",
            "vllm",
            "hf-deepseek-v4-free",
            "github-copilot",
        ] {
            assert!(
                cognee_api_style_for_provider(provider).is_ok(),
                "expected `{provider}` to be compatible with Phoenix semantic memory"
            );
        }
        assert_eq!(
            cognee_api_style_for_provider("openai-codex").unwrap(),
            "responses"
        );
        assert_eq!(
            cognee_api_style_for_provider("grok-cli").unwrap(),
            "chat_completions"
        );

        for provider in [
            "anthropic",
            "google",
            "google-gemini-cli",
            "meta",
            "future-native-provider",
        ] {
            let error = cognee_api_style_for_provider(provider)
                .expect_err("native/unknown provider must fail closed")
                .to_string();
            assert!(error.contains(provider), "{error}");
            assert!(error.contains("Memory provider"), "{error}");
        }
    }

    #[test]
    fn keyless_compatible_providers_get_only_the_runtime_placeholder() {
        use crate::providers::providers_data::AuthType;

        assert_eq!(
            cognee_api_key(None, AuthType::None).as_deref(),
            Some("not-needed")
        );
        assert_eq!(cognee_api_key(None, AuthType::ApiKey), None);
        assert_eq!(cognee_api_key(None, AuthType::Bearer), None);
        assert_eq!(
            cognee_api_key(Some("real-secret".to_string()), AuthType::None).as_deref(),
            Some("real-secret")
        );
    }

    #[test]
    fn default_dataset_matches_the_migrated_team_scope() {
        assert_eq!(DEFAULT_DATASET, "team");
        assert_eq!(DEFAULT_DATASET, super::super::memory::TEAM_DATASET);
    }

    #[tokio::test]
    async fn empty_scope_returns_nothing_without_opening_memory_services() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("must-remain-absent");
        let config = CogneeConfig {
            root: root.clone(),
            llm_endpoint: "https://example.invalid".into(),
            llm_api_key: String::new(),
            llm_model: "fixture".into(),
            llm_api_style: "chat_completions".into(),
            dataset: "team".into(),
            cognify_enabled: false,
        };
        let memory = CogneeMemory {
            handle: Arc::new(HandleState::from_settings(config.to_settings())),
            root: root.clone(),
            dataset: "team".into(),
            cognify_enabled: false,
        };
        assert!(memory
            .search_scoped("anything", "CHUNKS_LEXICAL", 10, Some(&[]))
            .await
            .unwrap()
            .is_empty());
        assert!(!root.exists());
    }

    #[test]
    fn add_wire_result_rearms_only_for_real_insertions() {
        let id = uuid::Uuid::from_u128(1);
        let owner = uuid::Uuid::from_u128(2);
        let data = cognee_lib::models::Data::builder(
            id,
            "note.txt",
            "file:///note.txt",
            "file:///note.txt",
            "txt",
            "text/plain",
            "content-hash",
            owner,
        )
        .build();

        // Exercise the vendored marshaller itself, not a hand-written
        // approximation of its add-result shape.
        let inserted = ops::pipeline::add_result_json(std::slice::from_ref(&data), &[], "team")
            .expect("serializing an add result");
        assert!(add_result_has_new_data(&inserted).unwrap());

        let duplicate = ops::pipeline::add_result_json(&[], &[data], "team")
            .expect("serializing a duplicate result");
        assert!(!add_result_has_new_data(&duplicate).unwrap());
        assert!(
            add_result_has_new_data(&serde_json::json!({ "added": [] })).is_err(),
            "wire-shape drift must fail loudly instead of stranding new data"
        );
    }

    #[test]
    fn failed_rearm_intent_survives_a_duplicate_retry() {
        let dir = tempfile::tempdir().unwrap();
        let marker = cognify_rearm_marker(dir.path(), "team");

        let first_attempt_pending = mark_cognify_rearm_pending(&marker, "team").unwrap();
        assert!(!first_attempt_pending);
        assert!(needs_cognify_rearm(true, first_attempt_pending));

        // Simulate: add committed, reset failed, and therefore the marker was
        // deliberately not cleared. Retrying the same note now deduplicates,
        // but the durable intent still forces exactly one reset attempt.
        let retry_pending = mark_cognify_rearm_pending(&marker, "team").unwrap();
        assert!(retry_pending);
        assert!(needs_cognify_rearm(false, retry_pending));
        clear_cognify_rearm_pending(&marker).unwrap();
        assert!(crate::config::private_io::read_private_file(&marker)
            .unwrap()
            .is_none());

        // A normal duplicate with no failed intent must not re-arm a completed
        // dataset on every save. Its pre-add crash marker is cleaned instead.
        let clean_duplicate_pending = mark_cognify_rearm_pending(&marker, "team").unwrap();
        assert!(!clean_duplicate_pending);
        assert!(!needs_cognify_rearm(false, clean_duplicate_pending));
        clear_cognify_rearm_pending(&marker).unwrap();
        assert!(crate::config::private_io::read_private_file(&marker)
            .unwrap()
            .is_none());
    }

    #[test]
    fn ort_fallback_prefers_handy_without_overriding_an_explicit_path() {
        let handy = "/usr/lib/Handy/libonnxruntime.so.1";
        let fallback = "/usr/local/lib/libonnxruntime.so";

        assert_eq!(
            select_ort_dylib_path(false, &ORT_DYLIB_CANDIDATES, |path| path == handy),
            Some(handy)
        );
        let candidates = [handy, fallback];
        assert_eq!(
            select_ort_dylib_path(false, &candidates, |path| path == fallback),
            Some(fallback)
        );

        let mut probed = false;
        assert_eq!(
            select_ort_dylib_path(true, &candidates, |_| {
                probed = true;
                true
            }),
            None
        );
        assert!(!probed, "an explicit ORT_DYLIB_PATH must bypass probing");

        let valid = validate_explicit_ort_dylib_path(
            Some(std::ffi::OsString::from("/opt/onnx/libonnxruntime.so")),
            |path| path == std::path::Path::new("/opt/onnx/libonnxruntime.so"),
        )
        .unwrap();
        assert_eq!(
            valid.as_deref(),
            Some(std::path::Path::new("/opt/onnx/libonnxruntime.so"))
        );
        assert!(validate_explicit_ort_dylib_path(
            Some(std::ffi::OsString::from("/missing/libonnxruntime.so")),
            |_| false,
        )
        .is_err());
    }

    /// LIVE smoke: remember → recall against the real configured provider and
    /// the real on-disk store (isolated `phoenix-live-test` dataset). Proves
    /// the whole chain — provider-driven cognify, local ONNX embeddings,
    /// brute-force vector recall. Run: `cargo test --lib -- --ignored live_cognee`.
    // multi_thread = daemon parity (the real caller runs a multi-thread
    // runtime). History: this test deadlocked forever on 2026-07-02 — root
    // cause was ort's default `api-24` rejecting the distro ONNX Runtime 1.23
    // and ort's error constructor re-entering its own OnceLock (see the ort
    // pin note in vendor/cognee-rs/Cargo.toml). Fixed by api-23 +
    // load-dynamic; production call sites are additionally timeout-bounded in
    // librarian/memory.rs so memory can never wedge a turn again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "live: uses the configured provider + real ~/.phoenix/cognee store"]
    async fn live_cognee_remember_recall_roundtrip() {
        let config = crate::config::PhoenixConfig::load().expect("no live phoenix config");
        let mut cognee_config =
            CogneeConfig::from_phoenix_config(&config).expect("provider resolution failed");
        cognee_config.dataset = "phoenix-live-test".to_string();
        let memory = CogneeMemory::open(&cognee_config).expect("cognee open failed");

        let fact = "Phoenix fixed browser cookie porting by replacing the per-cookie \
                    deleteCookies loop with one batched Network.SetCookies call.";
        memory.remember(fact).await.expect("remember failed");

        let chunks = memory
            .recall("how did phoenix fix browser cookie porting?", 4)
            .await
            .expect("recall failed");
        if cognee_config.cognify_enabled {
            memory.cognify_pending().await.expect("cognify failed");
            let indexed = memory
                .recall("how did phoenix fix browser cookie porting?", 4)
                .await
                .expect("indexed recall failed");
            assert!(
                indexed
                    .iter()
                    .any(|c| c.contains("SetCookies") || c.to_lowercase().contains("cookie")),
                "recall should surface the indexed fact, got {indexed:#?}"
            );
        } else {
            // Keyless (upstream parity): add ingests durably, cognify remains
            // deferred — a never-cognified store is COLD and recall knows
            // nothing yet, without erroring or wedging.
            assert!(
                chunks.is_empty(),
                "cold (never-cognified) store should recall nothing, got {chunks:#?}"
            );
            eprintln!(
                "note: cognify disabled (no librarian_provider credential) — verified durable ingest + cold recall only"
            );
        }
    }

    #[test]
    fn extract_texts_handles_multiple_shapes() {
        assert_eq!(
            extract_texts(&serde_json::json!(["a", "b"])),
            vec!["a", "b"]
        );
        assert_eq!(
            extract_texts(&serde_json::json!({"results": [{"text": "x"}, {"content": "y"}]})),
            vec!["x", "y"]
        );
        assert_eq!(
            extract_texts(&serde_json::json!({"nope": 1})),
            Vec::<String>::new()
        );
    }

    /// REGRESSION (2026-07-06): the vendored adapter's REAL return shape — a
    /// top-level `items` array of RecallItems whose graph-CHUNKS `content`
    /// nests the text under `payload.text`. The old parser only read a
    /// `results` key, so every recall parsed to empty and the memory READ side
    /// was silently dead (the OpenClaw turn dropped 8 retrieved chunks).
    /// Shape taken verbatim from the persisted cognee `results` row of that
    /// turn (query f1cfb1b9…, 2026-07-07T02:43:20Z).
    #[test]
    fn extract_texts_parses_the_real_recall_items_shape() {
        let value = serde_json::json!({
            "items": [
                {
                    "source": "graph",
                    "score": 1.0,
                    "content": {
                        "id": "4bbe7faf-b0cb-515a-9151-71386b68a86c",
                        "score": 0.70422035,
                        "payload": {
                            "created_at": 1783369866426u64,
                            "document_id": "29c1e4ec-b20c-596f-91c4-80b8cb35b854",
                            "chunk_index": 0,
                            "text": "Session digest — remember we did that openclaw research?…",
                            "field": "text"
                        }
                    }
                },
                {
                    "source": "graph",
                    "score": 0.99,
                    "content": "plain text output from a Texts search"
                }
            ],
            "searchTypeUsed": "Chunks",
            "autoRouted": false,
            "searchResponse": {"kind": "Items"}
        });
        assert_eq!(
            extract_texts(&value),
            vec![
                "Session digest — remember we did that openclaw research?…".to_string(),
                "plain text output from a Texts search".to_string(),
            ]
        );
    }

    #[test]
    fn settings_pin_brute_force_and_phoenix_root() {
        let cfg = CogneeConfig {
            root: PathBuf::from("/tmp/phx-cognee-test"),
            llm_endpoint: "http://localhost:1234/v1".into(),
            llm_api_key: "k".into(),
            llm_model: "gpt-x".into(),
            llm_api_style: "chat_completions".into(),
            dataset: DEFAULT_DATASET.into(),
            cognify_enabled: true,
        };
        let s = cfg.to_settings();
        assert_eq!(s.vector_db_provider, "brute-force");
        assert_eq!(s.graph_database_provider, "ladybug");
        assert!(s.data_root_directory.starts_with("/tmp/phx-cognee-test"));
        assert_eq!(s.llm_model, "gpt-x");
        // Embeddings are always local ONNX BGE-Small — never routed to the chat
        // provider (which may not serve /embeddings).
        assert_eq!(s.embedding_provider, "onnx");
        assert_eq!(s.embedding_dimensions, 384);
        assert_eq!(s.embedding_batch_size, 4);
        assert_eq!(s.embedding_onnx_batch_size, 4);
        assert!(s.embedding_model_path.starts_with("/tmp/phx-cognee-test"));
    }
}
