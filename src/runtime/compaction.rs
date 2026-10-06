//! Auto-compaction with continuation — the context-wall killer.
//!
//! Long sessions eventually outgrow the model context: render-time bounding
//! (`render_bounded_session`) and in-turn windowing shrink TOOL results, but
//! user/assistant/talk messages accumulate forever. This module compacts the
//! DURABLE session when the assembled prompt approaches the window: the older
//! portion of the transcript is summarized into one continuation message
//! (facts, decisions, paths, open work), recent messages stay verbatim, and
//! the turn keeps running — no wall, no user action.
//!
//! The portable summary is one provider call on the cheap librarian-tier model;
//! a capable acting route may compact its opaque replay concurrently. If the
//! summary call fails the fallback is a deterministic receipt digest, and a
//! rejected native result simply leaves the portable fold. Removed messages
//! are archived to `<session>.archive.jsonl` beside the session file before any
//! network call, so nothing is unrecoverable.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::providers::contracts::ToolDefinition;
use crate::providers::{
    ChatMessage, CompletionRequest, LLMProvider, NativeCompactionReplay, NativeCompactionRequest,
    NativeCompactionRoute,
};
use crate::session::{Message, Session};

/// Compact when the estimated request exceeds this share of the window.
///
/// 0.85 (USER VERDICT 2026-07-15): fold NEAR THE END of the context window, not
/// early. The prior 0.60-with-a-100k-cap folded a 500k-window model at 100k —
/// 20% of the window — throwing away 400k of context the model could still hold,
/// which is exactly what made it "suddenly dumb / forget my instruction / call
/// it done." Context-rot theory said fold early; the user's lived experience on
/// THIS stack says the early fold hurts far more than rot does, so use the
/// window. Compaction checks EVERY tool round (turn_loop), so 0.85 is safe: the
/// fold fires the instant the request crosses the line and never runs past the
/// window. The remaining 15% is headroom for the turn's output. Env-tunable:
/// PHOENIX_COMPACTION_TRIGGER_TOKENS.
pub const COMPACTION_TRIGGER_RATIO: f64 = 0.85;

/// Absolute ceiling on the fold trigger, independent of window size. Context
/// rot is an ABSOLUTE-length phenomenon: a 500k-window model at 60% is holding
/// 300k tokens — already deep in the rot zone — yet the fraction alone would
/// never fold it (measured: not one of 74 real sessions ever crossed 300k; the
/// heaviest parked at ~249k and never compacted, which is exactly why long
/// canvases lagged and every turn shipped a quarter-million tokens). Capping
/// the trigger here makes compaction actually FIRE on big-window models while
/// leaving small-window models on the fraction.
///
/// 450k (2026-07-15, USER VERDICT — "move compaction a lot further, to the end
/// of the context window"): the 100k cap was the real lobotomizer. On a 500k
/// window `min(500k*ratio, 100k)` pinned the fold at 100k = 20% of the window,
/// so the main orchestrator folded constantly and lost the user's own standing
/// instructions. This cap now exists ONLY to keep a sane ceiling on truly giant
/// windows (1M+): on the user's 500k grok-4.5 lane the 0.85 ratio (425k) wins
/// and folding happens near the end where it belongs. Earlier history said this
/// trades quota/rot for context — the user has explicitly chosen context. Env
/// overrides (no rebuild):
///   PHOENIX_COMPACTION_TRIGGER_TOKENS / PHOENIX_COMPACTION_KEEP_TOKENS
pub const COMPACTION_TRIGGER_MAX_TOKENS: u64 = 450_000;

/// Absolute ceiling on the verbatim tail kept after a fold, independent of
/// window size. Raised 32k → 160k (2026-07-15) to match the late trigger: with
/// the fold now firing near the end of the window (~425k on a 500k lane), a 32k
/// keep would be a CLIFF — dump 425k down to 32k in one shot, exactly the "lost
/// all my context" jolt. Keeping ~150k verbatim means the fold is a gentle step
/// (425k → ~150k + summary), the newest ~150k of real work stays word-for-word,
/// and the session still sawtooths high in the window instead of hugging a tiny
/// floor. Env override: PHOENIX_COMPACTION_KEEP_TOKENS.
pub const COMPACTION_KEEP_MAX_TOKENS: u64 = 160_000;

/// Env-tunable ceiling on the fold trigger (defaults to the const above).
/// Both knobs exist so a quota crunch or a quality regression is a shell
/// export away from being fixed — no rebuild, no code edit.
pub fn trigger_max_tokens() -> u64 {
    env_tokens(
        "PHOENIX_COMPACTION_TRIGGER_TOKENS",
        COMPACTION_TRIGGER_MAX_TOKENS,
    )
}

/// Env-tunable ceiling on the kept verbatim tail (defaults to the const above).
pub fn keep_max_tokens() -> u64 {
    env_tokens("PHOENIX_COMPACTION_KEEP_TOKENS", COMPACTION_KEEP_MAX_TOKENS)
}

/// Effective verbatim-tail target for one model window. The absolute keep
/// ceiling is appropriate for 500k+ lanes, but must not become the working
/// floor on a 200k lane.
pub fn keep_tokens_for_window(context_window: u64) -> u64 {
    ((context_window as f64 * COMPACTION_KEEP_RATIO) as u64).min(keep_max_tokens())
}

fn env_tokens(var: &str, default: u64) -> u64 {
    std::env::var(var)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

/// After compaction, recent verbatim messages should fit roughly this share
/// of the window — the rest of the headroom is for new rounds and output.
///
/// 0.30 (2026-07-15, back up from 0.20): with the late trigger the point is to
/// USE the window, so keep a generous verbatim tail after a fold rather than the
/// aggressive 0.20 that bought raw session length at the cost of context. On a
/// 500k window this is 150k (meets the 160k cap just below); eviction is still
/// relocation not deletion (archive + `recall` + the tool ledger), so nothing is
/// lost. If live telemetry shows duplicate tool calls, this is the LAST knob to
/// touch — the trigger and keep caps come first.
pub const COMPACTION_KEEP_RATIO: f64 = 0.30;

/// FIFO cap on the pinned tool ledger carried across folds — the in-context
/// tier of "remember every step it did." Raised 40 → 120: on a real 232-step
/// session the old cap remembered only the last 40 steps faithfully; 120 keeps
/// most of a long session's actual actions visible every turn (~4k tokens, tiny
/// beside a 128k budget). Steps beyond the cap are NOT lost — every folded
/// message is archived verbatim and retrievable via `recall`; the ledger is the
/// always-visible recent working set, the archive is the complete history.
const LEDGER_MAX_ENTRIES: usize = 80;

/// Bound on the anchored summary fed back as the anchor to UPDATE. Recursive
/// summaries must not grow without bound over many folds (Letta clips at 50k);
/// anything clipped is still in the archive behind `recall`.
const ANCHOR_CAP_CHARS: usize = 24_000;
const SUMMARIZER_MAX_OUTPUT_TOKENS: u32 = 6_000;

/// Keep both the established beginning and the newest decisions at the end of
/// an anchored summary. Every omitted byte remains recoverable from the
/// verbatim transcript archive.
fn bound_anchor_summary(summary: String) -> String {
    if summary.chars().count() <= ANCHOR_CAP_CHARS {
        return summary;
    }
    let head_cap = ANCHOR_CAP_CHARS * 3 / 5;
    let tail_cap = ANCHOR_CAP_CHARS - head_cap;
    let head = summary.chars().take(head_cap).collect::<String>();
    let tail = summary
        .chars()
        .rev()
        .take(tail_cap)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!(
        "{head}\n\n[... anchor clipped at {ANCHOR_CAP_CHARS} chars; use recall for exact archived detail ...]\n\n{tail}"
    )
}

/// Below this many messages there is nothing meaningful to fold.
const MIN_MESSAGES_TO_COMPACT: usize = 8;

/// Always keep at least this many of the newest messages verbatim.
const MIN_KEEP_TAIL: usize = 4;

/// Compaction is a best-effort pressure valve with a deterministic receipt
/// fallback. It must never outlive the actual agent work or become an
/// unbounded provider call of its own.
const SUMMARIZER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// The summarizer's own input must fit ITS context too — cap what we feed it.
/// Head + tail of the old half; the middle is elided with a marker.
const SUMMARIZER_INPUT_CAP_CHARS: usize = 360_000;

/// Conservative chars-per-token for mixed prose/code transcripts.
const CHARS_PER_TOKEN: u64 = 4;

pub fn estimated_tokens(chars: usize) -> u64 {
    (chars as u64) / CHARS_PER_TOKEN + 1
}

/// Estimate the durable transcript's token footprint using the same message
/// accounting that compaction uses to select a fold boundary. This differs
/// from the rendered prompt because old tool results are deliberately bounded
/// at render time; compaction must still prevent the durable session growing
/// without bound.
pub fn session_estimated_tokens(session: &Session) -> u64 {
    estimated_tokens(session.messages.iter().map(message_chars).sum())
}

/// The point where an assembled request or durable session needs folding.
pub fn trigger_tokens(context_window: u64) -> u64 {
    ((context_window as f64 * COMPACTION_TRIGGER_RATIO) as u64).min(trigger_max_tokens())
}

/// Compaction protects both the outbound prompt and the durable transcript.
/// Old tool output is intentionally bounded while rendering, so checking only
/// the outbound prompt lets the persisted session grow without bound.
pub fn should_compact(request_tokens: u64, session_tokens: u64, context_window: u64) -> bool {
    let trigger = trigger_tokens(context_window);
    // The durable transcript grew past the trigger — folding reduces it
    // directly, so this always compacts.
    if session_tokens >= trigger {
        return true;
    }
    // The outbound REQUEST is over the trigger but the durable session is not.
    // Folding only shrinks the session portion of the request; if there is
    // little foldable transcript, the request is dominated by FIXED overhead
    // (system prompt, tool schemas, injected team history) that compaction
    // cannot touch — folding would fire every turn and reduce nothing (the
    // observed "~63k → ~63k" overhead-bound loop). So fold for request pressure
    // only when there is enough foldable transcript for the fold to matter:
    // once a session is already folded down near the keep target, request-side
    // pressure alone must NOT re-trigger it. Real durable growth past the
    // trigger (the branch above) still always folds.
    let keep = keep_tokens_for_window(context_window);
    let foldable_floor = keep + keep / 2; // ~1.5× the effective keep target
    request_tokens >= trigger && session_tokens >= foldable_floor
}

/// Default prompt-cache time-to-live, in seconds. This is the caller's fallback
/// when the provider/config doesn't specify one — it is NOT a fixed truth.
///
/// 300s = Anthropic's default `ephemeral` cache TTL. Two caveats it encodes:
///  1. The TTL is a SLIDING window — each cache read refreshes it — so it's
///     really "seconds since the cache was last touched." The idle gap we
///     measure (now − last turn) is exactly that quantity, so this threshold is
///     well-matched to the 5-minute default.
///  2. It is provider- and tier-dependent. Anthropic also offers a 1-hour
///     `ephemeral` tier; other providers (xAI/grok-cli, ollama-cloud, …) cache
///     differently or not at all. When a lane uses a longer TTL, folding at 300s
///     idle is premature — pass that lane's real TTL to
///     [`should_compact_on_cache_miss`] instead of assuming this default.
pub const DEFAULT_CACHE_TTL_SECS: u64 = 300;

/// Only fold-on-cache-miss when the context is at least this big — below it the
/// full-price re-read is cheap enough that the summarizer call isn't worth it.
/// DERIVED from the keep ceiling (2× keep, floor 16k) so it tracks budget mode:
/// a fixed 48k floor above a 40k trigger would mean the idle fold NEVER fires.
pub fn cache_miss_floor_tokens(context_window: u64) -> u64 {
    let keep = keep_tokens_for_window(context_window);
    (keep + keep / 2).max(16_000)
}

/// Should we fold BEFORE this turn purely because the prompt cache went cold?
/// True when the session sat idle past `cache_ttl_secs` (the ACTIVE lane's real
/// cache TTL — see [`DEFAULT_CACHE_TTL_SECS`] for why this isn't a constant) and
/// holds enough context that shrinking it beats the summarizer cost — even below
/// the normal [`should_compact`] token trigger. This is the "compact on cache
/// miss" win: fold right before a full-price re-read, not during warm reuse.
pub fn should_compact_on_cache_miss(
    session_tokens: u64,
    idle_seconds: u64,
    cache_ttl_secs: u64,
    context_window: u64,
) -> bool {
    idle_seconds >= cache_ttl_secs && session_tokens >= cache_miss_floor_tokens(context_window)
}

/// The idle gap is captured once when the turn starts. Reusing it in later
/// tool rounds would repeatedly compact an already-active conversation.
pub fn should_compact_on_cache_miss_at_round(
    round_index: usize,
    session_tokens: u64,
    idle_seconds: u64,
    cache_ttl_secs: u64,
    context_window: u64,
) -> bool {
    round_index == 0 && cache_ttl_secs > 0
        && should_compact_on_cache_miss(session_tokens, idle_seconds, cache_ttl_secs, context_window)
}

/// Approximate rendered size of one session message in chars.
fn message_chars(message: &Message) -> usize {
    match message {
        Message::User { content } | Message::Assistant { content } => content.chars().count(),
        Message::Talk { subject, body, .. } => subject.chars().count() + body.chars().count() + 64,
        Message::GroupContribution {
            display_name,
            role_title,
            subject,
            body,
            ..
        } => {
            display_name.chars().count()
                + role_title.chars().count()
                + subject.chars().count()
                + body.chars().count()
                + 96
        }
        Message::ToolResult { input, output, .. } => {
            input.chars().count() + output.chars().count() + 64
        }
    }
}

/// Index where "keep verbatim" begins: walk from the newest message backward
/// accumulating chars until `keep_chars` is spent, then snap backward to a
/// turn opener only when the additional messages fit a quarter of the budget.
pub fn split_point(messages: &[Message], keep_chars: usize) -> usize {
    if messages.len() < MIN_MESSAGES_TO_COMPACT {
        return 0;
    }
    let mut accumulated = 0usize;
    let mut boundary = messages.len();
    for (index, message) in messages.iter().enumerate().rev() {
        accumulated += message_chars(message);
        if accumulated > keep_chars && messages.len() - index >= MIN_KEEP_TAIL {
            break;
        }
        boundary = index;
    }
    // Keep the floor of newest messages even if they are huge.
    let budget_boundary = boundary.min(messages.len().saturating_sub(MIN_KEEP_TAIL));
    // Snap BACKWARD to a turn opener so the kept tail never starts with an
    // orphaned tool result — keeping a little extra verbatim is always safe,
    // UP TO A POINT. An autonomous coworker can run hundreds of tool
    // calls with no User/Talk message between them; the nearest opener is then
    // far below the budget boundary, and snapping to it would pin the fold at
    // that fixed opener while the tail grows without bound — compaction folds
    // the same prefix every turn and reduces NOTHING (the observed
    // "~82k → ~82k, folded=109" busy-loop). So bound the snap-back by chars:
    // if reaching an opener would drag more than a quarter of `keep_chars` of extra
    // transcript into the kept tail, DON'T snap — fold at the budget boundary
    // instead. A single orphaned leading receipt is fine; the continuation
    // marker already explains the fold, and bounding the tail guarantees the
    // fold always makes progress.
    let mut snapped = budget_boundary;
    let mut extra_kept = 0usize;
    while snapped > 0
        && !matches!(
            messages[snapped],
            Message::User { .. } | Message::Talk { .. } | Message::GroupContribution { .. }
        )
    {
        // Count the message we would ADD, including the opener itself. The
        // boundary message is already in the tail and must not be counted twice.
        extra_kept += message_chars(&messages[snapped - 1]);
        if extra_kept > keep_chars / 4 {
            // Opener is too far back — snapping would blow the tail budget.
            return budget_boundary;
        }
        snapped -= 1;
    }
    // A turn with no opener anywhere before the boundary (one giant tool loop)
    // still must compact — orphaned receipts beat hitting the wall.
    if snapped == 0 {
        budget_boundary
    } else {
        snapped
    }
}

/// Deterministic digest of the folded messages — the no-model fallback. One
/// receipt line per message: role, then the first non-empty line, capped.
pub fn receipt_digest(messages: &[Message]) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(messages.len());
    for message in messages {
        let line = match message {
            Message::User { content } => format!("user: {}", snippet(content, 240)),
            Message::Assistant { content } => format!("assistant: {}", snippet(content, 200)),
            Message::Talk {
                from,
                to,
                subject,
                body,
                ..
            } => format!(
                "talk {from} -> {to}: {} — {}",
                snippet(subject, 120),
                snippet(body, 240)
            ),
            Message::GroupContribution {
                agent_id,
                display_name,
                subject,
                body,
                ..
            } => format!(
                "group {display_name} ({agent_id}): {} — {}",
                snippet(subject, 120),
                snippet(body, 240)
            ),
            Message::ToolResult {
                tool_name,
                input,
                success,
                output,
            } => format!(
                "tool {tool_name}({}) {} {}",
                snippet(input, 80),
                if *success { "ok" } else { "FAIL" },
                snippet(output, 180)
            ),
        };
        lines.push(line);
    }
    lines.join("\n")
}

