//! Deterministic memory trigger — the thin replacement for the LLM librarian.
//!
//! Cognee has no "librarian" agent: memory is `add → cognify → search` plus a
//! periodic `memify`. Phoenix follows the same shape. There is no model turn
//! deciding what to load or save; the runtime just **recalls** relevant graph
//! context before a turn and **remembers** the durable outcome after one. The
//! knowledge graph, entity extraction, and self-improvement all happen inside
//! Cognee (`src/librarian/cognee.rs`), driven by the user's configured provider
//! for cognify and local ONNX embeddings for vector recall.
//!
//! When the `cognee` feature is off, every call is a no-op so the crate still
//! builds and runs (with memory disabled) — there is no file-tier fallback.

use anyhow::Context as _;

/// The number of graph chunks pulled into a turn's context on recall. Small on
/// purpose: recall injects facts cheaply and the agent reasons over them.
const RECALL_TOP_K: usize = 8;

/// State reconciliation must still work while Cognee's asynchronous graph
/// pipeline is behind.  Raw notes are the durable source of truth, so scan a
/// bounded recent window and merge exact lexical matches with indexed search.
/// This is deliberately used only for progress/status queries.
#[cfg(feature = "cognee")]
const RAW_STATE_CANDIDATE_LIMIT: usize = 4_096;
#[cfg(feature = "cognee")]
const RAW_STATE_FILE_MAX_BYTES: u64 = 128 * 1024;
#[cfg(feature = "cognee")]
const RAW_STATE_TOTAL_MAX_BYTES: u64 = 16 * 1024 * 1024;
#[cfg(feature = "cognee")]
const RAW_STATE_EXCERPT_CHARS: usize = 6_000;

fn recall_words(text: &str) -> std::collections::HashSet<String> {
    const STOP: &[&str] = &[
        "what",
        "when",
        "where",
        "which",
        "with",
        "that",
        "this",
        "have",
        "has",
        "had",
        "does",
        "did",
        "doing",
        "done",
        "finished",
        "complete",
        "completed",
        "status",
        "next",
        "today",
        "already",
        "about",
        "from",
        "into",
        "user",
        "think",
        "thinks",
    ];
    text.split(|ch: char| !ch.is_alphanumeric())
        .map(|word| word.to_ascii_lowercase())
        .filter(|word| word.chars().count() >= 3 && !STOP.contains(&word.as_str()))
        .collect()
}

fn state_sensitive_query(query: &str) -> bool {
    let lower = query.to_ascii_lowercase();
    [
        "complete",
        "finished",
        "done",
        "submitted",
        "passed",
        "grade",
        "assessment",
        "assignment",
        "lesson",
        "progress",
        "status",
        "next",
        "today",
        "schedule",
        "plan",
    ]
    .iter()
    .any(|term| lower.contains(term))
}

fn rank_recall_chunks(query: &str, chunks: Vec<String>, top_k: usize) -> Vec<String> {
    let query_words = recall_words(query);
    let mut seen = std::collections::HashSet::new();
    let mut scored = chunks
        .into_iter()
        .enumerate()
        .filter_map(|(index, chunk)| {
            let identity = chunk
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_ascii_lowercase();
            if !seen.insert(identity) {
                return None;
            }
            let lower = chunk.to_ascii_lowercase();
            let overlap = recall_words(&chunk).intersection(&query_words).count();
            let state_evidence = [
                "marked complete",
                "verified complete",
                "already completed",
                "already finished",
                "submitted",
                "grade:",
                "gradebook",
                "passed",
                "no more",
                "next science",
            ]
            .iter()
            .filter(|term| lower.contains(*term))
            .count();
            // A short, recent correction is usually less lexically dense than an
            // old multi-paragraph plan.  Give provenance-bearing state language
            // enough weight to beat that verbosity: the user saying something is
            // complete (and especially externally verified) is authoritative,
            // while availability and scheduling prose is not completion evidence.
            let authoritative_state = [
                "user explicitly corrected",
                "newest explicit",
                "explicit completion correction",
                "externally verified",
                "do not reopen",
                "must not reopen",
                "are complete",
                "is complete",
            ]
            .iter()
            .filter(|term| lower.contains(*term))
            .count();
            Some((
                overlap * 100 + state_evidence * 25 + authoritative_state * 100,
                index,
                chunk,
            ))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
    scored
        .into_iter()
        .take(top_k)
        .map(|(_, _, chunk)| chunk)
        .collect()
}

#[cfg(feature = "cognee")]
async fn search_state_hybrid(
    mem: &crate::librarian::cognee::CogneeMemory,
    query: &str,
    top_k: usize,
    datasets: Option<&[String]>,
) -> anyhow::Result<Vec<String>> {
    let candidate_k = top_k.saturating_mul(3).max(top_k);
    let raw_query = query.to_string();
    let raw_datasets = datasets.map(|names| names.to_vec());
    let raw = tokio::task::spawn_blocking(move || {
        raw_state_candidates(&raw_query, candidate_k, raw_datasets.as_deref())
    });
    let (semantic, lexical, raw) = tokio::join!(
        mem.search_scoped(query, "CHUNKS", candidate_k, datasets),
        mem.search_scoped(query, "CHUNKS_LEXICAL", candidate_k, datasets),
        raw,
    );
    let mut chunks = Vec::new();
    let mut errors = Vec::new();
    match semantic {
        Ok(found) => chunks.extend(found),
        Err(error) => errors.push(format!("semantic: {error:#}")),
    }
    match lexical {
        Ok(found) => chunks.extend(found),
        Err(error) => errors.push(format!("lexical: {error:#}")),
    }
    match raw {
        Ok(Ok(found)) => chunks.extend(found),
        Ok(Err(error)) => errors.push(format!("durable raw state: {error:#}")),
        Err(error) => errors.push(format!("durable raw state worker: {error}")),
    }
    if chunks.is_empty() && errors.len() == 3 {
        anyhow::bail!("all state-recall paths failed ({})", errors.join("; "));
    }
    Ok(rank_recall_chunks(query, chunks, top_k))
}

#[cfg(feature = "cognee")]
fn raw_state_candidates(
    query: &str,
    top_k: usize,
    datasets: Option<&[String]>,
) -> anyhow::Result<Vec<String>> {
    raw_state_candidates_at(
        &crate::config::phoenix_cognee_root(),
        query,
        top_k,
        datasets,
    )
}

#[cfg(feature = "cognee")]
fn raw_state_candidates_at(
    root: &std::path::Path,
    query: &str,
    top_k: usize,
    datasets: Option<&[String]>,
) -> anyhow::Result<Vec<String>> {
    use rusqlite::OpenFlags;

    let db = root.join("cognee.db");
    let metadata = match std::fs::symlink_metadata(&db) {
        Ok(metadata) if metadata.file_type().is_file() => metadata,
        Ok(_) => anyhow::bail!("memory catalog is not a regular file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("reading memory catalog metadata"),
    };
    if metadata.len() > 512 * 1024 * 1024 {
        anyhow::bail!("memory catalog exceeds the 512 MiB read-side limit");
    }
    let conn = rusqlite::Connection::open_with_flags(
        &db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening durable memory catalog {}", db.display()))?;
    let mut stmt = conn.prepare(
        "SELECT DISTINCT d.raw_data_location, d.created_at, ds.name \
         FROM data d \
         JOIN dataset_data dd ON dd.data_id = d.id \
         JOIN datasets ds ON ds.id = dd.dataset_id \
         ORDER BY d.created_at DESC \
         LIMIT ?1",
    )?;
    let rows = stmt.query_map([RAW_STATE_CANDIDATE_LIMIT as i64], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let allowed = datasets.map(|names| names.iter().collect::<std::collections::HashSet<_>>());
    let data_root = root
        .join("data")
        .canonicalize()
        .unwrap_or_else(|_| root.join("data"));
    let query_words = recall_words(query);
    let mut total_bytes = 0_u64;
    let mut texts = Vec::new();
    for row in rows {
        let (location, _created_at, dataset) = row?;
        if allowed
            .as_ref()
            .is_some_and(|names| !names.contains(&dataset))
        {
            continue;
        }
        let Ok(url) = url::Url::parse(&location) else {
            continue;
        };
        let Ok(path) = url.to_file_path() else {
            continue;
        };
        let Ok(file_meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !file_meta.file_type().is_file() || file_meta.len() > RAW_STATE_FILE_MAX_BYTES {
            continue;
        }
        let Ok(canonical) = path.canonicalize() else {
            continue;
        };
        if !canonical.starts_with(&data_root) {
            continue;
        }
        if total_bytes.saturating_add(file_meta.len()) > RAW_STATE_TOTAL_MAX_BYTES {
            break;
        }
        total_bytes += file_meta.len();
        let Ok(text) = std::fs::read_to_string(&canonical) else {
            continue;
        };
        if recall_words(&text).intersection(&query_words).count() < 2 {
            continue;
        }
        texts.push(text.chars().take(RAW_STATE_EXCERPT_CHARS).collect());
    }
    Ok(rank_recall_chunks(query, texts, top_k))
}

/// The shared team dataset — every agent recalls it; only Phoenix (or the
/// indexer's explicit TEAM routing) writes it. The legacy single dataset
/// (`phoenix`) is renamed to this once at startup, so all pre-scoping
/// memories stay team-visible (user decision 2026-07-18: cross-project
/// sharing is FINE — the store is a whole-computer assistant's memory; the
/// bug was that nothing recorded WHO learned WHAT).
pub const TEAM_DATASET: &str = "team";

/// Pre-scoping team dataset name. Most stores are renamed in place, but a
/// store that already contains both names cannot be renamed without merging
/// user data. Reads include this dataset only when it still exists.
#[cfg(feature = "cognee")]
const LEGACY_TEAM_DATASET: &str = "phoenix";

/// A memory scope = one cognee dataset inside the single store.
///
/// `Team` is the cross-agent tier. `Agent(role)` is a persona's own memory
/// (keyed by ROLE — stable across persona renames). Recall for an agent
/// always spans `[its own scope, team]`; writes go where the caller (or the
/// lib-beat indexer) routes them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoryScope {
    Team,
    Agent(String),
}

impl MemoryScope {
    /// Scope for an agent role label ("coder", "planner", …). Instance
    /// suffixes ("#2") and display decorations are stripped by the caller;
    /// this normalizes case/whitespace only.
    pub fn agent(role: &str) -> Self {
        let role = role.trim().to_ascii_lowercase();
        if role.is_empty() || role == "team" {
            return MemoryScope::Team;
        }
        MemoryScope::Agent(role)
    }

    /// The cognee dataset name backing this scope.
    pub fn dataset(&self) -> String {
        match self {
            MemoryScope::Team => TEAM_DATASET.to_string(),
            MemoryScope::Agent(role) => format!("agent-{role}"),
        }
    }

    /// The dataset filter for RECALL in this scope: the agent's own memory
    /// plus the shared team tier (team recalls only team).
    pub fn recall_datasets(&self) -> Vec<String> {
        match self {
            MemoryScope::Team => vec![TEAM_DATASET.to_string()],
            MemoryScope::Agent(_) => vec![self.dataset(), TEAM_DATASET.to_string()],
        }
    }
}

/// Provenance stamp — the "who is who" fix. Every note carries who learned
/// it and for which scope, so a recalled chunk can never be mistaken for the
/// recalling agent's own first-person history.
pub fn stamp(note: &str, learned_by: &str, scope: &MemoryScope) -> String {
    let tier = match scope {
        MemoryScope::Team => "team-wide".to_string(),
        MemoryScope::Agent(role) => format!("{role}'s own memory"),
    };
    format!("[from {learned_by} — {tier}] {note}")
}

/// Hard ceilings so memory can NEVER wedge a turn. The vendored engine was
/// observed deadlocking inside `add` (2026-07-02: every thread futex-parked,
/// no network, fresh store) — an unbounded await there would freeze preload,
/// turn-end saves, and the memory_recall tool forever. Timeouts degrade to
/// the same honest no-op as any other memory failure.
#[cfg(feature = "cognee")]
const RECALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
#[cfg(feature = "cognee")]
const SEARCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
#[cfg(feature = "cognee")]
const REMEMBER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
#[cfg(feature = "cognee")]
const GRAPH_EXPORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Honest, non-fatal receipt for a memory save attempt.
///
/// Memory is deliberately best-effort, so callers should not fail a turn when
/// this is anything other than [`Stored`](Self::Stored). They can, however,
/// report that the durable note did not land instead of claiming success after
/// a timeout or unavailable store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RememberOutcome {
    Stored,
    Dropped,
    Unavailable(String),
    TimedOut(String),
    Error(String),
}

impl RememberOutcome {
    pub fn is_stored(&self) -> bool {
        matches!(self, Self::Stored)
    }
}

impl std::fmt::Display for RememberOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stored => formatter.write_str("stored"),
            Self::Dropped => formatter.write_str("dropped (empty note)"),
            Self::Unavailable(reason) => write!(formatter, "unavailable: {reason}"),
            Self::TimedOut(reason) => write!(formatter, "timed out: {reason}"),
            Self::Error(reason) => write!(formatter, "error: {reason}"),
        }
    }
}

/// Honest outcome for an explicit `memory_recall` tool search. Preload recall
/// intentionally degrades when memory is unavailable, but a user/model-issued
/// search must distinguish a genuine zero-hit result from a broken index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchOutcome {
    Hits(Vec<String>),
    Empty,
    Unavailable(String),
    TimedOut(String),
    Error(String),
}

