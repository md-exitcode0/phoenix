//! Pending `ask_user` questions: the bridge between an agent turn awaiting
//! an answer and the TUI modal where the user types it.
//!
//! Interactive terminal clients can register a pending ask and await its
//! oneshot receiver. Canvas uses a detached ask instead: the card is durable,
//! the asking turn keeps doing independent work, and the eventual answer is
//! queued as a continuation in the same conversation. Same-process global —
//! the daemon and the mesh share the runtime.
//!
//! An ask also remembers WHICH SESSION asked (bounded, outliving the pending
//! entry): an answer that arrives after the asking turn died — the popup
//! outlived a 15-minute timeout, a watcher halt, an Esc — must WAKE that
//! session with the answer instead of being silently dropped while the TUI
//! claims "continues" (live 2026-07-09, main-f1a95e88).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

static PENDING: Mutex<Option<HashMap<String, oneshot::Sender<String>>>> = Mutex::new(None);

/// ask id → asking session, kept AFTER the pending entry is gone so a late
/// answer can still find its way home. Bounded: cleared wholesale past the
/// cap (it is a recent-lookups cache, not a store).
static ASK_SESSIONS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
const ASK_SESSIONS_CAP: usize = 64;
const ASK_BREADCRUMB_VERSION: u32 = 2;
const ASK_BREADCRUMB_MAX_BYTES: usize = 64 * 1024;
const ASK_HISTORY_FILE_CAP: usize = 4_096;

