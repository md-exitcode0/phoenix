//! Free helpers for the mesh turn-runner: tool timeouts, transient-error
//! classification, file-watch bookkeeping, and the
//! small formatting utilities the turn loop leans on.

use std::io::Read;

use super::*;

pub(super) const MAX_TOOL_CALLS_PER_RESPONSE: usize = 16;

/// Previously captured desktop pixels cannot describe the state after input.
/// Failed input can also be partial, so invalidate before attempting it.
pub(super) fn changes_desktop_view(tool: &str) -> bool {
    matches!(tool,
        "computer_move" | "computer_click" | "computer_drag" | "computer_scroll"
        | "computer_type" | "computer_key" | "computer_act" | "computer_window_act"
        | "computer_open" | "computer_focus_window" | "computer_lower_window")
}

#[cfg(test)]
#[test]
fn desktop_input_invalidates_previous_pixels_but_reads_do_not() {
    for tool in ["computer_click", "computer_act", "computer_key", "computer_open",
        "computer_window_act", "computer_focus_window", "computer_scroll"] {
        assert!(changes_desktop_view(tool), "{tool}");
    }
    for tool in ["computer_screenshot", "computer_capture_window", "computer_list_windows",
        "computer_status", "image_analyze", "recall"] {
        assert!(!changes_desktop_view(tool), "{tool}");
    }
}
const MAX_EXPLICIT_USER_TOOL_CALLS: usize = 10_000;

/// Honor a user's explicit whole-turn tool-call ceiling at the runtime
/// boundary. Models routinely promise "one last check" and then take another
/// round; an exact numerical cap is authority, not prose advice.
pub(super) fn explicit_tool_call_limit(request: &str) -> Option<usize> {
    let lower = request.to_ascii_lowercase();
    let mut best: Option<usize> = None;

    // A narrow verification request often names the one allowed tool instead
    // of spelling out "one tool call" (for example: "call index_codebase
    // exactly once; use no other tools").  Treat that combination as the same
    // whole-turn ceiling.  Requiring both halves avoids interpreting ordinary
    // prose such as "refresh exactly once" as a tool budget.
    if lower.contains("exactly once")
        && (lower.contains("use no other tool") || lower.contains("no other tool"))
    {
        best = Some(1);
    }
    for marker in ["tool calls", "tool call"] {
        let mut offset = 0usize;
        while let Some(relative) = lower[offset..].find(marker) {
            let index = offset + relative;
            let start = index.saturating_sub(72);
            let prefix = &lower[start..index];
            let states_limit = [
                "at most",
                "no more than",
                "do not use more than",
                "don't use more than",
                "maximum",
                "max ",
                "limit ",
                "ceiling ",
            ]
            .iter()
            .any(|phrase| prefix.contains(phrase));
            if states_limit {
                let parsed = prefix
                    .split(|ch: char| !ch.is_ascii_digit())
                    .filter(|part| !part.is_empty())
                    .filter_map(|part| part.parse::<usize>().ok())
                    .last()
                    .filter(|limit| (1..=MAX_EXPLICIT_USER_TOOL_CALLS).contains(limit));
                if let Some(limit) = parsed {
                    best = Some(best.map_or(limit, |current| current.min(limit)));
                }
            }
            offset = index + marker.len();
        }
    }
    best
}

/// Admit one provider batch. A single malformed response must not allocate
/// thousands of calls, but valid work is not stopped after an arbitrary
/// turn-wide count: anchored compaction and the repeat/failure guards own long
/// turns without deleting their context.
pub(super) fn admit_tool_calls(seen: &mut usize, requested: usize) -> Result<(), String> {
    if requested > MAX_TOOL_CALLS_PER_RESPONSE {
        return Err(format!(
            "provider requested {requested} tools in one response (limit {MAX_TOOL_CALLS_PER_RESPONSE})"
        ));
    }
    let next = seen
        .checked_add(requested)
        .ok_or_else(|| "tool-call counter overflowed while enforcing the turn limit".to_string())?;
    *seen = next;
    Ok(())
}

/// Honest runtime-authored finish for a wall-clock or tool-count boundary.
/// Successful evidence is bounded and retained; no uncompleted action is
/// claimed as stopped or successful.
pub(super) fn bounded_turn_response(
    agent_name: &str,
    reason: &str,
    results: &[ToolCallResult],
) -> FinalResponse {
    let evidence = activity_digest(results);
    let evidence_block = if evidence.trim().is_empty() {
        "No completed tool result was available before the boundary.".to_string()
    } else {
        format!("Completed tool evidence (newest bounded receipt):\n{evidence}")
    };
    FinalResponse {
        summary: format!("{agent_name} stopped at a runtime safety boundary."),
        final_markdown: format!(
            "Phoenix stopped this agent at a hard runtime boundary: {reason}. Unfinished actions are not reported as completed.\n\n{evidence_block}"
        ),
        changes_made: vec![],
        verification: vec![
            "Runtime-generated bounded receipt; any action without a completed tool result remains unconfirmed."
                .to_string(),
        ],
        execution_mode: "runtime_boundary_with_preserved_evidence".to_string(),
        tool_transcript: vec![],
    }
}