impl SearchOutcome {
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Hits(_) | Self::Empty)
    }
}

impl std::fmt::Display for SearchOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Hits(chunks) => write!(formatter, "{} chunk(s)", chunks.len()),
            Self::Empty => formatter.write_str("no matching chunks"),
            Self::Unavailable(reason) => write!(formatter, "memory unavailable: {reason}"),
            Self::TimedOut(reason) => write!(formatter, "memory search timed out: {reason}"),
            Self::Error(reason) => write!(formatter, "memory search failed: {reason}"),
        }
    }
}

/// One `memory:` line through the canonical gateway-log gate. A recall that FAILS
/// must be distinguishable from one that found nothing: for days the read side
/// was silently dead (a result-shape mismatch parsed every hit to empty) and
/// nothing anywhere said so — the receipts claimed "cold or empty store" while
/// the store held 128 records. Failures are turn-degrading events the user and
/// the next forensic pass must be able to see. Runtime owns timestamping,
/// private permissions, cross-process serialization, and bounded rotation.
#[cfg(feature = "cognee")]
fn mlog(line: &str) {
    crate::runtime::gwlog(&format!("memory: {line}"));
}

/// Recall the most relevant remembered context for a task, as plain text chunks
/// ready to inject into the turn prompt. Never errors out a turn — a cold store,
/// a missing model, or a provider hiccup degrades to an empty list.
#[cfg(feature = "cognee")]
pub async fn recall(query: &str) -> Vec<String> {
    recall_scoped(query, &MemoryScope::Team).await
}

/// Scope-filtered recall: the agent's own dataset + the team tier. This is
/// what turn preloads and the memory_recall tool use — an agent recalls what
/// IT learned plus what the team shares, never another agent's private lane.
#[cfg(feature = "cognee")]
pub async fn recall_scoped(query: &str, scope: &MemoryScope) -> Vec<String> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let datasets = recall_datasets_with_legacy(scope);
    match tokio::time::timeout(RECALL_TIMEOUT, async {
        // Opening Cognee initializes its local ONNX embedder on a cold
        // process. That is synchronous CPU work and used to run directly on a
        // Tokio worker, so the surrounding timeout could not fire: every new
        // Phoenix process appeared to "think" for 30-60 seconds before the
        // first model call. Keep the singleton, but initialize it on the
        // blocking pool so callers can enforce a real deadline.
        let mem = open_memory_async().await?;
        if state_sensitive_query(query) {
            search_state_hybrid(&mem, query, RECALL_TOP_K, Some(&datasets)).await
        } else {
            mem.search_scoped(query, "CHUNKS", RECALL_TOP_K, Some(&datasets))
                .await
        }
    })
    .await
    {
        Ok(Ok(chunks)) => chunks,
        Ok(Err(error)) => {
            tracing::warn!("Phoenix memory recall FAILED — turn proceeds memory-blind: {error:#}");
            mlog(&format!(
                "recall FAILED — this turn ran memory-blind: {error:#}"
            ));
            Vec::new()
        }
        Err(_) => {
            tracing::warn!(
                        "Phoenix memory recall timed out after {}s — memory degraded to empty for this turn",
                        RECALL_TIMEOUT.as_secs()
                    );
            mlog(&format!(
                "recall TIMED OUT after {}s — this turn ran memory-blind",
                RECALL_TIMEOUT.as_secs()
            ));
            Vec::new()
        }
    }
}

/// Persist a durable learning into memory. Graph indexing is deferred so a
/// slow provider cannot turn a successful ingest into a foreground timeout.
/// Failures never fail the turn, but the receipt lets callers report them.
#[cfg(feature = "cognee")]
pub async fn remember(text: &str) -> RememberOutcome {
    remember_scoped(text, &MemoryScope::Team).await
}