fn snippet(text: &str, cap: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= cap {
        collapsed
    } else {
        format!("{}...", collapsed.chars().take(cap).collect::<String>())
    }
}

// A model summary is allowed to fail, but a user's durable plan is not.  The
// receipt fallback used to keep only the first 240 characters of each prompt;
// in a long school-planning thread that cut away the vacation/work-ahead
// constraint and left the agent with only the newest subject-specific task.
// Keep a small deterministic lane of high-signal user requests outside the
// model-written summary, just like the tool ledger below.
const USER_INTENT_HEADER: &str =
    "=== USER INTENT LEDGER — direct excerpts that survive compaction ===";
const USER_INTENT_MAX_ENTRIES: usize = 24;
const USER_INTENT_ENTRY_CHARS: usize = 700;
const USER_INTENT_MAX_CHARS: usize = 14_000;
const USER_INTENT_RECENT_FLOOR: usize = 16;
const STATE_EVIDENCE_MAX_ENTRIES: usize = 24;
const STATE_EVIDENCE_ENTRY_CHARS: usize = 700;
const STATE_EVIDENCE_MAX_CHARS: usize = 14_000;

fn has_state_signal(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "i finished",
        "i have finished",
        "i completed",
        "i have completed",
        "i'm done",
        "i am done",
        "already finished",
        "already completed",
        "is complete",
        "submitted",
        "passed",
        "mark it complete",
        "marked complete",
        "checked off",
        "not done",
        "not finished",
        "incomplete",
        "still need",
        "still thinks",
        "done",
        "finished",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn has_confirmed_state_signal(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "marked complete",
        "verified complete",
        "already completed",
        "already finished",
        "submission receipt",
        "submitted/completed",
        "is graded",
        "grade:",
        "gradebook",
        "passed:",
        "next science",
        "no more science",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn user_intent_score(text: &str, index: usize, len: usize) -> usize {
    let lower = text.to_lowercase();
    let signals = [
        "remember",
        "always",
        "never",
        "do not",
        "don't",
        "must",
        "instead",
        "wrong",
        "everything",
        "all ",
        "every ",
        "vacation",
        "school-free",
        "work ahead",
        "before ",
        "tomorrow",
        "plan",
        "goal",
        "i want",
        "i need",
    ];
    let signal_score = signals
        .iter()
        .filter(|needle| lower.contains(*needle))
        .count()
        * 4;
    // User-reported state transitions are not merely preferences. They are
    // the authoritative facts future planning mutates around. The old ledger
    // did not score "I finished X", so a dense thread could retain dozens of
    // planning requests while silently dropping the fact that X was already
    // complete. That is how a later reschedule reopened finished schoolwork.
    let state_score = usize::from(has_state_signal(text)) * 64;
    let edge_score = usize::from(index < 4) * 3 + usize::from(index + 12 >= len) * 5;
    signal_score + state_score + edge_score
}

fn user_intent_lines(messages: &[Message]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut candidates = messages
        .iter()
        .enumerate()
        .rev()
        .filter_map(|(index, message)| match message {
            Message::User { content } if !content.trim().is_empty() => {
                let excerpt = snippet(content, USER_INTENT_ENTRY_CHARS);
                if !seen.insert(excerpt.clone()) {
                    return None;
                }
                Some((
                    user_intent_score(content, index, messages.len()),
                    index,
                    excerpt,
                ))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|(_, index, _)| *index);
    // Keep a verbatim floor of the newest user messages even if they contain
    // no keyword that our scorer knows. Pronouns such as "these three" only
    // make sense against nearby turns, and losing those turns is still context
    // loss. Fill the remaining budget with the strongest durable directives
    // and state transitions from the older history.
    let mut selected = std::collections::HashSet::new();
    for (_, index, _) in candidates.iter().rev().take(USER_INTENT_RECENT_FLOOR) {
        selected.insert(*index);
    }
    let mut ranked = candidates.clone();
    ranked.sort_by(|left, right| right.0.cmp(&left.0).then(right.1.cmp(&left.1)));
    for (_, index, _) in ranked {
        if selected.len() >= USER_INTENT_MAX_ENTRIES {
            break;
        }
        selected.insert(index);
    }
    candidates.retain(|(_, index, _)| selected.contains(index));
    candidates.sort_by_key(|(_, index, _)| std::cmp::Reverse(*index));

    let mut used = 0usize;
    candidates
        .into_iter()
        .filter_map(|(_, _, excerpt)| {
            let line = format!("- user: {excerpt}");
            let chars = line.chars().count();
            if used + chars > USER_INTENT_MAX_CHARS {
                return None;
            }
            used += chars;
            Some(line)
        })
        .collect::<Vec<_>>()
        .into_iter().rev().collect()
}

fn archived_messages(dir: &Path, session_id: &str) -> Result<Vec<Message>> {
    crate::session::SessionStore::validate_session_id(session_id)?;
    let path = dir.join(format!("{session_id}.archive.jsonl"));
    // A fresh session has no archive. Observing absence must not create a
    // directory/lock for every disposable worker. Existing archives still
    // take the shared writer lock and are opened through private I/O below.
    // A concurrent first append may follow this absent snapshot normally.
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("cannot inspect continuity archive"),
        Ok(_) => {}
    }
    crate::config::private_io::with_private_lock(&path, || read_archive_under_lock(&path))
}

/// Caller holds the archive's private lock for the entire read/mutation.
fn read_archive_under_lock(path: &Path) -> Result<Vec<Message>> {
    use std::io::{BufRead, Read};
    let Some(file) = crate::config::private_io::open_private_read_stream(path)? else { return Ok(Vec::new()); };
    const MAX_BYTES: u64 = 64 * 1024 * 1024;
    anyhow::ensure!(file.metadata()?.len() <= MAX_BYTES, "archive exceeds bounded continuity read; use exact recall");
    let mut messages = Vec::new();
    let mut bytes = 0u64;
    for (index, line) in std::io::BufReader::new(file.take(MAX_BYTES + 1)).lines().enumerate() {
        anyhow::ensure!(index < 100_000, "archive exceeds continuity record limit; use exact recall");
        let line = line.with_context(|| format!("archive read failed at record {}", index + 1))?;
        bytes += line.len() as u64 + 1;
        anyhow::ensure!(bytes <= MAX_BYTES, "archive grew beyond continuity read limit");
        messages.push(serde_json::from_str::<Message>(&line)
            .with_context(|| format!("invalid archive record {}; continuity is incomplete", index + 1))?);
    }
    Ok(messages)
}

fn continuity_messages(session: &Session, archive_dir: Option<&Path>) -> (Vec<Message>, &'static str) {
    let (mut messages, warning) = match archive_dir.map(|dir| read_continuity_archive(session, dir)).transpose() {
        Ok(messages) => (messages.unwrap_or_default(), ""),
        Err(error) => {
            tracing::warn!(session_id = %session.id, "archived continuity unavailable: {error:#}");
            (Vec::new(), "HISTORY INCOMPLETE: archived continuity could not be read safely. The evidence below covers live history only. Missing evidence is not proof of incompletion; do not reschedule or repeat work on that basis. Recover exact records before making history-dependent decisions.\n")
        }
    };
    messages.extend(session.messages.iter().cloned());
    (messages, warning)
}

fn read_continuity_archive(session: &Session, dir: &Path) -> Result<Vec<Message>> {
    let messages = archived_messages(dir, &session.id)?;
    anyhow::ensure!(!messages.is_empty() || !session.messages.iter().any(is_continuation_message),
        "compacted history requires its nonempty verbatim archive");
    Ok(messages)
}

/// Resolve exact group result receipts across the live tail and verbatim
/// archive. This does not summarize, silently skip corrupt rows, or load the
/// entire archive into memory. Only requested same-group contributions escape.
pub fn resolve_group_contributions(
    dir: &Path,
    session_id: &str,
    group_id: &str,
    receipts: &[String],
    live: &[Message],
) -> Result<Vec<Message>> {
    use std::io::{BufRead, Read};
    crate::session::SessionStore::validate_session_id(session_id)?;
    let mut needed = receipts.iter().cloned().collect::<std::collections::HashSet<_>>();
    anyhow::ensure!(needed.len() == receipts.len(), "duplicate required group receipts");
    let mut found = std::collections::HashMap::new();
    let mut accept = |message: &Message| {
        if let Message::GroupContribution { message_id, group_id: stored_group, .. } = message {
            if stored_group == group_id && needed.remove(message_id) {
                found.insert(message_id.clone(), message.clone());
            }
        }
        needed.is_empty()
    };
    let complete = live.iter().any(&mut accept);
    if !complete && !receipts.is_empty() {
        let path = dir.join(format!("{session_id}.archive.jsonl"));
        crate::config::private_io::with_private_lock(&path, || -> Result<()> {
            let Some(file) = crate::config::private_io::open_private_read_stream(&path)? else { return Ok(()); };
            let mut reader = std::io::BufReader::new(file);
            let mut row = Vec::new();
            let mut line = 0;
            const MAX_ROW_BYTES: u64 = 16 * 1024 * 1024;
            loop {
                row.clear();
                let bytes = Read::by_ref(&mut reader).take(MAX_ROW_BYTES + 1).read_until(b'\n', &mut row)?;
                if bytes == 0 { break; }
                line += 1;
                anyhow::ensure!(bytes as u64 <= MAX_ROW_BYTES, "group archive row {line} exceeds the safe read limit");
                let message: Message = serde_json::from_slice(&row)
                    .with_context(|| format!("invalid group archive row {line}; exact recovery cannot continue"))?;
                if accept(&message) { break; }
            }
            Ok(())
        })?;
    }
    anyhow::ensure!(needed.is_empty(), "required group contributions are missing from live and archived history: {}", {
        let mut ids = needed.into_iter().collect::<Vec<_>>(); ids.sort(); ids.join(", ")
    });
    receipts.iter().map(|id| found.remove(id).context("resolved group receipt disappeared")).collect()
}

fn state_excerpt(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = collapsed.to_ascii_lowercase();
    let byte = [
        "marked complete",
        "verified complete",
        "already completed",
        "already finished",
        "submission receipt",
        "submitted/completed",
        "is graded",
        "grade:",
        "gradebook",
        "passed:",
        "next science",
        "no more science",
        "i finished",
        "i have finished",
        "i completed",
        "i have completed",
        "i'm done",
        "i am done",
        "mark it complete",
        "not done",
        "not finished",
        "incomplete",
        "done",
        "finished",
    ]
    .iter()
    .filter_map(|needle| lower.find(needle))
    .min()
    .unwrap_or(0);
    let char_at = collapsed[..byte].chars().count();
    let start = char_at.saturating_sub(100);
    let end = (start + STATE_EVIDENCE_ENTRY_CHARS).min(collapsed.chars().count());
    let chars = collapsed.chars().collect::<Vec<_>>();
    let mut excerpt = chars[start..end].iter().collect::<String>();
    if start > 0 {
        excerpt.insert_str(0, "…");
    }
    if end < chars.len() {
        excerpt.push('…');
    }
    excerpt
}

fn state_evidence_lines(messages: &[Message]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut selected = Vec::new();
    let mut used = 0usize;
    // Reserve a bounded share for direct user state before filling with the
    // newest reports. Polling volume is not authority to evict corrections.
    // Sort back into chronology afterward so newer reopenings remain first.
    for user_pass in [true, false] {
      for (index, message) in messages.iter().enumerate().rev() {
        if selected.len() >= STATE_EVIDENCE_MAX_ENTRIES || (user_pass && selected.len() >= 8) { break; }
        let candidate = match message {
            Message::User { content } if has_state_signal(content) => {
                Some(("user", content.as_str()))
            }
            Message::Assistant { content } if !is_continuation_message(message) && has_confirmed_state_signal(content) => {
                Some(("assistant report; verify against source", content.as_str()))
            }
            Message::ToolResult {
                success: true,
                tool_name,
                output,
                ..
            } if !matches!(tool_name.as_str(), "recall" | "composio_search") && has_confirmed_state_signal(output) => {
                Some(("tool-reported state; may include interpretation", output.as_str()))
            }
            _ => None,
        };
        let Some((source, text)) = candidate else {
            continue;
        };
        if user_pass && source != "user" { continue; }
        let excerpt = state_excerpt(text);
        let identity = (source, excerpt.to_ascii_lowercase());
        if !seen.insert(identity) {
            continue;
        }
        let line = format!("- [{source}] {excerpt}");
        let chars = line.chars().count();
        if used + chars > STATE_EVIDENCE_MAX_CHARS { continue; }
        used += chars;
        selected.push((index, line));
      }
    }
    selected.sort_by(|a, b| b.0.cmp(&a.0));
    selected.into_iter().map(|(_, line)| line).collect()
}

/// A small, query-independent state projection used by both the parent agent
/// and disposable workers. Newest evidence is listed first. This is not a
/// second memory system: every line is a bounded excerpt of the canonical
/// transcript/archive, with source precedence stated explicitly.
pub fn authoritative_state_context(session: &Session, archive_dir: Option<&Path>) -> String {
    let (messages, warning) = continuity_messages(session, archive_dir);
    format!("{warning}{}", render_state_context(&messages))
}

fn render_state_context(messages: &[Message]) -> String {
    let lines = state_evidence_lines(messages);
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "=== AUTHORITATIVE STATE EVIDENCE — newest first ===\n\
         These are source-labeled excerpts, not independently verified facts. A successful tool call can still contain a worker's mistaken interpretation. Precedence: newer explicit user correction/completion > verified external completion, submission, or grade > older user report > planning metadata. An available/unlocked link or unchecked planning row is not proof of incompletion. When a grade/completion conflicts with a proposed next lesson, reconcile the exact task identity before scheduling; do not infer readiness merely from accessibility.\n{}",
        lines.join("\n")
    )
}

fn merge_user_intent(previous: Vec<String>, messages: &[Message]) -> Vec<String> {
    let mut merged = previous;
    merged.extend(user_intent_lines(messages));
    let mut seen = std::collections::HashSet::new();
    merged.reverse();
    merged.retain(|line| seen.insert(line.clone()));
    merged.reverse();
    if merged.len() > USER_INTENT_MAX_ENTRIES {
        merged.drain(..merged.len() - USER_INTENT_MAX_ENTRIES);
    }
    let mut used = 0usize;
    merged
        .into_iter()
        .rev()
        .filter_map(|line| {
            let chars = line.chars().count();
            if used + chars > USER_INTENT_MAX_CHARS {
                None
            } else {
                used += chars;
                Some(line)
            }
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn render_user_intent(lines: &[String]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "\n\n{USER_INTENT_HEADER}\nDeterministic excerpts from the user's own prompts. User-reported completions and corrections are authoritative state: never reopen completed work merely because an older external row or summary is stale. Preserve scope and constraints; use `recall` when an excerpt ends in `...`.\n{}",
        lines.join("\n")
    )
}

/// Query-independent user context for turn-start injection.
///
/// Semantic memory recall is intentionally relevance-ranked, so it cannot be
/// the only carrier for durable user state: a request phrased as "prepare
/// tomorrow" may not retrieve an older "I finished Test Drive 1.3" note. This
/// deterministic lane reads the verbatim archive plus the live tail and is
/// injected on every turn. It is bounded by the same ledger limits used by
/// compaction and therefore remains small on arbitrarily long sessions.
pub fn durable_user_context(session: &Session, archive_dir: Option<&Path>) -> String {
    let (user_messages, warning) = continuity_messages(session, archive_dir);
    let state = format!("{warning}{}", render_state_context(&user_messages));
    // Deduplicate only an excerpt actually present in the state lane. A long
    // completion report may carry other constraints outside its state excerpt;
    // a bounded state lane may also omit a message entirely.
    let intent_messages = user_messages
        .into_iter()
        .filter(|message| match message {
            Message::User { content } => {
                let excerpt = snippet(content, USER_INTENT_ENTRY_CHARS);
                !state.lines().any(|line| line.strip_prefix("- [user] ") == Some(excerpt.as_str()))
            }
            _ => false,
        })
        .collect::<Vec<_>>();
    let intent = render_user_intent(&user_intent_lines(&intent_messages))
        .trim()
        .to_string();
    match (state.is_empty(), intent.is_empty()) {
        (true, true) => String::new(),
        (false, true) => state,
        (true, false) => intent,
        (false, false) => format!("{state}\n\n{intent}"),
    }
}

// ─── Pinned tool ledger (the dropped-manifest) ──────────────────────────
//
// The LLM summary can drop "we already grepped X and found nothing" as
// unimportant — and the agent then re-issues the identical call, paying for
// the full result to re-enter context. The ledger is the deterministic
// `(tool, target, verdict)` record of every folded tool call, built from the
// messages themselves (never the summarizer), appended OUTSIDE the summary,
// and re-merged across folds — so it is faithful by construction and survives
// even total summarizer failure.

/// Heading of the ledger block inside the continuation message. Also the
/// parse anchor when the next fold peels the prior continuation apart.
const LEDGER_HEADER: &str = "=== TOOL LEDGER — calls already issued before this fold ===";

/// One ledger line per folded tool result: tool, target, verdict.
fn ledger_lines(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult {
                tool_name,
                input,
                success,
                output,
            } => Some(format!(
                "- {tool_name}({}) {} — {}",
                snippet(input, 60),
                if *success { "ok" } else { "FAIL" },
                snippet(output, 100)
            )),
            _ => None,
        })
        .collect()
}

/// Merge the prior fold's ledger with this fold's new entries: order kept,
/// exact duplicates dropped, oldest evicted once over the cap.
fn merge_ledger(previous: Vec<String>, new: Vec<String>) -> Vec<String> {
    let mut merged: Vec<String> = Vec::with_capacity(previous.len() + new.len());
    for line in previous.into_iter().chain(new) {
        if !merged.contains(&line) {
            merged.push(line);
        }
    }
    let overflow = merged.len().saturating_sub(LEDGER_MAX_ENTRIES);
    merged.drain(..overflow);
    merged
}

fn render_ledger(lines: &[String]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "\n\n{LEDGER_HEADER}\nDeterministic record — these already ran; their full results live in the \
recall archive. Do NOT re-issue one of these to rediscover the same result; call `recall` with a query instead.\n{}",
        lines.join("\n")
    )
}

/// Tools whose SUCCESSFUL output is bulk and re-derivable: stale results from
/// these are safe to collapse to a one-line receipt during compaction, because
/// the agent can re-read the source (or the archive) if it ever needs them
/// again. Mirrors the compressor's COMPRESSIBLE set plus the file-dump readers.
const ROUTINE_TOOLS: &[&str] = &[
    "read",
    "list_directory",
    "glob",
    "grep",
    "codebase_search",
    "bash",
    "file_symbols",
    "symbol_search",
    "browser_state",
    // Browser/vision dumps are bulky and reproducible (a screenshot is base64,
    // an evaluate returns a big JSON blob) — an autonomous browser task
    // produces hundreds of these, and without collapsing them its durable
    // session never shrinks mechanically, forcing the LLM summary path (or,
    // before the split_point fix, looping forever). The freshest few and any
    // carrying an exact literal are still kept verbatim by importance_trim.
    "browser_screenshot",
    "browser_evaluate",
    "browser_navigate",
    "browser_click",
    "computer_screenshot",
    "image_analyze",
];

/// Keep this many of the NEWEST tool results verbatim even when routine — the
/// agent usually acts on the freshest read/listing/build output.
const KEEP_RECENT_TOOL_RESULTS: usize = 3;

/// Don't collapse a tool result below this size — there's nothing to save and a
/// receipt line could be longer than the result.
const ROUTINE_COLLAPSE_MIN_CHARS: usize = 500;

/// A mechanical pass must materially shrink the folded prefix before it is
/// allowed to count as compaction. Historical telemetry contained 123
/// "mechanical" folds whose rounded before/after sizes were identical; those
/// passes paid orchestration cost without buying context headroom.
const MECHANICAL_MIN_SAVINGS_CHARS: usize = 4_096;
const MECHANICAL_MIN_SAVINGS_PERCENT: usize = 5;

fn mechanical_savings_meaningful(before_chars: usize, after_chars: usize) -> bool {
    let saved = before_chars.saturating_sub(after_chars);
    saved >= MECHANICAL_MIN_SAVINGS_CHARS
        && saved.saturating_mul(100) >= before_chars.saturating_mul(MECHANICAL_MIN_SAVINGS_PERCENT)
}

/// Conservative "this output carries a value the agent must reproduce" test, used
/// to KEEP a routine result verbatim even when it would otherwise collapse. Biased
/// toward over-keeping (the governing rule: don't over-compress — re-fetching what
/// you forgot costs more than keeping it). A source file that merely mentions
/// "token" is kept; that's the safe direction.
fn carries_exact_literal(output: &str) -> bool {
    let lower = output.to_lowercase();
    output.contains("://") // URLs, connection strings, DSNs
        || lower.contains("dsn")
        || lower.contains("password")
        || lower.contains("secret")
        || lower.contains("token")
        || lower.contains("api_key")
        || lower.contains("api key")
        || lower.contains("bearer ")
        || lower.contains("-----begin")
}

/// Stage A of the compaction cascade — a mechanical, model-free importance pass
/// over the region about to be folded. KEEPS verbatim: every user / talk /
/// assistant message (decisions, findings, the user's own words, prior anchors),
/// every FAILED tool result (errors are the highest-value lines), the newest few
/// tool results, and any routine result carrying an exact literal. COLLAPSES only
/// stale, successful, bulky routine tool dumps to a one-line receipt — the full
/// text is already in the session archive, so this is reversible. This is the
/// "collapse routine successes to one line, keep decisions/findings/errors/
/// user-constraints near-verbatim" tier, run before the cascade decides whether
/// the expensive LLM summary is needed at all.
pub fn importance_trim(messages: &[Message]) -> Vec<Message> {
    let recent_tool_positions: std::collections::HashSet<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| matches!(m, Message::ToolResult { .. }))
        .map(|(index, _)| index)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .take(KEEP_RECENT_TOOL_RESULTS)
        .collect();

    messages
        .iter()
        .enumerate()
        .map(|(index, message)| match message {
            Message::ToolResult {
                tool_name,
                input,
                success,
                output,
            } if *success
                && ROUTINE_TOOLS.contains(&tool_name.as_str())
                && output.chars().count() > ROUTINE_COLLAPSE_MIN_CHARS
                && !recent_tool_positions.contains(&index)
                && !carries_exact_literal(output) =>
            {
                Message::ToolResult {
                    tool_name: tool_name.clone(),
                    input: input.clone(),
                    success: true,
                    output: format!(
                        "ok — {} chars elided during compaction (recoverable from the session archive; re-run the tool if the content is needed again)",
                        output.chars().count()
                    ),
                }
            }
            other => other.clone(),
        })
        .collect()
}