/// A typed decision applies only to approval cards. Ordinary questions keep
/// their long-standing `pending` / `answered` / `answered_late` / `dismissed`
/// lifecycle and leave this field empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalOutcome {
    Approved,
    Denied,
    Expired,
    Canceled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalDecisionReceipt {
    pub outcome: ApprovalOutcome,
    pub decided_at: String,
    /// Hash of the exact runtime-bound action, never of the human label alone.
    pub action_fingerprint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedActionState {
    Ready,
    Executing,
    Succeeded,
    Failed,
    Blocked,
    Canceled,
    Denied,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtectedActionReceipt {
    pub action_fingerprint: String,
    pub state: ProtectedActionState,
    #[serde(default)]
    pub execution_id: Option<String>,
    #[serde(default)]
    pub executor_instance: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub result_summary: Option<String>,
}

/// Proof that this process exclusively consumed one approved continuation.
/// The opaque execution id must match when the result is recorded.
#[derive(Debug)]
pub struct ProtectedActionLease {
    pub ask_id: String,
    pub action_fingerprint: String,
    execution_id: String,
    executor_instance: String,
    finished: std::sync::atomic::AtomicBool,
}

impl Drop for ProtectedActionLease {
    fn drop(&mut self) {
        if self.finished.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        if let Err(error) = finish_protected_action(
            self,
            ProtectedActionResult::Blocked,
            "The protected continuation ended before a durable result was recorded; retry is blocked to prevent a duplicate.",
        ) {
            tracing::error!(
                ask_id = %self.ask_id,
                "abandoned protected action could not be marked blocked: {error:#}"
            );
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectedActionResult {
    Succeeded,
    Failed,
    Blocked,
    Canceled,
}

/// Durable conversation record for a question Phoenix showed the user. The
/// complete typed payload lives outside the model transcript because a turn
/// can be waiting on the answer when Canvas or the gateway restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskRecord {
    version: u32,
    pub ask_id: String,
    pub session_id: String,
    pub created_at: String,
    #[serde(default)]
    pub resolved_at: Option<String>,
    #[serde(default)]
    pub agent: String,
    /// Immutable directory identity of the coworker that opened the card.
    /// Older breadcrumbs omit it and are resolved from `agent` only as a
    /// compatibility fallback.
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub questions: Vec<crate::tools::ask_user::AskUserQuestion>,
    #[serde(default)]
    pub approval: Option<crate::tools::ask_user::ApprovalRequest>,
    #[serde(default)]
    pub answer: Option<String>,
    #[serde(default = "pending_status")]
    pub status: String,
    /// Present only for a typed approval. Additive/defaulted for legacy JSON.
    #[serde(default)]
    pub approval_decision: Option<ApprovalDecisionReceipt>,
    /// Exact-once continuation state for protected actions. A decision can be
    /// durable without an action having begun yet (`ready`).
    #[serde(default)]
    pub action_receipt: Option<ProtectedActionReceipt>,
}

fn pending_status() -> String {
    "pending".to_string()
}

/// Register a pending ask; the returned receiver resolves when the user
/// answers (or is dropped if the ask is abandoned).
pub fn register(id: &str, session_id: &str) -> oneshot::Receiver<String> {
    register_with_payload(id, session_id, "", &[], None)
}

/// Register and durably describe a pending question before emitting it. This
/// ordering guarantees a Canvas relaunch can reconstruct the exact card even
/// if the process dies immediately after the live event is delivered.
pub fn register_with_payload(
    id: &str,
    session_id: &str,
    agent: &str,
    questions: &[crate::tools::ask_user::AskUserQuestion],
    approval: Option<&crate::tools::ask_user::ApprovalRequest>,
) -> oneshot::Receiver<String> {
    let (tx, rx) = oneshot::channel();
    let mut pending = PENDING.lock().unwrap_or_else(|p| p.into_inner());
    pending
        .get_or_insert_with(HashMap::new)
        .insert(id.to_string(), tx);
    drop(pending);
    remember(id, session_id, agent, questions, approval);
    rx
}

/// Persist a Canvas question without parking the asking agent on a receiver.
/// Answers deliberately miss `PENDING` and follow the daemon's durable late-
/// answer continuation path instead.
pub fn register_detached_with_payload(
    id: &str,
    session_id: &str,
    agent: &str,
    questions: &[crate::tools::ask_user::AskUserQuestion],
    approval: Option<&crate::tools::ask_user::ApprovalRequest>,
) {
    if let Some(map) = PENDING.lock().unwrap_or_else(|p| p.into_inner()).as_mut() {
        map.remove(id);
    }
    remember(id, session_id, agent, questions, approval);
}

fn remember(
    id: &str,
    session_id: &str,
    agent: &str,
    questions: &[crate::tools::ask_user::AskUserQuestion],
    approval: Option<&crate::tools::ask_user::ApprovalRequest>,
) {
    let mut sessions = ASK_SESSIONS.lock().unwrap_or_else(|p| p.into_inner());
    let map = sessions.get_or_insert_with(HashMap::new);
    if map.len() >= ASK_SESSIONS_CAP {
        map.clear();
    }
    map.insert(id.to_string(), session_id.to_string());
    if let Err(error) = persist_breadcrumb(id, session_id, agent, questions, approval) {
        tracing::error!("ask {id}: durable session breadcrumb failed: {error:#}");
    }
}

/// Deliver the user's answer. False = no such pending ask (timed out,
/// answered already, or the asking turn died) — callers should then route
/// the answer through [`session_for`] + a session wake.
pub fn answer(id: &str, text: String) -> bool {
    let sender = {
        let mut pending = PENDING.lock().unwrap_or_else(|p| p.into_inner());
        pending.as_mut().and_then(|map| map.remove(id))
    };
    // Persist a typed decision before waking the protected continuation. The
    // receiver can resume on another Tokio worker immediately after `send`;
    // making the receipt durable first closes that authorize-before-receipt
    // crash/race window. A persistence failure is delivered as a denial, so
    // it can never accidentally authorize the action.
    let durable = prepare_typed_decision(id, &text, None);
    let delivered = match sender {
        Some(tx) if durable.is_ok() => tx.send(text.clone()).is_ok(),
        Some(tx) => {
            let error = durable.expect_err("typed decision persistence failed");
            tracing::error!("ask {id}: approval decision was not durable: {error:#}");
            tx.send("Approval could not be saved; do not execute the action.".to_string())
                .is_ok()
        }
        None => false,
    };
    if delivered {
        if let Err(error) = archive_resolution(id, &text, "answered") {
            tracing::warn!("ask {id}: answer history could not be persisted: {error:#}");
        }
    }
    delivered
}

/// Close a question without turning the close gesture into user-authored
/// prose. A live turn receives the neutral `Not now` choice; a stale card is
/// simply consumed. In neither case may dismissal create a wake turn.
pub fn dismiss(id: &str) -> bool {
    let sender = {
        let mut pending = PENDING.lock().unwrap_or_else(|p| p.into_inner());
        pending.as_mut().and_then(|map| map.remove(id))
    };
    if let Err(error) = prepare_typed_decision(id, "Not now", Some(ApprovalOutcome::Canceled)) {
        tracing::warn!("ask {id}: cancellation receipt could not be persisted: {error:#}");
    }
    let delivered = sender.is_some_and(|tx| tx.send("Not now".to_string()).is_ok());
    if let Err(error) = archive_resolution(id, "Not now", "dismissed") {
        tracing::warn!("ask {id}: dismissal history could not be persisted: {error:#}");
    }
    delivered
}

/// A stale card was answered after its original turn ended. Call only after
/// the wake carrying that answer has been durably queued.
pub fn archive_late_answer(id: &str, answer: &str) {
    if let Err(error) = prepare_typed_decision(id, answer, None) {
        tracing::warn!("ask {id}: late decision receipt could not be persisted: {error:#}");
    }
    if let Err(error) = block_unresumable_protected_action(id) {
        tracing::warn!("ask {id}: stranded protected action could not be blocked: {error:#}");
    }
    if let Err(error) = archive_resolution(id, answer, "answered_late") {
        tracing::warn!("ask {id}: late-answer history could not be persisted: {error:#}");
    }
}

fn block_unresumable_protected_action(id: &str) -> anyhow::Result<()> {
    let path = breadcrumb_path(id)?;
    crate::config::private_io::with_private_lock(&path, || {
        let Some(mut record) = load_record_at(&path, id)? else {
            return Ok(());
        };
        let Some(receipt) = record.action_receipt.as_mut() else {
            return Ok(());
        };
        if receipt.state != ProtectedActionState::Ready {
            return Ok(());
        }
        receipt.state = ProtectedActionState::Blocked;
        receipt.finished_at = Some(chrono::Utc::now().to_rfc3339());
        receipt.result_summary = Some(
            "The original protected continuation had already ended when approval arrived; Phoenix blocked execution and requires a fresh exact-action approval."
                .into(),
        );
        crate::config::private_io::atomic_write_private_under_lock(
            &path,
            &serde_json::to_vec_pretty(&record)?,
        )
    })
}

/// Expire a typed approval without changing the lifecycle of ordinary
/// questions. This is intentionally explicit: an abandoned live receiver may
/// still have a durable Canvas card and therefore is not automatically an
/// expired decision.
pub fn expire_approval(id: &str) -> anyhow::Result<bool> {
    let Some(record) = load_breadcrumb(id)? else {
        return Ok(false);
    };
    if record.approval.is_none() || record.approval_decision.is_some() {
        return Ok(false);
    }
    prepare_typed_decision(id, "", Some(ApprovalOutcome::Expired))?;
    if let Some(map) = PENDING.lock().unwrap_or_else(|p| p.into_inner()).as_mut() {
        map.remove(id);
    }
    archive_resolution(id, "", "dismissed")?;
    Ok(true)
}

/// The session that asked — survives timeout/abandon so late answers can be
/// delivered as a wake.
pub fn session_for(id: &str) -> Option<String> {
    let cached = ASK_SESSIONS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .and_then(|map| map.get(id).cloned());
    if cached.is_some() {
        return cached;
    }
    match load_breadcrumb(id) {
        Ok(Some(breadcrumb)) => Some(breadcrumb.session_id),
        Ok(None) => None,
        Err(error) => {
            tracing::warn!("ask {id}: durable session breadcrumb is unreadable: {error:#}");
            None
        }
    }
}

/// Durable provenance for a popup, including the exact asking coworker. Late
/// answers must use this rather than guessing from the owning conversation.
pub fn record_for(id: &str) -> Option<AskRecord> {
    match load_breadcrumb(id) {
        Ok(record) => record,
        Err(error) => {
            tracing::warn!("ask {id}: durable provenance is unreadable: {error:#}");
            None
        }
    }
}

/// Resolve an exact decision handle, including its committed history. This
/// is for authorization and retry receipts, never for reviving pending work.
/// Share the archival lock and prefer history after a partial archive crash.
pub fn decision_record_for(id: &str) -> anyhow::Result<Option<AskRecord>> {
    let pending = breadcrumb_path(id)?;
    let history = ask_history_path(id)?;
    crate::config::private_io::with_private_lock(&pending, || {
        match load_record_at(&history, id)? {
            Some(record) => Ok(Some(record)),
            None => load_record_at(&pending, id),
        }
    })
}

/// Scheduling reads exact ledger-owned questions, never the capped and
/// presentation-filtered conversation history. Share the resolver's lock so
/// archiving a reply cannot look like a missing or resurrected question.
pub(crate) fn group_question_pending(id: &str, session_id: &str, agent_id: &str) -> anyhow::Result<bool> {
    let pending = breadcrumb_path(id)?;
    let history = ask_history_path(id)?;
    group_question_pending_at(&pending, &history, id, session_id, agent_id)
}

fn group_question_pending_at(pending: &Path, history: &Path, id: &str, session_id: &str, agent_id: &str) -> anyhow::Result<bool> {
    crate::session::SessionStore::validate_session_id(session_id)?;
    crate::config::private_io::with_private_lock(pending, || {
        // A committed resolution is authoritative even if a crash left the
        // old pending file. An invalid authoritative copy is an error.
        let record = match load_record_at(history, id)? {
            Some(record) => record,
            None => load_record_at(pending, id)?
                .ok_or_else(|| anyhow::anyhow!("ledger-owned group question is missing"))?,
        };
        anyhow::ensure!(record.session_id == session_id, "group question conversation mismatch");
        // Legacy records have no directory ID; ownership is still fenced by
        // the immutable group-ask binding supplied by the caller.
        anyhow::ensure!(record.agent_id.as_deref().is_none_or(|owner| owner == agent_id), "group question owner mismatch");
        if record.status == "pending" {
            anyhow::ensure!(record.resolved_at.is_none(), "pending group question has a resolution timestamp");
            Ok(true)
        } else {
            anyhow::ensure!(record.resolved_at.is_some(), "group question has no durable resolution");
            Ok(false)
        }
    })
}

/// Drop a pending ask without answering (timeout path) so the map can't
/// accumulate dead entries. The session mapping stays — that is the late-
/// answer breadcrumb.
pub fn abandon(id: &str) {
    let mut pending = PENDING.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(map) = pending.as_mut() {
        map.remove(id);
    }
}

/// Consume the late-answer breadcrumb after the daemon has durably queued its
/// wake. A repeated click must not inject the same authorization twice.
pub fn forget(id: &str) {
    if let Some(map) = ASK_SESSIONS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_mut()
    {
        map.remove(id);
    }
    if let Ok(path) = breadcrumb_path(id) {
        if let Err(error) = crate::config::private_io::remove_private_file(&path) {
            tracing::warn!("ask {id}: could not remove consumed breadcrumb: {error:#}");
        }
    }
}

/// Permanently remove question/approval records that belonged to a deleted
/// transcript turn.  Validation happens for the complete set before any
/// mutation so a malformed client id cannot produce a partial purge.
pub fn purge_records(session_id: &str, ids: &[String]) -> anyhow::Result<usize> {
    crate::session::SessionStore::validate_session_id(session_id)?;
    let paths = ids
        .iter()
        .map(|id| Ok((id.clone(), breadcrumb_path(id)?, ask_history_path(id)?)))
        .collect::<anyhow::Result<Vec<_>>>()?;

    // Establish ownership for the complete set before dropping a sender or
    // removing a single file. A malformed/mixed-session request therefore
    // fails atomically instead of partially erasing another conversation's
    // approvals.
    let cached_sessions = ASK_SESSIONS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
        .unwrap_or_default();
    let pending_ids = PENDING
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map(|map| {
            map.keys()
                .cloned()
                .collect::<std::collections::HashSet<_>>()
        })
        .unwrap_or_default();
    for (id, pending_path, history_path) in &paths {
        let mut durable_owner = None;
        for path in [pending_path, history_path] {
            let Some(raw) = crate::config::private_io::read_private_file_limited(
                path,
                ASK_BREADCRUMB_MAX_BYTES,
            )?
            else {
                continue;
            };
            let record: AskRecord = serde_json::from_slice(&raw)?;
            anyhow::ensure!(
                matches!(record.version, 1 | ASK_BREADCRUMB_VERSION) && record.ask_id == *id,
                "ask record identity/version mismatch"
            );
            crate::session::SessionStore::validate_session_id(&record.session_id)?;
            anyhow::ensure!(
                record.session_id == session_id,
                "ask {} belongs to another conversation",
                record.ask_id
            );
            durable_owner = Some(record.session_id);
        }
        if let Some(owner) = cached_sessions.get(id) {
            anyhow::ensure!(
                owner == session_id,
                "ask {id} belongs to another conversation"
            );
        } else if pending_ids.contains(id) && durable_owner.is_none() {
            anyhow::bail!("cannot verify owning conversation for pending ask {id}");
        }
    }

    {
        let mut pending = PENDING.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(map) = pending.as_mut() {
            for (id, _, _) in &paths {
                map.remove(id);
            }
        }
    }
    {
        let mut sessions = ASK_SESSIONS.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(map) = sessions.as_mut() {
            for (id, _, _) in &paths {
                map.remove(id);
            }
        }
    }

    let mut removed = 0usize;
    for (_, pending_path, history_path) in paths {
        removed += usize::from(crate::config::private_io::remove_private_file(
            &pending_path,
        )?);
        removed += usize::from(crate::config::private_io::remove_private_file(
            &history_path,
        )?);
    }
    Ok(removed)
}

fn persist_breadcrumb(
    id: &str,
    session_id: &str,
    agent: &str,
    questions: &[crate::tools::ask_user::AskUserQuestion],
    approval: Option<&crate::tools::ask_user::ApprovalRequest>,
) -> anyhow::Result<()> {
    crate::session::SessionStore::validate_session_id(session_id)?;
    let path = breadcrumb_path(id)?;
    // Pending questions are live workflow state, not a bounded cache. Their
    // resolver removes them after durably archiving the answer. Do not scan
    // and evict other tasks' questions while creating this one.
    let breadcrumb = AskRecord {
        version: ASK_BREADCRUMB_VERSION,
        ask_id: id.to_string(),
        session_id: session_id.to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        resolved_at: None,
        agent: agent.to_string(),
        agent_id: immutable_agent_id(agent),
        questions: questions.to_vec(),
        approval: approval.cloned(),
        answer: None,
        status: pending_status(),
        approval_decision: None,
        action_receipt: None,
    };
    let bytes = serde_json::to_vec_pretty(&breadcrumb)?;
    crate::config::private_io::atomic_write_private(&path, &bytes)
}

fn immutable_agent_id(agent: &str) -> Option<String> {
    let company = crate::runtime::company::global_if_initialized()?;
    let snapshot = company.directory_snapshot().ok()?;
    let needle = agent
        .trim()
        .trim_start_matches('@')
        .split(" (")
        .next()
        .unwrap_or(agent)
        .trim();
    let mut matches = snapshot.agents.iter().filter(|record| {
        [
            record.profile.agent_id.as_str(),
            record.profile.internal_role.as_str(),
            record.profile.display_name.as_str(),
        ]
        .into_iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(needle))
    });
    let first = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(first.profile.agent_id.clone())
}

/// Canonical fingerprint helper for runtime-bound action payloads. Callers
/// should include every identity/scope/input field that changes what will be
/// executed; display copy and the approval button label do not belong here.
pub fn action_fingerprint(value: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn valid_fingerprint(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..].chars().all(|ch| ch.is_ascii_hexdigit())
}

fn approval_action_fingerprint(approval: &crate::tools::ask_user::ApprovalRequest) -> String {
    // Runtime-owned approval flows may bind a richer exact payload than the
    // user-facing details card should render. Never trust this alone at
    // consumption: `begin_protected_action` also requires the runtime's
    // independently recomputed expected fingerprint.
    if let Some(bound) = approval.details.get("action_fingerprint") {
        if valid_fingerprint(bound) {
            return bound.to_ascii_lowercase();
        }
    }
    action_fingerprint(&serde_json::json!({
        "action": approval.action,
        "subject": approval.subject,
        "details": approval.details,
    }))
}

fn process_instance() -> &'static str {
    static INSTANCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    INSTANCE
        .get_or_init(|| format!("{}-{}", std::process::id(), uuid::Uuid::new_v4()))
        .as_str()
}

fn prepare_typed_decision(
    id: &str,
    answer: &str,
    forced: Option<ApprovalOutcome>,
) -> anyhow::Result<()> {
    let path = breadcrumb_path(id)?;
    crate::config::private_io::with_private_lock(&path, || {
        let Some(raw) =
            crate::config::private_io::read_private_file_limited(&path, ASK_BREADCRUMB_MAX_BYTES)?
        else {
            return Ok(());
        };
        let mut record: AskRecord = serde_json::from_slice(&raw)?;
        anyhow::ensure!(
            matches!(record.version, 1 | ASK_BREADCRUMB_VERSION) && record.ask_id == id,
            "ask breadcrumb identity/version mismatch"
        );
        let Some(approval) = record.approval.as_ref() else {
            return Ok(());
        };
        if record.approval_decision.is_some() {
            return Ok(());
        }
        let has_bound_continuation = approval
            .details
            .get("action_fingerprint")
            .is_some_and(|value| valid_fingerprint(value));
        let fingerprint = approval_action_fingerprint(approval);
        let outcome = forced.unwrap_or_else(|| {
            if approval.is_presented_in(&record.questions) && approval.confirmed_by(answer) {
                ApprovalOutcome::Approved
            } else {
                ApprovalOutcome::Denied
            }
        });
        record.version = ASK_BREADCRUMB_VERSION;
        record.approval_decision = Some(ApprovalDecisionReceipt {
            outcome,
            decided_at: chrono::Utc::now().to_rfc3339(),
            action_fingerprint: fingerprint.clone(),
        });
        record.action_receipt = has_bound_continuation.then(|| ProtectedActionReceipt {
            action_fingerprint: fingerprint,
            state: match outcome {
                ApprovalOutcome::Approved => ProtectedActionState::Ready,
                ApprovalOutcome::Denied => ProtectedActionState::Denied,
                ApprovalOutcome::Expired | ApprovalOutcome::Canceled => {
                    ProtectedActionState::Canceled
                }
            },
            execution_id: None,
            executor_instance: None,
            started_at: None,
            finished_at: matches!(
                outcome,
                ApprovalOutcome::Denied | ApprovalOutcome::Expired | ApprovalOutcome::Canceled
            )
            .then(|| chrono::Utc::now().to_rfc3339()),
            result_summary: match outcome {
                ApprovalOutcome::Approved => None,
                ApprovalOutcome::Denied => Some("The user denied the protected action.".into()),
                ApprovalOutcome::Expired => Some("The approval expired without execution.".into()),
                ApprovalOutcome::Canceled => {
                    Some("The approval was canceled without execution.".into())
                }
            },
        });
        crate::config::private_io::atomic_write_private_under_lock(
            &path,
            &serde_json::to_vec_pretty(&record)?,
        )
    })
}

fn load_record_at(path: &Path, id: &str) -> anyhow::Result<Option<AskRecord>> {
    let Some(raw) =
        crate::config::private_io::read_private_file_limited(path, ASK_BREADCRUMB_MAX_BYTES)?
    else {
        return Ok(None);
    };
    let record: AskRecord = serde_json::from_slice(&raw)?;
    anyhow::ensure!(
        matches!(record.version, 1 | ASK_BREADCRUMB_VERSION) && record.ask_id == id,
        "ask record identity/version mismatch"
    );
    Ok(Some(record))
}

/// Atomically consume one approved protected action. The caller supplies its
/// independently computed exact fingerprint, so changing the tool input,
/// target coworker, group, or scope after the click invalidates the grant.
pub fn begin_protected_action(
    id: &str,
    expected_fingerprint: &str,
) -> anyhow::Result<ProtectedActionLease> {
    anyhow::ensure!(
        valid_fingerprint(expected_fingerprint),
        "invalid protected-action fingerprint"
    );
    let history = ask_history_path(id)?;
    let pending = breadcrumb_path(id)?;
    for path in [&history, &pending] {
        let result = crate::config::private_io::with_private_lock(path, || {
            let Some(mut record) = load_record_at(path, id)? else {
                return Ok(None);
            };
            let decision = record
                .approval_decision
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("approval decision receipt is missing"))?;
            anyhow::ensure!(
                decision.action_fingerprint == expected_fingerprint,
                "protected action changed after approval"
            );
            anyhow::ensure!(
                decision.outcome == ApprovalOutcome::Approved,
                "protected action was not approved ({:?})",
                decision.outcome
            );
            let receipt = record
                .action_receipt
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("protected action receipt is missing"))?;
            anyhow::ensure!(
                receipt.action_fingerprint == expected_fingerprint,
                "protected action receipt fingerprint mismatch"
            );
            match receipt.state {
                ProtectedActionState::Ready => {}
                ProtectedActionState::Executing => {
                    receipt.state = ProtectedActionState::Blocked;
                    receipt.finished_at = Some(chrono::Utc::now().to_rfc3339());
                    receipt.result_summary = Some(
                        "A previous execution began without a durable result; Phoenix blocked the action instead of risking a duplicate."
                            .into(),
                    );
                    crate::config::private_io::atomic_write_private_under_lock(
                        path,
                        &serde_json::to_vec_pretty(&record)?,
                    )?;
                    anyhow::bail!(
                        "protected action has an uncertain prior execution and is now blocked"
                    );
                }
                ProtectedActionState::Succeeded => {
                    anyhow::bail!("protected action was already completed")
                }
                ProtectedActionState::Failed => {
                    anyhow::bail!("protected action already ran and failed")
                }
                ProtectedActionState::Blocked => {
                    anyhow::bail!("protected action is blocked after an uncertain execution")
                }
                ProtectedActionState::Canceled => {
                    anyhow::bail!("protected action was canceled")
                }
                ProtectedActionState::Denied => {
                    anyhow::bail!("protected action was denied")
                }
            }
            let execution_id = uuid::Uuid::new_v4().to_string();
            let executor_instance = process_instance().to_string();
            receipt.state = ProtectedActionState::Executing;
            receipt.execution_id = Some(execution_id.clone());
            receipt.executor_instance = Some(executor_instance.clone());
            receipt.started_at = Some(chrono::Utc::now().to_rfc3339());
            receipt.finished_at = None;
            receipt.result_summary = None;
            crate::config::private_io::atomic_write_private_under_lock(
                path,
                &serde_json::to_vec_pretty(&record)?,
            )?;
            Ok(Some(ProtectedActionLease {
                ask_id: id.to_string(),
                action_fingerprint: expected_fingerprint.to_string(),
                execution_id,
                executor_instance,
                finished: std::sync::atomic::AtomicBool::new(false),
            }))
        });
        match result {
            Ok(Some(lease)) => return Ok(lease),
            Ok(None) => continue,
            Err(error) => return Err(error),
        }
    }
    anyhow::bail!("approval receipt is missing")
}

