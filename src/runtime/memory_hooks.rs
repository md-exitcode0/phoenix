//! Deterministic memory hooks — the thin replacement for the archived LLM
//! librarian (`_archive/librarian-filetier/`).
//!
//! Memory works exactly like Cognee: `add → cognify → search` plus a periodic
//! `memify`. No model turn decides what to load or save. Before a turn the
//! runtime **recalls** relevant knowledge-graph chunks; after a turn it
//! **remembers** the durable outcome; on the daemon's 12h timer **memify**
//! self-improves the graph (triplet embeddings). The old prune phase is gone —
//! render-time transcript bounding plus the runtime compressor keep the re-fed
//! context lean without an extra model trip.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;

use crate::librarian::{GroundingLabel, KnowledgeDoc, LoadedMemories, SessionCache};
use crate::providers::LLMProvider;
use crate::runtime::{
    AgentOutcome, CliEvent, LibrarianPassRecord, LibrarianPhase, MemoryBundle,
    OrchestratorDecision, SessionScope, TaskEnvelope,
};
use crate::session::Session;

/// Recall-hit telemetry for Phoenix's server-side citation loop:
/// each preload appends one JSONL line recording WHICH memory chunks actually
/// served a turn. The gardener prunes on this evidence — a chunk that never
/// surfaces across months of recalls is a fluff candidate; one that surfaces
/// weekly is load-bearing and must survive. Previews (not full text) keep the
/// ledger small; ~2MB rotation keeps it bounded. Best-effort: telemetry must
/// never fail a turn.
fn record_recall_hits(memory_root: &Path, session_id: &str, query: &str, hits: &[String]) {
    let path = memory_root.join("recall_hits.jsonl");
    let preview = |s: &str, n: usize| s.chars().take(n).collect::<String>().replace('\n', " ");
    let line = serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "session": preview(session_id, 192),
        "query": preview(query, 120),
        "hit_count": hits.len(),
        "hits": hits.iter().take(128).map(|h| preview(h, 100)).collect::<Vec<_>>(),
    });
    use std::io::Write;
    let _ = crate::config::private_io::with_private_lock(&path, || {
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_file() && meta.len() > 2_000_000 => {
                let previous = memory_root.join("recall_hits.jsonl.old");
                match std::fs::symlink_metadata(&previous) {
                    Ok(meta) if meta.file_type().is_file() => std::fs::remove_file(&previous)?,
                    Ok(_) => anyhow::bail!(
                        "refusing non-regular rotated recall ledger {}",
                        previous.display()
                    ),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                std::fs::rename(&path, &previous)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&previous, std::fs::Permissions::from_mode(0o600))?;
                }
            }
            Ok(meta) if !meta.file_type().is_file() => {
                anyhow::bail!("refusing non-regular recall ledger {}", path.display())
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let mut file = crate::runtime::open_private_log_append(&path)?;
        writeln!(file, "{line}")?;
        Ok(())
    });
}

/// Derive the Phoenix state root (`.phoenix/`) from the memory root, mirroring
/// the runner's `default_state_root` so the CodeGraph index path is identical
/// whether reached via tools or via preload injection.
fn codegraph_state_root(memory_root: &Path) -> std::path::PathBuf {
    if memory_root.file_name().and_then(|n| n.to_str()) == Some("memory") {
        if let Some(parent) = memory_root.parent() {
            if parent.file_name().and_then(|n| n.to_str()) == Some(".phoenix") {
                return parent.to_path_buf();
            }
            return parent.join(".phoenix");
        }
    }
    memory_root.join(".phoenix")
}

