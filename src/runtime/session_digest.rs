//! Session-close project digests — the "where did we leave off" memory.
//!
//! Per-turn saves capture each outcome, but nothing captured the *state of the
//! work* when a session stops: goal, decisions, what's done, what's next. This
//! pass runs on a daemon timer: any MAIN session with new activity since its
//! last digest that has gone idle gets ONE compact project-state note
//! remembered into Cognee. The note leads with the session title and names the
//! project explicitly in the body, so a future "let's continue <project>"
//! opener recalls it semantically even in a brand-new session.
//!
//! There is no explicit "session end" in Phoenix (sessions are resumable), so
//! idle-past-threshold is the end signal. A session resumed later simply earns
//! a fresh digest at its next idle. Crashes are covered for free: the timer
//! scans on wall-clock mtimes, so the first pass after a daemon restart
//! digests whatever the crash orphaned.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::providers::{ChatMessage, CompletionRequest};
use crate::session::{Message, Session, SessionStore};

/// How long a session must sit untouched before it counts as "stopped".
/// Shorter risks digesting mid-conversation lulls; longer delays continuity.
const IDLE_BEFORE_DIGEST: Duration = Duration::from_secs(30 * 60);
/// Hard deadline on the digest model call — the daemon tick must never wedge.
const DIGEST_LLM_TIMEOUT: Duration = Duration::from_secs(90);
/// Sessions with fewer user messages than this ("who am I" pokes, one-shot
/// questions) carry no project state worth a digest; the per-turn save
/// already covered them.
const MIN_USER_MESSAGES: usize = 2;
/// Transcript budget fed to the digest model. Head keeps the goal as stated,
/// tail keeps the latest state; the middle is where elision hurts least.
const HEAD_BUDGET: usize = 6_000;
const TAIL_BUDGET: usize = 18_000;
const MAX_DIGEST_SESSION_BYTES: usize = 32 * 1024 * 1024;
const MAX_DIGEST_STATE_ENTRIES: usize = 4_096;
/// A failed graph write is usually infrastructure contention, not a reason to
/// regenerate the same LLM digest every five minutes. Retry promptly once,
/// then back off while keeping the session fully eligible for eventual save.
const RETRY_BASE_SECS: u64 = 15 * 60;
const RETRY_MAX_SECS: u64 = 6 * 60 * 60;

#[derive(Debug, Default)]
pub(crate) struct DigestPassOutcome {
    pub receipts: Vec<String>,
    pub stored_any: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct DigestRetry {
    session_mtime: u64,
    next_attempt: u64,
    failures: u8,
}

/// Digest ONE main session immediately — the explicit-end path (`/quit`).
/// Bypasses the idle threshold but keeps every other rule: main sessions only,
/// trivial sessions skipped, and the state map updated so the daemon's timer
/// pass does not digest the same activity again. Returns the receipt line, or
/// None when there was nothing new or substantial enough to remember.
pub async fn digest_session_now(session_id: &str) -> Result<Option<String>> {
    SessionStore::validate_session_id(session_id).context("invalid digest session id")?;
    let stem = session_id;
    if !stem.starts_with("main-") || stem.contains("__") {
        return Ok(None);
    }
    let path = crate::config::phoenix_home()
        .join("sessions")
        .join(format!("{stem}.json"));
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_file() => meta,
        Ok(_) => anyhow::bail!("session digest refuses non-regular file {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("inspecting session {}", path.display()));
        }
    };
    let mtime_secs = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
        .unwrap_or(Duration::ZERO)
        .as_secs();
    let state = load_state()?;
    if state.get(stem).copied().unwrap_or(0) >= mtime_secs {
        return Ok(None); // nothing new since the last digest
    }
    let session = read_session_for_digest(&path, stem)?;
    let user_messages = session
        .messages
        .iter()
        .filter(|m| matches!(m, Message::User { .. }))
        .count();
    if user_messages < MIN_USER_MESSAGES {
        merge_state_updates(std::iter::once((stem.to_string(), mtime_secs)))?;
        return Ok(None);
    }
    let title = session.display_name().to_string();
    let note = compose_digest(&session, &title).await;
    let remember_outcome = crate::librarian::memory::remember(&note).await;
    if remember_outcome.is_stored() {
        merge_state_updates(std::iter::once((stem.to_string(), mtime_secs)))?;
        clear_retry(stem)?;
        Ok(Some(format!(
            "session digest → \"{title}\" ({user_messages} user turns) remembered"
        )))
    } else {
        record_retry_failure(stem, mtime_secs, unix_now())?;
        tracing::warn!(
            "session digest for {stem} was not confirmed durable ({remember_outcome}); leaving it retryable"
        );
        Ok(Some(format!(
            "session digest → \"{title}\" durability not confirmed ({remember_outcome}); retry pending"
        )))
    }
}