/// Persist into a specific memory scope (team or an agent's own dataset).
#[cfg(feature = "cognee")]
pub async fn remember_scoped(text: &str, scope: &MemoryScope) -> RememberOutcome {
    let text = text.trim();
    if text.is_empty() {
        return RememberOutcome::Dropped;
    }
    let dataset = scope.dataset();
    match open_memory() {
        Ok(mem) => {
            // `remember_into` performs durable add + indexing re-arm. It uses
            // the same lease as backlog maintenance because Cognee's advisory
            // pipeline row is store-wide, not per caller, but it does not run
            // the slow provider-driven cognify phase in this foreground call.
            let deadline = std::time::Instant::now() + REMEMBER_TIMEOUT;
            let mut lease = match acquire_cognify_lease_until(deadline, "Phoenix memory save").await
            {
                Ok(lease) => lease,
                Err(error) => {
                    tracing::warn!("{error:#} — this note was dropped, the turn continues");
                    return RememberOutcome::TimedOut(format!("{error:#}"));
                }
            };
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                tracing::warn!(
                    "Phoenix memory save timed out after {}s waiting for the memory pipeline — this note was dropped, the turn continues",
                    REMEMBER_TIMEOUT.as_secs()
                );
                return RememberOutcome::TimedOut(format!(
                    "waited {}s for the memory pipeline",
                    REMEMBER_TIMEOUT.as_secs()
                ));
            }
            match lease
                .run_with_timeout(left, mem.remember_into(text, &dataset))
                .await
            {
                Ok(Ok(())) => RememberOutcome::Stored,
                Ok(Err(error)) => {
                    tracing::debug!("Phoenix memory save failed: {error:#}");
                    RememberOutcome::Error(format!("{error:#}"))
                }
                Err(_) => {
                    recover_timed_out_pipeline_state(&lease, "Phoenix memory save");
                    tracing::warn!(
                        "Phoenix memory save timed out after {}s — durability is unknown, the turn continues",
                        REMEMBER_TIMEOUT.as_secs()
                    );
                    RememberOutcome::TimedOut(format!(
                        "memory pipeline exceeded {}s",
                        REMEMBER_TIMEOUT.as_secs()
                    ))
                }
            }
        }
        Err(error) => {
            tracing::debug!("Phoenix memory unavailable for save: {error:#}");
            RememberOutcome::Unavailable(format!("{error:#}"))
        }
    }
}

/// Typed graph search for the `memory_recall` tool: the full upstream
/// SearchType surface. Unlike preload recall, this returns a typed outcome so
/// the explicit tool never reports an unavailable index as a genuine miss.
#[cfg(feature = "cognee")]
pub async fn search(query: &str, search_type: &str, top_k: usize) -> SearchOutcome {
    search_in(query, search_type, top_k, None).await
}

/// Typed search, optionally scope-filtered. `scope: None` searches the WHOLE
/// store (the memory_recall tool's `all_agents` escape hatch — "did anyone
/// ever…"); `Some(scope)` filters to that agent + team.
#[cfg(feature = "cognee")]
pub async fn search_in(
    query: &str,
    search_type: &str,
    top_k: usize,
    scope: Option<&MemoryScope>,
) -> SearchOutcome {
    let query = query.trim();
    if query.is_empty() {
        return SearchOutcome::Empty;
    }
    let datasets = scope.map(recall_datasets_with_legacy);
    match tokio::time::timeout(SEARCH_TIMEOUT, async {
        let mem = open_memory_async().await?;
        if search_type.eq_ignore_ascii_case("CHUNKS") && state_sensitive_query(query) {
            search_state_hybrid(&mem, query, top_k, datasets.as_deref()).await
        } else {
            mem.search_scoped(query, search_type, top_k, datasets.as_deref())
                .await
        }
    })
    .await
    {
        Ok(Ok(chunks)) if chunks.is_empty() => SearchOutcome::Empty,
        Ok(Ok(chunks)) => SearchOutcome::Hits(chunks),
        Ok(Err(error)) => {
            tracing::warn!("Phoenix memory search ({search_type}) FAILED: {error:#}");
            mlog(&format!(
                "search ({search_type}) FAILED — returned empty: {error:#}"
            ));
            SearchOutcome::Error(format!("{error:#}"))
        }
        Err(_) => {
            tracing::warn!(
                "Phoenix memory search timed out after {}s — returned no chunks",
                SEARCH_TIMEOUT.as_secs()
            );
            mlog(&format!(
                "search ({search_type}) TIMED OUT after {}s — returned empty",
                SEARCH_TIMEOUT.as_secs()
            ));
            SearchOutcome::TimedOut(format!("exceeded {}s", SEARCH_TIMEOUT.as_secs()))
        }
    }
}

/// Generous bound for indexing a large backlog: cognify runs several
/// reasoning-model calls per digest, so 89 queued items legitimately need
/// tens of minutes. Never held under the turn lock (see callers).
// Background indexing is best-effort desktop maintenance, never foreground
// work. A damaged or oversized graph must yield quickly instead of occupying
// CPU and the memory store for most of an hour.
const BACKLOG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3 * 60);

/// The scheduled pass must have room for the full 45-minute multi-dataset
/// cognify budget *and* the memify indexing phase that follows it.
#[cfg(feature = "cognee")]
const MAINTENANCE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// One graph-writing pipeline at a time from THIS process. Cognee's advisory
/// pipeline row is shared by ordinary saves, backlog indexing, and memify, so
/// every one of those operations must pass through this coordinator.
///
/// The live marker is deliberately coupled to the mutex. Recovery takes a
/// [`CognifyLease`] and refuses to touch rows while its RAII pipeline marker is
/// live; a PID scan can therefore never be the thing protecting a sibling task
/// in this process.
#[cfg(feature = "cognee")]
struct CognifyLock {
    mutex: tokio::sync::Mutex<()>,
    pipeline_live: std::sync::atomic::AtomicBool,
}

#[cfg(feature = "cognee")]
impl CognifyLock {
    const fn new() -> Self {
        Self {
            mutex: tokio::sync::Mutex::const_new(()),
            pipeline_live: std::sync::atomic::AtomicBool::new(false),
        }
    }

    async fn acquire(&self) -> CognifyLease<'_> {
        CognifyLease {
            lock: self,
            _guard: self.mutex.lock().await,
            #[cfg(unix)]
            _process_guard: None,
        }
    }
}

#[cfg(feature = "cognee")]
struct CognifyLease<'a> {
    lock: &'a CognifyLock,
    _guard: tokio::sync::MutexGuard<'a, ()>,
    /// Cross-process counterpart to `_guard`. Every Phoenix writer holds the
    /// same advisory file lock for the full pipeline lifetime, so stale-row
    /// recovery never relies only on a best-effort `/proc` scan.
    #[cfg(unix)]
    _process_guard: Option<PipelineProcessGuard>,
}

/// An explicit unlock is important in a process that launches helpers. A
/// child can briefly inherit an open-file description between `fork` and
/// `exec`; merely dropping our `File` would then leave the advisory lock held
/// until that child reaches `exec`. `LOCK_UN` releases the shared lock state
/// immediately and keeps both runtime hand-offs and parallel tests
/// deterministic. `O_CLOEXEC` remains the second line of defence.
#[cfg(all(feature = "cognee", unix))]
struct PipelineProcessGuard(std::fs::File);

#[cfg(all(feature = "cognee", unix))]
impl Drop for PipelineProcessGuard {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        // Drop cannot report an error. The descriptor is closed immediately
        // afterwards, so a failed explicit unlock still receives normal
        // kernel cleanup.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[cfg(feature = "cognee")]
impl CognifyLease<'_> {
    /// Run one pipeline future while an RAII marker records that its work is
    /// live. `tokio::time::timeout` drops the future before returning `Err`;
    /// the marker then drops before recovery can inspect the lease.
    async fn run_with_timeout<F>(
        &mut self,
        timeout: std::time::Duration,
        future: F,
    ) -> Result<F::Output, tokio::time::error::Elapsed>
    where
        F: std::future::Future,
    {
        let _live = CognifyPipelineLive::begin(self.lock);
        tokio::time::timeout(timeout, future).await
    }

    fn pipeline_is_live(&self) -> bool {
        self.lock
            .pipeline_live
            .load(std::sync::atomic::Ordering::Acquire)
    }
}

#[cfg(feature = "cognee")]
struct CognifyPipelineLive<'a> {
    lock: &'a CognifyLock,
}

#[cfg(feature = "cognee")]
impl<'a> CognifyPipelineLive<'a> {
    fn begin(lock: &'a CognifyLock) -> Self {
        let was_live = lock
            .pipeline_live
            .swap(true, std::sync::atomic::Ordering::AcqRel);
        assert!(
            !was_live,
            "COGNIFY_LOCK invariant violated: overlapping same-process pipelines"
        );
        Self { lock }
    }
}

#[cfg(feature = "cognee")]
impl Drop for CognifyPipelineLive<'_> {
    fn drop(&mut self) {
        let was_live = self
            .lock
            .pipeline_live
            .swap(false, std::sync::atomic::Ordering::AcqRel);
        debug_assert!(was_live, "cognify pipeline marker dropped while idle");
    }
}

#[cfg(feature = "cognee")]
static COGNIFY_LOCK: CognifyLock = CognifyLock::new();