pub async fn preload(
    memory_root: &Path,
    workspace_root: &Path,
    _provider: Arc<dyn LLMProvider>,
    _model: &str,
    session_scope: SessionScope,
    task: &TaskEnvelope,
    session: &Session,
    session_cache: &mut SessionCache,
    event_tx: Option<mpsc::Sender<CliEvent>>,
) -> Result<(MemoryBundle, LibrarianPassRecord, LoadedMemories)> {
    // Cognee-only recall: no model turn decides what to load. We search the
    // knowledge graph for context relevant to this task and inject the chunks,
    // plus the deterministic codegraph project map. A cold store or a recall
    // failure degrades to just the project map — never an error on the turn.
    if let Some(tx) = &event_tx {
        let _ = tx.try_send(CliEvent::StreamDelta {
            kind: "progress".to_string(),
            text: "recalling memory…".to_string(),
        });
    }
    let query = format!("{} :: {}", task.title, task.user_request);
    let mut loaded = LoadedMemories::default();
    inject_deterministic_project_map(memory_root, workspace_root, &mut loaded);
    inject_deterministic_user_context(memory_root, session, &mut loaded);
    // Recall the active coworker's own scope plus the team tier. The old mesh
    // path hard-coded `orchestrator` here, which made every specialist preload
    // memory-blind even when the caller supplied `SessionScope::Specialist`.
    // Keep the historical orchestrator key for Phoenix so existing memory is
    // preserved; specialist labels are stable role keys across persona renames.
    let memory_role = match &session_scope {
        SessionScope::Main => "orchestrator",
        SessionScope::Specialist(agent) => crate::runtime::delegation::specialist_label(*agent),
    };
    // Turn preload is latency-sensitive; explicit `memory_recall` keeps the
    // longer search budget. On a cold process this short wait starts the ONNX
    // singleton on the blocking pool, then returns immediately while that
    // initialization finishes in the background. The stable project-map
    // snapshot above is still injected for the current turn.
    let recalled = match tokio::time::timeout(
        std::time::Duration::from_millis(600),
        crate::librarian::memory::recall_scoped(
            &query,
            &crate::librarian::memory::MemoryScope::agent(memory_role),
        ),
    )
    .await
    {
        Ok(chunks) => chunks,
        Err(_) => {
            crate::runtime::gwlog(
                "memory: turn preload exceeded 600ms — continuing from canonical context while the local index warms",
            );
            Vec::new()
        }
    };

    // Which memories actually served this turn → the gardener's prune evidence.
    record_recall_hits(memory_root, &task.session_id, &query, &recalled);

    let mut receipts = Vec::new();
    if !recalled.is_empty() {
        // Identity orientation (2026-07-18): recalled chunks are the TEAM's
        // shared knowledge graph — spanning every agent, session, and project
        // on this machine. Without this header, agents narrated team history
        // as their own first-person past ("my memory says…", "we built…"),
        // which is exactly the who-is-who confusion the user reported.
        let orientation = "SHARED TEAM MEMORY — notes below may have been learned by ANY \
             agent in ANY project on this machine (provenance is stamped per note). \
             Attribute facts to whoever learned them; do not present them as your own \
             first-person history unless the stamp says they are yours.";
        loaded.knowledge_docs.push(KnowledgeDoc {
            path: "cognee://recall".to_string(),
            summary: format!("{orientation}\n\n{}", recalled.join("\n\n---\n\n")),
            grounding: GroundingLabel::Observed,
            score: 1.0,
            reasons: vec!["semantic + graph recall from Phoenix memory".to_string()],
        });
        loaded.grounding_receipts.push(format!(
            "Phoenix memory recall → {} chunk(s) injected (local embeddings, no model turn)",
            recalled.len()
        ));
        receipts.push(format!(
            "phoenix_memory_recall → {} chunk(s)",
            recalled.len()
        ));
    } else {
        // Careful wording: an empty recall can be a cold store, a genuine
        // no-match, OR a swallowed failure — a failure logs a loud `memory:`
        // line in gateway.log (memory.rs mlog). Don't claim "empty store"
        // as fact; that masked the dead-parser regression for days.
        loaded.grounding_receipts.push(
            "Phoenix memory recall → no chunks returned (cold store, no match, or a failure — a failure logs `memory:` in gateway.log)"
                .to_string(),
        );
    }
    let injected_paths = loaded
        .memories
        .iter()
        .map(|item| item.path.clone())
        .chain(loaded.knowledge_docs.iter().map(|item| item.path.clone()))
        .collect::<Vec<_>>();
    session_cache.loaded = injected_paths.clone();
    session_cache.last_injected_paths = injected_paths;
    session_cache.last_omitted_items = loaded.omitted_items.clone();
    session_cache.last_grounding_receipts = loaded.grounding_receipts.clone();
    session_cache.last_completion_state = loaded.completion_state.clone();
    session_cache.last_recommended_next = loaded.recommended_next_agent_or_tool.clone();
    session_cache.last_updated = chrono::Utc::now();

    let bundle = MemoryBundle {
        session_scope: session_scope.clone(),
        task_frame: format!("{} :: {}", task.title, task.user_request),
        loaded_memory_paths: loaded
            .memories
            .iter()
            .map(|item| item.path.clone())
            .collect(),
        loaded_knowledge_paths: loaded
            .knowledge_docs
            .iter()
            .map(|item| item.path.clone())
            .collect(),
        ranked_context_items: loaded.ranked_context_items.clone(),
        omitted_items: loaded.omitted_items.clone(),
        grounding_receipts: loaded.grounding_receipts.clone(),
        completion_state: loaded.completion_state.clone(),
        open_questions: loaded.open_questions.clone(),
        recommended_next_agent_or_tool: loaded.recommended_next_agent_or_tool.clone(),
        context_budget_used: loaded.context_budget_used,
        summary: format!(
            "{}-session memory recall selected {} context item(s).",
            scope_label(&session_scope),
            loaded.memories.len() + loaded.knowledge_docs.len()
        ),
    };

    let record = LibrarianPassRecord {
        session_scope,
        phase: LibrarianPhase::Preload,
        memory_paths: bundle.loaded_memory_paths.clone(),
        knowledge_paths: bundle.loaded_knowledge_paths.clone(),
        saved_memory_paths: vec![],
        omitted_items: vec![],
        receipts,
        context_budget_used: bundle.context_budget_used,
        pruned_message_count: 0,
        summary: bundle.summary.clone(),
    };

    Ok((bundle, record, loaded))
}

