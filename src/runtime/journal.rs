//! Turn execution journal — the run's shared memory of WHAT ACTUALLY HAPPENED
//! (2026-07-06, the "gate ruled blind" fix).
//!
//! The live forensic failure: the completion gate bounced an honest blocker
//! final back into a login wall the agent had already hit five times, the
//! sweep steered onto a surface the team had already ruled out, and a "stop"
//! final was judged against a mission the user had just superseded — every
//! watcher ruling on the final answer alone, blind to the run it capped.
//!
//! This module is the fix's backbone: a rolling, bounded, durable record
//! per MAIN session of run-level events — user prompts, background spawns and
//! returns, chain handoffs, steers delivered, asks and how they resolved,
//! gate bounces, environment changes (browser crash/relaunch), login walls.
//! Every watcher that rules on work reads `history_digest` first, so verdicts
//! are made against the lived run, not the final paragraph. The bounded log is
//! stored below `~/.phoenix/company/journal/`; a gateway restart therefore
//! cannot make a resumed coworker repeat a login wall, discarded route, or
//! question the user already answered.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Events kept per session — enough to span a long multi-agent request
/// without ever letting one chatty run grow the digest unbounded.
const MAX_EVENTS: usize = 80;
/// Sessions tracked at once (idle sessions are pruned oldest-first).
const MAX_SESSIONS: usize = 24;
/// Per-line and whole-digest caps keep judge calls cheap.
const LINE_CAP: usize = 220;
const DIGEST_CAP: usize = 3000;
const JOURNAL_VERSION: u32 = 1;
const JOURNAL_FILE_MAX_BYTES: usize = 128 * 1024;
const DELETED_TURN_KIND: &str = "deleted-turn";
const DELETED_RESPONSE_KIND: &str = "deleted-response";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    at: chrono::DateTime<chrono::Utc>,
    /// Short kind tag: "user", "spawned", "returned", "handoff", "steer",
    /// "ask", "gate-bounce", "environment", "login-wall".
    kind: String,
    /// Agent label the event belongs to ("user" for user messages).
    agent: String,
    detail: String,
    /// Structured lifecycle identity retained alongside the compact human
    /// digest. Defaults keep every v1 journal written before this field valid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    handoff_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    requester: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    receiver: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reply_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    causation_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SessionLog {
    events: VecDeque<Entry>,
    last_touched: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DurableJournal {
    version: u32,
    session_id: String,
    log: SessionLog,
}

fn state() -> &'static Mutex<HashMap<String, SessionLog>> {
    static STATE: OnceLock<Mutex<HashMap<String, SessionLog>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Append one event to a session's journal. `agent` is the postbox-style
/// label ("browser", "coder#2", "orchestrator", "user").
pub fn record(session_id: &str, kind: &'static str, agent: &str, detail: &str) {
    let entry = Entry {
        at: chrono::Utc::now(),
        kind: kind.to_string(),
        agent: agent.to_string(),
        detail: cap(
            detail
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .as_str(),
            LINE_CAP,
        ),
        handoff_id: None,
        requester: None,
        receiver: None,
        subject: None,
        status: None,
        reply_to: None,
        causation_id: None,
    };
    record_entry(session_id, entry);
}

/// Persist one handoff lifecycle beat with exact correlation metadata. The
/// ordinary digest stays readable while recovery/diagnostics no longer have
/// to infer identity from mutable display names and repeated subjects.
#[allow(clippy::too_many_arguments)]
pub fn record_handoff(
    session_id: &str,
    handoff_id: &str,
    requester: &str,
    receiver: &str,
    subject: &str,
    status: &str,
    reply_to: Option<&str>,
    causation_id: Option<&str>,
) {
    let detail = format!("{requester} → {receiver}: {subject} [{status}]");
    let entry = Entry {
        at: chrono::Utc::now(),
        kind: "handoff".to_string(),
        agent: requester.to_string(),
        detail: cap(&detail, LINE_CAP),
        handoff_id: (!handoff_id.is_empty()).then(|| handoff_id.to_string()),
        requester: Some(requester.to_string()),
        receiver: Some(receiver.to_string()),
        subject: Some(subject.to_string()),
        status: Some(status.to_string()),
        reply_to: reply_to
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        causation_id: causation_id
            .filter(|value| !value.is_empty())
            .map(str::to_string),
    };
    record_entry(session_id, entry);
}

