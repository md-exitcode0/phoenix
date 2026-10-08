//! Turn-local protection against agents repeating a failed action unchanged.
//!
//! Historical Phoenix traces contained identical failed writes eight times in
//! a row, thirteen consecutive validation failures, and GUI captures repeated
//! sixteen times without a state change. A round cap eventually stopped those
//! turns, but only after the model had burned a large context and token budget.
//! This guard acts at the tool boundary: two real attempts are allowed, then
//! the exact call is rejected with a first-party recovery route. Repeating the
//! rejected call twice permanently abandons that tool lane for the current
//! turn.  A broken peripheral lane must never kill unrelated work that can
//! still complete from other tools or evidence.

use std::collections::HashMap;

use serde_json::Value;
use sha2::{Digest, Sha256};

const REAL_ATTEMPTS_BEFORE_BLOCK: u8 = 2;
const BLOCKED_REPEATS_BEFORE_STOP: u8 = 2;
const REAL_OBSERVATIONS_BEFORE_BLOCK: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FailureLoopDecision {
    Execute,
    Block { feedback: String },
    Abandon { feedback: String },
}

#[derive(Debug, Default, Clone)]
struct FailureState {
    failures: u8,
    blocked_repeats: u8,
    last_failure: String,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct ToolFailureGuard {
    failures: HashMap<String, FailureState>,
    /// Same broken strategy with cosmetically different inputs (another stale
    /// element ref, another guessed path, another protected URL). Exact-call
    /// fingerprints alone cannot see this class of loop.
    families: HashMap<String, FailureFamilyState>,
    /// Successful status reads can still form an expensive polling loop. The
    /// postbox is event-driven, so two reads of the same work target in one
    /// model turn are enough; further reads must wait for a background return.
    observations: HashMap<String, ObservationState>,
    shell_receipts: HashMap<String, (String, u8, u8)>,
}

#[derive(Debug, Default, Clone)]
struct FailureFamilyState {
    class: String,
    failures: u8,
    blocked_repeats: u8,
    last_failure: String,
}

#[derive(Debug, Default, Clone)]
struct ObservationState {
    successes: u8,
    blocked_repeats: u8,
}

impl ToolFailureGuard {
    pub(crate) fn has_failure(&self, tool: &str, input: &Value) -> bool {
        self.failures.contains_key(&fingerprint(tool, input))
    }

    pub(crate) fn has_observation(&self, tool: &str, input: &Value) -> bool {
        observation_key(tool, input)
            .and_then(|key| self.observations.get(&key))
            .is_some_and(|state| state.successes > 0)
    }