fn inject_deterministic_project_map(
    memory_root: &Path,
    workspace_root: &Path,
    loaded: &mut LoadedMemories,
) {
    // Every agent gets the current stable map outside the semantic recall
    // path. Reading the existing snapshot is cheap; changed files are
    // reindexed once after a completed task. Never crawl the workspace here:
    // preload sits directly on the first-token critical path.
    let state_root = codegraph_state_root(memory_root);
    match crate::codegraph::project_map(&state_root, workspace_root, 12) {
        Some(map) => {
            loaded.knowledge_docs.insert(
                0,
                KnowledgeDoc {
                    path: "codegraph://project-map".to_string(),
                    summary: map,
                    grounding: GroundingLabel::Observed,
                    score: 1.0,
                    reasons: vec![
                        "deterministic runtime injection from the local symbol index".to_string(),
                    ],
                },
            );
            loaded.grounding_receipts.push(
                "codegraph project map → deterministic runtime injection (not memory-selected)"
                    .to_string(),
            );
        }
        None => loaded.grounding_receipts.push(
            "codegraph project map → unavailable; no symbols indexed for this workspace"
                .to_string(),
        ),
    }
}

/// Always inject the bounded, verbatim user-state lane before semantic recall.
///
/// The memory graph stores agent outcomes and ranks by the current query. It is
/// useful supporting context, but it cannot guarantee that an older completion
/// fact will match a later request such as "prepare tomorrow". The session
/// archive is the authoritative source for those direct user statements, so a
/// deterministic bounded excerpt travels on every turn even when Cognee is
/// cold, times out, or returns semantically adjacent but stale notes.
fn inject_deterministic_user_context(
    memory_root: &Path,
    session: &Session,
    loaded: &mut LoadedMemories,
) {
    let state_root = codegraph_state_root(memory_root);
    ensure_deterministic_user_context(&state_root, session, loaded);
}

/// Install the query-independent user-history lane even when semantic memory
/// preload times out. Custom coworkers are deliberately given a tight preload
/// latency budget; previously that outer timeout discarded the already-cheap
/// deterministic ledger together with the optional Cognee lookup.
pub(crate) fn ensure_deterministic_user_context(
    state_root: &Path,
    session: &Session,
    loaded: &mut LoadedMemories,
) {
    const USER_CONTEXT_PATH: &str = "session://user-context";
    loaded
        .knowledge_docs
        .retain(|doc| doc.path != USER_CONTEXT_PATH);
    loaded
        .grounding_receipts
        .retain(|receipt| !receipt.starts_with("user state ledger →"));
    let archive_dir = state_root.join("sessions");
    let context = crate::runtime::compaction::durable_user_context(session, Some(&archive_dir));
    if context.is_empty() {
        return;
    }
    let chars = context.chars().count();
    loaded.knowledge_docs.push(KnowledgeDoc {
        path: USER_CONTEXT_PATH.to_string(),
        summary: context,
        grounding: GroundingLabel::Observed,
        score: 1.0,
        reasons: vec![
            "deterministic bounded excerpts from the verbatim session archive and live tail"
                .to_string(),
        ],
    });
    loaded.grounding_receipts.push(format!(
        "user state ledger → {chars} chars injected from verbatim session history (query-independent)"
    ));
}