/// Digest every idle main session that changed since its last digest. Returns
/// one receipt line per digested session (empty = nothing was due). Failures
/// on one session never block the others.
pub async fn digest_idle_sessions() -> Result<Vec<String>> {
    Ok(digest_idle_sessions_report().await?.receipts)
}

/// Detailed daemon-facing digest pass. `stored_any` is intentionally separate
/// from receipts: failure receipts are useful diagnostics, but must not launch
/// an expensive backlog cognify pass when no note was actually stored.
pub(crate) async fn digest_idle_sessions_report() -> Result<DigestPassOutcome> {
    let sessions_root = crate::config::phoenix_home().join("sessions");
    if !sessions_root.is_dir() {
        return Ok(DigestPassOutcome::default());
    }
    let state = load_state()?;
    let retry_state = load_retry_state()?;
    let mut state_updates = HashMap::new();
    let mut receipts = Vec::new();
    let mut stored_any = false;
    let now = SystemTime::now();
    let now_secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();

    // Reuse the canonical bounded/no-follow store loader so one malformed,
    // oversized, or symlinked file cannot wedge the digest sweep.
    let mut store = SessionStore::new(&sessions_root);
    store.load_from_disk()?;
    let sessions: Vec<Session> = store.all().cloned().collect();
    for session in sessions {
        let stem = session.id.as_str();
        let path = store.session_path(stem);
        // Main sessions only: specialist transcripts (`__coder` etc.) are
        // working detail of the same project — the orchestrator transcript
        // carries the narrative.
        if !stem.starts_with("main-") || stem.contains("__") {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.file_type().is_file() {
            continue;
        }
        let Ok(mtime) = meta.modified() else { continue };
        // Still active (or clock skew) — leave it for a later tick.
        if now.duration_since(mtime).unwrap_or(Duration::ZERO) < IDLE_BEFORE_DIGEST {
            continue;
        }
        let mtime_secs = mtime
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();
        // Nothing new since the last digest of this session.
        if state.get(stem).copied().unwrap_or(0) >= mtime_secs {
            continue;
        }
        if !retry_is_due(&retry_state, stem, mtime_secs, now_secs) {
            continue;
        }

        let user_messages = session
            .messages
            .iter()
            .filter(|m| matches!(m, Message::User { .. }))
            .count();
        if user_messages < MIN_USER_MESSAGES {
            // Mark it digested so trivial sessions don't get rescanned forever.
            state_updates.insert(stem.to_string(), mtime_secs);
            continue;
        }

        let title = session.display_name().to_string();
        let note = compose_digest(&session, &title).await;
        let remember_outcome = crate::librarian::memory::remember(&note).await;
        if remember_outcome.is_stored() {
            state_updates.insert(stem.to_string(), mtime_secs);
            clear_retry(stem)?;
            stored_any = true;
            receipts.push(format!(
                "session digest → \"{title}\" ({user_messages} user turns) remembered"
            ));
        } else {
            record_retry_failure(stem, mtime_secs, now_secs)?;
            tracing::warn!(
                "session digest for {stem} was not confirmed durable ({remember_outcome}); leaving it retryable"
            );
            receipts.push(format!(
                "session digest → \"{title}\" durability not confirmed ({remember_outcome}); retry pending"
            ));
            // The memory pipeline is process-global. If one save timed out or
            // was unavailable, immediately trying every remaining session
            // only repeats the same failure and can make an idle desktop lag
            // for minutes. Preserve eligibility and retry on a later quiet
            // tick after the existing exponential backoff.
            break;
        }
    }

    if !state_updates.is_empty() {
        merge_state_updates(state_updates)?;
    }
    Ok(DigestPassOutcome {
        receipts,
        stored_any,
    })
}

/// LLM digest with a deterministic fallback: memory continuity must not
/// depend on a provider being up at the moment a session goes idle.
async fn compose_digest(session: &Session, title: &str) -> String {
    let date = chrono::Utc::now().format("%Y-%m-%d");
    let body = match llm_digest(session, title).await {
        Ok(note) if !note.trim().is_empty() => note.trim().to_string(),
        Ok(_) => deterministic_digest(session),
        Err(error) => {
            tracing::debug!("session digest model call failed, using fallback: {error:#}");
            deterministic_digest(session)
        }
    };
    format!("Session digest — {title} ({date})\n\n{body}")
}

async fn llm_digest(session: &Session, title: &str) -> Result<String> {
    let config = crate::config::PhoenixConfig::load()?;
    let llm = &config.profile.llm;
    let factory = crate::providers::ProviderFactory::new();
    // Same role preference as cognify: the librarian tier is the cheap model
    // meant exactly for this kind of mechanical summarization.
    // A dead primary must not skip the account-fallback chain below — build
    // it as a `Result` and let `lane_with_fallback_chain` treat a missing
    // first link as just that.
    let provider = match llm
        .librarian_provider
        .as_deref()
        .filter(|id| !id.trim().is_empty() && *id != llm.provider)
    {
        Some(id) => factory.build_role_provider(llm, id),
        None => factory.build_llm_provider(llm),
    };
    // The librarian's account-fallback chain covers digests too.
    let librarian_pid = llm
        .librarian_provider
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .unwrap_or(&llm.provider)
        .to_string();
    let provider = factory.lane_with_fallback_chain(
        provider,
        llm,
        "librarian",
        &librarian_pid,
        &llm.fallback.librarian,
    )?;
    let transcript = truncate_middle(&render_transcript(session), HEAD_BUDGET, TAIL_BUDGET);
    let mut request = CompletionRequest::new(
        llm.librarian(),
        vec![
            ChatMessage::system(
                "You write project-state digests for an AI agent's long-term memory. \
                 Output ONLY the note text — no preamble, no markdown headers.",
            ),
            ChatMessage::user(format!(
                "Session title: {title}\n\nTranscript (middle may be elided):\n{transcript}\n\n\
                 Write one compact project-state note (max 200 words) with these lines:\n\
                 Project: <name the project/topic explicitly — this is the recall key>\n\
                 Goal: <what the user is trying to achieve>\n\
                 Decisions: <choices made and why, if any>\n\
                 State: <what is done / working / broken right now>\n\
                 Next steps: <the immediate continuation>\n\
                 Skip lines with nothing real to say."
            )),
        ],
    );
    request.max_tokens = Some(600);
    request.temperature = Some(0.2);
    let response = tokio::time::timeout(DIGEST_LLM_TIMEOUT, provider.complete(request))
        .await
        .context("session digest model call timed out")??;
    Ok(response.content)
}

/// No-provider fallback: first request (the goal) + last assistant message
/// (the latest state). Cruder than the model's synthesis but always available,
/// and still recallable by project vocabulary.
fn deterministic_digest(session: &Session) -> String {
    let first_request = session
        .messages
        .iter()
        .find_map(|m| match m {
            Message::User { content } => Some(content.as_str()),
            _ => None,
        })
        .unwrap_or("");
    let last_state = session
        .messages
        .iter()
        .rev()
        .find_map(|m| match m {
            Message::Assistant { content } => Some(content.as_str()),
            _ => None,
        })
        .unwrap_or("");
    format!(
        "Request: {}\n\nLast state: {}",
        truncate_end(first_request, 600),
        truncate_end(last_state, 1200),
    )
}

/// Render the transcript for the digest model: user/assistant text plus
/// delegation subjects. Tool payloads are working detail, not project state —
/// including them would just crowd the budget.
fn render_transcript(session: &Session) -> String {
    let mut out = String::new();
    for message in &session.messages {
        match message {
            Message::User { content } => {
                out.push_str("User: ");
                out.push_str(content);
            }
            Message::Assistant { content } => {
                out.push_str("Assistant: ");
                out.push_str(content);
            }
            Message::Talk { to, subject, .. } => {
                out.push_str(&format!("[delegated to {to}: {subject}]"));
            }
            Message::GroupContribution {
                display_name,
                subject,
                body,
                ..
            } => {
                out.push_str(&format!("[{display_name} in group: {subject}] {body}"));
            }
            Message::ToolResult {
                tool_name, success, ..
            } => {
                out.push_str(&format!(
                    "[tool {tool_name}: {}]",
                    if *success { "ok" } else { "failed" }
                ));
            }
        }
        out.push('\n');
    }
    out
}

/// Keep the head and tail of an oversized string, eliding the middle on char
/// boundaries (transcripts contain arbitrary UTF-8).
fn truncate_middle(s: &str, head: usize, tail: usize) -> String {
    if s.len() <= head + tail {
        return s.to_string();
    }
    let head_end = floor_char_boundary(s, head);
    let tail_start = floor_char_boundary(s, s.len() - tail);
    format!(
        "{}\n…[{} chars elided]…\n{}",
        &s[..head_end],
        tail_start - head_end,
        &s[tail_start..]
    )
}

fn truncate_end(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.len() <= max {
        return s.to_string();
    }
    format!("{}…", &s[..floor_char_boundary(s, max)])
}

fn floor_char_boundary(s: &str, mut index: usize) -> usize {
    while index > 0 && !s.is_char_boundary(index) {
        index -= 1;
    }
    index
}

// ---- digest bookkeeping ----------------------------------------------------
//
// One tiny JSON map (session file stem → mtime secs at digest time) so a
// session is digested once per stretch of activity, not once per tick.

fn state_path() -> PathBuf {
    crate::config::phoenix_home().join("session_digest_state.json")
}

fn retry_state_path() -> PathBuf {
    crate::config::phoenix_home().join("session_digest_retries.json")
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

fn parse_retry_state(bytes: Option<&[u8]>) -> Result<HashMap<String, DigestRetry>> {
    let Some(bytes) = bytes else {
        return Ok(HashMap::new());
    };
    let state: HashMap<String, DigestRetry> =
        serde_json::from_slice(bytes).context("session digest retry state is corrupt")?;
    if state.len() > MAX_DIGEST_STATE_ENTRIES.saturating_mul(4) {
        anyhow::bail!(
            "session digest retry state has {} entries; safety maximum is {}",
            state.len(),
            MAX_DIGEST_STATE_ENTRIES * 4
        );
    }
    for id in state.keys() {
        SessionStore::validate_session_id(id)
            .with_context(|| format!("session digest retry state contains invalid id {id:?}"))?;
    }
    Ok(state)
}

fn load_retry_state() -> Result<HashMap<String, DigestRetry>> {
    let bytes = crate::config::private_io::read_private_file(&retry_state_path())?;
    parse_retry_state(bytes.as_deref())
}

fn retry_is_due(
    state: &HashMap<String, DigestRetry>,
    session_id: &str,
    session_mtime: u64,
    now: u64,
) -> bool {
    state
        .get(session_id)
        .is_none_or(|retry| retry.session_mtime != session_mtime || now >= retry.next_attempt)
}

fn record_retry_failure(session_id: &str, session_mtime: u64, now: u64) -> Result<()> {
    SessionStore::validate_session_id(session_id).context("invalid digest retry session id")?;
    let path = retry_state_path();
    crate::config::private_io::read_modify_write_private(&path, |current| {
        let mut state = parse_retry_state(current)?;
        let failures = state
            .get(session_id)
            .filter(|retry| retry.session_mtime == session_mtime)
            .map_or(1, |retry| retry.failures.saturating_add(1));
        let shift = u32::from(failures.saturating_sub(1)).min(20);
        let delay = RETRY_BASE_SECS
            .saturating_mul(1u64 << shift)
            .min(RETRY_MAX_SECS);
        state.insert(
            session_id.to_string(),
            DigestRetry {
                session_mtime,
                next_attempt: now.saturating_add(delay),
                failures,
            },
        );
        prune_retry_state(&mut state);
        Ok(((), serde_json::to_vec_pretty(&state)?))
    })
    .context("saving session digest retry state")
}

fn clear_retry(session_id: &str) -> Result<()> {
    SessionStore::validate_session_id(session_id).context("invalid digest retry session id")?;
    let path = retry_state_path();
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("inspecting {}", path.display()));
        }
    }
    crate::config::private_io::read_modify_write_private(&path, |current| {
        let mut state = parse_retry_state(current)?;
        state.remove(session_id);
        Ok(((), serde_json::to_vec_pretty(&state)?))
    })
    .context("clearing session digest retry state")
}