    pub(crate) fn before_call(&mut self, tool: &str, input: &Value) -> FailureLoopDecision {
        if tool == "bash" {
            if let Some((_, repeats, blocked)) = self.shell_receipts.get_mut(&fingerprint(tool, input)) {
                if *repeats >= 4 {
                    *blocked = blocked.saturating_add(1);
                    if *blocked >= 2 {
                        return FailureLoopDecision::Abandon { feedback: "REPEATED COMMAND LANE STOPPED: the identical successful command was requested again after recovery feedback. Finish from preserved evidence or use another tool to make a concrete change.".to_string() };
                    }
                    return FailureLoopDecision::Block {
                        feedback: "REPEATED COMMAND BLOCKED: this command already succeeded four times with identical output and no observed file edit. Reuse the saved result. Make a concrete change before testing again, or finish with the evidence already collected.".to_string(),
                    };
                }
            }
        }
        if let Some(key) = observation_key(tool, input) {
            if let Some(state) = self.observations.get_mut(&key) {
                if state.successes >= REAL_OBSERVATIONS_BEFORE_BLOCK {
                    state.blocked_repeats = state.blocked_repeats.saturating_add(1);
                    if state.blocked_repeats >= BLOCKED_REPEATS_BEFORE_STOP {
                        return FailureLoopDecision::Abandon {
                            feedback: "STATUS-POLL LANE ABANDONED FOR THIS TURN: background coworker returns are event-driven, so this target will not be polled again. Continue every other foreground action now and finish without a waiting placeholder. The return remains durable and will enter context on the next natural user turn.".to_string(),
                        };
                    }
                    return FailureLoopDecision::Block {
                        feedback: "REPEATED STATUS POLL BLOCKED: this work target was already read twice in this turn. Do not call `work inspect` or `work status` again, do not stop with a waiting placeholder, and do not promise a result that has not arrived. Continue every independent action; the durable return enters context on the next natural user turn.".to_string(),
                    };
                }
            }
        }
        let key = fingerprint(tool, input);
        if let Some(state) = self.failures.get_mut(&key) {
            if state.failures >= REAL_ATTEMPTS_BEFORE_BLOCK {
                state.blocked_repeats = state.blocked_repeats.saturating_add(1);
                let route = recovery_route(tool, &state.last_failure);
                if state.blocked_repeats >= BLOCKED_REPEATS_BEFORE_STOP {
                    return FailureLoopDecision::Abandon {
                        feedback: format!(
                            "TOOL LANE ABANDONED FOR THIS TURN: the identical `{tool}` call was requested again after two failures and a recovery instruction, so it will not run again during this turn. This is not a whole-task failure. Continue every independent part of the user's request now; report this lane as a blocker only if the final outcome truly depends on it. {route}"
                        ),
                    };
                }
                return FailureLoopDecision::Block {
                    feedback: format!(
                        "REPEATED FAILURE BLOCKED: this exact `{tool}` call already failed twice and was not executed again. Do not retry it unchanged. {route}"
                    ),
                };
            }
        }

        if let Some(state) = self.families.get_mut(tool) {
            if state.failures >= REAL_ATTEMPTS_BEFORE_BLOCK {
                state.blocked_repeats = state.blocked_repeats.saturating_add(1);
                let route = recovery_route(tool, &state.last_failure);
                if state.blocked_repeats >= BLOCKED_REPEATS_BEFORE_STOP {
                    return FailureLoopDecision::Abandon {
                        feedback: format!(
                            "TOOL LANE ABANDONED FOR THIS TURN: `{tool}` kept hitting the same `{}` failure with changed inputs after a recovery instruction, so this strategy will not run again during this turn. This is not a whole-task failure. Continue every independent part of the user's request now; report this lane as a blocker only if the final outcome truly depends on it. {route}",
                            state.class
                        ),
                    };
                }
                return FailureLoopDecision::Block {
                    feedback: format!(
                        "STRATEGY LOOP BLOCKED: `{tool}` has already hit the same `{}` failure twice with changed inputs. This variation was not executed. {route}",
                        state.class
                    ),
                };
            }
        }
        FailureLoopDecision::Execute
    }

    /// Record the result and return concise recovery guidance to append to a
    /// failed result. A successful changed/recovered call clears its own key.
    pub(crate) fn record_result(
        &mut self,
        tool: &str,
        input: &Value,
        success: bool,
        output: &str,
    ) -> Option<String> {
        let key = fingerprint(tool, input);
        if success {
            if matches!(tool, "write" | "str_replace" | "apply_patch") {
                self.shell_receipts.clear();
            } else if tool == "bash" && is_verification_command(input) {
                let receipt_text = verification_receipt(output);
                let digest = format!("{:x}", Sha256::digest(receipt_text.as_bytes()));
                let receipt = self.shell_receipts.entry(key.clone()).or_insert((digest.clone(), 0, 0));
                receipt.1 = if receipt.0 == digest { receipt.1.saturating_add(1) } else { 1 };
                receipt.0 = digest;
            }
            if let Some(observation) = observation_key(tool, input) {
                let state = self.observations.entry(observation).or_default();
                state.successes = state.successes.saturating_add(1);
                state.blocked_repeats = 0;
            }
            self.failures.remove(&key);
            self.families.remove(tool);
            let pending_login = tool == "ask_for_login"
                && serde_json::from_str::<Value>(output)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("decision")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .as_deref()
                    == Some("pending_user");
            if !pending_login {
                self.clear_after_recovery(tool, input);
            }
            if mutates_observed_state(tool) {
                self.observations.clear();
            }
            return None;
        }
        let state = self.failures.entry(key).or_default();
        state.failures = state.failures.saturating_add(1);
        state.blocked_repeats = 0;
        state.last_failure = first_line(output);
        // A shell is every strategy at once: two unrelated commands failing
        // is not one repeated strategy. Its output also carries shell-startup
        // noise (a stale `~/.profile` source line printed "No such file or
        // directory" on every command), which made any two failures look like
        // the same `missing_path` loop and abandoned the whole command lane
        // for the turn. Exact repeats are still blocked by the fingerprint.
        if let Some(class) = failure_class(output).filter(|_| tool != "bash") {
            let family = self.families.entry(tool.to_string()).or_default();
            if family.class == class {
                family.failures = family.failures.saturating_add(1);
            } else {
                family.class = class.to_string();
                family.failures = 1;
            }
            family.blocked_repeats = 0;
            family.last_failure = first_line(output);
        } else {
            self.families.remove(tool);
        }
        let route = recovery_route(tool, output);
        Some(if state.failures >= REAL_ATTEMPTS_BEFORE_BLOCK {
            format!(
                "\n\nLOOP GUARD: this exact call has now failed {} times. It will not run again unchanged. {route}",
                state.failures
            )
        } else {
            format!("\n\nRECOVERY ROUTE: {route}")
        })
    }