/// ARCHIVED (disabled): the prune phase is a no-op.
///
/// Prune previously ran a full librarian model turn to compact bulky tool-result
/// messages mid-session. Render-time transcript bounding plus the runtime
/// compressor already keep the re-fed context lean, so the extra model turn is
/// gone with the LLM librarian.
pub async fn prune_session(
    _memory_root: &Path,
    _workspace_root: &Path,
    _provider: Arc<dyn LLMProvider>,
    _model: &str,
    session_scope: SessionScope,
    _task: &TaskEnvelope,
    _session: &mut Session,
    bundle: &MemoryBundle,
    _session_cache: &mut SessionCache,
    _event_tx: Option<mpsc::Sender<CliEvent>>,
) -> LibrarianPassRecord {
    LibrarianPassRecord {
        session_scope,
        phase: LibrarianPhase::Prune,
        memory_paths: bundle.loaded_memory_paths.clone(),
        knowledge_paths: bundle.loaded_knowledge_paths.clone(),
        saved_memory_paths: vec![],
        omitted_items: vec![],
        receipts: vec![],
        context_budget_used: bundle.context_budget_used,
        pruned_message_count: 0,
        summary: "Prune phase archived (disabled).".to_string(),
    }
}

pub async fn save_phase(
    _memory_root: &Path,
    _workspace_root: &Path,
    _provider: Arc<dyn LLMProvider>,
    _model: &str,
    session_scope: SessionScope,
    task: &TaskEnvelope,
    _session: &Session,
    _decision: &OrchestratorDecision,
    outcome: &AgentOutcome,
    session_cache: &mut SessionCache,
    event_tx: Option<mpsc::Sender<CliEvent>>,
) -> Result<LibrarianPassRecord> {
    // Cognee-only save: no model turn decides what to persist. We compose a
    // compact, self-contained note from the task + this agent's result and hand
    // it to Cognee, which cognifies it into graph entities/relationships. A save
    // failure is logged, never fatal to the turn.
    let mut receipts = Vec::new();
    let mut saved_memory_paths = Vec::new();
    match crate::librarian::memory::compose_learning(
        &task.title,
        &task.user_request,
        &outcome.summary,
    ) {
        Some(note) => {
            if let Some(tx) = &event_tx {
                let _ = tx.try_send(CliEvent::StreamDelta {
                    kind: "progress".to_string(),
                    text: "saving memory…".to_string(),
                });
            }
            let outcome = crate::librarian::memory::remember(&note).await;
            record_remember_outcome(&outcome, &mut saved_memory_paths, &mut receipts);
        }
        None => {
            receipts.push("Phoenix memory save → nothing durable in this turn; skipped".to_string())
        }
    }

    session_cache.new_this_session = saved_memory_paths.clone();
    session_cache.receipt_log.extend(receipts.clone());
    session_cache.last_updated = chrono::Utc::now();

    Ok(LibrarianPassRecord {
        session_scope,
        phase: LibrarianPhase::Save,
        memory_paths: vec![],
        knowledge_paths: vec![],
        saved_memory_paths,
        omitted_items: vec![],
        receipts,
        context_budget_used: 0,
        pruned_message_count: 0,
        summary: "Phoenix memory save pass complete.".to_string(),
    })
}

fn record_remember_outcome(
    outcome: &crate::librarian::memory::RememberOutcome,
    saved_memory_paths: &mut Vec<String>,
    receipts: &mut Vec<String>,
) {
    if outcome.is_stored() {
        saved_memory_paths.push("cognee://remember".to_string());
        receipts.push(
            "Phoenix memory → outcome stored and durably ingested; graph indexing may be deferred"
                .to_string(),
        );
    } else {
        receipts.push(format!(
            "Phoenix memory → DURABILITY NOT CONFIRMED ({outcome}); no success receipt was recorded and a later retry may be needed"
        ));
    }
}

/// Store-wide memory maintenance on the daemon's 12h timer. With Cognee this is
/// no longer an LLM gardening loop over Markdown files — it runs `memify`, the
/// deterministic self-improvement pipeline (triplet embeddings over every graph
/// edge). Idempotent and safe to re-run; a failure is reported, never fatal.
pub async fn run_maintenance(event_tx: Option<mpsc::Sender<CliEvent>>) -> Result<Vec<String>> {
    if let Some(tx) = &event_tx {
        let _ = tx.try_send(CliEvent::GatewayNotice(
            "memory: memify maintenance pass started".to_string(),
        ));
    }
    let summary = crate::librarian::memory::memify().await?;
    Ok(vec![format!("Phoenix memory maintenance → {summary}")])
}