fn prune_retry_state(state: &mut HashMap<String, DigestRetry>) {
    if state.len() <= MAX_DIGEST_STATE_ENTRIES {
        return;
    }
    let mut oldest: Vec<(String, u64)> = state
        .iter()
        .map(|(id, retry)| (id.clone(), retry.next_attempt))
        .collect();
    oldest.sort_by_key(|(_, next_attempt)| *next_attempt);
    for (id, _) in oldest
        .into_iter()
        .take(state.len() - MAX_DIGEST_STATE_ENTRIES)
    {
        state.remove(&id);
    }
}

fn read_session_for_digest(path: &std::path::Path, expected_id: &str) -> Result<Session> {
    let bytes = crate::config::private_io::read_private_file(path)?
        .with_context(|| format!("session {} disappeared while reading", path.display()))?;
    if bytes.len() > MAX_DIGEST_SESSION_BYTES {
        anyhow::bail!(
            "session {} is {} bytes; maximum is {MAX_DIGEST_SESSION_BYTES}",
            path.display(),
            bytes.len()
        );
    }
    let session: Session =
        serde_json::from_slice(&bytes).with_context(|| format!("parsing session {expected_id}"))?;
    SessionStore::validate_session_id(&session.id)
        .context("session file contains an invalid id")?;
    if session.id != expected_id {
        anyhow::bail!(
            "session file {} belongs to `{}`, not `{expected_id}`",
            path.display(),
            session.id
        );
    }
    Ok(session)
}