    fn clear_after_recovery(&mut self, tool: &str, input: &Value) {
        match tool {
            "list_directory" | "glob" | "grep" | "codebase_search" | "read" => {
                self.clear_family_class("missing_path");
                self.clear_family_class("edit_precondition");
            }
            "browser_state" | "browser_status" | "browser_visibility" => {
                self.clear_tool_prefix_classes("browser_", &["stale_state", "timeout"]);
            }
            "computer_status" | "computer_screenshot" | "computer_app_targets" => {
                self.clear_tool_prefix_classes("computer_", &["stale_state", "unsupported"]);
            }
            "ask_for_login" => self.clear_family_class("authentication"),
            "ask_user" if input.to_string().to_ascii_lowercase().contains("vault") => {
                self.clear_family_class("vault_locked")
            }
            "web_search" => {
                self.clear_tool_prefix_classes("web_", &["transport", "timeout"]);
            }
            _ => {}
        }
    }

    fn clear_family_class(&mut self, class: &str) {
        self.families.retain(|_, state| state.class != class);
    }

    fn clear_tool_prefix_classes(&mut self, prefix: &str, classes: &[&str]) {
        self.families.retain(|tool, state| {
            !(tool.starts_with(prefix) && classes.contains(&state.class.as_str()))
        });
    }
}

pub(crate) fn is_verification_command(input: &Value) -> bool {
    let command = input.get("command").or_else(|| input.get("cmd"))
        .and_then(Value::as_str).unwrap_or("");
    ["node --test", "cargo test", "npm test", "npm run test", "pnpm test", "pytest", "vitest", "jest"]
        .iter().any(|runner| command.contains(runner))
}

fn verification_receipt(output: &str) -> String {
    // TAP timings change on every run even when the test evidence is identical.
    // Keep test names, counts, failures, and other command output intact.
    output.lines().filter(|line| !line.contains("duration_ms"))
        .collect::<Vec<_>>().join("\n")
}

fn observation_key(tool: &str, input: &Value) -> Option<String> {
    if matches!(
        tool,
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
            | "skill"
            | "design_reference"
            | "composio_search"
            | "composio_schemas"
            | "composio_connections"
    ) {
        return Some(format!("observation:{}", fingerprint(tool, input)));
    }
    if tool != "work" {
        return None;
    }
    let action = input.get("action")?.as_str()?;
    if !matches!(action, "inspect" | "status") {
        return None;
    }
    let target = input
        .get("target_id")
        .or_else(|| input.get("node_id"))
        .or_else(|| input.get("workflow_run_id"))?
        .as_str()?
        .trim();
    (!target.is_empty()).then(|| format!("work:{target}"))
}

fn mutates_observed_state(tool: &str) -> bool {
    matches!(
        tool,
        "write"
            | "str_replace"
            | "bash"
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
            | "composio_run"
            | "mcp_call"
    )
}

pub(crate) fn fingerprint(tool: &str, input: &Value) -> String {
    let encoded = serde_json::to_vec(input).unwrap_or_default();
    let mut hash = Sha256::new();
    hash.update(tool.as_bytes());
    hash.update([0]);
    hash.update(encoded);
    format!("{:x}", hash.finalize())
}

fn first_line(value: &str) -> String {
    value
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(240)
        .collect()
}

fn failure_class(output: &str) -> Option<&'static str> {
    let lower = output.to_ascii_lowercase();
    if lower.starts_with("str_replace: old_str not found") {
        return Some("edit_precondition");
    }
    if input_decode_error(&lower) {
        return Some("schema");
    }
    if lower.contains("vault") && lower.contains("locked") {
        return Some("vault_locked");
    }
    if lower.contains("multiple ")
        && lower.contains(" accounts connected")
        && lower.contains("specify which to use")
    {
        return Some("account_selection");
    }
    if lower.contains("stale") || lower.contains("detached") || lower.contains("not visible") {
        return Some("stale_state");
    }
    if http_forbidden(&lower) {
        return Some("access_denied");
    }
    if lower.contains("login")
        || lower.contains("oauth")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("credential")
    {
        return Some("authentication");
    }
    if lower.contains("no such file")
        || lower.contains("does not exist")
        || lower.contains("path escapes workspace")
        || lower.contains("path does not exist")
    {
        return Some("missing_path");
    }
    if lower.contains("old_str not found") || lower.contains("read this file before editing") {
        return Some("edit_precondition");
    }
    if lower.contains("invalid tool input")
        || lower.contains("missing required")
        || lower.contains("expected")
        || lower.contains("schema")
    {
        return Some("schema");
    }
    if lower.contains("timed out") || lower.contains("timeout") {
        return Some("timeout");
    }
    if lower.contains("403") || lower.contains("429") || lower.contains("connection") {
        return Some("transport");
    }
    if lower.contains("not supported") || lower.contains("doesn't support") {
        return Some("unsupported");
    }
    None
}