/// Record the only terminal result allowed for a consumed action. A stale or
/// forged lease cannot overwrite another execution's receipt.
pub fn finish_protected_action(
    lease: &ProtectedActionLease,
    result: ProtectedActionResult,
    summary: impl Into<String>,
) -> anyhow::Result<()> {
    let history = ask_history_path(&lease.ask_id)?;
    let pending = breadcrumb_path(&lease.ask_id)?;
    let summary = summary.into();
    for path in [&history, &pending] {
        let updated = crate::config::private_io::with_private_lock(path, || {
            let Some(mut record) = load_record_at(path, &lease.ask_id)? else {
                return Ok(false);
            };
            let receipt = record
                .action_receipt
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("protected action receipt is missing"))?;
            anyhow::ensure!(
                receipt.state == ProtectedActionState::Executing
                    && receipt.execution_id.as_deref() == Some(&lease.execution_id)
                    && receipt.executor_instance.as_deref() == Some(&lease.executor_instance)
                    && receipt.action_fingerprint == lease.action_fingerprint,
                "protected action lease is stale or does not own this execution"
            );
            receipt.state = match result {
                ProtectedActionResult::Succeeded => ProtectedActionState::Succeeded,
                ProtectedActionResult::Failed => ProtectedActionState::Failed,
                ProtectedActionResult::Blocked => ProtectedActionState::Blocked,
                ProtectedActionResult::Canceled => ProtectedActionState::Canceled,
            };
            receipt.finished_at = Some(chrono::Utc::now().to_rfc3339());
            receipt.result_summary = Some(summary.clone());
            crate::config::private_io::atomic_write_private_under_lock(
                path,
                &serde_json::to_vec_pretty(&record)?,
            )?;
            Ok(true)
        })?;
        if updated {
            lease
                .finished
                .store(true, std::sync::atomic::Ordering::Release);
            return Ok(());
        }
    }
    anyhow::bail!("protected action receipt disappeared before result commit")
}