fn record_entry(session_id: &str, entry: Entry) {
    let log = match append_durable(session_id, entry.clone()) {
        Ok(log) => log,
        Err(error) => {
            tracing::warn!("journal [{session_id}] could not persist event: {error:#}");
            let mut map = state().lock().unwrap_or_else(|p| p.into_inner());
            let log = map.entry(session_id.to_string()).or_default();
            append_entry(log, entry);
            log.clone()
        }
    };
    remember(session_id, log);
}

fn remember(session_id: &str, log: SessionLog) {
    let mut map = state().lock().unwrap_or_else(|p| p.into_inner());
    if map.len() >= MAX_SESSIONS && !map.contains_key(session_id) {
        // Prune the coldest session so the map never grows unbounded.
        if let Some(coldest) = map
            .iter()
            .min_by_key(|(_, log)| log.last_touched)
            .map(|(id, _)| id.clone())
        {
            map.remove(&coldest);
        }
    }
    map.insert(session_id.to_string(), log);
}

fn append_entry(log: &mut SessionLog, entry: Entry) {
    log.last_touched = chrono::Utc::now().timestamp();
    if log.events.len() >= MAX_EVENTS {
        log.events.pop_front();
    }
    log.events.push_back(entry);
}

fn append_durable(session_id: &str, entry: Entry) -> anyhow::Result<SessionLog> {
    crate::session::SessionStore::validate_session_id(session_id)?;
    let path = journal_path(session_id)?;
    crate::config::private_io::read_modify_write_private(&path, |current| {
        let mut durable = decode(current, session_id)?;
        append_entry(&mut durable.log, entry);
        let bytes = serde_json::to_vec(&durable)?;
        anyhow::ensure!(
            bytes.len() <= JOURNAL_FILE_MAX_BYTES,
            "execution journal exceeded its bounded file limit"
        );
        Ok((durable.log.clone(), bytes))
    })
}

fn decode(current: Option<&[u8]>, session_id: &str) -> anyhow::Result<DurableJournal> {
    let Some(bytes) = current else {
        return Ok(DurableJournal {
            version: JOURNAL_VERSION,
            session_id: session_id.to_string(),
            log: SessionLog::default(),
        });
    };
    anyhow::ensure!(
        bytes.len() <= JOURNAL_FILE_MAX_BYTES,
        "execution journal is unexpectedly large"
    );
    let durable: DurableJournal = serde_json::from_slice(bytes)?;
    anyhow::ensure!(
        durable.version == JOURNAL_VERSION && durable.session_id == session_id,
        "execution journal identity/version mismatch"
    );
    anyhow::ensure!(
        durable.log.events.len() <= MAX_EVENTS,
        "execution journal contains too many events"
    );
    Ok(durable)
}

fn journal_path(session_id: &str) -> anyhow::Result<PathBuf> {
    crate::session::SessionStore::validate_session_id(session_id)?;
    let hash = format!("{:x}", Sha256::digest(session_id.as_bytes()));
    Ok(crate::config::phoenix_home()
        .join("company/journal")
        .join(format!("{hash}.json")))
}

fn load_durable(session_id: &str) -> anyhow::Result<Option<SessionLog>> {
    let path = journal_path(session_id)?;
    let Some(bytes) =
        crate::config::private_io::read_private_file_limited(&path, JOURNAL_FILE_MAX_BYTES)?
    else {
        return Ok(None);
    };
    Ok(Some(decode(Some(&bytes), session_id)?.log))
}