fn recovery_route(tool: &str, output: &str) -> &'static str {
    let lower = output.to_ascii_lowercase();
    if tool == "bash" && !http_forbidden(&lower) && !(lower.contains("vault") && lower.contains("locked")) {
        return "Read the last lines of stderr: they name what failed (a missing program or module, a bad path, a script error). Change the command to address exactly that, for example another interpreter or tool that is installed, and keep using bash (use its cwd parameter rather than cd). Startup warnings from the user's shell profile are unrelated noise.";
    }
    if tool == "str_replace" && lower.starts_with("str_replace: old_str not found") {
        return "Use the current-text excerpt, when present, for an exact retry; if it is partial or insufficient, read the indicated region once. Preserve whitespace and do not guess or apply fuzzy changes.";
    }
    if input_decode_error(&lower) {
        return "Correct the call against the installed Phoenix tool schema and the allowed values in this error. A value listed as allowed is not a report that the browser, login, or workspace is broken.";
    }
    if (lower.contains("vault") || lower.contains("passes")) && lower.contains("locked") {
        return "Passes is locked. Retry the same pass_use / browser_input_credential call: Phoenix shows the user its one-time unlock card and waits. If the card is already open, continue independent work. Never use ask_for_login for a locked vault and never request the master password in prose.";
    }
    if tool == "composio_run"
        && lower.contains(" accounts connected")
        && lower.contains("specify which to use")
    {
        return "Use the connected account id returned by composio_search as each tool item's top-level `account` field beside `tool_slug`; keep Gmail `user_id` and every other app parameter inside `arguments`. Do not re-search or re-read the schema.";
    }
    // This tool only places a vault secret into a field; it does not submit
    // the form and therefore cannot prove that the site rejected the saved
    // password.  Its errors are commonly stale indexes, framework-remounted
    // fields, or scope/origin mismatches.  The generic authentication route
    // used to turn any of those into `ask_for_login`, making the user re-enter
    // credentials Phoenix already had.
    if tool == "browser_input_credential" {
        return "The saved credential remains available, and a fill failure before form submission is NOT evidence that its password is wrong. Get fresh browser_state and credential_list metadata, confirm the live site and current password-field index, then retry browser_input_credential once when the field or page changed. If the credential is out of scope or bound to another site, select the matching saved credential instead. Use ask_for_login only after the SITE rejects a submitted credential or a genuine user-only MFA/passkey step is required; never ask the user to re-enter a password Phoenix can already use.";
    }
    if tool.starts_with("browser_") && (lower.contains("stale") || lower.contains("detached") || lower.contains("not visible")) {
        return "Refresh browser_state, use the fresh element reference, and prefer browser_act when the target can be described semantically.";
    }
    if tool == "work" && lower.contains("work node `") && lower.contains("does not exist in run") {
        return "Inspect the authoritative work ledger, not the filesystem. A workflow assignment and a legacy work node are different identities. If the ID came from workflow install_plan, set action=workflow, workflow_action=inspect, and workflow_run_id to the exact returned run_id. Use the returned node and claim/lease identity for tick or transition. Otherwise inspect the current run's legacy work nodes. Do not guess a replacement ID, bypass a lease, or repeat the same status call against another namespace.";
    }
    if matches!(tool, "bash" | "web_fetch" | "web_search") && http_forbidden(&lower) {
        return "HTTP 403 alone does not establish that a user login is needed. Check the source page and service's stated requirements; use another accessible public source if appropriate. Do not repeat an unchanged request, bypass access controls, or ask the user to log in without evidence of an actual interactive login requirement.";
    }
    if lower.contains("login")
        || lower.contains("oauth")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("credential")
    {
        return "Use ask_for_login so Phoenix can import site cookies, open a user login, or create an account through the scoped vault; do not invent an authentication workaround.";
    }
    if lower.contains("no such file")
        || lower.contains("does not exist")
        || lower.contains("path escapes workspace")
        || lower.contains("path does not exist")
    {
        return "Re-observe the workspace with list_directory, glob, grep, or codebase_search, then use the exact workspace-relative path returned by Phoenix.";
    }
    if lower.contains("old_str not found") || lower.contains("read this file before editing") {
        return "Read the current file once, anchor the edit to its exact returned bytes, then retry with str_replace or write; do not guess stale content.";
    }
    if lower.contains("invalid tool input")
        || lower.contains("missing required")
        || lower.contains("expected")
        || lower.contains("schema")
    {
        return "Correct the call against the installed Phoenix tool schema shown in the tool definition; do not switch to shell or a third-party workaround.";
    }
    if lower.contains("timed out") || lower.contains("timeout") {
        return "Inspect the action's current state before retrying because it may have completed; change the strategy or use the dedicated first-party tool for the next attempt.";
    }
    if lower.contains("403") || lower.contains("429") || lower.contains("connection") {
        return "Change route instead of replaying the request: use the authenticated browser for protected pages, web_search for discovery, or the configured provider fallback for transport failures.";
    }
    "Re-observe the authoritative state, change at least one material input or route, and prefer an installed first-party Phoenix tool over an ad-hoc workaround."
}