fn parse_state(bytes: Option<&[u8]>) -> Result<HashMap<String, u64>> {
    let Some(bytes) = bytes else {
        return Ok(HashMap::new());
    };
    let state: HashMap<String, u64> =
        serde_json::from_slice(bytes).context("session digest state is corrupt")?;
    if state.len() > MAX_DIGEST_STATE_ENTRIES.saturating_mul(4) {
        anyhow::bail!(
            "session digest state has {} entries; safety maximum is {}",
            state.len(),
            MAX_DIGEST_STATE_ENTRIES * 4
        );
    }
    for id in state.keys() {
        SessionStore::validate_session_id(id)
            .with_context(|| format!("session digest state contains invalid id {id:?}"))?;
    }
    Ok(state)
}

fn load_state() -> Result<HashMap<String, u64>> {
    let bytes = crate::config::private_io::read_private_file(&state_path())?;
    parse_state(bytes.as_deref())
}

fn merge_state_updates(updates: impl IntoIterator<Item = (String, u64)>) -> Result<()> {
    let updates: Vec<(String, u64)> = updates.into_iter().collect();
    for (id, _) in &updates {
        SessionStore::validate_session_id(id)
            .with_context(|| format!("invalid digest-state update id {id:?}"))?;
    }
    let path = state_path();
    crate::config::private_io::read_modify_write_private(&path, |current| {
        let mut state = parse_state(current)?;
        for (id, mtime) in &updates {
            let value = state.entry(id.clone()).or_default();
            *value = (*value).max(*mtime);
        }
        if state.len() > MAX_DIGEST_STATE_ENTRIES {
            let mut oldest: Vec<(String, u64)> = state
                .iter()
                .map(|(id, mtime)| (id.clone(), *mtime))
                .collect();
            oldest.sort_by_key(|(_, mtime)| *mtime);
            for (id, _) in oldest
                .into_iter()
                .take(state.len() - MAX_DIGEST_STATE_ENTRIES)
            {
                state.remove(&id);
            }
        }
        Ok(((), serde_json::to_vec_pretty(&state)?))
    })
    .context("saving session digest state")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_with(messages: Vec<Message>) -> Session {
        let mut s = Session::new_main("test-model", "sys");
        s.messages = messages;
        s
    }

    #[test]
    fn deterministic_digest_pairs_first_request_with_last_state() {
        let s = session_with(vec![
            Message::User {
                content: "build the parser".into(),
            },
            Message::Assistant {
                content: "started on it".into(),
            },
            Message::User {
                content: "use recursive descent".into(),
            },
            Message::Assistant {
                content: "parser done, tests failing on unicode".into(),
            },
        ]);
        let d = deterministic_digest(&s);
        assert!(d.contains("Request: build the parser"));
        assert!(d.contains("Last state: parser done, tests failing on unicode"));
    }

    #[test]
    fn transcript_includes_delegations_but_not_tool_payloads() {
        let s = session_with(vec![
            Message::Talk {
                from: "orchestrator".into(),
                to: "coder".into(),
                subject: "fix the build".into(),
                body: "very long body that must not appear".into(),
                reply_expected: false,
                handoff_id: "message_fix_build".into(),
                reply_to: None,
                causation_id: None,
                status: "queued".into(),
            },
            Message::ToolResult {
                tool_name: "bash".into(),
                input: "cargo build".into(),
                success: true,
                output: "huge output that must not appear".into(),
            },
        ]);
        let t = render_transcript(&s);
        assert!(t.contains("[delegated to coder: fix the build]"));
        assert!(t.contains("[tool bash: ok]"));
        assert!(!t.contains("must not appear"));
    }

    #[test]
    fn truncate_middle_elides_on_char_boundaries() {
        let s = "é".repeat(100); // 2 bytes per char
        let t = truncate_middle(&s, 21, 21); // both cut points mid-char
        assert!(t.contains("elided"));
        assert!(t.starts_with('é') && t.ends_with('é'));
    }

    #[test]
    fn truncate_middle_passes_short_strings_through() {
        assert_eq!(truncate_middle("short", 100, 100), "short");
    }

    #[test]
    fn digest_state_updates_merge_across_writers() {
        let dir = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(dir.path());
        std::thread::scope(|scope| {
            for index in 0..32u64 {
                scope.spawn(move || {
                    merge_state_updates(std::iter::once((
                        format!("main-digest-{index}"),
                        index + 1,
                    )))
                    .unwrap();
                });
            }
        });
        let state = load_state().unwrap();
        assert_eq!(state.len(), 32);
        assert_eq!(state["main-digest-31"], 32);
    }

    #[test]
    fn corrupt_digest_state_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(dir.path());
        crate::config::private_io::atomic_write_private(&state_path(), b"{broken").unwrap();
        assert!(merge_state_updates(std::iter::once(("main-safe".to_string(), 1))).is_err());
        assert_eq!(
            crate::config::private_io::read_private_file(&state_path())
                .unwrap()
                .unwrap(),
            b"{broken"
        );
    }

    #[test]
    fn failed_digest_retries_back_off_and_new_activity_bypasses_the_delay() {
        let dir = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(dir.path());
        let id = "main-retry-backoff";

        record_retry_failure(id, 10, 1_000).unwrap();
        let first = load_retry_state().unwrap();
        assert!(!retry_is_due(&first, id, 10, 1_899));
        assert!(retry_is_due(&first, id, 10, 1_900));
        assert!(
            retry_is_due(&first, id, 11, 1_001),
            "new session activity must be eligible immediately"
        );

        record_retry_failure(id, 10, 1_900).unwrap();
        let second = load_retry_state().unwrap();
        assert!(!retry_is_due(&second, id, 10, 3_699));
        assert!(retry_is_due(&second, id, 10, 3_700));
        assert_eq!(second[id].failures, 2);

        clear_retry(id).unwrap();
        assert!(!load_retry_state().unwrap().contains_key(id));
    }

    #[test]
    fn failed_digest_retry_delay_is_capped_and_private() {
        let dir = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(dir.path());
        let id = "main-retry-cap";
        for attempt in 0..20 {
            record_retry_failure(id, 10, 1_000 + attempt).unwrap();
        }
        let retry = load_retry_state().unwrap().remove(id).unwrap();
        assert_eq!(retry.next_attempt, 1_019 + RETRY_MAX_SECS);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(retry_state_path())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn digest_retry_state_never_follows_a_symlink() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(dir.path());
        let outside = dir.path().join("outside.json");
        std::fs::write(&outside, b"keep me").unwrap();
        symlink(&outside, retry_state_path()).unwrap();

        assert!(record_retry_failure("main-symlink", 1, 1).is_err());
        assert!(clear_retry("main-symlink").is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"keep me");
    }

    #[cfg(unix)]
    #[test]
    fn digest_state_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(dir.path());
        merge_state_updates(std::iter::once(("main-private".to_string(), 1))).unwrap();
        assert_eq!(
            std::fs::metadata(state_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