/// Abandonment belongs to a request, not a tool name. A successful recovery
/// can also reopen that request, so consult the current guard before stopping.
/// Clone only on this rare candidate-stop path; probing must not advance the
/// real guard's blocked-repeat counters.
pub(super) fn repeats_only_abandoned_calls(
    calls: &[crate::runtime::RequestedToolCall],
    abandoned: &std::collections::HashSet<String>,
    guard: &crate::runtime::tool_failure_guard::ToolFailureGuard,
) -> bool {
    use crate::runtime::tool_failure_guard::{fingerprint, FailureLoopDecision};
    if calls.is_empty() || !calls.iter().all(|call| {
        call.tool_name != "final_answer" && abandoned.contains(&fingerprint(&call.tool_name, &call.input))
    }) {
        return false;
    }
    let mut probe = guard.clone();
    calls.iter().all(|call| !matches!(probe.before_call(&call.tool_name, &call.input), FailureLoopDecision::Execute))
}

#[cfg(test)]
#[test]
fn abandoned_poll_does_not_disable_workflow_actions_or_recovered_reads() {
    use crate::runtime::tool_failure_guard::{fingerprint, FailureLoopDecision, ToolFailureGuard};
    let make = |tool: &str, input| crate::runtime::RequestedToolCall { tool_name: tool.into(), input };
    let poll = make("work", serde_json::json!({"action":"status","node_id":"pending-job"}));
    let mut guard = ToolFailureGuard::default();
    for _ in 0..2 { guard.record_result(&poll.tool_name, &poll.input, true, "working"); }
    assert!(matches!(guard.before_call(&poll.tool_name, &poll.input), FailureLoopDecision::Block { .. }));
    assert!(matches!(guard.before_call(&poll.tool_name, &poll.input), FailureLoopDecision::Abandon { .. }));
    let mut abandoned = std::collections::HashSet::from([fingerprint(&poll.tool_name, &poll.input)]);
    assert!(repeats_only_abandoned_calls(std::slice::from_ref(&poll), &abandoned, &guard));
    let workflow = make("work", serde_json::json!({"action":"workflow","workflow_action":"tick","workflow_run_id":"my-run"}));
    assert!(!repeats_only_abandoned_calls(std::slice::from_ref(&workflow), &abandoned, &guard));
    assert!(matches!(guard.before_call(&workflow.tool_name, &workflow.input), FailureLoopDecision::Execute));
    assert!(!repeats_only_abandoned_calls(&[poll, workflow], &abandoned, &guard));

    let read = make("read", serde_json::json!({"path":"scene.py"}));
    for _ in 0..2 { guard.record_result(&read.tool_name, &read.input, true, "old source"); }
    let _ = guard.before_call(&read.tool_name, &read.input);
    assert!(matches!(guard.before_call(&read.tool_name, &read.input), FailureLoopDecision::Abandon { .. }));
    abandoned.insert(fingerprint(&read.tool_name, &read.input));
    assert!(repeats_only_abandoned_calls(std::slice::from_ref(&read), &abandoned, &guard));
    guard.record_result("write", &serde_json::json!({"path":"scene.py","content":"changed source"}), true, "saved");
    assert!(!repeats_only_abandoned_calls(std::slice::from_ref(&read), &abandoned, &guard));
    assert!(matches!(guard.before_call(&read.tool_name, &read.input), FailureLoopDecision::Execute));
}

/// Finish only when the provider's entire next action is a tool lane that was
/// already retried, blocked, and abandoned. This is semantic loop detection,
/// not an arbitrary round boundary: useful completed evidence remains intact,
/// no context is compacted away, and a multi-call response containing other
/// independent work is still allowed to continue.
pub(super) fn repeated_action_response(
    agent_name: &str,
    tool_name: &str,
    results: &[ToolCallResult],
) -> FinalResponse {
    let evidence = activity_digest(results);
    let evidence_block = if evidence.trim().is_empty() {
        "No tool action completed before this blocked lane.".to_string()
    } else {
        format!("Completed evidence remains preserved:\n{evidence}")
    };
    FinalResponse {
        summary: format!("{agent_name} stopped repeating a blocked action."),
        final_markdown: format!(
            "I could not advance because my only next action repeated the already-blocked `{tool_name}` lane after Phoenix supplied its recovery route. Phoenix did not execute the duplicate, and it did not delete or reset the conversation context.\n\n{evidence_block}"
        ),
        changes_made: vec![],
        verification: vec![
            "Semantic no-progress guard: the duplicate tool lane was not executed and prior evidence was retained."
                .to_string(),
        ],
        execution_mode: "no_progress_repeat_guard".to_string(),
        tool_transcript: vec![],
    }
}

/// A provider can disappear after the agent already ran successful tools. In
/// that case the work is not equivalent to an empty failed turn: preserve a
/// bounded evidence receipt so a background chain can continue/report without
/// asking another model to rediscover the same state.
pub(super) fn provider_failure_after_evidence(
    error: &anyhow::Error,
    results: &[ToolCallResult],
) -> Option<FinalResponse> {
    if !results.iter().any(|result| result.success) {
        return None;
    }
    let evidence = activity_digest(results);
    if evidence.trim().is_empty() {
        return None;
    }
    let formatted_error = format!("{error:#}");
    let cause = first_line(&formatted_error);
    // Tell whoever receives this receipt (the user, or a coworker that may
    // re-delegate) what will and will not help, so the same oversized step is
    // not simply asked for again.
    let cause = if is_response_size_limit_error(error) {
        format!("{cause}\n\nThe model's reply was larger than Phoenix accepts in one response (usually one very large file write). Retrying the same step will fail the same way; ask for the output in smaller parts.")
    } else {
        cause.to_string()
    };
    Some(FinalResponse {
        summary: "Provider unavailable after tool evidence was collected".to_string(),
        final_markdown: format!(
            "The provider became unavailable after this agent had already performed work. Phoenix preserved the completed tool evidence instead of discarding the turn. The intended final synthesis or handoff did not run.\n\nProvider failure: {cause}\n\nTool evidence (newest bounded receipt):\n{evidence}"
        ),
        changes_made: vec![],
        verification: vec![
            "Runtime-generated partial receipt from successful tool results; no model-authored completion was available."
                .to_string(),
        ],
        execution_mode: "provider_failure_with_preserved_evidence".to_string(),
        tool_transcript: vec![],
    })
}