/// Render the old half for the summarizer, middle-elided to fit its window.
fn render_for_summarizer(messages: &[Message]) -> String {
    let full = messages
        .iter()
        .map(|m| match m {
            Message::User { content } => format!("User:\n{content}"),
            Message::Assistant { content } => format!("Assistant:\n{content}"),
            Message::Talk {
                from,
                to,
                subject,
                body,
                ..
            } => format!("Talk {from} -> {to} [{subject}]:\n{body}"),
            Message::GroupContribution {
                agent_id,
                display_name,
                role_title,
                subject,
                body,
                ..
            } => Message::format_group_contribution(
                agent_id,
                display_name,
                role_title,
                subject,
                body,
            ),
            Message::ToolResult {
                tool_name,
                input,
                success,
                output,
            } => format!(
                "Tool {tool_name}({input}) {}:\n{output}",
                if *success { "ok" } else { "FAILED" }
            ),
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    if full.chars().count() <= SUMMARIZER_INPUT_CAP_CHARS {
        return full;
    }
    let head: String = full.chars().take(SUMMARIZER_INPUT_CAP_CHARS / 3).collect();
    let tail: String = full
        .chars()
        .rev()
        .take(SUMMARIZER_INPUT_CAP_CHARS / 2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}\n\n[... middle of history elided for the summarizer ...]\n\n{tail}")
}

/// Prefix of the folded-history continuation message. Used both to label the
/// message AND to detect a prior summary on the next fold (anchored compaction).
const CONTINUATION_MARKER: &str = "[AUTO-COMPACTED HISTORY";

/// True only for Phoenix's synthetic portable compaction anchor.  Permanent
/// transcript deletion uses this to distinguish raw authored history from a
/// summary which may paraphrase a turn that is being erased.
pub fn is_continuation_message(message: &Message) -> bool {
    matches!(message, Message::Assistant { content } if content.starts_with(CONTINUATION_MARKER))
}

/// Rebuild a portable anchor exclusively from the surviving raw archive.
///
/// A model-written anchor cannot be edited safely after a permanent deletion:
/// it may paraphrase the deleted prompt or response even when no exact string
/// remains.  This deterministic replacement is deliberately derived only from
/// surviving messages.  The archive remains the verbatim source of truth and
/// the bounded head/tail digest keeps the next model request compact.
pub fn rebuild_continuation_after_deletion(messages: &[Message]) -> Option<Message> {
    let raw = messages
        .iter()
        .filter(|message| !is_continuation_message(message))
        .cloned()
        .collect::<Vec<_>>();
    if raw.is_empty() {
        return None;
    }

    let digest = receipt_digest(&raw);
    let digest_chars = digest.chars().count();
    let summary = if digest_chars <= ANCHOR_CAP_CHARS {
        digest
    } else {
        let head_cap = ANCHOR_CAP_CHARS * 3 / 5;
        let tail_cap = ANCHOR_CAP_CHARS - head_cap;
        let head = digest.chars().take(head_cap).collect::<String>();
        let tail = digest
            .chars()
            .rev()
            .take(tail_cap)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<String>();
        format!(
            "{head}\n\n[... surviving archived detail omitted from this bounded anchor; use recall for exact rows ...]\n\n{tail}"
        )
    };
    let ledger = render_ledger(&ledger_lines(&raw));
    Some(Message::Assistant {
        content: format!(
            "[AUTO-COMPACTED HISTORY — {} surviving earlier messages remain after a permanent transcript deletion. This anchor was rebuilt only from those surviving rows; deleted material is not part of it. Treat the archive as the exact source of what happened and continue without repeating completed work. MUTABLE EXTERNAL STATUS in this history (for example in-progress, incomplete, unchecked, logged-in, available, or unlocked) is only a historical observation, never proof of current state. Re-read the live source in this turn before scheduling or describing such an item as current; if verification fails, report the state as unknown rather than carrying the old state forward.]\n\n{summary}{ledger}",
            raw.len()
        ),
    })
}

const SUMMARIZER_SYSTEM: &str =
    "You maintain an ANCHORED running summary of an agent conversation \
so the agent can keep working with most of the transcript replaced by your summary. The agent \
treats your output as a bounded navigation index, not as authority over direct user statements, \
the deterministic user-intent ledger, the verbatim archive, or freshly read external state.\n\n\
If a <previous-summary> block is given, it IS the current anchor: UPDATE it — prioritize the active outcome, latest corrections, unfinished work, and decisive evidence. \
Rewrite the working index rather than accumulating history. Older unrelated discussions and \
completed tasks belong in a short recall pointer, not a chronological recap. Preserve older \
constraints and exact facts only when still relevant to current work; never let them crowd out newer corrections.\n\n\
Capture, densely and factually, under these headings (keep a heading only when it has content):\n\
- Active intent: the user's current goal/request, quoted VERBATIM where short.\n\
- Status: the active outcome and unfinished requirements; distinguish completed milestones from the whole goal.\n\
- User state: every user-reported completion, submission, pass/fail, correction, or reopening. \n\
Copy the affected item and new state exactly. A later planning request does not make completed \n\
work incomplete.\n\
- Mutable external state: distinguish history from current truth. Statuses read from an app, \
site, database, calendar, browser, or tool (for example in-progress, incomplete, unchecked, \
logged-in, available, or unlocked) are LAST-OBSERVED facts only. Preserve their source and \
observation date/time when known; never summarize them as current after that observation. A \
future turn must re-read the live source before using mutable status to schedule work. If the \
live check fails, state is unknown rather than inherited from this summary.\n\
- Standing directives: any explicit user instruction that CHANGES how the work must be done — \
especially DISCARD/START-OVER/FULL-REDESIGN/REPLACE/REBUILD-FROM-SCRATCH orders, hard constraints, \
and things the user said NOT to do. Carry these VERBATIM and never soften, generalize, or drop \
them: 'redo the landing page' is NOT the same as the user's 'discard everything and do a FULL \
redesign, make the images green.' If the user overrode a default (e.g. 'do not preserve the \
template'), that override outranks the default and must survive every fold.\n\
- Decisions: what was decided and why.\n\
- Team & delegations: this is a MULTI-AGENT run — preserve the orchestration \
state or the orchestrator forgets it has a team and collapses to poking ONE \
specialist. Capture: which specialists are involved and what each OWNS, any \
multi-stage plan and its stage->owner assignments (e.g. planner decomposed X \
into coder->tester->critic), every background job still PENDING and who owns it, \
and any lane a task still needs to reach (tests->tester, review->critic, \
browser->browser). Never drop a teammate just because they were mentioned once; \
a plan that routed work across the team MUST survive the fold intact.\n\
- Files & changes: current artifacts and relevant changes only; omit unrelated historical file inventories.\n\
- Findings: tool results still worth knowing — numbers, what worked or failed, URLs.\n\
- Exact literals: any precise value the agent must reproduce later — connection strings/DSNs, \
ports, hostnames, env values, tokens/keys, full file paths, full command lines with flags, IDs, \
seeds, version pins — copied BYTE-FOR-BYTE in backticks, NEVER paraphrased. Summarizing \"the user \
gave a DB config\" LOSES the value; keep the literal.\n\
- Preferences: explicit user preferences and corrections, verbatim where short.\n\
- Open threads: what is unresolved and what the agent was about to do next.\n\n\
Terse bullets; preserve exact paths and identifiers. No preamble, no commentary about \
summarizing — output only the summary content itself.";

/// Provider-native compactors must create a complete replacement context, not
/// a casual synopsis. The original agent/system prompt travels as a System
/// ChatMessage; this is the dedicated compaction directive used by provider
/// context-edit APIs.
const NATIVE_COMPACTION_INSTRUCTIONS: &str =
    "Create a focused replacement context for the conversation prefix. Put current objective, latest user corrections, unfinished work, and decisive dated evidence first. Treat prior summaries as revisable indexes, not text to accumulate. Replace unrelated old topics and finished tool-by-tool history with brief recall pointers; retain older constraints only when relevant to current work. Preserve the user's active intent and exact standing instructions; every user-reported completion, submission, pass/fail, correction, or reopening as authoritative state; settled decisions and their reasons; every file/path changed; exact identifiers, commands, versions, URLs, ports, and values still needed; completed work that must not be repeated; active team/delegation state; failures and constraints; and all unresolved next steps. A later planning request does not make completed work incomplete. Update prior compaction state as a lossy working index: direct user statements and corrections outrank it, and mutable external state must be freshly verified. Do not call tools, answer the user, or add commentary about compacting. The replacement must let the same agent continue without restarting or silently changing the task.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeCompactionMode {
    NotAttempted,
    Installed,
    PortableOnlyRequestRejected,
    PortableOnlyProviderFailed,
    PortableOnlyInvalidResult,
    PortableOnlyReplayTooLarge,
}

impl NativeCompactionMode {
    pub fn notice(self) -> &'static str {
        match self {
            Self::NotAttempted => "portable",
            Self::Installed => "provider-native + portable fallback",
            Self::PortableOnlyRequestRejected => "portable (native request rejected locally)",
            Self::PortableOnlyProviderFailed => "portable (native endpoint unavailable)",
            Self::PortableOnlyInvalidResult => "portable (native result rejected)",
            Self::PortableOnlyReplayTooLarge => "portable (native replay exceeded session limit)",
        }
    }
}