#[cfg(feature = "cognee")]
async fn acquire_cognify_lease_until(
    deadline: std::time::Instant,
    operation: &str,
) -> anyhow::Result<CognifyLease<'static>> {
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    if left.is_zero() {
        anyhow::bail!("{operation} timed out waiting for the memory pipeline");
    }
    let mut lease = tokio::time::timeout(left, COGNIFY_LOCK.acquire())
        .await
        .map_err(|_| anyhow::anyhow!("{operation} timed out waiting for the memory pipeline"))?;

    #[cfg(unix)]
    loop {
        let lock_path = crate::config::phoenix_cognee_root().join(".pipeline.lock");
        if let Some(file) = try_lock_pipeline_at(&lock_path)? {
            lease._process_guard = Some(file);
            break;
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("{operation} timed out waiting for another Phoenix memory process");
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    Ok(lease)
}

#[cfg(all(feature = "cognee", unix))]
fn try_lock_pipeline_at(path: &std::path::Path) -> anyhow::Result<Option<PipelineProcessGuard>> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    crate::config::private_io::prepare_private_parent(path)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("failed to open memory pipeline lock {}", path.display()))?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to secure memory pipeline lock {}", path.display()))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(Some(PipelineProcessGuard(file)));
    }
    let error = std::io::Error::last_os_error();
    if error
        .raw_os_error()
        .is_some_and(|code| code == libc::EWOULDBLOCK || code == libc::EAGAIN)
    {
        return Ok(None);
    }
    Err(error).with_context(|| format!("failed to lock memory pipeline at {}", path.display()))
}

/// Cognify the deferred backlog ONLY (no full memify): index saved-but-
/// unprocessed texts so they become searchable. Called after digest and
/// session-boundary work writes new notes. Near-free when the backlog is empty.
#[cfg(feature = "cognee")]
pub async fn cognify_backlog() -> anyhow::Result<String> {
    let deadline = std::time::Instant::now() + BACKLOG_TIMEOUT;
    let mut lease = acquire_cognify_lease_until(deadline, "backlog cognify").await?;
    let receipt = cognify_backlog_under_lease(&mut lease, deadline).await?;
    if let Err(error) = after_releasing_cognify_lease(lease, graph_export()).await {
        tracing::warn!("graph export after cognify failed: {error}");
    }
    Ok(receipt)
}

/// Backlog implementation for callers that already own [`COGNIFY_LOCK`].
/// Keeping acquisition out of this helper lets explicit maintenance perform
/// its guarded stale-row repair and then continue without locking itself.
#[cfg(feature = "cognee")]
async fn cognify_backlog_under_lease(
    lease: &mut CognifyLease<'_>,
    deadline: std::time::Instant,
) -> anyhow::Result<String> {
    // A crashed/timed-out client leaves the pipeline stuck at
    // DATASET_PROCESSING_STARTED forever ("already running" on every retry,
    // live 2026-07-04). Age-gated so a genuinely live run in ANOTHER process
    // is never clobbered.
    clear_stale_cognify_locks(lease);
    let mem = open_memory()?;
    // Cognify EVERY scope with pending data — a single-dataset pass would
    // leave agent partitions saved-but-unsearchable (the 2026-07-04 disease).
    // One shared wall-clock budget across the loop.
    let mut receipts: Vec<String> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    for dataset in list_datasets()? {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            anyhow::bail!(
                "backlog cognify timed out after {}m — remaining scopes retry on the next digest",
                BACKLOG_TIMEOUT.as_secs() / 60
            );
        }
        match lease
            .run_with_timeout(left, mem.cognify_pending_in(&dataset))
            .await
        {
            Ok(Ok(receipt)) => receipts.push(format!("{dataset}: {receipt}")),
            Ok(Err(error)) => failures.push(format!("{dataset}: {error:#}")),
            Err(_) => {
                // Our own abandonment strands the lock — release it for the
                // next attempt instead of poisoning every future run.
                recover_timed_out_pipeline_state(lease, "backlog cognify");
                anyhow::bail!(
                    "backlog cognify timed out after {}m (in `{dataset}`) — will retry on the next digest",
                    BACKLOG_TIMEOUT.as_secs() / 60
                )
            }
        }
    }
    if !failures.is_empty() {
        anyhow::bail!(
            "backlog cognify failed in {} scope(s) after processing the full store: {}",
            failures.len(),
            failures.join("; ")
        );
    }
    Ok(receipts.join("; "))
}