/// The judge-facing digest: the session's recent execution history as one
/// compact chronological block (oldest first, newest last), or "" when the
/// session has no recorded events. This is what lets a watcher rule on a
/// final WITH the run in view — what was spawned, what returned and how,
/// what the user was asked and answered, what crashed, what already bounced.
pub fn history_digest(session_id: &str) -> String {
    let cached = state()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(session_id)
        .cloned();
    // Disk is authoritative even when this process has a cache: another
    // thread/process may have appended after our last `remember`. Fall back
    // to memory only when persistence is absent or temporarily unreadable.
    let log = match load_durable(session_id) {
        Ok(Some(log)) => {
            remember(session_id, log.clone());
            log
        }
        Ok(None) => match cached {
            Some(log) => log,
            None => return String::new(),
        },
        Err(error) => {
            tracing::warn!("journal [{session_id}] could not be loaded: {error:#}");
            match cached {
                Some(log) => log,
                None => return String::new(),
            }
        }
    };
    if log.events.is_empty() {
        return String::new();
    }
    let now = chrono::Utc::now();
    let mut lines: Vec<String> = log
        .events
        .iter()
        // Deletion tombstones preserve turn ordinals for crash-safe retries,
        // but are not execution history and must never be reinjected into a
        // model prompt.
        .filter(|entry| {
            !matches!(
                entry.kind.as_str(),
                DELETED_TURN_KIND | DELETED_RESPONSE_KIND
            )
        })
        .map(|entry| {
            let mins = (now - entry.at).num_minutes().max(0);
            format!(
                "[{mins}m ago] {} ({}): {}",
                entry.kind, entry.agent, entry.detail
            )
        })
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    // The digest is capped from the FRONT: when a long run overflows, the
    // oldest lines fold away and the recent history — the part verdicts
    // depend on — survives intact.
    loop {
        let text = lines.join("\n");
        if text.chars().count() <= DIGEST_CAP || lines.len() <= 1 {
            return text;
        }
        lines.remove(0);
    }
}

fn prompt_identity(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn journal_prompt_matches(stored: &str, expected: &str) -> bool {
    let stored = prompt_identity(stored);
    let stored = stored.trim_end_matches('…');
    let expected = prompt_identity(expected);
    stored == expected || (!stored.is_empty() && expected.starts_with(stored))
}

fn deletion_identity(expected_prompt: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(prompt_identity(expected_prompt).as_bytes())
    )
}

fn is_turn_boundary(entry: &Entry) -> bool {
    matches!(entry.kind.as_str(), "user" | DELETED_TURN_KIND)
}