pub(super) fn activity_digest(results: &[ToolCallResult]) -> String {
    results
        .iter()
        .rev()
        .take(20)
        .map(|result| {
            format!(
                "{} {}: {}",
                if result.success { "ok" } else { "failed" },
                result.tool_name,
                cap_chars(&result.output, 240)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// No tool call is unbounded: a wedged CDP websocket, a hung subprocess, or
/// a dead network must cost the turn one failed tool result, never freeze
/// the gateway (observed live: `browser_navigate` on x.com blocked forever
/// inside a raw CDP call that no tab timeout covers). Budgets are generous —
/// the point is bounding, not rushing.
pub(super) fn tool_call_timeout(tool_name: &str) -> std::time::Duration {
    use std::time::Duration;
    match tool_name {
        // The user thinking is not a tool failure.
        "ask_user" | "teach_workflow" => Duration::from_secs(3600),
        // Builds and test suites legitimately run long.
        "bash" => Duration::from_secs(900),
        // Workspace discovery is local and bounded. If it cannot finish
        // promptly, fail fast instead of freezing a chat for five minutes.
        "glob" | "grep" | "list_directory" | "read" => Duration::from_secs(20),
        name if name.starts_with("browser_") => Duration::from_secs(120),
        name if name.starts_with("computer_") => Duration::from_secs(180),
        _ => Duration::from_secs(300),
    }
}

/// Preserve the semantic model round across a transient provider retry. The
/// whole-turn deadline (and user Stop) is the outer bound; a temporary outage
/// must never consume the agent's finite reasoning-round budget.
pub(super) fn prepare_transient_provider_retry(
    attempts: &mut u32,
    round_cursor: &mut usize,
    round_index: usize,
) -> std::time::Duration {
    *attempts = attempts.saturating_add(1);
    *round_cursor = round_index;
    let seconds = 2_u64.saturating_pow((*attempts).min(5)).min(30);
    std::time::Duration::from_secs(seconds)
}

/// Is this provider error worth an in-turn retry? Transport-level failures
/// (reqwest "error sending request", timeouts, resets) and server-side 5xx /
/// 429 are transient; auth and schema errors (400/401/403) are not.
pub(super) fn is_transient_provider_error(error: &anyhow::Error) -> bool {
    let text = format!("{error:#}").to_lowercase();
    [
        "error sending request",
        "timed out",
        "timeout",
        "connection reset",
        "connection closed",
        "incomplete message",
        "rate limit",
        "overloaded",
        "temporarily unavailable",
        "service unavailable",
        "try again later",
        "(429)",
        "error (500",
        "error (502",
        "error (503",
        "error (529",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

/// The provider stream was larger than Phoenix accepts (typed limit error, or
/// its rendered text after fallback/context layers). Not transient: resending
/// the same request regenerates the same oversized reply.
pub(super) fn is_response_size_limit_error(error: &anyhow::Error) -> bool {
    crate::providers::fallback::is_response_size_limit_error(error, &format!("{error:#}"))
}

/// Fed back to the model once when its reply was too large to receive.
pub(super) const OVERSIZED_REPLY_FEEDBACK: &str = "Your last reply was too large for Phoenix to receive, so it was discarded and nothing from it ran (no file was written). Do not repeat it in one step. Redo it in smaller parts: keep each tool call's content under about 50 KB, e.g. write the file's first section, then add the remaining sections with further edits. Keep your text replies short.";

/// Context exhaustion is recoverable by compaction, but it is not a transport
/// retry: resending the same prompt cannot help. Providers use several different
/// phrases/status bodies for the same condition, so keep the classifier narrow
/// to context/token-limit language instead of treating every HTTP 400 as one.
pub(super) fn is_context_overflow_error(error: &anyhow::Error) -> bool {
    let text = format!("{error:#}").to_lowercase();
    [
        "context window exceeded",
        "context length exceeded",
        "maximum context length",
        "too many tokens",
        "prompt is too long",
        "input is too long",
        "request too large for model",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

#[cfg(test)]
mod provider_error_tests {
    use super::{
        is_context_overflow_error, is_transient_provider_error, prepare_transient_provider_retry,
    };

    #[test]
    fn context_overflow_is_compacted_not_transport_retried() {
        let overflow =
            anyhow::anyhow!("OpenAI API error (400): maximum context length is 128000 tokens");
        assert!(is_context_overflow_error(&overflow));
        assert!(!is_transient_provider_error(&overflow));
    }

    #[test]
    fn ordinary_bad_request_is_not_mislabeled_as_overflow() {
        let schema = anyhow::anyhow!("OpenAI API error (400): invalid tool schema");
        assert!(!is_context_overflow_error(&schema));
    }

    #[test]
    fn provider_overload_stream_errors_are_retried() {
        let overloaded = anyhow::anyhow!(
            "codex stream reported an error: Our servers are currently overloaded. Please try again later."
        );
        assert!(is_transient_provider_error(&overloaded));
    }

    #[test]
    fn transient_retry_preserves_the_same_semantic_round_without_a_hard_retry_cap() {
        let mut attempts = 0;
        let mut next_round = 7;
        assert_eq!(
            prepare_transient_provider_retry(&mut attempts, &mut next_round, 6),
            std::time::Duration::from_secs(2)
        );
        assert_eq!(attempts, 1);
        assert_eq!(next_round, 6);
        for _ in 0..1000 {
            let delay = prepare_transient_provider_retry(&mut attempts, &mut next_round, 6);
            assert!(delay <= std::time::Duration::from_secs(30));
            assert_eq!(next_round, 6);
        }
        assert_eq!(attempts, 1001);
    }
}

/// Tools that are read-only and side-effect-free, so a round consisting only of
/// them can run concurrently. Every tool call in one round is emitted before any
/// result returns, so calls in a round never depend on each other's output — the
/// only reason to serialize is side effects (writes, bash, stateful browser /
/// desktop sessions, talk, final_answer). A pure gather burst (glob+grep+read+
/// symbol_search) or a researcher's web fan-out collapses from N serial
/// round-trips into one concurrent batch.
pub(super) fn is_parallel_safe_tool(name: &str) -> bool {
    matches!(
        name,
        "read"
            | "grep"
            | "glob"
            | "codebase_search"
            | "list_directory"
            | "symbol_search"
            | "callers"
            | "callees"
            | "impact"
            | "file_symbols"
            | "web_search"
            | "web_fetch"
            | "web_scrape"
            | "web_crawl"
            | "skill"
            | "design_reference"
            | "composio_search"
            | "composio_schemas"
            | "composio_connections"
    )
}

/// Actions where ignoring a confident installed-skill match would turn a
/// learned company playbook into decorative prompt text. Discovery stays
/// open so the coworker can inspect reality first; before its first material
/// side effect it must load the matched skill, which then remains pinned for
/// the endless coworker session.
pub(super) fn skill_gate_applies_to_tool(name: &str) -> bool {
    matches!(
        name,
        "write"
            | "str_replace"
            | "bash"
            | "image_gen"
            | "browser_click"
            | "browser_input"
            | "browser_send_keys"
            | "browser_select_dropdown"
            | "browser_upload_file"
            | "browser_download"
            | "computer_click"
            | "computer_drag"
            | "computer_type"
            | "computer_key"
            | "computer_act"
            | "computer_window_act"
            | "account_manage"
            | "credential_generate"
            | "pass_use"
            | "cron"
    )
}

pub(super) fn missing_matched_skill_for_action(
    session: &Session,
    workspace_root: &std::path::Path,
    mission: &str,
    tool_name: &str,
) -> Option<String> {
    if !skill_gate_applies_to_tool(tool_name) {
        return None;
    }
    // A fuzzy recommendation must never become a mandatory action gate.
    // Real native acceptance caught a Blender write being blocked until a
    // React-component browsing skill was loaded. Honor explicit instructions,
    // including multiple requested skills, without enforcing lexical guesses.
    let skill = crate::tools::skills::explicitly_requested_skill_names(workspace_root, mission)
        .into_iter()
        .find(|name| !session.pinned_refs.iter().any(|reference| {
            reference.tool == "skill" && reference.key == *name
        }))?;
    Some(skill)
}

pub(super) fn missing_matched_routine_for_action<'a>(
    required_routine_id: Option<&'a str>,
    begun_routine_ids: &std::collections::HashSet<String>,
    tool_name: &str,
) -> Option<&'a str> {
    if !skill_gate_applies_to_tool(tool_name) {
        return None;
    }
    let routine_id = required_routine_id?;
    (!begun_routine_ids.contains(routine_id)).then_some(routine_id)
}

/// Extract a human-readable message from a caught panic payload (the `Box<dyn
/// Any>` that `catch_unwind` returns). Panics carry either a `&str` or a
/// `String`; anything else degrades to a generic label.
pub(super) fn panic_payload_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

/// Record the mtime of a file a tool just touched, keyed by absolute path —
/// the watch list for ambient change whispers. Called for reads (start
/// watching) AND the agent's own writes (so self-edits don't false-alarm).
pub(super) fn record_file_mtime(
    watch: &mut HashMap<PathBuf, std::time::SystemTime>,
    workspace_root: &std::path::Path,
    input: &serde_json::Value,
) {
    let Some(path) = input.get("path").and_then(|v| v.as_str()) else {
        return;
    };
    let abs = if std::path::Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        workspace_root.join(path)
    };
    if let Ok(mtime) = std::fs::metadata(&abs).and_then(|m| m.modified()) {
        watch.insert(abs, mtime);
    }
}

/// Files whose on-disk state moved underneath the agent since it read them
/// (another process, another agent, or the user). Each change reports once:
/// the recorded mtime advances, deleted files leave the watch list.
pub(super) fn ambient_changed_files(
    watch: &mut HashMap<PathBuf, std::time::SystemTime>,
) -> Vec<String> {
    let mut changed = Vec::new();
    let mut deleted: Vec<PathBuf> = Vec::new();
    for (path, recorded) in watch.iter_mut() {
        match std::fs::metadata(path).and_then(|m| m.modified()) {
            Ok(current) if current != *recorded => {
                *recorded = current;
                changed.push(path.display().to_string());
            }
            Ok(_) => {}
            Err(_) => {
                changed.push(format!("{} (deleted)", path.display()));
                deleted.push(path.clone());
            }
        }
    }
    for path in deleted {
        watch.remove(&path);
    }
    changed
}

pub(super) fn empty_loaded_memories() -> LoadedMemories {
    LoadedMemories {
        memories: vec![],
        knowledge_docs: vec![],
        ranked_context_items: vec![],
        omitted_items: vec![],
        grounding_receipts: vec![],
        trust_receipts: vec![],
        completion_state: "mesh-v1: librarian preload skipped".to_string(),
        open_questions: vec![],
        recommended_next_agent_or_tool: None,
        context_budget_used: 0,
    }
}

pub(crate) fn cap_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}\n…(truncated)")
    }
}

/// Extract the saved file path from a `browser_screenshot` output
/// ("Screenshot saved: /path/x.png").
pub(super) fn screenshot_path(output: &str) -> Option<String> {
    // The executor renders tool output as "{summary}\n{content}", so the
    // marker line is NOT necessarily first (computer_screenshot: line 2).
    output
        .lines()
        .find_map(|line| line.strip_prefix("Screenshot saved: "))
        .map(|path| path.trim().to_string())
}

// ── Edit display diffs (verbose UI mode) ────────────────────────────────
//
// The TUI's verbose display mode renders file edits as red/green diffs. The
// diff is a pure UI artifact carried on `CliEvent::ToolCallCompleted.diff` —
// it never enters model context (the model already knows what it changed).

/// Cap on rendered diff lines — a UI preview, not a patch.
const DIFF_MAX_LINES: usize = 400;
const DIFF_MAX_INPUT_BYTES: usize = 1024 * 1024;
/// Unchanged lines shown above the first edit and below the last, so a diff can
/// be read in place. The canvas renders edits in FULL (no inner scrollbar).
const DIFF_CONTEXT: usize = 10;

/// Pre-image of the file an edit tool is about to touch, captured BEFORE the
/// tool runs so `write` overwrites can show what was removed. Bounded; any
/// failure (missing file, too big, binary) degrades to no pre-image.
pub(super) fn capture_edit_preimage(
    workspace_root: &std::path::Path,
    tool_name: &str,
    input: &serde_json::Value,
) -> Option<String> {
    if !matches!(tool_name, "write" | "str_replace") {
        return None;
    }
    let raw = input.get("path").and_then(|v| v.as_str())?;
    let workspace_root = std::fs::canonicalize(workspace_root).ok()?;
    let raw_path = std::path::Path::new(raw);
    let lexical_path = if raw_path.is_absolute() {
        raw_path.to_path_buf()
    } else {
        workspace_root.join(raw_path)
    };
    let lexical_meta = std::fs::symlink_metadata(&lexical_path).ok()?;
    if lexical_meta.file_type().is_symlink() {
        return None;
    }
    let path = std::fs::canonicalize(&lexical_path).ok()?;
    if !path.starts_with(&workspace_root) {
        return None;
    }
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > 512 * 1024 {
        return None;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(&path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.by_ref()
        .take(512 * 1024 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 512 * 1024 {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// Render a display diff for a SUCCESSFUL edit. `str_replace` renders the
/// exact old/new strings from the input (no algorithm needed — they ARE the
/// diff). `write` line-diffs the pre-image against the new content.
pub(crate) fn edit_display_diff(
    tool_name: &str,
    input: &serde_json::Value,
    preimage: Option<&str>,
) -> Option<String> {
    let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("?");
    let input_bytes = match tool_name {
        "str_replace" => input
            .get("old_str")
            .and_then(|v| v.as_str())?
            .len()
            .saturating_add(input.get("new_str").and_then(|v| v.as_str())?.len())
            .saturating_add(preimage.map(str::len).unwrap_or(0)),
        "write" => input
            .get("content")
            .and_then(|v| v.as_str())?
            .len()
            .saturating_add(preimage.map(str::len).unwrap_or(0)),
        _ => return None,
    };
    if input_bytes > DIFF_MAX_INPUT_BYTES {
        return Some(format!(
            "@@ {path}\n… diff preview omitted: edit input is {input_bytes} bytes (preview limit {DIFF_MAX_INPUT_BYTES})"
        ));
    }
    let body: Vec<String> = match tool_name {
        "str_replace" => {
            let old = input.get("old_str").and_then(|v| v.as_str())?;
            let new = input.get("new_str").and_then(|v| v.as_str())?;
            // With the file's pre-edit content we can show the change WHERE it
            // happened: DIFF_CONTEXT real lines above and below. Without it,
            // fall back to the bare -/+ block.
            match preimage.and_then(|pre| replace_diff_with_context(pre, old, new)) {
                Some(lines) => lines,
                None => old
                    .lines()
                    .map(|l| format!("- {l}"))
                    .chain(new.lines().map(|l| format!("+ {l}")))
                    .collect(),
            }
        }
        "write" => {
            let new = input.get("content").and_then(|v| v.as_str())?;
            line_diff(preimage.unwrap_or(""), new)
        }
        _ => return None,
    };
    if body.is_empty() {
        return None;
    }
    let total = body.len();
    let mut shown: Vec<String> = body.into_iter().take(DIFF_MAX_LINES).collect();
    if total > DIFF_MAX_LINES {
        shown.push(format!("… +{} more diff lines", total - DIFF_MAX_LINES));
    }
    Some(format!("@@ {path}\n{}", shown.join("\n")))
}

/// Minimal line diff: trim the common prefix and suffix, mark everything
/// between as removed/added. Not an LCS — edits usually touch one region and
/// this is a preview, not a patch. Zero deps (the lean rule).
fn line_diff(old: &str, new: &str) -> Vec<String> {
    let old: Vec<&str> = old.lines().collect();
    let new: Vec<&str> = new.lines().collect();
    let mut start = 0;
    while start < old.len() && start < new.len() && old[start] == new[start] {
        start += 1;
    }
    let (mut end_old, mut end_new) = (old.len(), new.len());
    while end_old > start && end_new > start && old[end_old - 1] == new[end_new - 1] {
        end_old -= 1;
        end_new -= 1;
    }
    let mut out = Vec::new();
    // Real context, not "… 42 unchanged lines": DIFF_CONTEXT lines of the file
    // above the first changed line and below the last, so an edit can be READ
    // in place without opening the file.
    let head_from = start.saturating_sub(DIFF_CONTEXT);
    if head_from > 0 {
        out.push(format!("  … {head_from} unchanged lines"));
    }
    out.extend(old[head_from..start].iter().map(|l| format!("  {l}")));
    out.extend(old[start..end_old].iter().map(|l| format!("- {l}")));
    out.extend(new[start..end_new].iter().map(|l| format!("+ {l}")));
    let tail_to = (end_old + DIFF_CONTEXT).min(old.len());
    out.extend(old[end_old..tail_to].iter().map(|l| format!("  {l}")));
    let tail = old.len().saturating_sub(tail_to);
    if tail > 0 {
        out.push(format!("  … {tail} unchanged lines"));
    }
    out
}

/// A str_replace shown IN ITS FILE: locate `old` in the pre-edit content and
/// render DIFF_CONTEXT real lines either side of the replaced region.
fn replace_diff_with_context(preimage: &str, old: &str, new: &str) -> Option<Vec<String>> {
    let offset = preimage.find(old)?;
    // The index of the line holding `offset`. `lines().count()` on the prefix
    // over-counted when `old` began mid-line, and near the end of the file the
    // context slice then ran past the last line and panicked the whole turn.
    let file: Vec<&str> = preimage.lines().collect();
    let start_line = preimage[..offset].matches('\n').count().min(file.len());
    let removed: Vec<&str> = old.lines().collect();
    let end_line = (start_line + removed.len()).min(file.len());

    let head_from = start_line.saturating_sub(DIFF_CONTEXT);
    let tail_to = (end_line + DIFF_CONTEXT).min(file.len());

    let mut out = Vec::new();
    if head_from > 0 {
        out.push(format!("  … {head_from} unchanged lines"));
    }
    out.extend(file[head_from..start_line].iter().map(|l| format!("  {l}")));
    out.extend(removed.iter().map(|l| format!("- {l}")));
    out.extend(new.lines().map(|l| format!("+ {l}")));
    out.extend(file[end_line..tail_to].iter().map(|l| format!("  {l}")));
    let tail = file.len().saturating_sub(tail_to);
    if tail > 0 {
        out.push(format!("  … {tail} unchanged lines"));
    }
    Some(out)
}

#[cfg(test)]
mod edit_diff_tests {
    use super::*;

    #[test]
    fn mid_line_replacement_at_end_of_file_does_not_panic() {
        let file: String = (1..=12).map(|n| format!("line {n}\n")).collect();
        let out = replace_diff_with_context(&file, "12\n", "twelve\n").expect("diff");
        assert!(out.iter().any(|line| line == "- 12"));
        assert!(out.iter().any(|line| line == "+ twelve"));
        let unterminated = "alpha\nbeta";
        assert!(replace_diff_with_context(unterminated, "ta", "TA").is_some());
    }

    #[test]
    fn str_replace_diff_carries_ten_lines_of_file_context() {
        // A 40-line file; the edit lands on line 21 (index 20).
        let file: String = (1..=40)
            .map(|n| format!("line {n}\n"))
            .collect::<Vec<_>>()
            .concat();
        let input = serde_json::json!({
            "path": "src/a.rs",
            "old_str": "line 21",
            "new_str": "line 21 EDITED",
        });
        let diff = edit_display_diff("str_replace", &input, Some(&file)).expect("diff");
        // Exactly 10 context lines above (11..=20) and 10 below (22..=31).
        assert!(diff.contains("  line 11"), "10 lines above: {diff}");
        assert!(diff.contains("  line 20"));
        assert!(!diff.contains("  line 10"), "no 11th line above: {diff}");
        assert!(diff.contains("- line 21"));
        assert!(diff.contains("+ line 21 EDITED"));
        assert!(diff.contains("  line 22"));
        assert!(diff.contains("  line 31"), "10 lines below: {diff}");
        assert!(!diff.contains("  line 32"), "no 11th line below: {diff}");
        // The rest of the file is summarised, not dumped.
        assert!(diff.contains("… 10 unchanged lines"));
    }

    #[test]
    fn str_replace_diff_is_exact_old_red_new_green() {
        let input = serde_json::json!({
            "path": "src/a.rs",
            "old_str": "let x = 1;\nlet y = 2;",
            "new_str": "let x = 10;"
        });
        let diff = edit_display_diff("str_replace", &input, None).expect("diff");
        assert!(diff.starts_with("@@ src/a.rs\n"));
        assert!(diff.contains("- let x = 1;"));
        assert!(diff.contains("- let y = 2;"));
        assert!(diff.contains("+ let x = 10;"));
    }

    #[test]
    fn write_diff_shows_surrounding_lines_as_context() {
        let old = "a\nb\nc\nd";
        let new = "a\nB2\nc\nd";
        let input = serde_json::json!({ "path": "f.txt", "content": new });
        let diff = edit_display_diff("write", &input, Some(old)).expect("diff");
        // Within DIFF_CONTEXT of the change, so the neighbours are shown as real
        // context lines rather than summarised away.
        assert!(diff.contains("  a"), "prefix shown as context: {diff}");
        assert!(diff.contains("- b"));
        assert!(diff.contains("+ B2"));
        assert!(diff.contains("  c"), "suffix shown as context: {diff}");
        assert!(diff.contains("  d"));
        assert!(!diff.contains("- a"), "unchanged prefix not marked removed");
        assert!(
            !diff.contains("unchanged lines"),
            "nothing left to summarise"
        );
    }

    #[test]
    fn new_file_write_is_all_green_and_capped() {
        let content = (0..600)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let input = serde_json::json!({ "path": "new.txt", "content": content });
        let diff = edit_display_diff("write", &input, None).expect("diff");
        assert!(diff.contains("+ line 0"));
        assert!(diff.contains("more diff lines"), "capped at DIFF_MAX_LINES");
        assert!(!diff.contains("- "), "nothing removed in a new file");
    }

    #[test]
    fn non_edit_tools_have_no_diff() {
        let input = serde_json::json!({ "path": "f.txt" });
        assert!(edit_display_diff("read", &input, None).is_none());
    }

    #[test]
    fn oversized_edits_render_a_constant_size_omission_receipt() {
        let input = serde_json::json!({
            "path": "huge.txt",
            "content": "x".repeat(DIFF_MAX_INPUT_BYTES + 1),
        });
        let diff = edit_display_diff("write", &input, None).expect("omission receipt");
        assert!(diff.contains("preview omitted"));
        assert!(diff.len() < 256);
    }

    #[cfg(unix)]
    #[test]
    fn edit_preimage_does_not_follow_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        std::fs::write(&target, "secret").unwrap();
        symlink(&target, dir.path().join("linked.txt")).unwrap();
        let input = serde_json::json!({ "path": "linked.txt", "content": "new" });
        assert!(capture_edit_preimage(dir.path(), "write", &input).is_none());
    }
}

#[cfg(test)]
mod provider_failure_receipt_tests {
    use super::provider_failure_after_evidence;
    use crate::runtime::ToolCallResult;

    #[test]
    fn successful_tool_evidence_survives_a_later_provider_failure() {
        let evidence = ToolCallResult {
            tool_name: "bash".to_string(),
            input_summary: "cargo test --lib".to_string(),
            success: true,
            output: "test result: ok. 772 passed; 0 failed".to_string(),
        };
        let failure = anyhow::anyhow!("dns lookup failed");
        let final_response = provider_failure_after_evidence(&failure, &[evidence])
            .expect("successful work must produce a partial receipt");
        assert_eq!(
            final_response.execution_mode,
            "provider_failure_with_preserved_evidence"
        );
        assert!(final_response.final_markdown.contains("772 passed"));
        assert!(final_response.final_markdown.contains("dns lookup failed"));
    }

    #[test]
    fn no_successful_evidence_remains_a_real_failure() {
        let failed_call = ToolCallResult {
            tool_name: "read".to_string(),
            input_summary: "missing".to_string(),
            success: false,
            output: "not found".to_string(),
        };
        assert!(
            provider_failure_after_evidence(&anyhow::anyhow!("provider down"), &[failed_call])
                .is_none()
        );
    }
}

#[cfg(test)]
mod skill_gate_tests {
    use super::{
        missing_matched_routine_for_action, missing_matched_skill_for_action,
        skill_gate_applies_to_tool,
    };
    use crate::session::{Message, Session};

    #[test]
    fn heuristic_skill_match_is_advisory_but_explicit_request_is_enforced() {
        let workspace = tempfile::tempdir().unwrap();
        let skill_dir = workspace
            .path()
            .join(".phoenix/skills/nebula-quantum-renderer");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: nebula-quantum-renderer\ndescription: render nebula quantum scenes with deterministic shaders\n---\nFollow the renderer playbook.\n",
        )
        .unwrap();
        let mission = "Render a nebula quantum scene with deterministic shaders";
        let mut session = Session::new_main_with_id("skill-gate", "model", "system");

        assert_eq!(crate::tools::skills::matching_skill_name(workspace.path(), mission).as_deref(), Some("nebula-quantum-renderer"));
        assert!(missing_matched_skill_for_action(&session, workspace.path(), mission, "write").is_none());
        let mission = "Use nebula-quantum-renderer to render a nebula quantum scene";

        assert_eq!(
            missing_matched_skill_for_action(&session, workspace.path(), mission, "write")
                .as_deref(),
            Some("nebula-quantum-renderer")
        );
        assert!(
            missing_matched_skill_for_action(&session, workspace.path(), mission, "read").is_none()
        );

        session.push_message(Message::ToolResult {
            tool_name: "skill".to_string(),
            input: r#"{"name":"nebula-quantum-renderer"}"#.to_string(),
            success: true,
            output: "loaded".to_string(),
        });
        assert!(
            missing_matched_skill_for_action(&session, workspace.path(), mission, "write")
                .is_none()
        );
    }

    #[test]
    fn native_component_inspection_does_not_require_web_component_skill() {
        let workspace = tempfile::tempdir().unwrap();
        let skill_dir = workspace.path().join(".phoenix/skills/21st-dev-component-inspect");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "---\nname: 21st-dev-component-inspect\ndescription: WHEN you need to browse 21st.dev components, verify previews, or identify exact React dependencies.\n---\nInspect web components.\n").unwrap();
        let mission = "Offline Blender task: inspect the render and report measured dimensions and component names. Do not browse websites.";
        let session = Session::new_main_with_id("native-skill-gate", "model", "system");
        // Preserve the actual false-positive recommendation to prove it no
        // longer blocks the write, rather than hiding it behind a blacklist.
        assert_eq!(crate::tools::skills::matching_skill_name(workspace.path(), mission).as_deref(), Some("21st-dev-component-inspect"));
        assert!(missing_matched_skill_for_action(&session, workspace.path(), mission, "write").is_none());
        assert!(missing_matched_skill_for_action(&session, workspace.path(), "Use 21st-dev-component-inspect", "write").is_some());
        assert!(missing_matched_skill_for_action(&session, workspace.path(), "Do not use 21st-dev-component-inspect", "write").is_none());
    }

    #[test]
    fn design_reference_prevents_generic_visual_skill_stacking() {
        let workspace = tempfile::tempdir().unwrap();
        let skill_dir = workspace
            .path()
            .join(".phoenix/skills/frontend-design-deslop");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: frontend-design-deslop\ndescription: frontend design UI polish workflow\n---\nPolish it.\n",
        )
        .unwrap();
        let mission = "Build and polish this frontend design UI";
        let mut session = Session::new_main_with_id("visual-skill-gate", "model", "system");
        session.push_message(Message::ToolResult {
            tool_name: "design_reference".to_string(),
            input: r#"{"path":"taste/SKILL.md"}"#.to_string(),
            success: true,
            output: "loaded".to_string(),
        });

        assert!(
            missing_matched_skill_for_action(&session, workspace.path(), mission, "write")
                .is_none()
        );
    }

    #[test]
    fn action_classifier_targets_side_effects_not_discovery() {
        for tool in ["write", "bash", "browser_click"] {
            assert!(skill_gate_applies_to_tool(tool), "{tool}");
        }
        for tool in [
            "read",
            "grep",
            "web_search",
            "composio_run",
            "mcp_call",
            "skill",
            "talk",
            "final_answer",
        ] {
            assert!(!skill_gate_applies_to_tool(tool), "{tool}");
        }
    }

    #[test]
    fn taught_workflow_must_begin_in_the_current_turn_before_side_effects() {
        let mut begun = std::collections::HashSet::new();
        assert_eq!(
            missing_matched_routine_for_action(Some("weekly-inbox"), &begun, "browser_click"),
            Some("weekly-inbox")
        );
        assert!(
            missing_matched_routine_for_action(Some("weekly-inbox"), &begun, "browser_state")
                .is_none()
        );
        begun.insert("weekly-inbox".to_string());
        assert!(
            missing_matched_routine_for_action(Some("weekly-inbox"), &begun, "browser_click")
                .is_none()
        );
    }
}

#[cfg(test)]
mod tool_budget_tests {
    use super::{
        admit_tool_calls, bounded_turn_response, explicit_tool_call_limit,
        MAX_TOOL_CALLS_PER_RESPONSE,
    };

    #[test]
    fn extracts_only_explicit_whole_turn_tool_call_ceilings() {
        assert_eq!(
            explicit_tool_call_limit("Do not use more than 9 tool calls. Finish with the answer."),
            Some(9)
        );
        assert_eq!(
            explicit_tool_call_limit("Use at most 12 tool calls; ideally fewer."),
            Some(12)
        );
        assert_eq!(
            explicit_tool_call_limit(
                "At most four connected-app calls, and 20 messages in the answer."
            ),
            None
        );
        assert_eq!(
            explicit_tool_call_limit("The previous run used 24 tool calls."),
            None
        );
        assert_eq!(
            explicit_tool_call_limit(
                "Call index_codebase exactly once, then answer with its receipt. Use no other tools."
            ),
            Some(1)
        );
        assert_eq!(
            explicit_tool_call_limit("Refresh the index exactly once, then summarize it."),
            None
        );
    }

    #[test]
    fn valid_batches_do_not_hit_an_arbitrary_turn_cap() {
        let mut seen = 0usize;
        for _ in 0..10_000 {
            admit_tool_calls(&mut seen, 1).unwrap();
        }
        assert_eq!(seen, 10_000);
    }

    #[test]
    fn oversized_response_is_rejected_without_consuming_budget() {
        let mut seen = 7usize;
        assert!(admit_tool_calls(&mut seen, MAX_TOOL_CALLS_PER_RESPONSE + 1).is_err());
        assert_eq!(seen, 7);
    }

    #[test]
    fn runtime_boundary_preserves_only_completed_bounded_evidence() {
        let completed = crate::runtime::ToolCallResult {
            tool_name: "bash".to_string(),
            input_summary: "cargo test".to_string(),
            success: true,
            output: "test result: ok. 12 passed".to_string(),
        };
        let response = bounded_turn_response("Leo", "tool cap", &[completed]);
        assert!(response.final_markdown.contains("12 passed"));
        assert!(response.final_markdown.contains("Unfinished actions"));
        assert_eq!(
            response.execution_mode,
            "runtime_boundary_with_preserved_evidence"
        );
    }
}