/// Make the lock boundary mechanical: graph export is a read-side dashboard
/// refresh, not part of Cognee's serialized write pipeline. It must begin only
/// after the lease is gone so a wedged graph traversal cannot block saves or
/// the next maintenance pass.
#[cfg(feature = "cognee")]
async fn after_releasing_cognify_lease<T, F>(lease: CognifyLease<'_>, future: F) -> T
where
    F: std::future::Future<Output = T>,
{
    drop(lease);
    future.await
}

/// Mark long-stale in-flight cognify pipeline runs as errored so they stop
/// blocking new runs. cognee has no API for this; the status lives in its
/// relational store. Stale = still "STARTED" after 65+ minutes (longer than
/// the largest 60-minute bounded run we launch). Best-effort — a
/// locked/absent DB is a no-op.
#[cfg(feature = "cognee")]
fn clear_stale_cognify_locks(lease: &CognifyLease<'_>) {
    if lease.pipeline_is_live() {
        tracing::error!("refusing stale cognify cleanup while a same-process pipeline is live");
        return;
    }
    let db = crate::config::phoenix_cognee_root().join("cognee.db");
    let Ok(conn) = rusqlite::Connection::open(&db) else {
        return;
    };
    let cleared = conn
        .execute(
            "UPDATE pipeline_runs SET status = 'DATASET_PROCESSING_ERRORED', \
             run_info = '{\"error\":\"stale in-flight run auto-cleared (client died or timed out)\"}' \
             WHERE status = 'DATASET_PROCESSING_STARTED' \
             AND replace(substr(created_at, 1, 19), 'T', ' ') < datetime('now', '-65 minutes')",
            [],
        )
        .unwrap_or(0);
    if cleared > 0 {
        tracing::info!("cleared {cleared} stale cognify pipeline lock(s)");
    }
}

/// Backlog cognify for the explicit `phoenix memory --maintain` path. The
/// 65-minute age gate in [`clear_stale_cognify_locks`] protects daemon-side
/// callers from clobbering a live run — but it also means a user standing at
/// the CLI after a crashed run waits most of an hour for a dead advisory row
/// to expire (live 2026-07-06: the 12:54 run died, 13:19 maintain refused).
/// Here we sweep in-flight rows of ANY age, guarded by the real mutual
/// exclusion: the graph store is single-writer, so if no other process holds
/// it open, no cognify is genuinely running.
#[cfg(feature = "cognee")]
pub async fn cognify_backlog_now() -> anyhow::Result<String> {
    let deadline = std::time::Instant::now() + BACKLOG_TIMEOUT;
    let mut lease = acquire_cognify_lease_until(deadline, "explicit backlog cognify").await?;
    if graph_store_held_elsewhere() {
        anyhow::bail!(
            "another process is holding the memory graph open — a cognify run \
             is likely live; retry when it finishes (or stop the gateway)"
        );
    }
    clear_inflight_cognify_locks(
        &lease,
        "in-flight run cleared by explicit maintenance (no live writer held the graph)",
    );
    let receipt = cognify_backlog_under_lease(&mut lease, deadline).await?;
    if let Err(error) = after_releasing_cognify_lease(lease, graph_export()).await {
        tracing::warn!("graph export after explicit cognify failed: {error}");
    }
    Ok(receipt)
}

/// True when a process other than us has the cognee graph store open
/// (checked via /proc fd scan; best-effort, absent /proc = assume unheld).
#[cfg(feature = "cognee")]
fn graph_store_held_elsewhere() -> bool {
    let graph = crate::config::phoenix_cognee_root().join("system/graph");
    let graph = graph.canonicalize().unwrap_or(graph);
    let me = std::process::id().to_string();
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return false;
    };
    for proc_entry in procs.flatten() {
        let pid = proc_entry.file_name();
        let pid = pid.to_string_lossy();
        if !pid.chars().all(|c| c.is_ascii_digit()) || pid == me {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(proc_entry.path().join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            if std::fs::read_link(fd.path())
                .is_ok_and(|target| fd_target_belongs_to_graph(&target, &graph))
            {
                return true;
            }
        }
    }
    false
}

#[cfg(feature = "cognee")]
fn fd_target_belongs_to_graph(target: &std::path::Path, graph: &std::path::Path) -> bool {
    // Ladybug opens files *inside* its database directory. Checking only the
    // directory inode misses every normal live connection and lets explicit
    // maintenance clear pipeline rows underneath another process.
    target == graph || target.starts_with(graph)
}

/// Sweep in-flight cognify rows regardless of age. Only safe once the caller
/// has established no other process is writing the graph (see
/// [`cognify_backlog_now`]).
#[cfg(feature = "cognee")]
fn clear_inflight_cognify_locks(lease: &CognifyLease<'_>, reason: &str) {
    let cleared =
        clear_inflight_cognify_locks_at(lease, &crate::config::phoenix_cognee_root(), reason);
    if cleared > 0 {
        tracing::info!("maintenance cleared {cleared} in-flight cognify pipeline row(s)");
    }
}

/// Recover rows left in an in-flight state when one of our bounded pipeline
/// futures is cancelled by a timeout. Dropping the future cannot run Cognee's
/// normal `ERRORED` watcher hook, so without this repair the next maintenance
/// pass sees "pipeline already running" forever. The required lease plus its
/// RAII activity marker prove no sibling task in this process is live; the
/// `/proc` check separately protects a writer in another Phoenix process.
#[cfg(feature = "cognee")]
fn recover_timed_out_pipeline_state(lease: &CognifyLease<'_>, operation: &str) {
    if lease.pipeline_is_live() {
        // This guard is the safety invariant, not a PID heuristic. It also
        // makes accidental future recovery calls from inside a live pipeline
        // fail closed instead of corrupting that pipeline's advisory row.
        tracing::error!(
            "{operation} requested timeout recovery while a same-process pipeline is live; leaving in-flight rows intact"
        );
        return;
    }
    if graph_store_held_elsewhere() {
        tracing::warn!(
            "{operation} timed out, but another process still holds the memory graph; leaving in-flight pipeline rows intact"
        );
        return;
    }
    clear_inflight_cognify_locks(
        lease,
        &format!("{operation} timed out; cancelled in-flight run auto-cleared"),
    );
}

#[cfg(feature = "cognee")]
fn clear_inflight_cognify_locks_at(
    lease: &CognifyLease<'_>,
    root: &std::path::Path,
    reason: &str,
) -> usize {
    if lease.pipeline_is_live() {
        tracing::error!("refusing in-flight cognify cleanup while a same-process pipeline is live");
        return 0;
    }
    let db = root.join("cognee.db");
    let Ok(conn) = rusqlite::Connection::open(&db) else {
        return 0;
    };
    let run_info = serde_json::json!({ "error": reason }).to_string();
    conn.execute(
        "UPDATE pipeline_runs SET status = 'DATASET_PROCESSING_ERRORED', \
             run_info = ?1 \
             WHERE status IN ('DATASET_PROCESSING_STARTED', 'DATASET_PROCESSING_INITIATED')",
        [run_info],
    )
    .unwrap_or(0)
}

#[cfg(not(feature = "cognee"))]
pub async fn cognify_backlog() -> anyhow::Result<String> {
    Ok("Phoenix semantic memory is disabled in this minimal build".to_string())
}

#[cfg(not(feature = "cognee"))]
pub async fn cognify_backlog_now() -> anyhow::Result<String> {
    Ok("Phoenix semantic memory is disabled in this minimal build".to_string())
}

/// Run Cognee's maintenance pass: cognify any deferred backlog (saves made
/// while no provider was usable), then memify (triplet embeddings over the
/// whole graph). Called from the daemon's 12h timer. Errors bubble up so the
/// daemon can log the failure — but never crash.
#[cfg(feature = "cognee")]
pub async fn memify() -> anyhow::Result<String> {
    // One deadline includes time queued behind an ordinary save or another
    // maintenance pass. Once acquired, the lease is passed through directly;
    // helpers never recursively acquire it and therefore cannot self-deadlock.
    let deadline = std::time::Instant::now() + MAINTENANCE_TIMEOUT;
    let mut lease = acquire_cognify_lease_until(deadline, "scheduled memify maintenance").await?;
    clear_stale_cognify_locks(&lease);
    let mem = open_memory()?;
    let pass = async {
        // Scope migration moved the shared dataset from `phoenix` to `team`,
        // and agent memories now live in their own datasets. Maintenance must
        // therefore drain every dataset in the store, not just the wrapper's
        // default, or non-default scopes remain saved but unsearchable.
        let mut backlog_receipts = Vec::new();
        let mut backlog_failures = Vec::new();
        for dataset in list_datasets()? {
            match mem.cognify_pending_in(&dataset).await {
                Ok(receipt) => backlog_receipts.push(format!("{dataset}: {receipt}")),
                Err(error) => backlog_failures.push(format!("{dataset}: {error:#}")),
            }
        }
        let backlog = backlog_receipts.join("; ");
        if !backlog_failures.is_empty() {
            anyhow::bail!(
                "maintenance cognify failed in {} scope(s) after processing the full store: {}; memify skipped until the backlog is healthy",
                backlog_failures.len(),
                backlog_failures.join("; ")
            );
        }
        let memify = mem.memify().await?;
        anyhow::Ok(format!("{backlog}; memify: {memify}"))
    };
    // One wall-clock budget covers every dataset plus memify. Maintenance is
    // incremental and retryable; keeping the desktop responsive is more
    // important than completing an oversized graph in one monopolizing pass.
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    if left.is_zero() {
        anyhow::bail!(
            "memify timed out after {}m waiting for the memory pipeline — maintenance will retry next cycle",
            MAINTENANCE_TIMEOUT.as_secs() / 60
        );
    }
    let receipt = match lease.run_with_timeout(left, pass).await {
        Ok(result) => result?,
        Err(_) => {
            recover_timed_out_pipeline_state(&lease, "scheduled memify maintenance");
            anyhow::bail!(
                "memify timed out after {}m — maintenance will retry next cycle",
                MAINTENANCE_TIMEOUT.as_secs() / 60
            )
        }
    };
    if let Err(error) = after_releasing_cognify_lease(lease, graph_export()).await {
        tracing::warn!("graph export after memify failed: {error}");
    }
    Ok(receipt)
}

/// Export the REAL Cognee knowledge graph (entities + relationship edges) to
/// `~/.phoenix/cognee/graph.json`, using the gateway's live singleton handle.
///
/// This is how the dashboard sees the true graph: ladybug is single-writer and
/// the gateway holds the handle for its whole lifetime (see `open_memory`), so
/// the canvas — a SEPARATE process — can't open the store itself (it would hit
/// a lock error and fall back to raw note files, which is the bug this fixes).
/// The gateway writes the graph here after each cognify/memify; the canvas
/// reads it. Best-effort: a failure logs and leaves the last export in place.
#[cfg(feature = "cognee")]
pub async fn graph_export() -> anyhow::Result<usize> {
    bounded_graph_export(GRAPH_EXPORT_TIMEOUT, graph_export_inner()).await
}

#[cfg(feature = "cognee")]
async fn bounded_graph_export<F>(timeout: std::time::Duration, future: F) -> anyhow::Result<usize>
where
    F: std::future::Future<Output = anyhow::Result<usize>>,
{
    match tokio::time::timeout(timeout, future).await {
        Ok(result) => result,
        Err(_) => anyhow::bail!(
            "graph export timed out after {}s; the previous dashboard snapshot remains available",
            timeout.as_secs()
        ),
    }
}

#[cfg(feature = "cognee")]
async fn graph_export_inner() -> anyhow::Result<usize> {
    let mem = open_memory()?;
    // Ladybug exposes an async trait, but its embedded database query is
    // synchronous. Keep that work off Tokio's worker threads; the vendored
    // adapter also applies a query-level timeout so dropping the outer future
    // cannot leave an unbounded database operation behind.
    let data = tokio::task::spawn_blocking(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("building graph-export runtime")?;
        runtime.block_on(mem.graph_data())
    })
    .await
    .context("graph-export worker failed")??;
    let n = data
        .get("nodes")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let path = crate::config::phoenix_cognee_root().join("graph.json");
    let encoded = serde_json::to_vec(&data)?;
    crate::config::private_io::atomic_write_private(&path, &encoded)?;
    Ok(n)
}

#[cfg(not(feature = "cognee"))]
pub async fn graph_export() -> anyhow::Result<usize> {
    Ok(0)
}

/// Open the Cognee engine from the live Phoenix config — as a PROCESS
/// SINGLETON. A fresh `open` per call built a new handle each time, which
/// re-initialized the ONNX embedder (the "Schema error: already registered"
/// wall in the gateway terminal) and multiplied cognify LLM clients. The
/// handle is config-fingerprinted: a config change on disk rebuilds it once.
#[cfg(feature = "cognee")]
fn open_memory() -> anyhow::Result<super::cognee::CogneeMemory> {
    use std::sync::Mutex;
    // Test builds must never touch the live store: the read side would feed
    // the user's real memories into assertions, and `remember` would pollute
    // the knowledge graph with fixtures — the receipts guard's twin.
    if crate::config::test_isolated_from_live_home() {
        anyhow::bail!(
            "test build: refusing to open the live memory store (set PHOENIX_HOME to a tempdir to opt in)"
        );
    }
    static CACHED: Mutex<Option<(String, super::cognee::CogneeMemory)>> = Mutex::new(None);
    let config = crate::config::PhoenixConfig::load()?;
    let cognee_config = super::cognee::CogneeConfig::from_phoenix_config(&config)?;
    let fingerprint = format!("{cognee_config:?}");
    let mut cached = CACHED.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((cached_fp, memory)) = cached.as_ref() {
        if *cached_fp == fingerprint {
            return Ok(memory.clone());
        }
    }
    migrate_legacy_dataset(&cognee_config.root);
    let memory = super::cognee::CogneeMemory::open(&cognee_config)?;
    *cached = Some((fingerprint, memory.clone()));
    Ok(memory)
}