/// Remove the execution-journal slice owned by one permanently deleted turn.
/// `keep_user` is true for response-only deletion.  Legacy specialist journals
/// did not record user boundaries; in that ambiguous case the bounded
/// operational journal is cleared rather than risk reinjecting deleted work.
pub fn purge_turn(
    session_id: &str,
    turns_from_end: usize,
    expected_prompt: &str,
    keep_user: bool,
) -> anyhow::Result<usize> {
    crate::session::SessionStore::validate_session_id(session_id)?;
    let path = journal_path(session_id)?;
    let mut updated = None;
    let removed = crate::config::private_io::read_modify_write_private(&path, |current| {
        let mut durable = decode(current, session_id)?;
        let boundaries = durable
            .log
            .events
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| is_turn_boundary(entry).then_some(index))
            .collect::<Vec<_>>();
        let selected = boundaries
            .len()
            .checked_sub(turns_from_end.saturating_add(1))
            .and_then(|ordinal| boundaries.get(ordinal).copied());

        let before = durable.log.events.len();
        let removed = match selected {
            Some(selected) if durable.log.events[selected].kind == DELETED_TURN_KIND => 0,
            Some(selected)
                if journal_prompt_matches(
                    &durable.log.events[selected].detail,
                    expected_prompt,
                ) =>
            {
                let end = boundaries
                    .iter()
                    .copied()
                    .find(|index| *index > selected)
                    .unwrap_or(before);
                let marker = deletion_identity(expected_prompt);
                if keep_user
                    && durable
                        .log
                        .events
                        .range(selected.saturating_add(1)..end)
                        .any(|entry| entry.kind == DELETED_RESPONSE_KIND && entry.detail == marker)
                {
                    0
                } else if keep_user {
                    let start = selected.saturating_add(1);
                    let removed = end.saturating_sub(start);
                    if start < end {
                        durable.log.events.drain(start..end);
                    }
                    durable.log.events.insert(
                        start,
                        Entry {
                            at: chrono::Utc::now(),
                            kind: DELETED_RESPONSE_KIND.to_string(),
                            agent: "system".to_string(),
                            detail: marker,
                            handoff_id: None,
                            requester: None,
                            receiver: None,
                            subject: None,
                            status: None,
                            reply_to: None,
                            causation_id: None,
                        },
                    );
                    removed
                } else {
                    let removed = end.saturating_sub(selected);
                    let at = durable.log.events[selected].at;
                    durable.log.events.drain(selected..end);
                    durable.log.events.insert(
                        selected,
                        Entry {
                            at,
                            kind: DELETED_TURN_KIND.to_string(),
                            agent: "system".to_string(),
                            detail: marker,
                            handoff_id: None,
                            requester: None,
                            receiver: None,
                            subject: None,
                            status: None,
                            reply_to: None,
                            causation_id: None,
                        },
                    );
                    removed
                }
            }
            Some(_) => 0,
            None if boundaries.is_empty() => {
                // No trustworthy boundary exists in pre-hardening journals.
                // This log is only a bounded anti-repeat aid; canonical
                // conversation history remains untouched.
                let removed = durable.log.events.len();
                durable.log.events.clear();
                removed
            }
            // A journal containing turn boundaries is modern. If the target
            // ordinal is gone or the prompt identity does not match, this is
            // an idempotent recovery retry (or stale request), never license
            // to clear unrelated turns.
            None => 0,
        };
        if removed > 0 {
            durable.log.last_touched = chrono::Utc::now().timestamp();
        }
        updated = Some(durable.log.clone());
        Ok((removed, serde_json::to_vec(&durable)?))
    })?;
    if let Some(log) = updated {
        remember(session_id, log);
    }
    Ok(removed)
}

/// Classify an ask_user outcome for the journal from the resolved answer
/// text: the plumbing writes fixed sentinels for dismissal and timeout, and
/// anything else is the user's actual answer.
pub fn ask_resolution(answer: &str) -> String {
    if answer.contains("dismissed the question") {
        "DISMISSED by the user without answering".to_string()
    } else if answer.contains("No answer from the user within") {
        "TIMED OUT unanswered".to_string()
    } else {
        format!("answered: {}", cap(answer, 120))
    }
}

fn cap(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max).collect();
        format!("{cut}…")
    }
}

/// Test hook: wipe one session's journal.
#[cfg(test)]
pub fn clear(session_id: &str) {
    state()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(session_id);
    if let Ok(path) = journal_path(session_id) {
        let _ = crate::config::private_io::remove_private_file(&path);
    }
}