/// Result of one compaction pass, for events/telemetry.
#[derive(Debug)]
pub struct CompactionOutcome {
    pub folded_messages: usize,
    pub before_chars: usize,
    pub after_chars: usize,
    pub summarizer_input_tokens: u32,
    pub summarizer_output_tokens: u32,
    pub native_input_tokens: u32,
    pub native_output_tokens: u32,
    /// False when the model call failed and the receipt fallback was used.
    pub used_model: bool,
    /// The independent specialist summary failed, but a validated readable
    /// provider-native summary was available for the portable continuation.
    pub provider_summary_used: bool,
    /// True when Stage A (mechanical importance trim) alone got the transcript
    /// under budget, so no LLM summary was produced — the cheap, lossless-ish
    /// path. The folded region stays as importance-trimmed verbatim messages
    /// instead of a single summary.
    pub mechanical_only: bool,
    /// Honest persisted mode; never contains opaque provider state or response
    /// fragments.
    pub native_mode: NativeCompactionMode,
}

/// Compact `session` in place: fold everything before the split point into one
/// continuation message. Returns None when the transcript is too small to
/// bother. Never errors the turn — the receipt fallback absorbs model failure.
pub async fn compact_session(
    provider: &Arc<dyn LLMProvider>,
    model: &str,
    session: &mut Session,
    window_tokens: u64,
    archive_dir: Option<&Path>,
) -> Option<CompactionOutcome> {
    compact_session_inner(provider, model, session, window_tokens, archive_dir, None).await
}

/// Boundary-only compaction that races the acting provider's native context
/// edit against Phoenix's portable specialist summary. The archive is durable
/// before either network call begins, and the portable continuation is always
/// installed even when native state is rejected.
#[allow(clippy::too_many_arguments)]
pub async fn compact_session_native(
    native_provider: &Arc<dyn LLMProvider>,
    native_route: NativeCompactionRoute,
    tools: Vec<ToolDefinition>,
    trigger_tokens: u64,
    summarizer: &Arc<dyn LLMProvider>,
    summary_model: &str,
    session: &mut Session,
    window_tokens: u64,
    archive_dir: &Path,
) -> Option<CompactionOutcome> {
    compact_session_inner(
        summarizer,
        summary_model,
        session,
        window_tokens,
        Some(archive_dir),
        Some(NativeAttempt {
            provider: native_provider,
            route: native_route,
            tools,
            trigger_tokens,
        }),
    )
    .await
}

struct NativeAttempt<'a> {
    provider: &'a Arc<dyn LLMProvider>,
    route: NativeCompactionRoute,
    tools: Vec<ToolDefinition>,
    trigger_tokens: u64,
}

async fn compact_session_inner(
    provider: &Arc<dyn LLMProvider>,
    model: &str,
    session: &mut Session,
    window_tokens: u64,
    archive_dir: Option<&Path>,
    native: Option<NativeAttempt<'_>>,
) -> Option<CompactionOutcome> {
    let keep_tokens = keep_tokens_for_window(window_tokens);
    let keep_chars = keep_tokens as usize * CHARS_PER_TOKEN as usize;
    let split = split_point(&session.messages, keep_chars);
    // Token pressure, not message count, decides whether there is meaningful
    // work to fold. A single connected-app response can exceed 100k chars; the
    // old `split < 4` gate then logged "compaction due" every tool round but
    // returned None forever. Archival and the bounded continuation make even a
    // one-message prefix safe to fold.
    if split == 0 {
        return None;
    }

    let before_chars: usize = session.messages.iter().map(message_chars).sum();
    let old = &session.messages[..split];

    // Validate existing history before appending or taking either fold path.
    // Reuse the read for the intent ledger; do not scan the newly appended
    // prefix again or let a mechanical fold bypass a corrupt archive.
    let archived_intent = match archive_dir.map(|dir| read_continuity_archive(session, dir)).transpose() {
        Ok(messages) => messages.unwrap_or_default(),
        Err(error) => {
            tracing::error!("compaction continuity failed; preserving live transcript: {error:#}");
            return None;
        }
    };

    if let Some(dir) = archive_dir {
        if let Err(error) = archive_messages(dir, &session.id, old) {
            tracing::error!("compaction archive failed; preserving live transcript: {error:#}");
            return None;
        }
    }

    // ── Stage A — mechanical importance trim (cascade: cheapest first) ──
    // Collapse stale routine tool dumps before spending a model call. If that
    // alone brings the folded region under the keep budget, keep the
    // importance-trimmed messages verbatim and SKIP the LLM entirely — the
    // "mechanical compression first, summarization last" rule, and the "don't
    // over-compress" rule (a verbatim trimmed transcript beats a lossy summary
    // whenever it fits). This is compaction as the USER understands it
    // (2026-07-14): "remove old tool call results that are not needed" — the
    // conversation itself stays word-for-word; only stale dumps become
    // one-line receipts. Post-fold ≤ 2×keep, still well under the trigger, so
    // no immediate re-fold; the LLM summary is strictly the last resort.
    let trimmed_old = importance_trim(old);
    let old_chars: usize = old.iter().map(message_chars).sum();
    let trimmed_old_chars: usize = trimmed_old.iter().map(message_chars).sum();
    let mechanical_is_meaningful = mechanical_savings_meaningful(old_chars, trimmed_old_chars);
    // A configured provider-native route is part of the compaction contract,
    // not an optional afterthought.  Do not let the cheap importance-trim
    // return bypass the native request: Phoenix's portable summary still runs
    // alongside it and remains the durable fallback.
    let tail_chars: usize = session.messages[split..].iter().map(message_chars).sum();
    // Leave headroom below the request/cache-miss fold floor (1.5x keep).
    // Two separately bounded halves can still total nearly 2x keep and trigger
    // another fold on the very next round when fixed prompt overhead is large.
    let mechanical_after_chars = trimmed_old_chars.saturating_add(tail_chars);
    if native.is_none()
        && mechanical_after_chars < keep_chars + keep_chars / 2
        && mechanical_is_meaningful
    {
        let mut kept: Vec<Message> =
            Vec::with_capacity(trimmed_old.len() + session.messages.len() - split);
        kept.extend(trimmed_old);
        kept.extend_from_slice(&session.messages[split..]);
        session.replace_messages(kept);
        let after_chars: usize = session.messages.iter().map(message_chars).sum();
        return Some(CompactionOutcome {
            folded_messages: split,
            before_chars,
            after_chars,
            summarizer_input_tokens: 0,
            summarizer_output_tokens: 0,
            native_input_tokens: 0,
            native_output_tokens: 0,
            used_model: false,
            provider_summary_used: false,
            mechanical_only: true,
            native_mode: NativeCompactionMode::NotAttempted,
        });
    }

    // ── Stage B — anchored LLM summary (only when mechanical trim wasn't enough) ──
    // Feed the ALREADY-TRIMMED region to the summarizer: less routine noise in,
    // a denser summary out, fewer input tokens billed.
    let old = trimmed_old.as_slice();

    // Anchored compaction: if the oldest folded message is itself a prior
    // continuation summary, peel it off and pass it as the anchor to UPDATE,
    // rather than re-summarizing a summary (which degrades over repeated folds
    // on very long sessions — the exact case where compaction matters most).
    // The prior ledger is peeled separately: it must never round-trip through
    // the summarizer (deterministic in, deterministic out).
    let (previous_summary, previous_user_intent, previous_ledger, old_to_render): (
        Option<String>,
        Vec<String>,
        Vec<String>,
        &[Message],
    ) = match old.first() {
        Some(Message::Assistant { content }) if content.starts_with(CONTINUATION_MARKER) => {
            let body = content
                .split_once("]\n\n")
                .map(|(_, rest)| rest.to_string())
                .unwrap_or_else(|| content.clone());
            let (without_ledger, ledger) = match body.split_once(LEDGER_HEADER) {
                Some((summary_and_intent, ledger_part)) => (
                    summary_and_intent.trim_end().to_string(),
                    ledger_part
                        .lines()
                        .filter(|line| line.starts_with("- "))
                        .map(str::to_string)
                        .collect(),
                ),
                None => (body, Vec::new()),
            };
            let (summary_body, user_intent) = match without_ledger.split_once(USER_INTENT_HEADER) {
                Some((summary, intent_part)) => (
                    summary.trim_end().to_string(),
                    intent_part
                        .lines()
                        .filter(|line| line.starts_with("- user: "))
                        .map(str::to_string)
                        .collect(),
                ),
                None => (without_ledger, Vec::new()),
            };
            (Some(summary_body), user_intent, ledger, &old[1..])
        }
        _ => (None, Vec::new(), Vec::new(), old),
    };

    // Bound anchor growth: a summary re-fed as anchor over many folds must not
    // grow without limit. Clipped detail is still in the archive via `recall`.
    let previous_summary = previous_summary.map(bound_anchor_summary);

    // The ledger is built from the PRE-TRIM folded slice so verdicts carry the
    // real output snippet, not the "chars elided" placeholder Stage A wrote.
    let ledger = merge_ledger(previous_ledger, ledger_lines(&session.messages[..split]));
    let mut intent_messages = archived_intent;
    intent_messages.extend_from_slice(&session.messages[..split]);
    let user_intent = merge_user_intent(previous_user_intent, &intent_messages);

    let rendered = render_for_summarizer(old_to_render);

    // The archive above is the hard durability barrier. Do not insert another
    // provider call before the fold: the old pre-fold memory flush could stall
    // this critical path for 45 seconds and still did not prove that the active
    // agent had safely released its working set.

    let (native_call, mut native_mode) = match native {
        Some(native) => {
            let prior_replay = session
                .provider_compaction
                .as_ref()
                .filter(|replay| {
                    replay.matches_route(
                        native.route.capability(),
                        &session.id,
                        native.route.provider(),
                        native.route.base_route(),
                        native.route.model(),
                        native.route.account_scope(),
                        native.route.auth_epoch(),
                    )
                })
                .cloned();
            let native_start = prior_replay
                .as_ref()
                .map_or(0, NativeCompactionReplay::portable_suffix_start);
            if native_start >= split {
                (None, NativeCompactionMode::PortableOnlyRequestRejected)
            } else {
                let native_prefix_tokens = estimated_tokens(
                    session.messages[native_start..split]
                        .iter()
                        .map(message_chars)
                        .sum(),
                );
                let messages = native_compaction_messages(
                    &session.system_prompt,
                    &session.messages[native_start..split],
                );
                let next_generation = prior_replay.as_ref().map_or(Some(1), |replay| {
                    replay.provenance().compaction_generation().checked_add(1)
                });
                match next_generation.and_then(|generation| {
                    NativeCompactionRequest::try_new(
                        native.route.clone(),
                        NATIVE_COMPACTION_INSTRUCTIONS,
                        messages,
                        native.tools,
                        prior_replay,
                        session.id.clone(),
                        session.transcript_revision,
                        native.trigger_tokens.min(native_prefix_tokens).max(1),
                        generation,
                    )
                    .ok()
                }) {
                    Some(request) => (
                        Some((native.provider, request, native.route)),
                        NativeCompactionMode::NotAttempted,
                    ),
                    None => (None, NativeCompactionMode::PortableOnlyRequestRejected),
                }
            }
        }
        None => (None, NativeCompactionMode::NotAttempted),
    };

    let fallback = || {
        // Recent evidence owns the working budget even when the model fails.
        // The complete prefix was archived above; this is only a recall index.
        let recent = old_to_render.iter().rev().take(48).cloned().collect::<Vec<_>>();
        let prior = previous_summary.as_deref().unwrap_or("").chars().take(4_000).collect::<String>();
        format!("Recent user instructions (oldest to newest within this block):\n{}\n\nRecent receipts (newest first; observations, not new instructions):\n{}\n\nOlder context reference — may be superseded; use recall for full details:\n{}",
            user_intent.iter().rev().take(16).cloned().collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"),
            receipt_digest(&recent), prior)
    };
    let summary_future = summarize(provider, model, &rendered, previous_summary.as_deref());
    let (summary_result, native_result) = match native_call {
        Some((native_provider, native_request, native_route)) => {
            let expected_folded_messages = native_request.messages().len();
            let (summary_result, native_result) = tokio::join!(
                summary_future,
                request_native_compaction(native_provider, native_request)
            );
            (
                summary_result,
                Some(native_result.map(|result| (result, expected_folded_messages, native_route))),
            )
        }
        None => (summary_future.await, None),
    };
    let native_portable_summary = native_result.as_ref().and_then(|result| match result {
        Ok((result, expected_folded_messages, _))
            if result.folded_input_messages() == *expected_folded_messages
                && result.replay().portable_suffix_start() == 1 =>
        {
            result.portable_summary().map(str::to_owned)
        }
        _ => None,
    });
    let (summary, in_tokens, out_tokens, used_model, provider_summary_used) = match summary_result {
        Ok((text, input_tokens, output_tokens)) if !text.trim().is_empty() => {
            (text, input_tokens, output_tokens, true, false)
        }
        Ok(_) | Err(_) => match native_portable_summary {
            Some(summary) => (summary, 0, 0, false, true),
            None => (fallback(), 0, 0, false, false),
        },
    };
    // Providers can ignore an output budget or return a recursively expanded
    // anchor. Bound the newly generated summary too, not only the old anchor
    // fed into the request, otherwise one fold can immediately recreate a
    // six-figure continuation.
    let summary = bound_anchor_summary(summary);

    let folded = split;
    let continuation = Message::Assistant {
        content: format!(
            "[AUTO-COMPACTED HISTORY — {folded} earlier messages were folded into this summary to \
keep the session inside the model context. This is a lossy navigation index, not authority over \
the user's current request, direct user statements in the USER INTENT LEDGER, exact archived rows, \
or freshly read external state. If they conflict, the current user correction wins; use `recall` \
for the exact prior statement and the live source for mutable status. Do not redo completed work. Statuses \
read from an app/site/database (such as in-progress, incomplete, unchecked, logged-in, available, \
or unlocked) are historical observations only. Re-read the live source in the current turn before \
scheduling or describing them as current; if verification fails, call the state unknown instead \
of carrying the old state forward. This fold may have happened MID-TASK: do not restart from scratch, \
do not repeat updates you already delivered, and do not re-plan settled decisions — continue \
naturally, treating the work before and after this summary as one chain, and make reasonable \
assumptions about small details the summary omits. Every folded message is archived VERBATIM: \
if you need an exact value, file content, or finding from before this fold, call `recall` with \
a query — one cheap lookup — instead of re-reading files or re-running searches to \
         reconstruct it.]\n\n{summary}{user_intent_block}{ledger_block}",
            user_intent_block = render_user_intent(&user_intent),
            ledger_block = render_ledger(&ledger)
        ),
    };
    let mut kept: Vec<Message> = Vec::with_capacity(session.messages.len() - split + 1);
    kept.push(continuation);
    kept.extend_from_slice(&session.messages[split..]);

    let mut native_input_tokens = 0;
    let mut native_output_tokens = 0;
    match native_result {
        Some(Ok((result, expected_folded_messages, native_route))) => {
            native_input_tokens = result.usage().input_tokens;
            native_output_tokens = result.usage().output_tokens;
            let valid_coverage = result.folded_input_messages() == expected_folded_messages
                && result.replay().portable_suffix_start() == 1;
            if valid_coverage {
                match session.install_provider_compaction(
                    kept.clone(),
                    result.into_replay(),
                    &native_route,
                ) {
                    Ok(true) => native_mode = NativeCompactionMode::Installed,
                    Ok(false) => {
                        native_mode = NativeCompactionMode::PortableOnlyReplayTooLarge;
                    }
                    Err(_) => {
                        // A completed but incompatible result must never leave an
                        // older opaque prefix attached to newly folded history.
                        session.replace_messages(kept);
                        native_mode = NativeCompactionMode::PortableOnlyInvalidResult;
                    }
                }
            } else {
                session.replace_messages(kept);
                native_mode = NativeCompactionMode::PortableOnlyInvalidResult;
            }
        }
        Some(Err(_)) => {
            session.replace_messages(kept);
            native_mode = NativeCompactionMode::PortableOnlyProviderFailed;
        }
        None => session.replace_messages(kept),
    }

    let after_chars: usize = session.messages.iter().map(message_chars).sum();
    Some(CompactionOutcome {
        folded_messages: folded,
        before_chars,
        after_chars,
        summarizer_input_tokens: in_tokens,
        summarizer_output_tokens: out_tokens,
        native_input_tokens,
        native_output_tokens,
        used_model,
        provider_summary_used,
        mechanical_only: false,
        native_mode,
    })
}