/// Async boundary for the process-wide Cognee singleton. The singleton itself
/// deliberately stays synchronous, but its cold ONNX/database setup must never
/// occupy a runtime worker or make a Tokio timeout ineffective.
#[cfg(feature = "cognee")]
async fn open_memory_async() -> anyhow::Result<super::cognee::CogneeMemory> {
    tokio::task::spawn_blocking(open_memory)
        .await
        .context("memory initialization worker failed")?
}

/// One-time scoping migration: the pre-2026-07-18 store kept EVERYTHING in a
/// single `phoenix` dataset — rename it to the `team` scope so all existing
/// memories stay visible to every agent (they were shared before; scoping
/// must not orphan them) and so the recall filter always has a resolvable
/// dataset (an all-unresolved filter silently degrades to an unscoped
/// search). Renaming by NAME keeps the dataset ID stable, which is what the
/// engine's scoping actually matches on. Idempotent; direct sqlite because
/// cognee exposes no dataset-rename op (precedent: clear_stale_cognify_locks).
#[cfg(feature = "cognee")]
fn migrate_legacy_dataset(root: &std::path::Path) {
    static COMPLETED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if COMPLETED.load(std::sync::atomic::Ordering::Acquire) {
        return;
    }
    let db = root.join("cognee.db");
    if !db.exists() {
        return; // fresh store — nothing to migrate (do not latch yet)
    }
    let conn = match rusqlite::Connection::open(&db) {
        Ok(conn) => conn,
        Err(error) => {
            mlog(&format!(
                "legacy dataset migration deferred: could not open {} ({error})",
                db.display()
            ));
            return;
        }
    };
    let renamed = match conn.execute(
        "UPDATE datasets SET name = ?1 WHERE name = 'phoenix' \
         AND NOT EXISTS (SELECT 1 FROM datasets WHERE name = ?1)",
        [TEAM_DATASET],
    ) {
        Ok(renamed) => renamed,
        Err(error) => {
            mlog(&format!(
                "legacy dataset migration deferred: database update failed ({error})"
            ));
            return;
        }
    };
    COMPLETED.store(true, std::sync::atomic::Ordering::Release);
    if renamed > 0 {
        mlog("legacy dataset `phoenix` renamed to `team` — pre-scoping memories stay team-visible");
    }
}

/// Dataset filter for normal scoped reads, with a compatibility lane for the
/// one migration shape that cannot be resolved by a rename: both `team` and
/// legacy `phoenix` already exist. We discover the legacy row before adding
/// it so healthy migrated stores do not emit a missing-dataset warning on
/// every recall. This is read-only; the two datasets and their IDs remain
/// untouched.
#[cfg(feature = "cognee")]
fn recall_datasets_with_legacy(scope: &MemoryScope) -> Vec<String> {
    recall_datasets_with_legacy_at(scope, &crate::config::phoenix_cognee_root())
}

#[cfg(feature = "cognee")]
fn recall_datasets_with_legacy_at(scope: &MemoryScope, root: &std::path::Path) -> Vec<String> {
    let mut datasets = scope.recall_datasets();
    let db = root.join("cognee.db");
    if !db.exists() {
        return datasets;
    }
    let legacy_exists = rusqlite::Connection::open(&db)
        .and_then(|conn| {
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM datasets WHERE name = ?1)",
                [LEGACY_TEAM_DATASET],
                |row| row.get::<_, bool>(0),
            )
        })
        .unwrap_or(false);
    if legacy_exists {
        datasets.push(LEGACY_TEAM_DATASET.to_string());
    }
    datasets
}

/// Every dataset (memory scope) present in the store — the maintenance
/// cognify must cover all of them. A genuinely fresh store has no database or
/// datasets table yet and uses the team fallback; an existing unreadable or
/// corrupt store is an error so maintenance cannot falsely report success
/// while silently skipping agent scopes.
#[cfg(feature = "cognee")]
fn list_datasets() -> anyhow::Result<Vec<String>> {
    list_datasets_at(&crate::config::phoenix_cognee_root())
}

#[cfg(feature = "cognee")]
fn list_datasets_at(root: &std::path::Path) -> anyhow::Result<Vec<String>> {
    let db = root.join("cognee.db");
    let fallback = vec![TEAM_DATASET.to_string()];
    if !db.exists() {
        return Ok(fallback);
    }
    let conn = rusqlite::Connection::open(&db)
        .with_context(|| format!("opening memory dataset catalog {}", db.display()))?;
    let mut stmt = match conn.prepare("SELECT name FROM datasets") {
        Ok(stmt) => stmt,
        Err(error) if error.to_string().contains("no such table: datasets") => {
            return Ok(fallback);
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("reading memory dataset catalog {}", db.display()));
        }
    };
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .with_context(|| format!("querying memory dataset catalog {}", db.display()))?;
    let mut names = Vec::new();
    for row in rows {
        names.push(
            row.with_context(|| format!("decoding memory dataset catalog {}", db.display()))?,
        );
    }
    names.sort();
    names.dedup();
    if names.is_empty() {
        Ok(fallback)
    } else {
        Ok(names)
    }
}

// ---- no-op fallbacks when Cognee is compiled out --------------------------

#[cfg(not(feature = "cognee"))]
pub async fn recall(_query: &str) -> Vec<String> {
    Vec::new()
}

#[cfg(not(feature = "cognee"))]
pub async fn recall_scoped(_query: &str, _scope: &MemoryScope) -> Vec<String> {
    Vec::new()
}

#[cfg(not(feature = "cognee"))]
pub async fn remember(text: &str) -> RememberOutcome {
    remember_scoped(text, &MemoryScope::Team).await
}

#[cfg(not(feature = "cognee"))]
pub async fn remember_scoped(text: &str, _scope: &MemoryScope) -> RememberOutcome {
    if text.trim().is_empty() {
        RememberOutcome::Dropped
    } else {
        RememberOutcome::Unavailable("memory disabled in this build".to_string())
    }
}

#[cfg(not(feature = "cognee"))]
pub async fn search(_query: &str, _search_type: &str, _top_k: usize) -> SearchOutcome {
    SearchOutcome::Unavailable("memory disabled in this build".to_string())
}

#[cfg(not(feature = "cognee"))]
pub async fn search_in(
    _query: &str,
    _search_type: &str,
    _top_k: usize,
    _scope: Option<&MemoryScope>,
) -> SearchOutcome {
    SearchOutcome::Unavailable("memory disabled in this build".to_string())
}

#[cfg(not(feature = "cognee"))]
pub async fn memify() -> anyhow::Result<String> {
    Ok("Phoenix semantic memory is disabled in this minimal build".to_string())
}

/// Compose the durable-learning text a turn should remember, from the task and
/// the agent's result. Kept deterministic (no model): a compact, self-contained
/// note that Cognee's cognify step turns into graph entities/relationships.
/// Returns `None` when there is nothing worth persisting.
pub fn compose_learning(
    task_title: &str,
    user_request: &str,
    result_summary: &str,
) -> Option<String> {
    let result_summary = result_summary.trim();
    if result_summary.is_empty() {
        return None;
    }
    let title = task_title.trim();
    let request = user_request.trim();
    let mut note = String::new();
    if !title.is_empty() {
        note.push_str(title);
        note.push_str("\n\n");
    }
    if !request.is_empty() {
        note.push_str("Request: ");
        note.push_str(request);
        note.push_str("\n\n");
    }
    note.push_str("Outcome: ");
    note.push_str(result_summary);
    Some(note)
}

#[cfg(test)]
mod tests {
    #[test]
    fn completion_queries_use_hybrid_recall_but_generic_questions_do_not() {
        assert!(state_sensitive_query(
            "Did Avery finish science lesson 3 and the assessment?"
        ));
        assert!(state_sensitive_query("What should I schedule today?"));
        assert!(!state_sensitive_query(
            "Why is the Phoenix process using CPU?"
        ));
    }

    #[test]
    fn hybrid_ranking_promotes_exact_completion_evidence_over_generic_plans() {
        let query =
            "Avery science lessons 3 and 4 section 1 assignment completed what is next science";
        let ranked = rank_recall_chunks(
            query,
            vec![
                "School preference: make a balanced plan for science and math.".to_string(),
                "Science 9 Section 1 Assignment was marked complete after Lessons 3 and 4; next Science is Section 2.".to_string(),
                "Avery prefers short lessons today.".to_string(),
            ],
            3,
        );
        assert!(ranked[0].contains("marked complete"), "{ranked:#?}");
        assert!(
            ranked[0].contains("next Science is Section 2"),
            "{ranked:#?}"
        );
    }