fn recover_uncertain_execution(path: &Path, mut record: AskRecord) -> anyhow::Result<AskRecord> {
    let stale = record.action_receipt.as_ref().is_some_and(|receipt| {
        receipt.state == ProtectedActionState::Executing
            && receipt.executor_instance.as_deref() != Some(process_instance())
    });
    if !stale {
        return Ok(record);
    }
    crate::config::private_io::with_private_lock(path, || {
        let Some(mut current) = load_record_at(path, &record.ask_id)? else {
            return Ok(record.clone());
        };
        if let Some(receipt) = current.action_receipt.as_mut() {
            if receipt.state == ProtectedActionState::Executing
                && receipt.executor_instance.as_deref() != Some(process_instance())
            {
                receipt.state = ProtectedActionState::Blocked;
                receipt.finished_at = Some(chrono::Utc::now().to_rfc3339());
                receipt.result_summary = Some(
                    "Phoenix restarted after execution began without a durable result; the action is blocked to prevent a duplicate."
                        .into(),
                );
                crate::config::private_io::atomic_write_private_under_lock(
                    path,
                    &serde_json::to_vec_pretty(&current)?,
                )?;
            }
        }
        record = current;
        Ok(record.clone())
    })
}

fn load_breadcrumb(id: &str) -> anyhow::Result<Option<AskRecord>> {
    let path = breadcrumb_path(id)?;
    let Some(raw) =
        crate::config::private_io::read_private_file_limited(&path, ASK_BREADCRUMB_MAX_BYTES)?
    else {
        return Ok(None);
    };
    let breadcrumb: AskRecord =
        serde_json::from_slice(&raw).map_err(|error| anyhow::anyhow!(error))?;
    anyhow::ensure!(
        matches!(breadcrumb.version, 1 | ASK_BREADCRUMB_VERSION) && breadcrumb.ask_id == id,
        "ask breadcrumb identity/version mismatch"
    );
    crate::session::SessionStore::validate_session_id(&breadcrumb.session_id)?;
    Ok(Some(breadcrumb))
}