fn input_decode_error(lower: &str) -> bool {
    // Serde lists valid enum values in errors. Words like `stale` or `login`
    // in that list describe the schema, not the current application state.
    lower.contains("unknown variant `")
        || lower.contains("unknown field `")
        || lower.contains("missing field `")
        || lower.contains("invalid type:")
}

fn http_forbidden(lower: &str) -> bool {
    ["http error 403", "http 403", "http/1.1 403", "http/2 403", "403 forbidden", "403: forbidden"]
        .iter().any(|pattern| lower.contains(pattern))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_identity_failure_recovers_from_the_work_ledger_not_the_filesystem() {
        let output = "work node `node_5be5f6aa970f4971a4c3fa178925a545aabd860cdcb948a61d7dc3cfe37bfe02` does not exist in run `agent-coder`";
        let guidance = recovery_route("work", output);
        assert!(guidance.contains("workflow") && guidance.contains("inspect"), "{guidance}");
        assert!(guidance.contains("claim") && guidance.contains("lease"), "{guidance}");
        assert!(!guidance.contains("list_directory") && !guidance.contains("glob"), "{guidance}");
        assert!(recovery_route("read", "path does not exist").contains("list_directory"));
        assert!(recovery_route("work", "artifact path does not exist").contains("list_directory"));
        assert!(recovery_route("work", "invalid tool input: unknown variant `status`").contains("schema"));
    }

    #[test]
    fn public_resource_forbidden_does_not_prescribe_user_login() {
        assert_eq!(failure_class("HTTP Error 403: Forbidden"), Some("access_denied"));
        for tool in ["bash", "web_fetch", "web_search"] {
            let hint = recovery_route(tool, "urllib.error.HTTPError: HTTP Error 403: Forbidden");
            assert!(hint.contains("does not establish"));
            assert!(hint.contains("accessible public source"));
            assert!(!hint.contains("Use ask_for_login"));
        }
        assert!(recovery_route("browser_input_credential", "HTTP 403 credential error").contains("NOT evidence that its password is wrong"));
        assert!(recovery_route("web_fetch", "vault locked").contains("one-time unlock card"));
    }

    #[test]
    fn workflow_enum_error_is_schema_recovery_not_a_stale_browser() {
        let output = "workflow_payload does not match the selected workflow action: unknown variant `active`, expected one of `pending`, `ready`, `leased`, `running`, `review`, `waiting_user`, `waiting_peer`, `blocked`, `succeeded`, `failed`, `canceled`, `stale`";
        assert_eq!(failure_class(output), Some("schema"));
        let hint = ToolFailureGuard::default().record_result("work", &serde_json::json!({"workflow_payload":{"state":"active"}}), false, output).unwrap();
        assert!(hint.contains("allowed values"));
        assert!(!hint.contains("Refresh browser_state"));
        assert!(!hint.contains("Use ask_for_login"));
        assert!(!recovery_route("computer_click", "target not visible").contains("browser_state"));
    }

    #[test]
    fn edit_mismatch_context_does_not_misclassify_quoted_file_contents() {
        let output = "str_replace: old_str not found in config.rs.\nCurrent: stale login credential vault locked";
        assert_eq!(failure_class(output), Some("edit_precondition"));
        let hint = recovery_route("str_replace", output);
        assert!(hint.contains("current-text excerpt"));
        assert!(!hint.contains("ask_user"));
        assert!(!hint.contains("browser_state"));
    }

    #[test]
    fn repeated_successful_command_requires_new_evidence_or_edit() {
        let mut guard = ToolFailureGuard::default();
        let input = serde_json::json!({"command":"node --test acceptance.test.cjs"});
        for index in 0..4 {
            assert_eq!(guard.before_call("bash", &input), FailureLoopDecision::Execute);
            guard.record_result("bash", &input, true, &format!("all tests passed\n# duration_ms {}", 10+index));
        }
        assert!(matches!(guard.before_call("bash", &input), FailureLoopDecision::Block { .. }));
        assert!(matches!(guard.before_call("bash", &input), FailureLoopDecision::Abandon { .. }));
        guard.record_result("str_replace", &serde_json::json!({"path":"app.js"}), true, "edited");
        assert_eq!(guard.before_call("bash", &input), FailureLoopDecision::Execute);
        for index in 0..8 {
            guard.record_result("bash", &input, true, &format!("new result {index}"));
            assert_eq!(guard.before_call("bash", &input), FailureLoopDecision::Execute);
        }
    }

    #[test]
    fn exact_failed_call_is_executed_twice_then_blocked_then_abandoned() {
        let mut guard = ToolFailureGuard::default();
        let input = serde_json::json!({"path":"missing.txt"});
        assert_eq!(
            guard.before_call("read", &input),
            FailureLoopDecision::Execute
        );
        guard.record_result("read", &input, false, "path does not exist");
        assert_eq!(
            guard.before_call("read", &input),
            FailureLoopDecision::Execute
        );
        let guidance = guard
            .record_result("read", &input, false, "path does not exist")
            .unwrap();
        assert!(guidance.contains("will not run again unchanged"));
        assert!(matches!(
            guard.before_call("read", &input),
            FailureLoopDecision::Block { .. }
        ));
        assert!(matches!(
            guard.before_call("read", &input),
            FailureLoopDecision::Abandon { .. }
        ));
    }

    #[test]
    fn changed_input_runs_after_successful_workspace_recovery() {
        let mut guard = ToolFailureGuard::default();
        let old = serde_json::json!({"path":"missing.txt"});
        let fresh = serde_json::json!({"path":"src/lib.rs"});
        guard.record_result("read", &old, false, "path does not exist");
        guard.record_result("read", &old, false, "path does not exist");
        guard.record_result(
            "list_directory",
            &serde_json::json!({"path":"src"}),
            true,
            "src/lib.rs",
        );
        assert_eq!(
            guard.before_call("read", &fresh),
            FailureLoopDecision::Execute
        );
        guard.record_result("read", &fresh, true, "read ok");
        assert_eq!(
            guard.before_call("read", &fresh),
            FailureLoopDecision::Execute
        );
    }

    #[test]
    fn stale_browser_failure_points_to_fresh_state_not_blind_retry() {
        let mut guard = ToolFailureGuard::default();
        let input = serde_json::json!({"ref":128});
        let guidance = guard
            .record_result(
                "browser_click",
                &input,
                false,
                "Element is STALE — its DOM node is detached",
            )
            .unwrap();
        assert!(guidance.contains("Refresh browser_state"));
        assert!(guidance.contains("browser_act"));
    }

    #[test]
    fn credential_fill_failure_never_claims_the_password_is_bad_or_asks_for_login() {
        let mut guard = ToolFailureGuard::default();
        let input = serde_json::json!({"index":17,"credential_id":"cred-school"});
        let guidance = guard
            .record_result(
                "browser_input_credential",
                &input,
                false,
                "credential input into [17] did not remain stable in the field",
            )
            .unwrap();

        assert!(guidance.contains("NOT evidence that its password is wrong"));
        assert!(guidance.contains("fresh browser_state"));
        assert!(guidance.contains("credential_list"));
        assert!(guidance.contains("only after the SITE rejects"));
        assert!(!guidance.contains("Use ask_for_login so Phoenix can import"));
    }

    #[test]
    fn credential_loop_guard_preserves_secret_safe_recovery_route() {
        let mut guard = ToolFailureGuard::default();
        let input = serde_json::json!({"index":17,"credential_id":"cred-school"});
        let error = "credential field [17] changed before input (NOT_FOUND)";
        guard.record_result("browser_input_credential", &input, false, error);
        let second = guard
            .record_result("browser_input_credential", &input, false, error)
            .unwrap();
        assert!(second.contains("will not run again unchanged"));
        assert!(second.contains("saved credential remains available"));

        let blocked = guard.before_call("browser_input_credential", &input);
        let FailureLoopDecision::Block { feedback } = blocked else {
            panic!("expected the unchanged third fill to be blocked");
        };
        assert!(feedback.contains("fresh browser_state"));
        assert!(feedback.contains("never ask the user to re-enter"));
    }

    #[test]
    fn changed_inputs_cannot_hide_the_same_failure_strategy() {
        let mut guard = ToolFailureGuard::default();
        let first = serde_json::json!({"ref":128});
        let second = serde_json::json!({"ref":212});
        let third = serde_json::json!({"ref":900});
        guard.record_result(
            "browser_click",
            &first,
            false,
            "Element is stale and detached",
        );
        guard.record_result(
            "browser_click",
            &second,
            false,
            "Element is stale and detached",
        );
        assert!(matches!(
            guard.before_call("browser_click", &third),
            FailureLoopDecision::Block { .. }
        ));
    }

    #[test]
    fn composio_account_shape_variations_are_one_blocked_strategy() {
        let mut guard = ToolFailureGuard::default();
        let error =
            "Multiple gmail accounts connected. Specify which to use via the 'account' field";
        let account_id = serde_json::json!({"tools":[{"arguments":{"account_id":"gmail_one"}}]});
        let nested_account = serde_json::json!({"tools":[{"arguments":{"account":"gmail_one"}}]});
        let guessed_user = serde_json::json!({"tools":[{"arguments":{"user_id":"gmail_one"}}]});
        guard.record_result("composio_run", &account_id, false, error);
        let guidance = guard
            .record_result("composio_run", &nested_account, false, error)
            .unwrap();
        assert!(guidance.contains("top-level `account`"), "{guidance}");
        assert!(matches!(
            guard.before_call("composio_run", &guessed_user),
            FailureLoopDecision::Block { .. }
        ));
    }

    #[test]
    fn successful_first_party_recovery_reopens_the_original_tool() {
        let mut guard = ToolFailureGuard::default();
        let first = serde_json::json!({"ref":128});
        let second = serde_json::json!({"ref":212});
        let fresh = serde_json::json!({"ref":900});
        guard.record_result("browser_click", &first, false, "stale element");
        guard.record_result("browser_click", &second, false, "stale element");
        guard.record_result("browser_state", &serde_json::json!({}), true, "fresh state");
        assert_eq!(
            guard.before_call("browser_click", &fresh),
            FailureLoopDecision::Execute
        );
    }

    #[test]
    fn successful_vault_unlock_request_clears_only_the_vault_family() {
        let mut guard = ToolFailureGuard::default();
        let first = serde_json::json!({"site":"vvs"});
        let second = serde_json::json!({"site":"vvschool.ca"});
        let verified = serde_json::json!({"site":"vvs-moodle.pembinahills.ca"});
        guard.record_result(
            "credential_list",
            &first,
            false,
            "credential vault is locked",
        );
        guard.record_result(
            "credential_list",
            &second,
            false,
            "credential vault is locked",
        );
        assert!(matches!(
            guard.before_call("credential_list", &verified),
            FailureLoopDecision::Block { .. } | FailureLoopDecision::Abandon { .. }
        ));

        let login = serde_json::json!({"site":"vvs-moodle.pembinahills.ca"});
        guard.record_result("ask_for_login", &login, true, "cookie import completed");
        assert!(matches!(
            guard.before_call("credential_list", &verified),
            FailureLoopDecision::Block { .. } | FailureLoopDecision::Abandon { .. }
        ));

        let unlock = serde_json::json!({
            "questions":[{
                "header":"Unlock vault",
                "question":"Unlock the credential vault to continue.",
                "options":["Unlock here","Not now"]
            }]
        });
        guard.record_result("ask_user", &unlock, true, "Unlock here");
        assert_eq!(
            guard.before_call("credential_list", &verified),
            FailureLoopDecision::Execute
        );
    }

    #[test]
    fn pending_login_does_not_clear_authentication_failure_family() {
        let mut guard = ToolFailureGuard::default();
        let first = serde_json::json!({"site":"vvs"});
        let second = serde_json::json!({"site":"vvschool.ca"});
        let retry = serde_json::json!({"site":"vvs-moodle.pembinahills.ca"});
        guard.record_result("composio_run", &first, false, "login required");
        guard.record_result("composio_run", &second, false, "login is still required");
        guard.record_result(
            "ask_for_login",
            &retry,
            true,
            r#"{"decision":"pending_user","site":"vvs-moodle.pembinahills.ca"}"#,
        );
        assert!(matches!(
            guard.before_call("composio_run", &retry),
            FailureLoopDecision::Block { .. } | FailureLoopDecision::Abandon { .. }
        ));
    }

    #[test]
    fn generic_command_failures_do_not_disable_bash_as_a_family() {
        let mut guard = ToolFailureGuard::default();
        guard.record_result(
            "bash",
            &serde_json::json!({"command":"cargo test a"}),
            false,
            "Command FAILED with exit code 1",
        );
        guard.record_result(
            "bash",
            &serde_json::json!({"command":"cargo test b"}),
            false,
            "Command FAILED with exit code 1",
        );
        assert_eq!(
            guard.before_call("bash", &serde_json::json!({"command":"cargo test c"})),
            FailureLoopDecision::Execute
        );
    }

    #[test]
    fn shell_startup_noise_does_not_make_unrelated_commands_one_strategy() {
        let mut guard = ToolFailureGuard::default();
        let noise = "Command FAILED with exit code {}.\nstdout:\n\n\nstderr:\n/home/u/.profile: line 34: /tmp/uv/env: No such file or directory\n{}";
        guard.record_result("bash", &serde_json::json!({"command":"python -c 'import cv2'"}), false,
            &noise.replacen("{}", "127", 1).replacen("{}", "bash: line 1: python: command not found", 1));
        guard.record_result("bash", &serde_json::json!({"command":"python3 -c 'import cv2'"}), false,
            &noise.replacen("{}", "1", 1).replacen("{}", "ModuleNotFoundError: No module named 'cv2'", 1));
        for command in ["blender -b scene.blend --python render.py", "ls"] {
            assert_eq!(guard.before_call("bash", &serde_json::json!({"command":command})), FailureLoopDecision::Execute);
        }
    }

    #[test]
    fn event_driven_background_work_cannot_be_polled_forever() {
        let mut guard = ToolFailureGuard::default();
        let status = serde_json::json!({
            "action":"status",
            "node_id":"work_news",
            "state":"active"
        });
        let inspect = serde_json::json!({
            "action":"inspect",
            "target_id":"work_news"
        });
        guard.record_result("work", &status, true, "active");
        guard.record_result("work", &inspect, true, "active job");
        assert!(matches!(
            guard.before_call("work", &inspect),
            FailureLoopDecision::Block { .. }
        ));
        assert!(matches!(
            guard.before_call("work", &status),
            FailureLoopDecision::Abandon { .. }
        ));
    }

    #[test]
    fn successful_static_observation_cannot_be_replayed_forever() {
        let mut guard = ToolFailureGuard::default();
        let read = serde_json::json!({"path":"Cargo.toml"});
        for _ in 0..REAL_OBSERVATIONS_BEFORE_BLOCK {
            assert_eq!(
                guard.before_call("read", &read),
                FailureLoopDecision::Execute
            );
            guard.record_result("read", &read, true, "contents");
        }
        assert!(guard.has_observation("read", &read));
        assert!(matches!(
            guard.before_call("read", &read),
            FailureLoopDecision::Block { .. }
        ));
        assert!(matches!(
            guard.before_call("read", &read),
            FailureLoopDecision::Abandon { .. }
        ));
    }

    #[test]
    fn successful_mutation_reopens_static_observations() {
        let mut guard = ToolFailureGuard::default();
        let read = serde_json::json!({"path":"src/lib.rs"});
        guard.record_result("read", &read, true, "old");
        guard.record_result("read", &read, true, "old");
        guard.record_result(
            "str_replace",
            &serde_json::json!({"path":"src/lib.rs","old_str":"old","new_str":"new"}),
            true,
            "edited",
        );
        assert_eq!(
            guard.before_call("read", &read),
            FailureLoopDecision::Execute
        );
    }

    #[test]
    fn distinct_research_calls_are_not_stopped_by_an_arbitrary_count() {
        let mut guard = ToolFailureGuard::default();
        for index in 0..32 {
            let input = serde_json::json!({"url": format!("https://example.test/{index}")});
            assert_eq!(
                guard.before_call("browser_navigate", &input),
                FailureLoopDecision::Execute
            );
            guard.record_result("browser_navigate", &input, true, "fresh page state");
        }
    }
}