fn scope_label(scope: &SessionScope) -> &'static str {
    match scope {
        SessionScope::Main => "main",
        SessionScope::Specialist(_) => "specialist",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codegraph_state_root_walks_up_from_memory_dir() {
        let root = codegraph_state_root(Path::new("/home/u/.phoenix/memory"));
        assert_eq!(root, Path::new("/home/u/.phoenix"));
        let bare = codegraph_state_root(Path::new("/somewhere/else"));
        assert_eq!(bare, Path::new("/somewhere/else/.phoenix"));
    }

    #[test]
    fn scope_labels_are_stable() {
        assert_eq!(scope_label(&SessionScope::Main), "main");
        assert_eq!(
            scope_label(&SessionScope::Specialist(
                crate::session::SubAgentType::Coder
            )),
            "specialist"
        );
    }

    #[test]
    fn deterministic_user_context_is_injected_without_semantic_recall() {
        let dir = tempfile::tempdir().unwrap();
        let memory_root = dir.path().join(".phoenix/memory");
        std::fs::create_dir_all(&memory_root).unwrap();
        let mut session = Session::new_main("m", "agent-school_coach");
        session.push_message(crate::session::Message::User {
            content: "I finished Test Drive 1.3; do not put it back into tomorrow's work."
                .to_string(),
        });
        let mut loaded = LoadedMemories::default();

        inject_deterministic_user_context(&memory_root, &session, &mut loaded);

        let doc = loaded
            .knowledge_docs
            .iter()
            .find(|doc| doc.path == "session://user-context")
            .expect("user context must be injected independently of Cognee");
        assert!(doc.summary.contains("I finished Test Drive 1.3"));
        assert!(loaded
            .grounding_receipts
            .iter()
            .any(|receipt| receipt.contains("query-independent")));
    }

    #[test]
    fn deterministic_user_context_can_be_restored_after_optional_preload_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let state_root = dir.path().join(".phoenix");
        std::fs::create_dir_all(state_root.join("sessions")).unwrap();
        let mut session = Session::new_main("m", "agent-school_coach-timeout");
        session.push_message(crate::session::Message::User {
            content: "The vacation plan covers every assessment; verify Notion and Moodle before scheduling."
                .to_string(),
        });
        let mut loaded = LoadedMemories::default();

        ensure_deterministic_user_context(&state_root, &session, &mut loaded);
        ensure_deterministic_user_context(&state_root, &session, &mut loaded);

        let docs = loaded
            .knowledge_docs
            .iter()
            .filter(|doc| doc.path == "session://user-context")
            .collect::<Vec<_>>();
        assert_eq!(docs.len(), 1, "recovery must not duplicate the prompt lane");
        assert!(docs[0].summary.contains("verify Notion and Moodle"));
        assert_eq!(
            loaded
                .grounding_receipts
                .iter()
                .filter(|receipt| receipt.starts_with("user state ledger →"))
                .count(),
            1
        );
    }

    #[test]
    fn failed_remember_never_records_a_saved_path_or_success_receipt() {
        let mut paths = Vec::new();
        let mut receipts = Vec::new();
        record_remember_outcome(
            &crate::librarian::memory::RememberOutcome::TimedOut("pipeline deadline".to_string()),
            &mut paths,
            &mut receipts,
        );

        assert!(paths.is_empty());
        assert_eq!(receipts.len(), 1);
        assert!(receipts[0].contains("DURABILITY NOT CONFIRMED"));
        assert!(!receipts[0].contains("cognified"));
    }

    #[cfg(unix)]
    #[test]
    fn recall_ledger_is_private_and_refuses_symlinks() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let dir = tempfile::tempdir().unwrap();
        record_recall_hits(dir.path(), "main-safe", "query", &["hit".into()]);
        let ledger = dir.path().join("recall_hits.jsonl");
        assert_eq!(
            std::fs::metadata(&ledger).unwrap().permissions().mode() & 0o777,
            0o600
        );

        std::fs::remove_file(&ledger).unwrap();
        let outside = dir.path().join("outside");
        std::fs::write(&outside, "untouched").unwrap();
        symlink(&outside, &ledger).unwrap();
        record_recall_hits(dir.path(), "main-safe", "query", &["changed".into()]);
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "untouched");
    }
}