    #[test]
    fn hybrid_ranking_promotes_explicit_correction_over_verbose_stale_schedule() {
        let query =
            "Avery science lessons 3 and 4 section 1 assignment completed what is next science";
        let stale = "Science schedule: next Science block is tomorrow. Science Unit A Section 2 lessons and self-checks follow the marked Section 1 assignment. The known sequence remains Lesson 3, Lesson 4, then the Section 1 assessment.";
        let correction = "On September 3, 2026, the user explicitly corrected Science 9 status: Lessons 3 and 4 and the Unit A Section 1 marked assessment are complete; the assessment completion had already been externally verified. Do not reopen or reschedule those items unless the user explicitly asks.";
        let ranked = rank_recall_chunks(query, vec![stale.to_string(), correction.to_string()], 2);

        assert_eq!(ranked[0], correction, "{ranked:#?}");
    }

    #[cfg(feature = "cognee")]
    #[test]
    fn raw_state_fallback_reads_pending_scoped_completion_records() {
        let root = tempfile::tempdir().unwrap();
        let data_root = root.path().join("data/aa/bb");
        std::fs::create_dir_all(&data_root).unwrap();
        let complete = data_root.join("complete.txt");
        let foreign = data_root.join("foreign.txt");
        std::fs::write(
            &complete,
            "Outcome: Science 9 Lessons 3 and 4 are already finished. The Section 1 Assignment was marked complete. Next Science is Section 2.",
        )
        .unwrap();
        std::fs::write(
            &foreign,
            "Outcome: Science Section 1 Assignment marked complete in another agent's private scope.",
        )
        .unwrap();
        let conn = rusqlite::Connection::open(root.path().join("cognee.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE data (id TEXT PRIMARY KEY, raw_data_location TEXT, created_at TEXT); \
             CREATE TABLE datasets (id TEXT PRIMARY KEY, name TEXT); \
             CREATE TABLE dataset_data (dataset_id TEXT, data_id TEXT);",
        )
        .unwrap();
        conn.execute("INSERT INTO datasets VALUES ('t', 'team')", [])
            .unwrap();
        conn.execute("INSERT INTO datasets VALUES ('o', 'agent-other')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO data VALUES ('c', ?1, '2026-09-03T16:00:00Z')",
            [url::Url::from_file_path(&complete).unwrap().to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO data VALUES ('f', ?1, '2026-09-03T17:00:00Z')",
            [url::Url::from_file_path(&foreign).unwrap().to_string()],
        )
        .unwrap();
        conn.execute("INSERT INTO dataset_data VALUES ('t', 'c')", [])
            .unwrap();
        conn.execute("INSERT INTO dataset_data VALUES ('o', 'f')", [])
            .unwrap();
        drop(conn);

        let datasets = vec!["team".to_string()];
        let found = raw_state_candidates_at(
            root.path(),
            "science lessons section assignment completed next",
            8,
            Some(&datasets),
        )
        .unwrap();
        assert_eq!(found.len(), 1, "{found:#?}");
        assert!(found[0].contains("Lessons 3 and 4 are already finished"));
        assert!(!found[0].contains("another agent's private scope"));
    }

    use super::*;

    #[test]
    fn compose_learning_skips_empty_outcomes() {
        assert!(compose_learning("t", "r", "   ").is_none());
    }

    #[test]
    fn compose_learning_includes_request_and_outcome() {
        let note =
            compose_learning("Fix login", "why is login broken?", "cookie batch fix").unwrap();
        assert!(note.contains("Fix login"));
        assert!(note.contains("Request: why is login broken?"));
        assert!(note.contains("Outcome: cookie batch fix"));
    }

    #[tokio::test]
    async fn empty_memory_save_reports_that_it_was_dropped() {
        assert_eq!(
            remember_scoped("   ", &MemoryScope::agent("coder")).await,
            RememberOutcome::Dropped
        );
    }

    #[test]
    fn remember_outcomes_are_honest_and_human_readable() {
        assert!(RememberOutcome::Stored.is_stored());
        assert!(!RememberOutcome::TimedOut("pipeline deadline".into()).is_stored());
        assert_eq!(
            RememberOutcome::Unavailable("no provider".into()).to_string(),
            "unavailable: no provider"
        );
        assert_eq!(
            RememberOutcome::Error("write failed".into()).to_string(),
            "error: write failed"
        );
        assert!(SearchOutcome::Empty.is_success());
        assert!(SearchOutcome::Hits(vec!["known fact".into()]).is_success());
        assert!(!SearchOutcome::Unavailable("offline".into()).is_success());
        assert_eq!(
            SearchOutcome::TimedOut("30s bound".into()).to_string(),
            "memory search timed out: 30s bound"
        );
    }

    #[test]
    fn scopes_map_to_datasets_and_recall_filters() {
        assert_eq!(MemoryScope::Team.dataset(), "team");
        assert_eq!(MemoryScope::agent("Coder ").dataset(), "agent-coder");
        // Empty/`team` role strings collapse to the team scope, never a
        // phantom `agent-` dataset.
        assert_eq!(MemoryScope::agent(""), MemoryScope::Team);
        assert_eq!(MemoryScope::agent("TEAM"), MemoryScope::Team);
        // Recall = own lane + team; team recalls only team.
        assert_eq!(
            MemoryScope::agent("browser").recall_datasets(),
            vec!["agent-browser".to_string(), "team".to_string()]
        );
        assert_eq!(
            MemoryScope::Team.recall_datasets(),
            vec!["team".to_string()]
        );
    }

    #[test]
    fn stamp_carries_who_learned_it_and_the_tier() {
        let team = stamp(
            "the user's dev box runs GNOME",
            "Phoenix (imported browser history)",
            &MemoryScope::Team,
        );
        assert!(team.starts_with("[from Phoenix (imported browser history) — team-wide]"));
        let own = stamp(
            "old.reddit scrapes cleaner",
            "Phoenix (imported browser history)",
            &MemoryScope::agent("browser"),
        );
        assert!(own.starts_with("[from Phoenix (imported browser history) — browser's own memory]"));
        assert!(own.ends_with("old.reddit scrapes cleaner"));
    }

    #[cfg(feature = "cognee")]
    #[test]
    fn legacy_dataset_migrates_to_team_once() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("cognee.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("CREATE TABLE datasets (name TEXT, id TEXT)", [])
            .unwrap();
        conn.execute("INSERT INTO datasets VALUES ('phoenix', 'abc123')", [])
            .unwrap();
        drop(conn);
        // Exercise the SQL shape directly (the ONCE-latched wrapper is
        // process-global; the statement itself must be idempotent and must
        // never clobber an existing team dataset).
        let run = |dir: &std::path::Path| {
            let conn = rusqlite::Connection::open(dir.join("cognee.db")).unwrap();
            conn.execute(
                "UPDATE datasets SET name = ?1 WHERE name = 'phoenix' \
                 AND NOT EXISTS (SELECT 1 FROM datasets WHERE name = ?1)",
                [TEAM_DATASET],
            )
            .unwrap()
        };
        assert_eq!(run(dir.path()), 1); // renamed, id untouched
        assert_eq!(run(dir.path()), 0); // idempotent
        let conn = rusqlite::Connection::open(&db).unwrap();
        let (name, id): (String, String) = conn
            .query_row("SELECT name, id FROM datasets", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((name.as_str(), id.as_str()), ("team", "abc123"));
    }

    #[cfg(feature = "cognee")]
    #[test]
    fn maintenance_discovers_team_legacy_and_agent_datasets() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("cognee.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("CREATE TABLE datasets (name TEXT, id TEXT)", [])
            .unwrap();
        for (name, id) in [
            ("team", "team-id"),
            ("phoenix", "legacy-id"),
            ("agent-browser", "browser-id"),
            ("agent-coder", "coder-id"),
            ("team", "duplicate-name-id"),
        ] {
            conn.execute("INSERT INTO datasets VALUES (?1, ?2)", [name, id])
                .unwrap();
        }
        drop(conn);

        assert_eq!(
            list_datasets_at(dir.path()).unwrap(),
            vec![
                "agent-browser".to_string(),
                "agent-coder".to_string(),
                "phoenix".to_string(),
                "team".to_string(),
            ]
        );
    }

    #[cfg(feature = "cognee")]
    #[test]
    fn dataset_discovery_only_falls_back_for_a_fresh_store() {
        let missing = tempfile::tempdir().unwrap();
        assert_eq!(
            list_datasets_at(missing.path()).unwrap(),
            vec![TEAM_DATASET.to_string()]
        );

        let no_table = tempfile::tempdir().unwrap();
        rusqlite::Connection::open(no_table.path().join("cognee.db")).unwrap();
        assert_eq!(
            list_datasets_at(no_table.path()).unwrap(),
            vec![TEAM_DATASET.to_string()]
        );

        let corrupt = tempfile::tempdir().unwrap();
        std::fs::write(corrupt.path().join("cognee.db"), b"not a sqlite database").unwrap();
        assert!(
            list_datasets_at(corrupt.path()).is_err(),
            "an existing corrupt catalog must fail maintenance loudly"
        );
    }

    #[cfg(feature = "cognee")]
    #[test]
    fn graph_fd_detection_covers_the_directory_and_its_database_files() {
        let graph = std::path::Path::new("/state/cognee/system/graph");
        assert!(fd_target_belongs_to_graph(graph, graph));
        assert!(fd_target_belongs_to_graph(
            std::path::Path::new("/state/cognee/system/graph/data.lbug"),
            graph,
        ));
        assert!(fd_target_belongs_to_graph(
            std::path::Path::new("/state/cognee/system/graph/wal/0001"),
            graph,
        ));
        assert!(!fd_target_belongs_to_graph(
            std::path::Path::new("/state/cognee/system/graph-backup/data.lbug"),
            graph,
        ));
    }

    #[cfg(all(feature = "cognee", unix))]
    #[test]
    fn cross_process_pipeline_lock_is_exclusive_and_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".pipeline.lock");
        let first = try_lock_pipeline_at(&path)
            .unwrap()
            .expect("first writer acquires the lock");
        let second = try_lock_pipeline_at(&path).unwrap();
        assert!(second.is_none(), "a second writer must observe contention");
        drop(second);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(first);
        let reacquired = try_lock_pipeline_at(&path).unwrap();
        assert!(
            reacquired.is_some(),
            "dropping the lease must release the cross-process lock"
        );
    }

    #[cfg(feature = "cognee")]
    #[test]
    fn scoped_reads_keep_coexisting_legacy_team_memory_visible() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("cognee.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("CREATE TABLE datasets (name TEXT, id TEXT)", [])
            .unwrap();
        conn.execute("INSERT INTO datasets VALUES ('team', 'team-id')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO datasets VALUES ('phoenix', 'legacy-team-id')",
            [],
        )
        .unwrap();
        drop(conn);

        assert_eq!(
            recall_datasets_with_legacy_at(&MemoryScope::Team, dir.path()),
            vec!["team".to_string(), "phoenix".to_string()]
        );
        assert_eq!(
            recall_datasets_with_legacy_at(&MemoryScope::agent("coder"), dir.path()),
            vec![
                "agent-coder".to_string(),
                "team".to_string(),
                "phoenix".to_string(),
            ]
        );

        // Compatibility reads never rename, merge, or delete either row.
        let conn = rusqlite::Connection::open(&db).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM datasets", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }

    #[cfg(feature = "cognee")]
    #[test]
    fn scoped_reads_do_not_request_absent_legacy_dataset() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("cognee.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("CREATE TABLE datasets (name TEXT, id TEXT)", [])
            .unwrap();
        conn.execute("INSERT INTO datasets VALUES ('team', 'team-id')", [])
            .unwrap();
        drop(conn);

        assert_eq!(
            recall_datasets_with_legacy_at(&MemoryScope::Team, dir.path()),
            vec!["team".to_string()]
        );
    }

    #[cfg(feature = "cognee")]
    #[tokio::test]
    async fn cognify_lock_serializes_pipeline_leases() {
        let lock = std::sync::Arc::new(CognifyLock::new());
        let mut first = lock.acquire().await;
        let (attempted_tx, attempted_rx) = tokio::sync::oneshot::channel();
        let (acquired_tx, mut acquired_rx) = tokio::sync::oneshot::channel();
        let contender_lock = lock.clone();
        let contender = tokio::spawn(async move {
            attempted_tx.send(()).unwrap();
            let lease = contender_lock.acquire().await;
            acquired_tx.send(()).unwrap();
            drop(lease);
        });

        // Wait until the contender is definitely about to acquire. Its
        // completion signal must still be empty while the first lease lives;
        // no sleeps or scheduler timing are involved in this assertion.
        attempted_rx.await.unwrap();
        assert!(matches!(
            acquired_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));

        // A helper can run a pipeline with an already-owned lease. It does not
        // recursively acquire the coordinator (the old self-deadlock shape).
        assert_eq!(
            first
                .run_with_timeout(std::time::Duration::from_secs(1), async { 7_u8 })
                .await
                .unwrap(),
            7
        );
        assert!(!first.pipeline_is_live());

        drop(first);
        acquired_rx.await.unwrap();
        contender.await.unwrap();
    }

    #[cfg(feature = "cognee")]
    #[tokio::test]
    async fn graph_export_starts_only_after_pipeline_lease_is_released() {
        let lock = std::sync::Arc::new(CognifyLock::new());
        let lease = lock.acquire().await;
        let export_lock = lock.clone();
        let simulated_export = async move {
            let _next_pipeline = export_lock.acquire().await;
            17_u8
        };

        let value = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            after_releasing_cognify_lease(lease, simulated_export),
        )
        .await
        .expect("export future must not remain queued behind its old lease");
        assert_eq!(value, 17);
    }

    #[cfg(feature = "cognee")]
    #[tokio::test]
    async fn graph_export_has_a_hard_deadline() {
        let outcome = bounded_graph_export(
            std::time::Duration::ZERO,
            std::future::pending::<anyhow::Result<usize>>(),
        )
        .await;
        let error = outcome.expect_err("a hung graph traversal must time out");
        assert!(error.to_string().contains("graph export timed out"));
    }

    #[cfg(feature = "cognee")]
    #[tokio::test]
    async fn timed_out_pipeline_releases_live_marker_before_recovery() {
        let lock = CognifyLock::new();
        let mut lease = lock.acquire().await;
        let outcome = lease
            .run_with_timeout(std::time::Duration::ZERO, std::future::pending::<()>())
            .await;
        assert!(outcome.is_err());
        assert!(!lease.pipeline_is_live());
    }

    #[cfg(feature = "cognee")]
    #[tokio::test]
    async fn timeout_recovery_refuses_a_same_process_live_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("cognee.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "CREATE TABLE pipeline_runs (status TEXT, run_info TEXT)",
            [],
        )
        .unwrap();
        for status in [
            "DATASET_PROCESSING_INITIATED",
            "DATASET_PROCESSING_STARTED",
            "DATASET_PROCESSING_COMPLETED",
            "DATASET_PROCESSING_ERRORED",
        ] {
            conn.execute("INSERT INTO pipeline_runs VALUES (?1, '{}')", [status])
                .unwrap();
        }
        drop(conn);

        let lock = CognifyLock::new();
        let lease = lock.acquire().await;
        let reason = "scheduled \"memify\" timed out";
        let live = CognifyPipelineLive::begin(lease.lock);
        assert_eq!(
            clear_inflight_cognify_locks_at(&lease, dir.path(), reason),
            0
        );
        let conn = rusqlite::Connection::open(&db).unwrap();
        let still_live: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pipeline_runs \
                 WHERE status IN ('DATASET_PROCESSING_STARTED', 'DATASET_PROCESSING_INITIATED')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(still_live, 2);
        drop(conn);

        // Once the timed-out future's RAII marker has dropped, recovery owns
        // the exclusive lease and can repair exactly the stranded rows.
        drop(live);
        assert_eq!(
            clear_inflight_cognify_locks_at(&lease, dir.path(), reason),
            2
        );

        let conn = rusqlite::Connection::open(&db).unwrap();
        let recovered: Vec<String> = conn
            .prepare(
                "SELECT run_info FROM pipeline_runs \
                 WHERE run_info != '{}' ORDER BY rowid",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(recovered.len(), 2);
        for run_info in recovered {
            let value: serde_json::Value = serde_json::from_str(&run_info).unwrap();
            assert_eq!(value["error"], reason);
        }
        let completed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pipeline_runs \
                 WHERE status = 'DATASET_PROCESSING_COMPLETED'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(completed, 1);
    }

    #[cfg(feature = "cognee")]
    #[test]
    fn scheduled_maintenance_budget_covers_backlog_and_memify() {
        assert!(MAINTENANCE_TIMEOUT >= BACKLOG_TIMEOUT + std::time::Duration::from_secs(2 * 60));
    }
}

/// Live diagnostics for the memory lane — "is my indexing actually working?"
/// Run any time memory feels dead:
///   cargo test --lib probe_memory_cognify -- --ignored --nocapture
#[cfg(all(test, feature = "cognee"))]
mod memory_probe {
    #[tokio::test]
    #[ignore]
    async fn probe_memory_cognify() {
        match super::cognify_backlog().await {
            Ok(receipt) => println!("cognify ok: {receipt}"),
            Err(error) => panic!("cognify FAILED: {error:#}"),
        }
    }
}