fn native_compaction_messages(system_prompt: &str, messages: &[Message]) -> Vec<ChatMessage> {
    let mut native = Vec::with_capacity(messages.len() + 1);
    native.push(ChatMessage::system(system_prompt.to_string()));
    native.extend(messages.iter().map(|message| match message {
        Message::User { content } => ChatMessage::user(content.clone()),
        Message::Assistant { content } => ChatMessage::assistant(content.clone()),
        Message::Talk {
            from,
            to,
            subject,
            body,
            reply_expected,
            ..
        } => ChatMessage::user(Message::format_talk_envelope(
            from,
            to,
            subject,
            body,
            *reply_expected,
        )),
        Message::GroupContribution {
            agent_id,
            display_name,
            role_title,
            subject,
            body,
            ..
        } => ChatMessage::user(Message::format_group_contribution(
            agent_id,
            display_name,
            role_title,
            subject,
            body,
        )),
        Message::ToolResult {
            tool_name,
            input,
            success,
            output,
        } => ChatMessage::user(Message::format_tool_result(
            tool_name, input, *success, output,
        )),
    }));
    native
}

async fn request_native_compaction(
    provider: &Arc<dyn LLMProvider>,
    request: NativeCompactionRequest,
) -> Result<crate::providers::NativeCompactionResult> {
    match tokio::time::timeout(SUMMARIZER_TIMEOUT, provider.compact_context(request)).await {
        Ok(result) => result.context("provider-native compaction call failed"),
        Err(_) => anyhow::bail!(
            "provider-native compaction timed out after {}s",
            SUMMARIZER_TIMEOUT.as_secs()
        ),
    }
}

async fn summarize(
    provider: &Arc<dyn LLMProvider>,
    model: &str,
    rendered: &str,
    previous: Option<&str>,
) -> Result<(String, u32, u32)> {
    let user_prompt = match previous {
        Some(prev) => format!(
            "<previous-summary>\n{prev}\n</previous-summary>\n\nNew conversation history since that \
summary:\n\n{rendered}\n\nProduce the updated anchored summary now."
        ),
        None => format!(
            "Conversation history to compact:\n\n{rendered}\n\nProduce the continuation summary now."
        ),
    };
    let mut request = CompletionRequest::new(
        model.to_string(),
        vec![
            ChatMessage::system(SUMMARIZER_SYSTEM.to_string()),
            ChatMessage::user(user_prompt),
        ],
    );
    request.max_tokens = Some(SUMMARIZER_MAX_OUTPUT_TOKENS);
    // Summarizing is extraction, not reasoning — "low" by default (higher
    // effort just streams huge thinking blocks for no quality gain). An
    // EXPLICIT [profile.llm.efforts].librarian entry overrides it; the
    // global reasoning_effort deliberately does NOT reach this lane.
    let librarian_effort = crate::config::PhoenixConfig::load()
        .ok()
        .and_then(|c| c.profile.llm.efforts.get("librarian").cloned())
        .unwrap_or_else(|| "low".to_string());
    request.extra_body.insert(
        "reasoning".to_string(),
        serde_json::json!({ "effort": librarian_effort }),
    );
    let response = match tokio::time::timeout(SUMMARIZER_TIMEOUT, provider.complete(request)).await
    {
        Ok(result) => result.context("compaction summarizer call failed")?,
        Err(_) => anyhow::bail!(
            "compaction summarizer timed out after {}s",
            SUMMARIZER_TIMEOUT.as_secs()
        ),
    };
    Ok((
        response.content,
        response.usage.input_tokens,
        response.usage.output_tokens,
    ))
}