#[cfg(test)]
fn clear_memory(session_id: &str) {
    state()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(session_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_digest_chronologically_and_stays_bounded() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let sid = format!("journal-test-{}", uuid::Uuid::new_v4());
        assert_eq!(history_digest(&sid), "");

        record(&sid, "user", "user", "browse my discord announcements");
        record(
            &sid,
            "spawned",
            "browser",
            "Browse Discord Openclaw announcements",
        );
        record(
            &sid,
            "ask",
            "browser",
            "log into Discord → DISMISSED by the user without answering",
        );
        record(
            &sid,
            "gate-bounce",
            "browser",
            "did not read any announcements",
        );

        let digest = history_digest(&sid);
        let user_pos = digest.find("browse my discord").expect("user event");
        let bounce_pos = digest.find("gate-bounce").expect("bounce event");
        assert!(
            user_pos < bounce_pos,
            "oldest first, newest last:\n{digest}"
        );
        assert!(digest.contains("DISMISSED"));

        // Overflow drops the OLDEST events; the recent history survives.
        for i in 0..(MAX_EVENTS + 10) {
            record(&sid, "environment", "browser", &format!("relaunch #{i}"));
        }
        let digest = history_digest(&sid);
        assert!(!digest.contains("browse my discord"), "oldest folded away");
        assert!(digest.contains(&format!("relaunch #{}", MAX_EVENTS + 9)));
        assert!(digest.chars().count() <= DIGEST_CAP + 1);
        clear(&sid);
    }

    #[test]
    fn history_survives_gateway_memory_loss() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let sid = format!("journal-restart-{}", uuid::Uuid::new_v4());
        record(
            &sid,
            "login-wall",
            "researcher",
            "GitHub asked the user to sign in",
        );
        clear_memory(&sid);

        let digest = history_digest(&sid);
        assert!(digest.contains("GitHub asked the user to sign in"));
        assert!(digest.contains("login-wall"));
        clear(&sid);
    }

    #[test]
    fn ask_resolutions_classify_the_plumbing_sentinels() {
        assert_eq!(
            ask_resolution("The user dismissed the question without answering. Proceed…"),
            "DISMISSED by the user without answering"
        );
        assert_eq!(
            ask_resolution("No answer from the user within 15 minutes. Proceed…"),
            "TIMED OUT unanswered"
        );
        assert!(ask_resolution("done, logged in").starts_with("answered: done"));
    }

    #[test]
    fn purge_turn_removes_only_the_selected_user_bounded_slice() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let sid = format!("journal-purge-selected-{}", uuid::Uuid::new_v4());
        record(&sid, "user", "user", "keep the first turn");
        record(&sid, "tool", "phoenix", "FIRST-SURVIVES");
        record(&sid, "user", "user", "delete this exact turn");
        record(&sid, "tool", "phoenix", "SECRET-DELETED-ACTION");
        record(&sid, "returned", "avery", "SECRET-DELETED-RETURN");
        record(&sid, "user", "user", "keep the newest turn");
        record(&sid, "tool", "phoenix", "NEWEST-SURVIVES");

        assert_eq!(
            purge_turn(&sid, 1, "delete this exact turn", false).unwrap(),
            3
        );
        let digest = history_digest(&sid);
        assert!(digest.contains("FIRST-SURVIVES"), "{digest}");
        assert!(digest.contains("NEWEST-SURVIVES"), "{digest}");
        assert!(!digest.contains("delete this exact turn"), "{digest}");
        assert!(!digest.contains("SECRET-DELETED"), "{digest}");
        assert!(!digest.contains(DELETED_TURN_KIND), "{digest}");
    }

    #[test]
    fn duplicate_prompt_uses_ordinal_and_retry_does_not_delete_the_other_copy() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let sid = format!("journal-purge-duplicate-{}", uuid::Uuid::new_v4());
        record(&sid, "user", "user", "same prompt");
        record(&sid, "tool", "phoenix", "OLDER-DUPLICATE-RESPONSE");
        record(&sid, "user", "user", "same prompt");
        record(&sid, "tool", "phoenix", "NEWER-DUPLICATE-RESPONSE");

        assert_eq!(purge_turn(&sid, 1, "same prompt", false).unwrap(), 2);
        let digest = history_digest(&sid);
        assert!(!digest.contains("OLDER-DUPLICATE-RESPONSE"), "{digest}");
        assert!(digest.contains("NEWER-DUPLICATE-RESPONSE"), "{digest}");

        // Recovery may reapply the same manifest after the canonical session
        // was already committed. The content-free boundary tombstone makes
        // this an exact no-op even though the prompt text is duplicated.
        assert_eq!(purge_turn(&sid, 1, "same prompt", false).unwrap(), 0);
        let after_retry = history_digest(&sid);
        assert!(
            after_retry.contains("NEWER-DUPLICATE-RESPONSE"),
            "{after_retry}"
        );
    }

    #[test]
    fn response_only_purge_keeps_user_boundary_and_is_idempotent() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let sid = format!("journal-purge-response-{}", uuid::Uuid::new_v4());
        record(&sid, "user", "user", "keep my prompt");
        record(&sid, "tool", "phoenix", "DELETE-RESPONSE-TOOL");
        record(&sid, "returned", "coder", "DELETE-RESPONSE-RETURN");
        record(&sid, "user", "user", "later prompt");
        record(&sid, "tool", "phoenix", "LATER-SURVIVES");

        assert_eq!(purge_turn(&sid, 1, "keep my prompt", true).unwrap(), 2);
        let digest = history_digest(&sid);
        assert!(digest.contains("keep my prompt"), "{digest}");
        assert!(digest.contains("LATER-SURVIVES"), "{digest}");
        assert!(!digest.contains("DELETE-RESPONSE"), "{digest}");
        assert!(!digest.contains(DELETED_RESPONSE_KIND), "{digest}");

        assert_eq!(purge_turn(&sid, 1, "keep my prompt", true).unwrap(), 0);
        assert!(history_digest(&sid).contains("LATER-SURVIVES"));
    }

    #[test]
    fn missing_or_mismatched_modern_turn_is_a_no_op() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let sid = format!("journal-purge-stale-{}", uuid::Uuid::new_v4());
        record(&sid, "user", "user", "surviving prompt");
        record(&sid, "tool", "phoenix", "SURVIVING-ACTION");
        let before = history_digest(&sid);

        assert_eq!(purge_turn(&sid, 4, "surviving prompt", false).unwrap(), 0);
        assert_eq!(purge_turn(&sid, 0, "different prompt", false).unwrap(), 0);
        assert_eq!(history_digest(&sid), before);
    }

    #[test]
    fn legacy_journal_without_user_boundaries_is_cleared() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let sid = format!("journal-purge-legacy-{}", uuid::Uuid::new_v4());
        record(&sid, "spawned", "avery", "LEGACY-SPAWN");
        record(&sid, "tool", "avery", "LEGACY-TOOL");

        assert_eq!(purge_turn(&sid, 0, "unrecorded prompt", false).unwrap(), 2);
        assert_eq!(history_digest(&sid), "");
        assert_eq!(purge_turn(&sid, 0, "unrecorded prompt", false).unwrap(), 0);
    }

    #[test]
    fn handoff_journal_keeps_structured_identity_and_legacy_entries_decode() {
        let legacy: Entry = serde_json::from_str(
            r#"{"at":"2026-08-28T00:00:00Z","kind":"handoff","agent":"phoenix","detail":"Phoenix to Nico"}"#,
        )
        .expect("legacy journal entry");
        assert!(legacy.handoff_id.is_none());
        assert!(legacy.reply_to.is_none());

        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let sid = format!("journal-handoff-id-{}", uuid::Uuid::new_v4());
        record_handoff(
            &sid,
            "message_123",
            "phoenix",
            "scribe",
            "draft copy",
            "queued",
            None,
            Some("turn_7"),
        );
        let durable = load_durable(&sid).unwrap().expect("durable journal");
        let row = durable.events.back().expect("handoff journal row");
        assert_eq!(row.handoff_id.as_deref(), Some("message_123"));
        assert_eq!(row.requester.as_deref(), Some("phoenix"));
        assert_eq!(row.receiver.as_deref(), Some("scribe"));
        assert_eq!(row.status.as_deref(), Some("queued"));
        assert_eq!(row.causation_id.as_deref(), Some("turn_7"));
    }
}