fn ask_history_dir() -> PathBuf {
    crate::config::phoenix_home().join("approvals/ask-history")
}

fn ask_history_path(id: &str) -> anyhow::Result<PathBuf> {
    let name = breadcrumb_path(id)?
        .file_name()
        .expect("validated ask breadcrumb has a filename")
        .to_owned();
    Ok(ask_history_dir().join(name))
}

fn archive_resolution(id: &str, answer: &str, status: &str) -> anyhow::Result<()> {
    let pending = breadcrumb_path(id)?;
    crate::config::private_io::with_private_lock(&pending, || {
        let Some(mut record) = load_record_at(&pending, id)? else {
            return Ok(());
        };
        record.version = ASK_BREADCRUMB_VERSION;
        record.resolved_at = Some(chrono::Utc::now().to_rfc3339());
        record.answer = Some(answer.to_string());
        record.status = status.to_string();
        let history = ask_history_path(id)?;
        prune_breadcrumbs_with_cap(
            history.parent().expect("ask history path has a parent"),
            ASK_HISTORY_FILE_CAP,
        )?;
        crate::config::private_io::atomic_write_private(
            &history,
            &serde_json::to_vec_pretty(&record)?,
        )?;
        crate::config::private_io::remove_private_file_under_lock(&pending)?;
        Ok(())
    })?;
    if let Some(map) = ASK_SESSIONS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_mut()
    {
        map.remove(id);
    }
    Ok(())
}

/// Questions for one canonical conversation, both still awaiting a choice and
/// already resolved. Canvas merges these by ask id with its exact display
/// journal, so a relaunch neither loses a popup nor resurrects an answered one.
/// An open, unanswered question from this coworker in this conversation that
/// asks essentially the same thing. Re-asking made the user answer one
/// question several times, each answer then waking a separate turn.
pub fn pending_duplicate(
    session_id: &str,
    agent: &str,
    questions: &[crate::tools::ask_user::AskUserQuestion],
) -> Option<(String, String)> {
    conversation_records(session_id).ok()?.into_iter()
        .filter(|record| record.status == "pending" && record.resolved_at.is_none()
            && record.approval.is_none() && record.agent == agent)
        .find_map(|record| record.questions.iter()
            .find(|old| questions.iter().any(|new| similar_question(&old.question, &new.question)))
            .map(|old| (record.ask_id.clone(), old.question.clone())))
}

fn similar_question(left: &str, right: &str) -> bool {
    let words = |text: &str| text.to_lowercase()
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| word.chars().count() >= 4)
        .map(str::to_string)
        .collect::<std::collections::HashSet<_>>();
    let (left, right) = (words(left), words(right));
    let smaller = left.len().min(right.len());
    smaller >= 4 && left.intersection(&right).count() * 100 >= smaller * 35
}

pub fn conversation_records(session_id: &str) -> anyhow::Result<Vec<AskRecord>> {
    crate::session::SessionStore::validate_session_id(session_id)?;
    let mut records = Vec::new();
    for dir in [
        crate::config::phoenix_home().join("approvals/asks"),
        ask_history_dir(),
    ] {
        if !dir.exists() {
            continue;
        }
        for entry in std::fs::read_dir(&dir)?.take(ASK_HISTORY_FILE_CAP) {
            let entry = entry?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || path.extension().and_then(|value| value.to_str()) != Some("json")
            {
                continue;
            }
            let Some(raw) = crate::config::private_io::read_private_file_limited(
                &path,
                ASK_BREADCRUMB_MAX_BYTES,
            )?
            else {
                continue;
            };
            let Ok(record) = serde_json::from_slice::<AskRecord>(&raw) else {
                tracing::warn!(path = %path.display(), "skipping unreadable ask history record");
                continue;
            };
            let record = match recover_uncertain_execution(&path, record) {
                Ok(record) => record,
                Err(error) => {
                    tracing::warn!(path = %path.display(), "ask execution recovery failed: {error:#}");
                    continue;
                }
            };
            if record.session_id == session_id {
                records.push(record);
            }
        }
    }
    // A crash between publishing history and removing the pending breadcrumb
    // can leave both files. Prefer the resolved copy deterministically rather
    // than letting filesystem iteration order resurrect an approval card.
    let mut by_id = std::collections::BTreeMap::<String, AskRecord>::new();
    for record in records {
        match by_id.entry(record.ask_id.clone()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(record);
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let current = entry.get();
                if (
                    record.resolved_at.is_some(),
                    record.action_receipt.is_some(),
                ) > (
                    current.resolved_at.is_some(),
                    current.action_receipt.is_some(),
                ) {
                    entry.insert(record);
                }
            }
        }
    }
    let mut records = by_id.into_values().collect::<Vec<_>>();
    records.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.ask_id.cmp(&right.ask_id))
    });
    if records.iter().any(|record| !record.questions.is_empty()) {
        records.retain(|record| !record.questions.is_empty());
    } else if let Some(latest_legacy) = records
        .iter()
        .rposition(|record| record.status == "pending")
    {
        let record = records.remove(latest_legacy);
        records.clear();
        records.push(record);
    } else {
        records.clear();
    }
    Ok(records)
}