/// Append folded messages to `<session_id>.archive.jsonl` so compaction never
/// destroys history — `/rewind`-class features can read this back later.
fn archive_messages(dir: &Path, session_id: &str, messages: &[Message]) -> Result<()> {
    use std::io::Write;

    crate::session::SessionStore::validate_session_id(session_id)?;
    let path = dir.join(format!("{session_id}.archive.jsonl"));
    let mut batch = Vec::new();
    for message in messages {
        let line = serde_json::to_string(message)?;
        writeln!(batch, "{line}")?;
    }
    crate::config::private_io::with_private_lock(&path, || {
        // Revalidate at the commit boundary, not only before summarization.
        // Never extend a corrupt archive after another writer changes it.
        let old_count = read_archive_under_lock(&path)?.len();
        anyhow::ensure!(old_count.saturating_add(messages.len()) <= 100_000,
            "archive append exceeds continuity record limit; current history preserved");
        if batch.is_empty() { return Ok(()); }
        let mut replacement = crate::config::private_io::read_private_file(&path)?.unwrap_or_default();
        let separator = !replacement.is_empty() && replacement.last() != Some(&b'\n');
        anyhow::ensure!(replacement.len().saturating_add(batch.len()).saturating_add(usize::from(separator)) <= 64 * 1024 * 1024,
            "archive append exceeds continuity byte limit; current history preserved");
        if separator { replacement.push(b'\n'); }
        replacement.extend_from_slice(&batch);
        // Publish only a fully written and synced replacement. A failed or
        // interrupted staging write cannot leave a partial canonical tail.
        crate::config::private_io::atomic_write_private_under_lock(&path, &replacement)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn killed_archive_writer_preserves_canonical_history_and_releases_lock() {
        if let Ok(root) = std::env::var("PHOENIX_ARCHIVE_CRASH_CHILD_ROOT") {
            archive_messages(Path::new(&root), "crash", &[Message::User { content: "uncommitted addition".into() }]).unwrap();
            panic!("child must be killed before publication");
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crash.archive.jsonl");
        let original = Message::User { content: "Lessons 3 and 4 completed — exact ✓".into() };
        archive_messages(dir.path(), "crash", &[original.clone()]).unwrap();
        let bytes_before = std::fs::read(&path).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "runtime::compaction::tests::killed_archive_writer_preserves_canonical_history_and_releases_lock", "--nocapture"])
            .env("PHOENIX_ARCHIVE_CRASH_CHILD_ROOT", dir.path())
            .env("PHOENIX_TEST_ATOMIC_PAUSE_TARGET", &path)
            .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
            .spawn().unwrap();
        let started = std::time::Instant::now();
        while !path.with_extension("publication-ready").exists() {
            if child.try_wait().unwrap().is_some() { panic!("writer exited before the publication boundary"); }
            if started.elapsed().as_secs() >= 10 {
                let _ = child.kill(); let _ = child.wait();
                panic!("writer did not reach the publication boundary");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        child.kill().unwrap();
        let status = child.wait().unwrap();
        assert!(!status.success());
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.signal(), Some(9), "probe must use an actual SIGKILL");
        }
        assert_eq!(std::fs::read(&path).unwrap(), bytes_before, "uncommitted staging bytes reached canonical history");
        let recovered = Message::User { content: "Assessment completed after recovery".into() };
        archive_messages(dir.path(), "crash", &[recovered.clone()]).unwrap();
        assert_eq!(serde_json::to_value(archived_messages(dir.path(), "crash").unwrap()).unwrap(),
            serde_json::to_value(vec![original, recovered]).unwrap());
    }

    #[test]
    fn archive_record_limit_rejection_preserves_the_readable_original() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("full.archive.jsonl");
        let row = Message::User { content: "completed".into() };
        let original = (serde_json::to_string(&row).unwrap() + "\n").repeat(100_000);
        crate::config::private_io::write_private_file(&path, original.as_bytes()).unwrap();
        let error = archive_messages(dir.path(), "full", &[row]).unwrap_err();
        assert!(error.to_string().contains("record limit"));
        assert_eq!(std::fs::read(&path).unwrap(), original.as_bytes());
        assert_eq!(archived_messages(dir.path(), "full").unwrap().len(), 100_000);
    }

    #[test]
    fn concurrent_process_archive_appends_preserve_exact_records() {
        const WORKERS: usize = 4;
        const BATCHES: usize = 16;
        const ROWS: usize = 3;
        if let Ok(root) = std::env::var("PHOENIX_ARCHIVE_TEST_CHILD_ROOT") {
            let root = std::path::PathBuf::from(root);
            let worker: usize = std::env::var("PHOENIX_ARCHIVE_TEST_CHILD_ID").unwrap().parse().unwrap();
            std::fs::write(root.join(format!("ready-{worker}")), b"ready").unwrap();
            let start = std::time::Instant::now();
            while !root.join("go").exists() {
                assert!(start.elapsed().as_secs() < 10, "parent failed to release writer barrier");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            for batch in 0..BATCHES {
                let rows = (0..ROWS).map(|row| Message::User {
                    content: format!("worker={worker};batch={batch};row={row}; completed ✓\nexact second line"),
                }).collect::<Vec<_>>();
                archive_messages(&root, "shared", &rows).unwrap();
            }
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let executable = std::env::current_exe().unwrap();
        let mut children = (0..WORKERS).map(|worker| {
            std::process::Command::new(&executable)
                .args(["--exact", "runtime::compaction::tests::concurrent_process_archive_appends_preserve_exact_records", "--nocapture"])
                .env("PHOENIX_ARCHIVE_TEST_CHILD_ROOT", dir.path())
                .env("PHOENIX_ARCHIVE_TEST_CHILD_ID", worker.to_string())
                .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped())
                .spawn().unwrap()
        }).collect::<Vec<_>>();
        let start = std::time::Instant::now();
        while !(0..WORKERS).all(|worker| dir.path().join(format!("ready-{worker}")).exists()) {
            if start.elapsed().as_secs() >= 10 {
                for child in &mut children { let _ = child.kill(); let _ = child.wait(); }
                panic!("concurrent archive writers did not reach the start barrier");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        std::fs::write(dir.path().join("go"), b"go").unwrap();
        for child in children {
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success(), "archive child failed: {}", String::from_utf8_lossy(&output.stderr));
        }
        let rows = archived_messages(dir.path(), "shared").unwrap();
        assert_eq!(rows.len(), WORKERS * BATCHES * ROWS);
        let actual = rows.into_iter().map(|row| match row { Message::User { content } => content, _ => panic!("unexpected archive row") }).collect::<std::collections::BTreeSet<_>>();
        let expected = (0..WORKERS).flat_map(|worker| (0..BATCHES).flat_map(move |batch| (0..ROWS).map(move |row|
            format!("worker={worker};batch={batch};row={row}; completed ✓\nexact second line")))).collect();
        assert_eq!(actual, expected, "concurrent writers lost, duplicated or altered records");
    }

    #[test]
    fn append_revalidates_after_prior_read_and_preserves_corrupt_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("race.archive.jsonl");
        let row = Message::User { content: "Completed lesson four".into() };
        archive_messages(dir.path(), "race", &[row.clone()]).unwrap();
        assert_eq!(archived_messages(dir.path(), "race").unwrap().len(), 1);
        crate::config::private_io::with_private_lock(&path, || {
            crate::config::private_io::write_private_file(&path, b"{broken concurrent replacement}\n")
        }).unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(archive_messages(dir.path(), "race", &[row]).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn append_preserves_a_valid_final_record_without_a_newline() {
        let dir = tempfile::tempdir().unwrap();
        let first = Message::User { content: "Science 3 and 4 are complete — ✓".into() };
        let second = Message::User { content: "Assessment submitted".into() };
        let path = dir.path().join("boundary.archive.jsonl");
        crate::config::private_io::write_private_file(&path, &serde_json::to_vec(&first).unwrap()).unwrap();
        archive_messages(dir.path(), "boundary", &[second.clone()]).unwrap();
        let rows = archived_messages(dir.path(), "boundary").unwrap();
        assert_eq!(serde_json::to_value(rows).unwrap(), serde_json::to_value(vec![first,second]).unwrap());
    }

    #[test]
    fn reading_absent_archive_does_not_create_worker_state() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("not-created");
        assert!(archived_messages(&missing, "fresh-worker").unwrap().is_empty());
        assert!(!missing.exists(), "a read of absent history must remain read-only");
        assert!(archived_messages(dir.path(), "fresh-worker").unwrap().is_empty());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0,
            "missing archives must not leave one lock per disposable worker");
    }

    fn receipt_fixture(id: &str, group: &str, body: &str) -> Message {
        Message::GroupContribution {
            turn_id: "original".into(), message_id: id.into(), group_id: group.into(), agent_id: "iris".into(),
            internal_role: "frontend".into(), display_name: "Iris".into(), role_title: "Design".into(),
            color: "".into(), icon_seed: "".into(), avatar: None, subject: "Exact design".into(),
            body: body.into(), reply_to: None, causation_id: None,
        }
    }

    #[test]
    fn exact_group_receipts_survive_archive_relocation_without_context_bloat() {
        let dir = tempfile::tempdir().unwrap();
        let archived = receipt_fixture("old", "build", "42 × 18 mm; tolerances ±0.03\n```json\n{\"scale\":1.25}\n```");
        let current = receipt_fixture("new", "build", "Keep the user's revised blue finish.");
        let noise = Message::Assistant { content: "unrelated context ".repeat(200) };
        archive_messages(dir.path(), "group-archive", &vec![noise; 3_000]).unwrap();
        archive_messages(dir.path(), "group-archive", &[receipt_fixture("old", "other", "WRONG GROUP"), archived.clone()]).unwrap();
        let result = resolve_group_contributions(dir.path(), "group-archive", "build", &["old".into(), "new".into()], &[current.clone()]).unwrap();
        assert_eq!(serde_json::to_value(result).unwrap(), serde_json::to_value(vec![archived, current]).unwrap());
        assert!(resolve_group_contributions(dir.path(), "../escape", "build", &["old".into()], &[]).is_err());
        assert!(resolve_group_contributions(dir.path(), "group-archive", "build", &["missing".into()], &[]).is_err());
        assert!(resolve_group_contributions(dir.path(), "group-archive", "build", &["old".into(), "old".into()], &[]).is_err());
    }

    #[test]
    fn exact_group_archive_recovery_reports_corruption_instead_of_skipping_it() {
        let dir = tempfile::tempdir().unwrap();
        crate::config::private_io::write_private_file(&dir.path().join("group-corrupt.archive.jsonl"), b"{broken archive row}\n").unwrap();
        let rejected = archive_messages(dir.path(), "group-corrupt", &[receipt_fixture("wanted", "build", "Do not silently cross corrupt history")]);
        assert!(rejected.is_err(), "append must refuse corrupt history under its writer lock");
        let error = resolve_group_contributions(dir.path(), "group-corrupt", "build", &["wanted".into()], &[]).unwrap_err();
        assert!(error.to_string().contains("invalid group archive row 1"));
        // A complete live result needs no archive scan at all.
        assert!(resolve_group_contributions(dir.path(), "group-corrupt", "build", &["wanted".into()], &[receipt_fixture("wanted", "build", "live")]).is_ok());
    }

    #[test]
    fn new_anchor_summary_is_bounded_without_losing_the_newest_tail() {
        let huge = format!("BEGIN:{}:END", "x".repeat(ANCHOR_CAP_CHARS * 2));
        let bounded = bound_anchor_summary(huge);
        assert!(bounded.starts_with("BEGIN:"));
        assert!(bounded.ends_with(":END"));
        assert!(bounded.contains("anchor clipped"));
        assert!(bounded.chars().count() <= ANCHOR_CAP_CHARS + 120);
    }

    #[test]
    fn mechanical_compaction_requires_real_headroom() {
        assert!(!mechanical_savings_meaningful(100_000, 100_000));
        assert!(!mechanical_savings_meaningful(100_000, 97_000));
        assert!(!mechanical_savings_meaningful(1_000_000, 956_000));
        assert!(mechanical_savings_meaningful(100_000, 94_000));
        assert!(mechanical_savings_meaningful(1_000_000, 940_000));
    }

    fn user(text: &str) -> Message {
        Message::User {
            content: text.to_string(),
        }
    }

    fn assistant(text: &str) -> Message {
        Message::Assistant {
            content: text.to_string(),
        }
    }

    fn tool(output: &str) -> Message {
        Message::ToolResult {
            tool_name: "read".to_string(),
            input: "f.rs".to_string(),
            success: true,
            output: output.to_string(),
        }
    }

    fn named_tool(tool_name: &str, input: &str, output: &str) -> Message {
        Message::ToolResult {
            tool_name: tool_name.to_string(),
            input: input.to_string(),
            success: true,
            output: output.to_string(),
        }
    }

    fn failed_tool(tool_name: &str, input: &str, output: &str) -> Message {
        Message::ToolResult {
            tool_name: tool_name.to_string(),
            input: input.to_string(),
            success: false,
            output: output.to_string(),
        }
    }

    #[test]
    fn importance_trim_collapses_stale_routine_keeps_errors_findings_and_recent() {
        let big = "x".repeat(800);
        let messages = vec![
            user("the goal: ship the fix"),
            named_tool("read", "old.rs", &big), // stale routine success -> collapse
            failed_tool("bash", "cargo test", &format!("error[E0308] {big}")), // error -> keep
            named_tool(
                "read",
                "creds.rs",
                &format!("DB=postgres://u@h:5432/db {big}"),
            ), // literal -> keep
            named_tool("read", "recent3.rs", &big), // within recent 3 -> keep
            named_tool("read", "recent2.rs", &big), // recent -> keep
            named_tool("read", "recent1.rs", &big), // recent -> keep
        ];
        let trimmed = importance_trim(&messages);
        assert_eq!(trimmed.len(), messages.len(), "trim never drops messages");
        // The one stale routine read collapsed to a receipt.
        match &trimmed[1] {
            Message::ToolResult { output, .. } => {
                assert!(
                    output.contains("chars elided"),
                    "stale routine read collapses: {output}"
                );
            }
            other => panic!("expected tool result, got {other:?}"),
        }
        // Errors, exact-literals, the user message, and the 3 newest reads survive whole.
        assert!(
            matches!(&trimmed[0], Message::User { content } if content.contains("ship the fix"))
        );
        assert!(
            matches!(&trimmed[2], Message::ToolResult { output, .. } if output.contains("E0308"))
        );
        assert!(
            matches!(&trimmed[3], Message::ToolResult { output, .. } if output.contains("postgres://"))
        );
        assert!(matches!(&trimmed[6], Message::ToolResult { output, .. } if output.len() > 700));
    }

    #[test]
    fn importance_trim_keeps_small_routine_results_verbatim() {
        // Below the collapse floor there's nothing to save — keep it.
        let messages = vec![named_tool("read", "tiny.rs", "fn main() {}")];
        let trimmed = importance_trim(&messages);
        assert!(
            matches!(&trimmed[0], Message::ToolResult { output, .. } if output == "fn main() {}")
        );
    }

    #[tokio::test]
    async fn compaction_short_circuits_mechanically_without_a_model_call() {
        // Old half is dominated by stale routine reads that collapse mechanically;
        // Stage A alone gets under budget, so NO summary message is produced and no
        // model is called. A failed build in the old half must still survive verbatim.
        let provider: Arc<dyn LLMProvider> = Arc::new(EmptyProvider);
        let mut session = crate::session::Session::new_main("m", "s");
        let dump = "d".repeat(800);
        session.push_message(user("start the run"));
        for index in 0..12 {
            session.push_message(named_tool("read", &format!("file{index}.rs"), &dump));
        }
        session.push_message(failed_tool(
            "bash",
            "cargo build",
            "error[E0599]: no method `frobnicate`",
        ));
        // One big verbatim tail turn (~keep_chars) so split folds the reads into
        // the old half without itself spilling back into it.
        session.push_message(user("recent turn"));
        session.push_message(assistant(&"h".repeat(23_000)));
        let outcome = compact_session(&provider, "m", &mut session, 20_000, None)
            .await
            .expect("must compact");
        assert!(
            outcome.mechanical_only,
            "mechanical trim alone should suffice here"
        );
        assert!(!outcome.used_model, "no model call on the mechanical path");
        // No LLM summary message was inserted.
        assert!(
            !session.messages.iter().any(|m| matches!(m, Message::Assistant { content } if content.starts_with(CONTINUATION_MARKER))),
            "mechanical path must not insert a summary message"
        );
        // Stale reads collapsed, but the failed build survived verbatim.
        assert!(session.messages.iter().any(
            |m| matches!(m, Message::ToolResult { output, .. } if output.contains("chars elided"))
        ));
        assert!(session
            .messages
            .iter()
            .any(|m| matches!(m, Message::ToolResult { output, .. } if output.contains("E0599"))));
    }

    #[test]
    fn split_point_keeps_tail_and_snaps_to_turn_opener() {
        let big = "x".repeat(4_000);
        let mut messages = Vec::new();
        for _ in 0..6 {
            messages.push(user(&big));
            messages.push(assistant(&big));
            messages.push(tool(&big));
        }
        // Keep almost two turns; the opener fits the bounded snap budget.
        let split = split_point(&messages, 21_000);
        assert!(split > 0, "old half must fold");
        assert!(split < messages.len() - MIN_KEEP_TAIL + 1);
        // Boundary opens on a user/talk message, never a tool result.
        assert!(matches!(
            messages[split],
            Message::User { .. } | Message::Talk { .. } | Message::GroupContribution { .. }
        ));
    }

    #[test]
    fn split_point_refuses_tiny_transcripts() {
        let messages = vec![user("a"), assistant("b"), user("c")];
        assert_eq!(split_point(&messages, 10), 0);
    }

    #[test]
    fn split_point_does_not_keep_nearly_two_budgets_to_reach_an_opener() {
        let mut messages = vec![assistant("previous summary"), user("continue")];
        messages.extend((0..200).map(|_| assistant(&"x".repeat(4_000))));
        let keep_chars = 424_800; // 30% of Rory's 354k window, in characters
        let split = split_point(&messages, keep_chars);
        let tail_chars: usize = messages[split..].iter().map(message_chars).sum();
        assert!(split > 50, "a fold must free useful space, not just the summary");
        assert!(tail_chars <= keep_chars + keep_chars / 4);
    }

    #[test]
    fn split_point_includes_the_opener_in_its_extra_budget() {
        let mut messages = vec![assistant("previous summary"), user(&"u".repeat(80_000))];
        messages.extend((0..10).map(|_| assistant(&"x".repeat(4_000))));
        let split = split_point(&messages, 42_000);
        assert_eq!(split, 2, "a huge opener must not sneak into the bounded tail");
    }

    #[tokio::test]
    async fn mechanical_compaction_leaves_room_below_the_next_fold_floor() {
        let provider: Arc<dyn LLMProvider> = Arc::new(EmptyProvider);
        let window = 32_000;
        let keep_chars = (keep_tokens_for_window(window) * CHARS_PER_TOKEN) as usize;
        let mut session = Session::new_main("m", "mechanical-headroom");
        session.push_message(user("Finish the current task and preserve my corrections."));
        for _ in 0..70 {
            session.push_message(assistant(&"important context ".repeat(40)));
        }
        session.push_message(named_tool("read", "old dump", &"old output ".repeat(20_000)));
        for _ in 0..12 {
            session.push_message(assistant(&"recent evidence ".repeat(200)));
        }
        let outcome = compact_session(&provider, "m", &mut session, window, None)
            .await.expect("large transcript must fold");
        assert!(outcome.after_chars < keep_chars + keep_chars / 2,
            "post-fold context must stay below the repeated-fold floor: {}", outcome.after_chars);
        assert!(!should_compact(trigger_tokens(window) + 20_000,
            session_estimated_tokens(&session), window), "fixed overhead must not cause another fold");
    }

    #[test]
    fn split_point_folds_a_long_autonomous_tool_run_with_no_opener() {
        // The old browser/coder busy-loop shape: ONE opener, then a long run of tool
        // calls with no User/Talk between them. Snapping back to that lone
        // opener would pin the fold and let the tail grow without bound
        // ("~82k → ~82k, folded=109" forever). The char-bounded snap must
        // instead fold at the budget boundary, keeping the tail near budget.
        let big = "x".repeat(4_000);
        let keep_chars = 12_500;
        let mut messages = vec![user("kick off the browser mission")];
        for _ in 0..200 {
            messages.push(tool(&big)); // hundreds of tool results, no opener
        }
        let split = split_point(&messages, keep_chars);
        // The kept tail must stay within ~2× the keep budget — NOT balloon back
        // to the lone opener at index 0 (which would keep ~everything).
        let tail_chars: usize = messages[split..].iter().map(message_chars).sum();
        assert!(
            tail_chars <= keep_chars * 2,
            "tail must stay bounded ({tail_chars} chars) — the fold must make progress"
        );
        assert!(
            split > 1,
            "must actually fold the long tool run, not pin at the opener"
        );
    }

    #[tokio::test]
    async fn compaction_folds_a_small_message_count_with_one_huge_prefix() {
        let provider: Arc<dyn LLMProvider> = Arc::new(EmptyProvider);
        let mut session = crate::session::Session::new_main("m", "huge-connected-app-result");
        session.push_message(user("Prepare tomorrow without reopening completed work."));
        session.push_message(named_tool(
            "composio_run",
            "NOTION_QUERY_DATABASE_WITH_FILTER",
            &"large connected-app response ".repeat(30_000),
        ));
        for index in 0..3 {
            session.push_message(assistant(&format!("observed {index}")));
            session.push_message(user(&format!("continue {index}")));
        }
        assert_eq!(session.messages.len(), MIN_MESSAGES_TO_COMPACT);

        let outcome = compact_session(&provider, "m", &mut session, 8_000, None)
            .await
            .expect("one huge early result must not make compaction return None");

        assert!(outcome.folded_messages > 0);
        assert!(outcome.after_chars < outcome.before_chars);
    }

    #[test]
    fn should_compact_does_not_loop_on_overhead_bound_requests() {
        // Durable session already small (folded), but the REQUEST is over the
        // trigger because of fixed overhead (system prompt, tools, injected
        // history). Folding can't shrink that — so it must NOT re-fire (the
        // "~63k → ~63k" loop). Real durable growth still folds.
        let window = 500_000;
        let trigger = trigger_tokens(window);
        let small_session = keep_max_tokens(); // already at the keep target
        assert!(
            !should_compact(trigger + 20_000, small_session, window),
            "overhead-bound request with a small durable session must not loop"
        );
        // A big foldable session over the request threshold still compacts.
        assert!(should_compact(trigger + 20_000, trigger - 5_000, window));
        // And durable bloat past the trigger always compacts.
        assert!(should_compact(0, trigger + 1, window));
    }

    #[test]
    fn receipt_digest_is_one_line_per_message() {
        let messages = vec![
            user("fix the bug\nwith details"),
            assistant("done it"),
            tool("file contents here"),
        ];
        let digest = receipt_digest(&messages);
        assert_eq!(digest.lines().count(), 3);
        assert!(digest.contains("user: fix the bug"));
        assert!(digest.contains("tool read(f.rs) ok"));
    }

    #[test]
    fn receipt_digest_preserves_corrections_talk_bodies_and_tool_substance() {
        let messages = vec![
            user("Correction:\nThe browser login source is `zen`, not Chrome."),
            Message::Talk {
                from: "researcher".to_string(),
                to: "orchestrator".to_string(),
                subject: "Reddit login evidence".to_string(),
                body: "Researcher found the page showed user `Osprey6767`; orchestrator must attribute this to researcher.".to_string(),
                reply_expected: true,
                handoff_id: String::new(),
                reply_to: None,
                causation_id: None,
                status: String::new(),
            },
            named_tool(
                "web_fetch",
                "https://example.com/session",
                "HTTP 200. Visible evidence: signed in as Osprey6767.",
            ),
        ];
        let digest = receipt_digest(&messages);
        assert_eq!(digest.lines().count(), 3);
        assert!(digest.contains("Correction: The browser login source is `zen`, not Chrome."));
        assert!(digest.contains("Researcher found the page showed user `Osprey6767`"));
        assert!(digest.contains("tool web_fetch(https://example.com/session) ok HTTP 200"));
    }

    #[test]
    fn recent_corrections_get_space_before_old_directives() {
        let mut messages = (0..40).map(|i| user(&format!("Always remember every old plan {i} {}", "x".repeat(700)))).collect::<Vec<_>>();
        for i in 0..16 {
            messages.push(user(&format!("Current correction {i}: {}", "y".repeat(700))));
        }
        let lines = user_intent_lines(&messages);
        for i in 0..16 {
            assert!(lines.iter().any(|line| line.contains(&format!("Current correction {i}:"))));
        }
        assert!(lines.iter().map(|line|line.chars().count()).sum::<usize>() <= USER_INTENT_MAX_CHARS);
    }

    #[test]
    fn repeated_directive_is_promoted_across_folds() {
        let mut previous = vec!["- user: Keep the current objective".into()];
        previous.extend((0..23).map(|i| format!("- user: Old detail {i}")));
        let merged = merge_user_intent(previous, &[user("Keep the current objective"), user("New correction")]);
        assert_eq!(merged[merged.len()-2], "- user: Keep the current objective");
        assert_eq!(merged.last().unwrap(), "- user: New correction");
        assert_eq!(merged.iter().filter(|line|line.contains("Keep the current objective")).count(), 1);
    }

    #[test]
    fn deterministic_user_intent_keeps_a_mid_history_vacation_plan() {
        let mut messages = Vec::new();
        for index in 0..40 {
            messages.push(user(&format!("ordinary update {index}")));
            if index == 18 {
                messages.push(user(
                    "Prepare every assessment before my September 11 vacation so the vacation is school-free; do not narrow this to one subject.",
                ));
            }
            messages.push(assistant("acknowledged"));
        }

        let lines = user_intent_lines(&messages);
        let rendered = render_user_intent(&lines);
        assert!(rendered.contains("Prepare every assessment before my September 11 vacation"));
        assert!(rendered.contains("do not narrow this to one subject"));
    }

    #[test]
    fn deterministic_user_intent_survives_anchor_parsing_and_merge() {
        let prior = vec!["- user: Keep the September 11 vacation school-free.".to_string()];
        let merged = merge_user_intent(
            prior,
            &[user(
                "Tomorrow means the user's local calendar day, never UTC.",
            )],
        );
        let rendered = render_user_intent(&merged);
        let (_, parsed) = rendered.split_once(USER_INTENT_HEADER).unwrap();
        let recovered = parsed
            .lines()
            .filter(|line| line.starts_with("- user: "))
            .collect::<Vec<_>>();

        assert_eq!(recovered.len(), 2);
        assert!(rendered.contains("September 11 vacation school-free"));
        assert!(rendered.contains("local calendar day, never UTC"));
    }

    #[test]
    fn corrupt_archive_is_not_silently_presented_as_complete_history() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::new_main_with_id("archive-health", "m", "system");
        session.push_message(user("I already finished lesson 4."));
        let path = dir.path().join("archive-health.archive.jsonl");
        let valid = serde_json::to_string(&user("I already finished lesson 3." )).unwrap() + "\n";
        std::fs::write(&path, format!("{valid}broken-json\n")).unwrap();
        assert!(archived_messages(dir.path(), &session.id).is_err());
        let context = durable_user_context(&session, Some(dir.path()));
        assert!(context.contains("HISTORY INCOMPLETE"));
        assert!(context.contains("I already finished lesson 4."));
        assert!(!context.contains("I already finished lesson 3."), "partial archive must not look complete");
        std::fs::write(&path, valid).unwrap();
        let recovered = durable_user_context(&session, Some(dir.path()));
        assert!(!recovered.contains("HISTORY INCOMPLETE"));
        assert!(recovered.contains("I already finished lesson 3."));
        assert!(recovered.contains("I already finished lesson 4."));
        let missing = Session::new_main_with_id("fresh-no-archive", "m", "system");
        assert!(!durable_user_context(&missing, Some(dir.path())).contains("HISTORY INCOMPLETE"));
        let mut compacted = missing;
        compacted.push_message(assistant("[AUTO-COMPACTED HISTORY] previous summary"));
        assert!(durable_user_context(&compacted, Some(dir.path())).contains("HISTORY INCOMPLETE"));
    }

    #[tokio::test]
    async fn corrupt_archive_prevents_another_fold_without_changing_either_record() {
        let provider: Arc<dyn LLMProvider> = Arc::new(EmptyProvider);
        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::new_main_with_id("corrupt-fold", "m", "system");
        for _ in 0..10 { session.push_message(user(&"history ".repeat(2_000))); }
        let path = dir.path().join("corrupt-fold.archive.jsonl");
        std::fs::write(&path, b"broken-json\n").unwrap();
        let before = serde_json::to_string(&session.messages).unwrap();
        assert!(compact_session(&provider, "m", &mut session, 8_000, Some(dir.path())).await.is_none());
        assert_eq!(serde_json::to_string(&session.messages).unwrap(), before);
        assert_eq!(std::fs::read(&path).unwrap(), b"broken-json\n");
    }

    #[test]
    fn long_completion_report_keeps_separate_standing_instructions() {
        let mut session = Session::new_main_with_id("mixed-intent".to_string(), "m", "system");
        let instruction = "Always preserve the accepted baseline at artifacts/preferred-v2.png. Do not publish the private draft.";
        session.push_message(user(&format!("{instruction}\n\n{}\n\nI finished the source review; do not repeat it.", "Additional background. ".repeat(80))));
        let context = durable_user_context(&session, None);
        assert!(context.contains("I finished the source review"));
        assert!(context.contains(instruction), "a completion excerpt must not replace unrelated standing instructions");
    }

    #[tokio::test]
    async fn mixed_instructions_survive_two_folds_and_disk_restarts() {
        let provider: Arc<dyn LLMProvider> = Arc::new(EmptyProvider);
        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::new_main_with_id("mixed-restart".to_string(), "m", "system");
        let instruction = "Always preserve artifacts/preferred-v2.png as the accepted baseline. Do not publish the private draft.";
        let original = format!("{instruction}\n\n{}\n\nI finished the source review; do not repeat it.", "Additional background. ".repeat(80));
        session.push_message(user(&original));
        for round in 0..2 {
            for index in 0..20 {
                session.push_message(user(&format!("Continue step {round}-{index}.")));
                session.push_message(assistant(&"Working context. ".repeat(400)));
            }
            let outcome = compact_session(&provider, "m", &mut session, 8_000, Some(dir.path())).await.unwrap();
            assert!(outcome.after_chars < outcome.before_chars / 2, "fold must make useful room");
            assert!(!outcome.used_model, "exercise the provider-unavailable fallback");
            let id = session.id.clone();
            let mut store = crate::session::SessionStore::new(dir.path());
            store.upsert(session);
            store.save_one(&id).unwrap();
            let mut reopened = crate::session::SessionStore::new(dir.path());
            reopened.load_from_disk().unwrap();
            session = reopened.get(&id).unwrap().clone();
            let context = durable_user_context(&session, Some(dir.path()));
            assert!(context.contains(instruction), "round {round}: baseline and publication constraint survive");
            assert!(context.contains("I finished the source review"), "round {round}: completed work survives");
            assert!(archived_messages(dir.path(), &id).unwrap().iter().any(|message| matches!(message, Message::User { content } if content == &original)), "exact original remains recoverable");
        }
    }

    #[test]
    fn deterministic_user_context_keeps_completion_amid_many_planning_requests() {
        let mut messages = Vec::new();
        for index in 0..90 {
            messages.push(user(&format!(
                "Remember: plan every subject for tomorrow and keep everything before vacation, update {index}."
            )));
            if index == 21 {
                messages.push(user(
                    "I finished Test Drive 1.3. It is complete and must not be put back into the plan.",
                ));
            }
        }

        let mut session = crate::session::Session::new_main("m", "state-ledger-test");
        session.messages = messages;
        let rendered = durable_user_context(&session, None);

        assert!(rendered.contains("I finished Test Drive 1.3"), "{rendered}");
        assert!(rendered.contains("authoritative state"));
    }

    #[test]
    fn explicit_completion_survives_repeated_worker_planning_pressure() {
        let mut session = Session::new_main("m", "state-pressure");
        session.push_message(user("I have completed Science 9 Lessons 3 and 4 and the Unit A Section 1 assessment."));
        for index in 0..80 {
            session.push_message(Message::ToolResult { tool_name: "volume_work".into(), input: "{}".into(),
                success: true, output: format!("gradebook poll {index}: Lesson 3 is accessible; prepare the next lesson.") });
        }
        let context = authoritative_state_context(&session, None);
        assert!(context.contains("I have completed Science 9 Lessons 3 and 4"), "worker polling must not evict the user's completion correction");
        assert!(context.contains("gradebook poll 79"), "retain recent source context too");
        session.push_message(user("Correction: Lesson 4 is not finished. I want to reopen it."));
        let context = authoritative_state_context(&session, None);
        assert!(context.find("Lesson 4 is not finished").unwrap() < context.find("I have completed Science").unwrap(),
            "a newer explicit reopening must precede the older completion");
        assert!(state_evidence_lines(&session.messages).iter().map(|s| s.chars().count()).sum::<usize>() <= STATE_EVIDENCE_MAX_CHARS);
    }

    #[test]
    fn recalled_summaries_and_search_plans_are_not_promoted_to_verified_state() {
        let mut session = Session::new_main("m", "provenance");
        session.push_message(user("I already finished Science Lessons 3 and 4."));
        session.push_message(assistant("[AUTO-COMPACTED HISTORY] gradebook: OLD_SUMMARY"));
        for name in ["recall", "composio_search"] {
            session.push_message(Message::ToolResult { tool_name: name.into(), input: "{}".into(),
                success: true, output: "marked complete: SEARCH_OR_RECALL_TEXT".into() });
        }
        session.push_message(Message::ToolResult { tool_name: "volume_work".into(), input: "{}".into(),
            success: true, output: "gradebook: Section 1 is graded; Lesson 3 is accessible, so study it next.".into() });
        let context = authoritative_state_context(&session, None);
        assert!(!context.contains("OLD_SUMMARY"));
        assert!(!context.contains("SEARCH_OR_RECALL_TEXT"));
        assert!(context.contains("I already finished Science Lessons 3 and 4."));
        assert!(context.contains("tool-reported state; may include interpretation"));
        assert!(!context.contains("[verified tool evidence]"));
        assert!(context.contains("do not infer readiness merely from accessibility"));
    }

    #[test]
    #[ignore = "read-only replay of explicitly selected saved continuity"]
    fn saved_school_continuity_preserves_explicit_completion_correction() {
        let root = std::path::PathBuf::from(std::env::var("PHOENIX_CONTINUITY_ROOT").unwrap());
        let id = std::env::var("PHOENIX_CONTINUITY_SESSION").unwrap();
        let session = crate::session::SessionStore::read_one_from_disk(&root, &id).unwrap().unwrap();
        let context = durable_user_context(&session, Some(&root));
        assert!(!context.contains("HISTORY INCOMPLETE"));
        assert!(context.contains("I have completed Science 9 Lessons 3 and 4 and the Unit A Section 1 assessment"),
            "the explicit user correction must survive saved-history context selection");
        println!("SAVED_CONTINUITY: archive readable; exact science completion correction retained; context_chars={}", context.chars().count());
    }

    #[test]
    fn authoritative_state_keeps_user_and_verified_completion_but_rejects_stale_availability() {
        let mut session = crate::session::Session::new_main("m", "school-state-test");
        session.push_message(user(
            "I already finished Science Lessons 3 and 4; do not schedule them again.",
        ));
        session.push_message(assistant(
            "Verified complete: the Section 1 Assignment is graded at 80.95%. Next Science is Section 2.",
        ));
        session.push_message(Message::ToolResult {
            tool_name: "browser_state".into(),
            input: "{}".into(),
            success: true,
            output: "Lesson 3 is available and the old planner checkbox is unchecked.".into(),
        });

        let state = authoritative_state_context(&session, None);
        assert!(state.contains("I already finished Science Lessons 3 and 4"));
        assert!(state.contains("Section 1 Assignment is graded at 80.95%"));
        assert!(state.contains("Next Science is Section 2"));
        assert!(!state.contains("old planner checkbox"), "{state}");
        assert!(state.find("- [assistant report; verify against source]").unwrap() < state.find("- [user]").unwrap());
        assert!(state.contains("available/unlocked link or unchecked planning row is not proof"));
    }

    #[test]
    fn estimated_tokens_is_chars_over_four() {
        assert_eq!(estimated_tokens(4_000), 1_001);
    }

    #[tokio::test]
    async fn compact_session_folds_old_half_with_receipt_fallback() {
        // Scaffold provider returns canned text; for this test what matters is
        // that compaction REPLACES the old half and keeps the tail verbatim.
        let provider: Arc<dyn LLMProvider> = Arc::new(crate::providers::scaffold::ScaffoldProvider);
        let mut session = crate::session::Session::new_main("m", "s");
        let big = "y".repeat(8_000);
        for index in 0..10 {
            session.push_message(user(&format!("request {index} {big}")));
            session.push_message(assistant(&format!("answer {index} {big}")));
        }
        let tail_marker = "request 9";
        let outcome = compact_session(&provider, "m", &mut session, 8_000, None)
            .await
            .expect("large transcript must compact");
        assert!(outcome.folded_messages >= 2);
        assert!(outcome.after_chars < outcome.before_chars);
        // First message is the continuation summary.
        match &session.messages[0] {
            Message::Assistant { content } => {
                assert!(content.contains("AUTO-COMPACTED HISTORY"));
                assert!(content.contains("lossy navigation index"));
                assert!(content.contains("current user correction wins"));
                assert!(content.contains("historical observations only"));
                assert!(content.contains("call the state unknown"));
            }
            other => panic!("expected continuation summary first, got {other:?}"),
        }
        // The newest exchange survived verbatim.
        let rendered: String = session
            .messages
            .iter()
            .map(message_chars)
            .map(|c| c.to_string())
            .collect();
        let _ = rendered;
        assert!(session.messages.iter().any(|m| matches!(
            m,
            Message::User { content } if content.contains(tail_marker)
        )));
    }

    /// Returns empty content, forcing compaction's deterministic fallback —
    /// which is where anchor preservation must hold even without a live model.
    struct EmptyProvider;
    #[async_trait::async_trait]
    impl LLMProvider for EmptyProvider {
        fn name(&self) -> &str {
            "empty"
        }
        fn display_name(&self) -> &str {
            "Empty"
        }
        fn base_url(&self) -> &str {
            "mock://empty"
        }
        fn auth_type(&self) -> crate::providers::contracts::AuthType {
            crate::providers::contracts::AuthType::None
        }
        fn env_vars(&self) -> Vec<&str> {
            vec![]
        }
        fn default_headers(&self) -> std::collections::HashMap<String, String> {
            std::collections::HashMap::new()
        }
        fn has_model(&self, _model: &str) -> bool {
            true
        }
        fn default_model(&self) -> &str {
            "m"
        }
        fn fallback_models(&self) -> Vec<&str> {
            vec![]
        }
        async fn complete(
            &self,
            request: CompletionRequest,
        ) -> Result<crate::providers::CompletionResponse> {
            Ok(crate::providers::CompletionResponse {
                content: String::new(),
                model: request.model,
                usage: crate::providers::TokenUsage::new(1, 0),
                reasoning: None,
                stop_reason: Some("stop".to_string()),
                tool_calls: vec![],
                provider_replay: None,
            })
        }
        async fn health_check(&self) -> Result<bool> {
            Ok(true)
        }
        async fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Result<crate::providers::StreamingResponse> {
            unimplemented!()
        }
        async fn embeddings(&self, _texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
            unimplemented!()
        }
        async fn list_models(&self) -> Result<Vec<crate::providers::ModelInfo>> {
            Ok(vec![])
        }
    }

    #[tokio::test]
    async fn receipt_fallback_keeps_completed_work_out_of_a_later_plan() {
        let provider: Arc<dyn LLMProvider> = Arc::new(EmptyProvider);
        let dir = tempfile::tempdir().unwrap();
        let mut session = crate::session::Session::new_main("m", "school-state-fallback");
        let bulk = "course detail ".repeat(700);
        session.push_message(user("Plan the school week."));
        session.push_message(assistant(&bulk));
        session.push_message(user(
            "I finished Test Drive 1.3. Preserve it as completed and do not schedule it again.",
        ));
        session.push_message(assistant(&bulk));
        for index in 0..12 {
            session.push_message(user(&format!("ordinary planning update {index}")));
            session.push_message(assistant(&bulk));
        }

        let outcome = compact_session(&provider, "m", &mut session, 8_000, Some(dir.path()))
            .await
            .expect("large school transcript must compact");
        assert!(
            !outcome.used_model,
            "EmptyProvider must exercise receipt fallback"
        );
        let Message::Assistant { content } = &session.messages[0] else {
            panic!("expected compacted continuation");
        };
        assert!(content.contains("I finished Test Drive 1.3"), "{content}");
        assert!(content.contains("never reopen completed work"), "{content}");
        assert!(
            content.contains("historical observations only"),
            "{content}"
        );
        assert!(
            SUMMARIZER_SYSTEM.contains("LAST-OBSERVED facts only"),
            "summarizer must not turn an old Moodle/Notion status into current truth"
        );
    }

    #[tokio::test]
    async fn anchored_compaction_preserves_a_prior_summary() {
        // An already-compacted session: the oldest message is a prior summary
        // carrying an exact literal that must NOT be lost on the next fold.
        let provider: Arc<dyn LLMProvider> = Arc::new(EmptyProvider);
        let mut session = crate::session::Session::new_main("m", "s");
        let prior_fact = "DB_DSN=postgres://ro@host:5433/exp";
        session.push_message(assistant(&format!(
            "[AUTO-COMPACTED HISTORY — 12 earlier messages folded.]\n\nExact literals:\n- `{prior_fact}`"
        )));
        let big = "z".repeat(8_000);
        for index in 0..10 {
            session.push_message(user(&format!("turn {index} {big}")));
            session.push_message(assistant(&format!("reply {index} {big}")));
        }
        compact_session(&provider, "m", &mut session, 8_000, None)
            .await
            .expect("must compact");
        let head = match &session.messages[0] {
            Message::Assistant { content } => content.clone(),
            other => panic!("expected continuation first, got {other:?}"),
        };
        assert!(head.starts_with(CONTINUATION_MARKER));
        assert!(
            head.contains(prior_fact),
            "the prior summary's exact literal survives the re-fold: {head}"
        );
    }

    #[test]
    fn ledger_merges_dedupes_and_caps_fifo() {
        // Fill to the cap so the merge overflows and must evict oldest-first.
        let previous: Vec<String> = (0..LEDGER_MAX_ENTRIES)
            .map(|i| format!("- read(f{i}.rs) ok — x"))
            .collect();
        let new = vec![
            "- read(f0.rs) ok — x".to_string(), // duplicate of an old entry
            "- grep(needle src/) ok — 0 matches".to_string(),
            "- bash(cargo test) FAIL — error[E0308]".to_string(),
            "- read(new1.rs) ok — y".to_string(),
            "- read(new2.rs) ok — y".to_string(),
        ];
        let merged = merge_ledger(previous, new);
        assert_eq!(merged.len(), LEDGER_MAX_ENTRIES);
        // Oldest evicted first; newest entries all present.
        assert!(!merged.iter().any(|l| l.contains("f0.rs")));
        assert!(merged.iter().any(|l| l.contains("0 matches")));
        assert!(merged.iter().any(|l| l.contains("E0308")));
        assert!(merged.last().unwrap().contains("new2.rs"));
        // Exact duplicates collapse.
        assert_eq!(
            merged.iter().filter(|l| l.contains("grep(needle")).count(),
            1
        );
    }

    #[tokio::test]
    async fn continuation_carries_tool_ledger_and_recall_pointer() {
        let provider: Arc<dyn LLMProvider> = Arc::new(EmptyProvider);
        let mut session = crate::session::Session::new_main("m", "s");
        let big = "b".repeat(8_000);
        session.push_message(user(&format!("find the config loader {big}")));
        session.push_message(named_tool(
            "grep",
            "config_loader src/",
            "src/config/loader.rs:88: pub fn config_loader()",
        ));
        session.push_message(failed_tool("bash", "cargo build", "error[E0599]: nope"));
        for index in 0..8 {
            session.push_message(user(&format!("turn {index} {big}")));
            session.push_message(assistant(&format!("reply {index} {big}")));
        }
        compact_session(&provider, "m", &mut session, 8_000, None)
            .await
            .expect("must compact");
        let head = match &session.messages[0] {
            Message::Assistant { content } => content.clone(),
            other => panic!("expected continuation first, got {other:?}"),
        };
        // The fold advertises relocation-not-deletion…
        assert!(
            head.contains("`recall`"),
            "continuation must point at recall: {head}"
        );
        // …and pins the deterministic tool ledger with verdicts.
        assert!(head.contains(LEDGER_HEADER));
        assert!(head.contains("- grep(config_loader src/) ok"));
        assert!(head.contains("- bash(cargo build) FAIL"));
    }

    #[tokio::test]
    async fn ledger_survives_a_second_fold() {
        let provider: Arc<dyn LLMProvider> = Arc::new(EmptyProvider);
        let mut session = crate::session::Session::new_main("m", "s");
        let big = "c".repeat(8_000);
        session.push_message(user("start"));
        session.push_message(named_tool(
            "grep",
            "first_probe src/",
            "src/a.rs:1: first_probe hit",
        ));
        for index in 0..8 {
            session.push_message(user(&format!("t{index} {big}")));
            session.push_message(assistant(&format!("r{index} {big}")));
        }
        compact_session(&provider, "m", &mut session, 8_000, None)
            .await
            .expect("first fold");
        // Grow again past the trigger and fold a second time.
        session.push_message(named_tool(
            "read",
            "second_probe.rs",
            "contents of second probe",
        ));
        for index in 0..8 {
            session.push_message(user(&format!("u{index} {big}")));
            session.push_message(assistant(&format!("s{index} {big}")));
        }
        compact_session(&provider, "m", &mut session, 8_000, None)
            .await
            .expect("second fold");
        let head = match &session.messages[0] {
            Message::Assistant { content } => content.clone(),
            other => panic!("expected continuation first, got {other:?}"),
        };
        // Exactly one ledger block, carrying entries from BOTH folds.
        assert_eq!(head.matches(LEDGER_HEADER).count(), 1);
        assert!(
            head.contains("- grep(first_probe src/) ok"),
            "first fold's ledger entry survives the second fold: {head}"
        );
        assert!(head.contains("- read(second_probe.rs) ok"));
    }

    #[tokio::test]
    async fn compact_session_archives_folded_messages() {
        let provider: Arc<dyn LLMProvider> = Arc::new(crate::providers::scaffold::ScaffoldProvider);
        let dir = tempfile::tempdir().unwrap();
        let mut session = crate::session::Session::new_main("m", "s");
        let big = "z".repeat(8_000);
        for index in 0..10 {
            session.push_message(user(&format!("u{index} {big}")));
            session.push_message(assistant(&format!("a{index} {big}")));
        }
        let session_id = session.id.clone();
        compact_session(&provider, "m", &mut session, 8_000, Some(dir.path()))
            .await
            .expect("must compact");
        let archive = dir.path().join(format!("{session_id}.archive.jsonl"));
        let content = std::fs::read_to_string(archive).expect("archive written");
        assert!(content.lines().count() >= 2);
        assert!(content.contains("u0"));
    }

    #[tokio::test]
    async fn archive_failure_preserves_the_live_transcript() {
        let provider: Arc<dyn LLMProvider> = Arc::new(EmptyProvider);
        let dir = tempfile::tempdir().unwrap();
        let blocked_archive_dir = dir.path().join("not-a-directory");
        std::fs::write(&blocked_archive_dir, "occupied by a file").unwrap();
        let mut session = crate::session::Session::new_main("m", "s");
        let big = "z".repeat(8_000);
        for index in 0..10 {
            session.push_message(user(&format!("u{index} {big}")));
            session.push_message(assistant(&format!("a{index} {big}")));
        }
        let before = serde_json::to_string(&session.messages).unwrap();

        let outcome = compact_session(
            &provider,
            "m",
            &mut session,
            8_000,
            Some(&blocked_archive_dir),
        )
        .await;

        assert!(outcome.is_none(), "a lossy fold requires a durable archive");
        assert_eq!(serde_json::to_string(&session.messages).unwrap(), before);
    }

    #[tokio::test]
    async fn compacted_receipt_fallback_survives_save_and_reopen_with_corrections() {
        let provider: Arc<dyn LLMProvider> = Arc::new(EmptyProvider);
        let dir = tempfile::tempdir().unwrap();
        let mut store = crate::session::SessionStore::new(dir.path());
        let mut session = crate::session::Session::new_main_with_id(
            "reopen-compaction-test".to_string(),
            "m",
            "system",
        );

        session.push_message(user(
            "Correction:\nThe browser login source is `zen`, not Chrome.",
        ));
        session.push_message(Message::Talk {
            from: "researcher".to_string(),
            to: "orchestrator".to_string(),
            subject: "Reddit login evidence".to_string(),
            body: "Researcher found the page showed user `Osprey6767`; final answers must attribute this to researcher.".to_string(),
            reply_expected: true,
            handoff_id: String::new(),
            reply_to: None,
            causation_id: None,
            status: String::new(),
        });
        session.push_message(named_tool(
            "web_fetch",
            "https://reddit.com/",
            "HTTP 200. Visible evidence: signed in as Osprey6767.",
        ));

        let big = "context filler ".repeat(700);
        for index in 0..12 {
            session.push_message(user(&format!("older request {index}: {big}")));
            session.push_message(assistant(&format!("older answer {index}: {big}")));
        }

        let session_id = session.id.clone();
        compact_session(&provider, "m", &mut session, 8_000, Some(dir.path()))
            .await
            .expect("large transcript must compact");
        store.upsert(session);
        store.save_one(&session_id).unwrap();

        let mut reopened = crate::session::SessionStore::new(dir.path());
        reopened.load_from_disk().unwrap();
        let reopened_session = reopened.get(&session_id).expect("session reopens");
        let head = match &reopened_session.messages[0] {
            Message::Assistant { content } => content,
            other => panic!("expected continuation summary first, got {other:?}"),
        };

        assert!(head.contains("AUTO-COMPACTED HISTORY"));
        assert!(head.contains("Correction: The browser login source is `zen`, not Chrome."));
        assert!(head.contains("Researcher found the page showed user `Osprey6767`"));
        assert!(head.contains("Visible evidence: signed in as Osprey6767"));

        let archive = dir.path().join(format!("{session_id}.archive.jsonl"));
        let archive_content = std::fs::read_to_string(archive).expect("archive written");
        assert!(
            archive_content.contains("login source is `zen`"),
            "archive keeps raw folded correction"
        );
    }

    /// The 2026-07-11 outage shape: render-time compression bounds the
    /// outbound prompt, so a prompt-only trigger NEVER fires while the
    /// durable session grows without bound (multi-MB session files, "~0 tok
    /// compressed" on every turn). The durable transcript alone must be able
    /// to demand compaction.
    #[test]
    fn bloated_durable_session_triggers_compaction_despite_small_request() {
        let window = 1_000_000; // GLM-5.2 cloud
        let trigger = trigger_tokens(window);
        // Absolute cap wins over the 0.60 fraction on huge windows (0.60 × 1M
        // would be 600k — deep in the rot zone and never reached in practice).
        assert_eq!(trigger, trigger_max_tokens());

        let mut session = crate::session::Session::new_main("m", "s");
        // ~2.6M chars of history ≈ 650K estimated tokens — past the trigger.
        let chunk = "x".repeat(65_000);
        for _ in 0..40 {
            session.push_message(user(&chunk));
        }
        let session_tokens = session_estimated_tokens(&session);
        assert!(session_tokens > trigger, "test setup must exceed trigger");

        // The rendered request stays small (compression bounded it).
        let request_tokens = trigger - 1_000;
        assert!(
            should_compact(request_tokens, session_tokens, window),
            "durable bloat alone must demand compaction"
        );
        // The old prompt-only semantics would have skipped it — pin that too.
        assert!(
            request_tokens < trigger,
            "request alone stays under trigger (the regressed check)"
        );
        // And a genuinely small session with a small request stays untouched
        // (both under the absolute trigger cap).
        assert!(!should_compact(trigger - 2, trigger - 1, window));
    }

    #[test]
    fn idle_cache_compaction_runs_only_before_first_round() {
        // Actual failure: 126k -> 120k, then a second fold after two tools,
        // despite remaining far below the 222k normal trigger.
        assert!(should_compact_on_cache_miss_at_round(0, 126_000, 1059, 300, 262_144));
        for round in 1..10 {
            assert!(!should_compact_on_cache_miss_at_round(round, 124_000, 1059, 300, 262_144));
        }
        assert!(!should_compact_on_cache_miss_at_round(0, 126_000, 1059, 0, 262_144));
        assert!(should_compact(230_000, 230_000, 262_144));
    }

    #[test]
    fn cache_miss_folds_a_big_idle_context_below_the_normal_trigger() {
        // Just under the token trigger, so should_compact is FALSE…
        let window = 1_000_000;
        let ttl = DEFAULT_CACHE_TTL_SECS;
        let ctx = trigger_tokens(window) - 1;
        assert!(
            ctx >= cache_miss_floor_tokens(window),
            "floor must sit below the trigger"
        );
        assert!(!should_compact(ctx, ctx, window));
        // …but after the cache TTL, a re-read of it is a full-price miss, so we
        // fold it anyway.
        assert!(should_compact_on_cache_miss(ctx, ttl + 10, ttl, window));
    }

    #[test]
    fn cache_miss_does_not_fold_warm_or_tiny_contexts() {
        let ttl = DEFAULT_CACHE_TTL_SECS;
        let window = 1_000_000;
        // Still warm (idle < TTL): never fold on cache-miss grounds.
        assert!(!should_compact_on_cache_miss(200_000, ttl - 1, ttl, window));
        // Idle but tiny: the re-read is cheap, folding wastes a summarizer call.
        assert!(!should_compact_on_cache_miss(
            cache_miss_floor_tokens(window) - 1,
            ttl + 600,
            ttl,
            window
        ));
    }

    #[test]
    fn a_longer_cache_ttl_defers_the_fold() {
        // A context big enough to be worth folding (above the cache-miss floor,
        // which tracks the keep ceiling so a fold actually reduces something).
        let window = 1_000_000;
        let big = cache_miss_floor_tokens(window) + 10_000;
        // Same 6-minute idle gap that would fold on a 5-min cache…
        assert!(should_compact_on_cache_miss(big, 360, 300, window));
        // …does NOT fold on a lane whose cache lives an hour — the cache is still
        // warm, so folding would waste a summarizer call for no cache-price win.
        assert!(!should_compact_on_cache_miss(big, 360, 3_600, window));
    }

    #[test]
    fn two_hundred_k_lane_uses_its_real_keep_target_for_folding() {
        let window = 200_000;
        assert_eq!(keep_tokens_for_window(window), 60_000);
        assert_eq!(cache_miss_floor_tokens(window), 90_000);
        assert!(should_compact(180_000, 100_000, window));
        assert!(should_compact_on_cache_miss(
            100_000,
            DEFAULT_CACHE_TTL_SECS + 1,
            DEFAULT_CACHE_TTL_SECS,
            window
        ));
    }
}