fn breadcrumb_path(id: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        !id.is_empty()
            && id.len() <= 128
            && id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_')),
        "invalid ask id"
    );
    Ok(crate::config::phoenix_home()
        .join("approvals/asks")
        .join(format!("{id}.json")))
}

fn prune_breadcrumbs_with_cap(dir: &Path, cap: usize) -> anyhow::Result<()> {
    crate::config::private_io::prepare_phoenix_directory(dir)?;
    prune_prepared_breadcrumb_directory(dir, cap)
}

fn prune_prepared_breadcrumb_directory(dir: &Path, cap: usize) -> anyhow::Result<()> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || path.extension().and_then(|value| value.to_str()) != Some("json")
        {
            continue;
        }
        files.push((metadata.modified().ok(), path));
    }
    files.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    let remove_count = files.len().saturating_add(1).saturating_sub(cap);
    let mut removed = 0;
    for (_, path) in files {
        if removed >= remove_count { break; }
        // Capacity pressure must not delete unanswered questions or durable
        // task evidence. Unknown/corrupt ownership is not permission to prune.
        let Some(raw) = crate::config::private_io::read_private_file_limited(&path, ASK_BREADCRUMB_MAX_BYTES)? else { continue; };
        let Ok(record) = serde_json::from_slice::<AskRecord>(&raw) else { continue; };
        if record.status == "pending" || record.resolved_at.is_none() { continue; }
        let Some(company) = crate::runtime::company::global_if_initialized() else { continue; };
        if company.group_ask_is_bound(&record.session_id, &record.ask_id)? { continue; }
        let _ = crate::config::private_io::remove_private_file(&path)?;
        removed += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn rephrased_question_is_a_duplicate_but_a_new_topic_is_not() {
        let first = "Jessica says the Activity Hour Plan must be chosen with a parent. Are these planned activities and hours approved by you and your parent for Oct–Apr: swimming 25, dance 10, stretching 10, basketball 20, weight training 15? Also, do you have an adult supervisor/facility for each? I can put them in the school’s original template once you confirm.";
        let again = "Jessica says you and a parent must choose the activities together. Can I put the existing proposed 80-hour mix into the school’s original form as a draft—swimming 25, dance 10, stretching 10, basketball 20, weights 15—or what would you change? I’ll leave all signatures blank.";
        let other = "For Social Assignment 1, Part A(c) and Part B 2(c) sound templated. What would you personally say about how family/VVS shape you, and how school, safety, health care, and money affect your life?";
        assert!(super::similar_question(first, again));
        assert!(!super::similar_question(first, other));
        assert!(!super::similar_question(again, other));
    }

    use super::*;

    #[test]
    fn exact_decision_lookup_prefers_history_and_fails_on_corruption() {
        let home = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let id = "history-lookup";
        drop(register(id, "agent-avery"));
        let pending = breadcrumb_path(id).unwrap();
        let old = std::fs::read(&pending).unwrap();
        archive_late_answer(id, "Blue");
        // A crash can leave both files. The old pending copy cannot revive it.
        crate::config::private_io::atomic_write_private(&pending, &old).unwrap();
        let record = decision_record_for(id).unwrap().unwrap();
        assert_eq!(record.status, "answered_late");
        assert_eq!(record.answer.as_deref(), Some("Blue"));
        crate::config::private_io::atomic_write_private(&ask_history_path(id).unwrap(), b"broken").unwrap();
        assert!(decision_record_for(id).is_err());
        abandon(id);
    }

    #[test]
    fn question_capacity_never_evicts_pending_or_unreadable_records() {
        let fixture = tempfile::tempdir().unwrap();
        let pending = fixture.path().join("pending.json");
        let unreadable = fixture.path().join("unreadable.json");
        let bytes = br#"{"version":2,"ask_id":"pending","session_id":"group-retention","created_at":"2026-09-05T00:00:00Z","status":"pending"}"#;
        crate::config::private_io::atomic_write_private(&pending, bytes).unwrap();
        crate::config::private_io::atomic_write_private(&unreadable, b"{").unwrap();
        prune_prepared_breadcrumb_directory(fixture.path(), 0).unwrap();
        assert_eq!(std::fs::read(&pending).unwrap(), bytes);
        assert_eq!(std::fs::read(&unreadable).unwrap(), b"{");
    }

    #[test]
    fn group_readiness_uses_exact_durable_question() {
        let fixture = tempfile::tempdir().unwrap();
        let pending = fixture.path().join("pending.json");
        let history = fixture.path().join("history.json");
        let mut record = serde_json::json!({"version":2,"ask_id":"question","session_id":"group-fixture","agent_id":"iris","created_at":"2026-09-05T00:00:00Z","status":"pending"});
        let write = |path: &Path, value: &serde_json::Value| crate::config::private_io::atomic_write_private(path, &serde_json::to_vec(value).unwrap()).unwrap();
        let read = || group_question_pending_at(&pending, &history, "question", "group-fixture", "iris");
        assert!(read().is_err(), "missing is not answered");
        write(&pending, &record);
        assert!(read().unwrap(), "legacy empty question payload still gates work");
        assert!(group_question_pending_at(&pending, &history, "question", "group-fixture", "leo").is_err());
        assert!(group_question_pending_at(&pending, &history, "question", "other-group", "iris").is_err());
        // Unrelated corrupt files cannot poison or hide this exact record.
        crate::config::private_io::atomic_write_private(&fixture.path().join("unrelated.json"), b"{").unwrap();
        assert!(read().unwrap());
        record["status"] = "answered".into();
        write(&history, &record);
        assert!(read().is_err(), "status alone is not durable resolution evidence");
        record["resolved_at"] = "2026-09-05T00:01:00Z".into();
        write(&history, &record);
        assert!(!read().unwrap(), "resolution wins over leftover pending copy");
        crate::config::private_io::atomic_write_private(&history, b"{").unwrap();
        assert!(read().is_err(), "corrupt authoritative history is not silently skipped");
    }

    fn question(text: &str) -> crate::tools::ask_user::AskUserQuestion {
        crate::tools::ask_user::AskUserQuestion {
            question: text.to_string(),
            header: Some("Choice".to_string()),
            options: vec!["Yes".to_string(), "No".to_string()],
            multi_select: false,
        }
    }

    fn approval(fingerprint: &str) -> crate::tools::ask_user::ApprovalRequest {
        crate::tools::ask_user::ApprovalRequest {
            action: "governed_effect".to_string(),
            subject: "exact message".to_string(),
            approved_option: "Approve this exact action".to_string(),
            details: std::collections::BTreeMap::from([
                ("scope".to_string(), "single_call".to_string()),
                ("action_fingerprint".to_string(), fingerprint.to_string()),
            ]),
        }
    }

    fn approval_question() -> crate::tools::ask_user::AskUserQuestion {
        crate::tools::ask_user::AskUserQuestion {
            question: "Send this exact message?".into(),
            header: Some("Send".into()),
            options: vec!["Approve this exact action".into(), "Do not allow".into()],
            multi_select: false,
        }
    }

    #[tokio::test]
    async fn answer_resolves_registered_ask() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let rx = register("ask-t1", "main-s1");
        assert!(answer("ask-t1", "yes".into()));
        assert_eq!(rx.await.unwrap(), "yes");
        assert!(!answer("ask-t1", "again".into()));
        assert!(session_for("ask-t1").is_none());
    }

    #[tokio::test]
    async fn dismiss_resolves_live_ask_without_leaving_a_late_answer_breadcrumb() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let rx = register("ask-dismiss", "main-dismiss");
        assert!(dismiss("ask-dismiss"));
        assert_eq!(rx.await.unwrap(), "Not now");
        assert!(session_for("ask-dismiss").is_none());
        assert!(!dismiss("ask-dismiss"));
    }

    #[test]
    fn abandon_clears_pending_but_keeps_the_session_breadcrumb() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let _rx = register("ask-t2", "main-s2");
        abandon("ask-t2");
        // The pending entry is gone — a direct answer no longer delivers…
        assert!(!answer("ask-t2", "late".into()));
        // …but the asking session is still known, so the late answer can
        // wake it instead of vanishing.
        assert_eq!(session_for("ask-t2").as_deref(), Some("main-s2"));
        assert!(session_for("ask-never-registered").is_none());
    }

    #[test]
    fn session_breadcrumb_survives_process_memory_loss() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let _rx = register("ask-restart", "company-phoenix");
        abandon("ask-restart");
        *ASK_SESSIONS.lock().unwrap_or_else(|p| p.into_inner()) = None;

        assert_eq!(
            session_for("ask-restart").as_deref(),
            Some("company-phoenix")
        );
        forget("ask-restart");
        assert!(session_for("ask-restart").is_none());
    }

    #[test]
    fn complete_question_payload_survives_process_memory_loss() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let _rx = register_with_payload(
            "ask-full-restart",
            "main-full-restart",
            "phoenix",
            &[question("What should Phoenix do?")],
            None,
        );
        abandon("ask-full-restart");
        *ASK_SESSIONS.lock().unwrap_or_else(|p| p.into_inner()) = None;

        let rows = conversation_records("main-full-restart").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ask_id, "ask-full-restart");
        assert_eq!(rows[0].agent, "phoenix");
        assert_eq!(rows[0].questions[0].question, "What should Phoenix do?");
        assert_eq!(rows[0].status, "pending");
    }

    #[test]
    fn detached_question_stays_visible_and_routes_its_answer_as_a_continuation() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        register_detached_with_payload(
            "ask-detached",
            "main-detached",
            "phoenix",
            &[question("Which finish should Phoenix use?")],
            None,
        );

        assert!(!answer("ask-detached", "Matte".to_string()));
        assert_eq!(
            session_for("ask-detached").as_deref(),
            Some("main-detached")
        );
        let rows = conversation_records("main-detached").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "pending");
        assert_eq!(
            rows[0].questions[0].question,
            "Which finish should Phoenix use?"
        );
    }

    #[test]
    fn newest_legacy_pending_question_remains_recoverable() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let _old = register("ask-legacy-old", "main-legacy");
        let _new = register("ask-legacy-new", "main-legacy");
        abandon("ask-legacy-old");
        abandon("ask-legacy-new");

        let rows = conversation_records("main-legacy").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ask_id, "ask-legacy-new");
        assert!(rows[0].questions.is_empty());
    }

    #[tokio::test]
    async fn answered_question_moves_to_durable_conversation_history() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let rx = register_with_payload(
            "ask-full-answer",
            "main-full-answer",
            "phoenix",
            &[question("Teach Phoenix now?")],
            None,
        );
        assert!(answer("ask-full-answer", "Yes".to_string()));
        assert_eq!(rx.await.unwrap(), "Yes");

        let rows = conversation_records("main-full-answer").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "answered");
        assert_eq!(rows[0].answer.as_deref(), Some("Yes"));
        assert!(rows[0].resolved_at.is_some());
        assert!(session_for("ask-full-answer").is_none());
    }

    #[tokio::test]
    async fn typed_approval_is_durable_before_wake_and_consumed_exactly_once() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let fingerprint = action_fingerprint(&serde_json::json!({
            "tool": "send",
            "recipient": "one@example.com",
            "body": "one"
        }));
        let rx = register_with_payload(
            "ask-approved-once",
            "main-approved-once",
            "phoenix",
            &[approval_question()],
            Some(&approval(&fingerprint)),
        );

        assert!(answer(
            "ask-approved-once",
            "A: Approve this exact action".into()
        ));
        assert_eq!(rx.await.unwrap(), "A: Approve this exact action");
        let record = conversation_records("main-approved-once")
            .unwrap()
            .remove(0);
        assert_eq!(
            record.approval_decision.as_ref().unwrap().outcome,
            ApprovalOutcome::Approved
        );
        assert_eq!(
            record.action_receipt.as_ref().unwrap().state,
            ProtectedActionState::Ready
        );

        let lease = begin_protected_action("ask-approved-once", &fingerprint).unwrap();
        finish_protected_action(
            &lease,
            ProtectedActionResult::Succeeded,
            "sent exactly once",
        )
        .unwrap();
        let error = begin_protected_action("ask-approved-once", &fingerprint).unwrap_err();
        assert!(error.to_string().contains("already completed"));
        let record = conversation_records("main-approved-once")
            .unwrap()
            .remove(0);
        assert_eq!(
            record.action_receipt.as_ref().unwrap().state,
            ProtectedActionState::Succeeded
        );
    }

    #[tokio::test]
    async fn denial_and_changed_payload_never_execute() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let approved_fingerprint = action_fingerprint(&serde_json::json!({"message": "one"}));
        let changed_fingerprint = action_fingerprint(&serde_json::json!({"message": "two"}));

        let denied_rx = register_with_payload(
            "ask-denied-action",
            "main-denied-action",
            "phoenix",
            &[approval_question()],
            Some(&approval(&approved_fingerprint)),
        );
        assert!(answer("ask-denied-action", "A: Do not allow".into()));
        assert_eq!(denied_rx.await.unwrap(), "A: Do not allow");
        let denied = begin_protected_action("ask-denied-action", &approved_fingerprint)
            .unwrap_err()
            .to_string();
        assert!(denied.contains("not approved") || denied.contains("denied"));

        let changed_rx = register_with_payload(
            "ask-changed-action",
            "main-changed-action",
            "phoenix",
            &[approval_question()],
            Some(&approval(&approved_fingerprint)),
        );
        assert!(answer(
            "ask-changed-action",
            "A: Approve this exact action".into()
        ));
        assert_eq!(changed_rx.await.unwrap(), "A: Approve this exact action");
        let changed = begin_protected_action("ask-changed-action", &changed_fingerprint)
            .unwrap_err()
            .to_string();
        assert!(changed.contains("changed after approval"));
    }

    #[tokio::test]
    async fn dismissal_and_expiration_have_typed_non_executing_outcomes() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let fingerprint = action_fingerprint(&serde_json::json!({"purchase": "sku-1"}));

        let dismiss_rx = register_with_payload(
            "ask-canceled-action",
            "main-canceled-action",
            "phoenix",
            &[approval_question()],
            Some(&approval(&fingerprint)),
        );
        assert!(dismiss("ask-canceled-action"));
        assert_eq!(dismiss_rx.await.unwrap(), "Not now");
        let canceled = conversation_records("main-canceled-action")
            .unwrap()
            .remove(0);
        assert_eq!(
            canceled.approval_decision.as_ref().unwrap().outcome,
            ApprovalOutcome::Canceled
        );
        assert_eq!(
            canceled.action_receipt.as_ref().unwrap().state,
            ProtectedActionState::Canceled
        );

        let expire_rx = register_with_payload(
            "ask-expired-action",
            "main-expired-action",
            "phoenix",
            &[approval_question()],
            Some(&approval(&fingerprint)),
        );
        assert!(expire_approval("ask-expired-action").unwrap());
        assert!(expire_rx.await.is_err());
        let expired = conversation_records("main-expired-action")
            .unwrap()
            .remove(0);
        assert_eq!(
            expired.approval_decision.as_ref().unwrap().outcome,
            ApprovalOutcome::Expired
        );
        assert_eq!(
            expired.action_receipt.as_ref().unwrap().state,
            ProtectedActionState::Canceled
        );
    }

    #[tokio::test]
    async fn abandoned_or_crash_uncertain_execution_becomes_blocked() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let fingerprint = action_fingerprint(&serde_json::json!({"external_delete": "item-1"}));

        let rx = register_with_payload(
            "ask-abandoned-action",
            "main-abandoned-action",
            "phoenix",
            &[approval_question()],
            Some(&approval(&fingerprint)),
        );
        assert!(answer(
            "ask-abandoned-action",
            "A: Approve this exact action".into()
        ));
        rx.await.unwrap();
        let lease = begin_protected_action("ask-abandoned-action", &fingerprint).unwrap();
        drop(lease);
        let record = conversation_records("main-abandoned-action")
            .unwrap()
            .remove(0);
        assert_eq!(
            record.action_receipt.as_ref().unwrap().state,
            ProtectedActionState::Blocked
        );

        let rx = register_with_payload(
            "ask-crash-action",
            "main-crash-action",
            "phoenix",
            &[approval_question()],
            Some(&approval(&fingerprint)),
        );
        assert!(answer(
            "ask-crash-action",
            "A: Approve this exact action".into()
        ));
        rx.await.unwrap();
        let lease = begin_protected_action("ask-crash-action", &fingerprint).unwrap();
        std::mem::forget(lease); // simulate the old process disappearing
        let history = ask_history_path("ask-crash-action").unwrap();
        let mut record = load_record_at(&history, "ask-crash-action")
            .unwrap()
            .unwrap();
        record.action_receipt.as_mut().unwrap().executor_instance = Some("previous-process".into());
        crate::config::private_io::atomic_write_private(
            &history,
            &serde_json::to_vec_pretty(&record).unwrap(),
        )
        .unwrap();
        let recovered = conversation_records("main-crash-action").unwrap().remove(0);
        assert_eq!(
            recovered.action_receipt.as_ref().unwrap().state,
            ProtectedActionState::Blocked
        );
    }

    #[test]
    fn late_approval_cannot_resurrect_a_dead_protected_continuation() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let fingerprint = action_fingerprint(&serde_json::json!({"external_send": "message-1"}));
        register_detached_with_payload(
            "ask-late-protected",
            "main-late-protected",
            "phoenix",
            &[approval_question()],
            Some(&approval(&fingerprint)),
        );

        assert!(!answer(
            "ask-late-protected",
            "A: Approve this exact action".into()
        ));
        archive_late_answer("ask-late-protected", "A: Approve this exact action");
        let record = conversation_records("main-late-protected")
            .unwrap()
            .remove(0);
        assert_eq!(record.status, "answered_late");
        assert_eq!(
            record.approval_decision.as_ref().unwrap().outcome,
            ApprovalOutcome::Approved
        );
        assert_eq!(
            record.action_receipt.as_ref().unwrap().state,
            ProtectedActionState::Blocked
        );
        assert!(begin_protected_action("ask-late-protected", &fingerprint).is_err());
    }

    #[test]
    fn legacy_ask_json_defaults_new_receipt_fields() {
        let raw = serde_json::json!({
            "version": 1,
            "ask_id": "ask-legacy-json",
            "session_id": "main-legacy-json",
            "created_at": "2026-01-01T00:00:00Z",
            "status": "answered",
            "answer": "Yes"
        });
        let record: AskRecord = serde_json::from_value(raw).unwrap();
        assert!(record.approval_decision.is_none());
        assert!(record.action_receipt.is_none());
    }

    #[tokio::test]
    async fn purge_records_is_owned_atomic_and_idempotent() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let suffix = uuid::Uuid::new_v4();
        let owner = format!("main-purge-owner-{suffix}");
        let other = format!("main-purge-other-{suffix}");
        let pending_id = format!("ask-purge-pending-{suffix}");
        let history_id = format!("ask-purge-history-{suffix}");
        let other_id = format!("ask-purge-other-{suffix}");

        let pending_rx = register_with_payload(
            &pending_id,
            &owner,
            "phoenix",
            &[question("Pending owner question")],
            None,
        );
        let history_rx = register_with_payload(
            &history_id,
            &owner,
            "phoenix",
            &[question("Resolved owner question")],
            None,
        );
        assert!(answer(&history_id, "Yes".to_string()));
        assert_eq!(history_rx.await.unwrap(), "Yes");
        let other_rx = register_with_payload(
            &other_id,
            &other,
            "phoenix",
            &[question("Other conversation question")],
            None,
        );

        let malformed = "../not-an-ask".to_string();
        let error = purge_records(&owner, &[pending_id.clone(), malformed])
            .expect_err("all ids must validate before mutation");
        assert!(error.to_string().contains("invalid ask id"));
        assert_eq!(session_for(&pending_id).as_deref(), Some(owner.as_str()));

        // Mixed ownership is rejected before either valid owner record is
        // touched, including the in-memory pending sender.
        let error = purge_records(
            &owner,
            &[pending_id.clone(), history_id.clone(), other_id.clone()],
        )
        .expect_err("cross-conversation purge must fail");
        assert!(error.to_string().contains("another conversation"));
        assert_eq!(session_for(&pending_id).as_deref(), Some(owner.as_str()));
        assert!(record_for(&pending_id).is_some());
        assert!(record_for(&history_id).is_none());
        assert_eq!(conversation_records(&owner).unwrap().len(), 2);

        assert_eq!(
            purge_records(&owner, &[pending_id.clone(), history_id.clone()]).unwrap(),
            2
        );
        assert!(
            pending_rx.await.is_err(),
            "pending sender should be dropped"
        );
        assert!(session_for(&pending_id).is_none());
        assert!(conversation_records(&owner).unwrap().is_empty());

        // Missing files and already-dropped senders are an idempotent success.
        assert_eq!(
            purge_records(&owner, &[pending_id.clone(), history_id.clone()]).unwrap(),
            0
        );

        assert_eq!(
            purge_records(&other, std::slice::from_ref(&other_id)).unwrap(),
            1
        );
        assert!(other_rx.await.is_err());
    }
}
