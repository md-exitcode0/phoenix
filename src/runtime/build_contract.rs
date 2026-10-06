//! Provider-independent contract for substantial build work.
//!
//! Prompts describe the craft: establish the acceptance contract, build a thin
//! vertical slice, finish in verified slices, and preserve reusable learning.
//! This small runtime gate enforces the two facts that are cheap and reliable
//! to prove mechanically: a real 3--7 item plan must exist before mutation,
//! and a successful verification must follow the latest mutation before the
//! coworker can report completion.

use crate::runtime::{RequestedToolCall, ToolCallResult};

const PLAN_FEEDBACK: &str = "BUILD CONTRACT: this is substantial execution. Before changing anything, call `todo_write` by itself with 3–7 short outcome-shaped items covering the thin end-to-end slice, remaining slices, and real acceptance proof. Keep one accountable owner; do not delegate merely to gain tools.";
const VERIFY_FEEDBACK: &str = "BUILD CONTRACT: changes were made, but no successful post-change acceptance check is recorded. Run the real artifact-appropriate path now (tests/build for code, render/screenshot for visuals, or an authoritative end-to-end inspection for the built workflow), then finish with compact evidence.";
const SEQUENTIAL_VERIFY_FEEDBACK: &str = "BUILD CONTRACT: do not batch a mutation with the check meant to prove it; Phoenix may execute independent calls concurrently. Finish the slice first, observe the completed change, then run its real acceptance check in the next tool round.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildAdmission {
    Allow,
    Feedback(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellCheckKind {
    Unknown,
    Supporting,
    Verification,
}

#[derive(Debug, Clone)]
pub struct BuildContractGuard {
    required: bool,
    planned: bool,
    changed: bool,
    verified_after_change: bool,
    observed_results: usize,
    wrote_todos_this_turn: bool,
    pending_work_reminded: bool,
    completion_audit_sent: bool,
    original_request: String,
    unresolved_commits: std::collections::BTreeMap<String, String>,
    workflow_runs: std::collections::BTreeSet<String>,
    // Classification from the executed input, keyed by its next receipt index.
    // Display summaries can truncate a trailing command and are not authority.
    shell_verifications: std::collections::BTreeMap<usize, (String, ShellCheckKind)>,
}

impl BuildContractGuard {
    pub(crate) fn requires_durable_goal(&self) -> bool { self.required }
    /// Classification of an explicit group inspection controls this build
    /// workflow gate only. Tool/permission gates remain independently active.
    pub fn for_assignment(request:&str,inspection:bool)->Self {
        let mut guard=Self::new(request);
        if inspection {guard.required=false;}
        guard
    }
    pub fn new(request: &str) -> Self {
        Self {
            required: is_substantial_build_request(request),
            planned: false,
            changed: false,
            verified_after_change: false,
            observed_results: 0,
            wrote_todos_this_turn: false,
            pending_work_reminded: false,
            completion_audit_sent: false,
            original_request: request.to_string(),
            unresolved_commits: Default::default(),
            workflow_runs: Default::default(),
            shell_verifications: Default::default(),
        }
    }

    /// Consume only newly appended results, in execution order. This matters:
    /// a test run from before an edit must never validate the edit that follows.
    pub fn observe_results(&mut self, results: &[ToolCallResult]) {
        if self.observed_results > results.len() {
            // Defensive reset for a compacted/replaced accelerator. The
            // canonical turn normally only appends, but never index past it.
            self.observed_results = 0;
        }
        for (offset, result) in results[self.observed_results..].iter().enumerate() {
            let shell_check = self.shell_verifications.remove(&(self.observed_results + offset))
                .filter(|(summary, _)| result.tool_name == "bash" && summary == &result.input_summary)
                .map(|(_, kind)| kind)
                .or_else(|| (result.tool_name == "bash").then(|| shell_summary_kind(&result.input_summary)));
            let verification = shell_check.map(|kind| kind == ShellCheckKind::Verification)
                .unwrap_or_else(|| is_verification_result(result));
            let mutation = shell_check.map(|kind| kind == ShellCheckKind::Unknown)
                .unwrap_or_else(|| is_mutation_result(result));
            if !result.success {
                // An execution can change the artifact before failing. Unlike
                // a rejected atomic edit, a shell/browser/app action or an
                // external tool receipt does not imply rollback.
                let partial_execution = mutation
                    && matches!(result.tool_name.as_str(),
                        "bash" | "image_gen" | "browser_act" | "browser_click"
                        | "browser_type" | "browser_select" | "browser_upload"
                        | "computer_act" | "computer_click" | "computer_type"
                        | "computer_window_act" | "computer_key" | "computer_drag"
                        | "composio_run" | "mcp_call");
                if partial_execution || (self.changed && (verification || shell_check == Some(ShellCheckKind::Supporting))) {
                    self.changed |= partial_execution;
                    self.verified_after_change = false;
                    self.completion_audit_sent = false;
                }
                continue;
            }
            if result.tool_name == "todo_write" {
                self.wrote_todos_this_turn = true;
            }
            if result.tool_name == "todo_write" && valid_todo_summary(&result.input_summary) {
                self.planned = true;
            }
            if mutation {
                // A later repair invalidates the earlier completion audit.
                // Repeated prose, reads and todo rewrites do not rearm it,
                // so a model cannot spin on unchanged evidence.
                self.completion_audit_sent = false;
                self.pending_work_reminded = false;
                self.changed = true;
                // WindowBatch captures only after its ordered actions finish.
                // A successful capture is prepended by the production tool;
                // capture=false, focus changes and capture failures omit it.
                self.verified_after_change = result.tool_name == "computer_window_act"
                    && result.output.lines().nth(1).is_some_and(|line|
                        line.strip_prefix("Screenshot saved: ").is_some_and(|path| !path.trim().is_empty()));
            } else if self.changed && verification {
                self.verified_after_change = true;
            }
        }
        self.observed_results = results.len();
    }

    /// Called after execution, before this result is appended. Bind the full
    /// shell input to this exact receipt without changing the UI summary or
    /// observing it ahead of older results. Success is still read from the
    /// eventual result, including cancellation and post-processing failures.
    pub(crate) fn observe_tool_outcome(&mut self, tool: &str, input: &serde_json::Value, result_index: usize, result: &ToolCallResult) {
        if tool == "bash" && result.tool_name == tool {
            let verification = shell_input_kind(input);
            self.shell_verifications.insert(result_index, (result.input_summary.clone(), verification));
        }
        self.observe_workflow_outcome(tool, input, result.success, &result.output);
    }

    /// Use the executed call's full input, never the human-readable summary.
    /// A todo rewrite or an unrelated successful node cannot erase a rejected
    /// commit. Failed/canceled work is terminal, but is not completed work.
    pub fn observe_workflow_outcome(&mut self, tool: &str, input: &serde_json::Value, success: bool, output: &str) {
        if tool != "work" || input["action"] != "workflow" || input["workflow_action"] != "transition" {
            return;
        }
        let payload = &input["workflow_payload"];
        let Some(node) = payload["node_id"].as_str().filter(|id| !id.is_empty()) else { return; };
        match payload["state"].as_str() {
            Some("succeeded") if success => { self.unresolved_commits.remove(node); }
            Some("succeeded") => {
                let reason: String = output.chars().take(300).collect();
                if self.unresolved_commits.get(node) != Some(&reason) {
                    self.pending_work_reminded = false;
                }
                self.unresolved_commits.insert(node.into(), reason);
            }
            Some("failed" | "canceled") if success && self.unresolved_commits.contains_key(node) => {
                self.unresolved_commits.insert(node.into(), "Assignment stopped without completing the rejected commit.".into());
            }
            _ => {}
        }
    }

    /// Bind only a workflow explicitly selected for this assignment. Never
    /// use all workflows owned by an actor: unrelated conversations and
    /// status questions must not inherit each other's unfinished work.
    pub(crate) fn bind_workflow_run(&mut self, run_id: &str) {
        self.workflow_runs.insert(run_id.into());
    }

    fn pending_workflows(&self) -> Vec<String> {
        if self.workflow_runs.is_empty() { return vec![]; }
        let Some(store) = crate::runtime::company::global_if_initialized() else {
            return vec!["The selected workflow store is unavailable; completion is unverified.".into()];
        };
        self.pending_workflows_in_store(&store)
    }

    fn pending_workflows_in_store(&self, store: &crate::runtime::company::CompanyStore) -> Vec<String> {
        let mut pending = Vec::new();
        for run in &self.workflow_runs {
            match store.workflow_snapshot(Some(run)) {
                Ok(snapshot) if !snapshot.runs.is_empty() && !snapshot.nodes.is_empty() => {
                    let unfinished = snapshot.nodes.iter().filter(|node| node.state != "succeeded").collect::<Vec<_>>();
                    for node in unfinished.iter().take(8) {
                        pending.push(format!("Workflow assignment `{}` is {}: {}", node.node_id, node.state,
                            node.title.chars().take(160).collect::<String>()));
                    }
                    if unfinished.len() > 8 { pending.push(format!("{} additional assignments remain unfinished.", unfinished.len() - 8)); }
                }
                _ => pending.push(format!("Selected workflow `{run}` is unavailable; completion is unverified.")),
            }
        }
        pending
    }

    pub fn admit_tool_batch(&self, calls: &[RequestedToolCall]) -> BuildAdmission {
        if !self.required {
            return BuildAdmission::Allow;
        }

        if calls.iter().any(|call| call.tool_name == "final_answer") {
            // A waiting/read-only result must not need an invented build plan.
            // Conversely, the native final_answer path must enforce the same
            // post-change verification gate as a parsed final response.
            if calls.iter().any(is_mutation_call) {
                return BuildAdmission::Feedback(SEQUENTIAL_VERIFY_FEEDBACK);
            }
            if let Some(feedback) = self.final_feedback() {
                return BuildAdmission::Feedback(feedback);
            }
            if calls.iter().all(|call| call.tool_name == "final_answer" || is_preplan_tool(call)) {
                return BuildAdmission::Allow;
            }
        }

        if self.planned {
            let mutates = calls.iter().any(is_mutation_call);
            let verifies = calls.iter().any(is_verification_call);
            return if mutates && verifies {
                BuildAdmission::Feedback(SEQUENTIAL_VERIFY_FEEDBACK)
            } else {
                BuildAdmission::Allow
            };
        }

        // The plan succeeds in a later result. It stays standalone so a model
        // cannot schedule mutations concurrently with the plan it has not yet
        // established.
        if calls.len() == 1 && valid_todo_call(&calls[0]) {
            return BuildAdmission::Allow;
        }
        if calls.iter().all(is_preplan_tool) {
            BuildAdmission::Allow
        } else {
            BuildAdmission::Feedback(PLAN_FEEDBACK)
        }
    }

    /// Consult the durable plan, not a model's claim or a compressed receipt.
    /// Old plans alone never delay an unrelated status/question turn.
    fn pending_work(&self, session_id: &str) -> Option<String> {
        let mut pending = self.pending_todos(session_id).into_iter().collect::<Vec<_>>();
        pending.extend(self.pending_workflows());
        for (node, reason) in self.unresolved_commits.iter().take(8) {
            pending.push(format!("Workflow assignment `{node}` has no successful completion receipt: {reason}"));
        }
        if self.unresolved_commits.len() > 8 {
            pending.push(format!("{} additional workflow commits remain unresolved.", self.unresolved_commits.len() - 8));
        }
        (!pending.is_empty()).then(|| pending.join("\n"))
    }

    fn pending_todos(&self, session_id: &str) -> Option<String> {
        if !self.wrote_todos_this_turn { return None; }
        match crate::tools::todo_snapshot(session_id) {
            Ok(todos) if !todos.is_empty() => {
                let pending = todos.iter().filter(|item| !item.completed && item.status.as_deref() != Some("cancelled"))
                    .map(|item| format!("- [ ] {}", item.task.chars().take(240).collect::<String>()))
                    .collect::<Vec<_>>();
                (!pending.is_empty()).then(|| pending.join("\n"))
            }
            _ => Some("The task list written during this turn is unavailable; completion could not be verified.".into()),
        }
    }

    pub fn pending_work_feedback(&mut self, session_id: &str) -> Option<String> {
        if self.pending_work_reminded { return None; }
        let pending = self.pending_work(session_id)?;
        self.pending_work_reminded = true;
        Some(format!("UNFINISHED WORK: the saved task list or workflow receipts still record unfinished work. Continue the authorized work you can complete now. Repair rejected commits with the required verified evidence and a successful transition for that exact assignment; all-complete todos do not replace workflow receipts. If blocked or the user changed the scope, explain the remaining items and the concrete reason in your final response; do not mark them complete merely to finish. This reminder is sent once for the current changes; another successful repair requires a new completion check.\n\n{pending}"))
    }

    /// A successful screenshot/build only proves that an observation ran.
    /// Give substantial execution an explicit outcome audit before accepting
    /// its first final, including when the agent marked every todo done.
    /// This is bounded deliberation, not a mechanical claim of visual quality.
    pub fn completion_audit_feedback(&mut self) -> Option<String> {
        if !self.required || !self.changed || !self.verified_after_change || self.completion_audit_sent {
            return None;
        }
        self.completion_audit_sent = true;
        Some(format!("COMPLETION AUDIT: compare the actual saved outcome with the user's request below and earlier requirements/amendments. A successful tool, complete todos, or accepted repair does not prove the full goal. If prior versions exist, compare the candidate with the strongest known version on the same criteria; preserve accepted qualities and repair regressions before selecting what to deliver. For code, exercise realistic inputs, failure behavior and the affected integration against authoritative behavior, not just implementation-shaped tests. For visual or audible work, inspect the actual output against references and the best prior output in compatible whole/detail views or playback. More activity or detail is not evidence of improvement. Continue authorized work for unmet criteria when a meaningful next action exists. If evidence proves every criterion, finish concisely. At a real blocker or explicit budget, preserve the best result and report the unfinished outcome honestly. This audit is sent once for the current changes; successful repairs require another check. It does not expand your authority.\n\nRequest for this assignment (retain its original scope):\n{}", self.original_request))
    }

    /// The model may legitimately stop at a blocker. Preserve the unfinished
    /// scope in the canonical reply and parent handoff instead of looping.
    pub fn preserve_pending_work(&self, session_id: &str, response: &mut crate::runtime::FinalResponse) {
        let pending_todos = self.pending_todos(session_id);
        let workflow_unfinished = !self.unresolved_commits.is_empty() || !self.pending_workflows().is_empty();
        // Open to-dos already show in the To-dos panel; the reply itself stays
        // a human message instead of ending in a status report.
        if pending_todos.is_some() || workflow_unfinished {
            response.summary = if workflow_unfinished { "Unfinished workflow checks remain" } else { "Unfinished task items remain" }.into();
            response.execution_mode = "incomplete_work".into();
        }
    }

    pub fn final_feedback(&self) -> Option<&'static str> {
        if !self.required || !self.changed {
            None
        } else if !self.planned {
            Some(PLAN_FEEDBACK)
        } else if self.changed && !self.verified_after_change {
            Some(VERIFY_FEEDBACK)
        } else {
            None
        }
    }
}

pub fn is_substantial_build_request(request: &str) -> bool {
    let lower = request.to_ascii_lowercase();
    // Requests for an opinion, architecture map, or implementation approach
    // are research deliverables even when they discuss how a future system
    // could be built. Do not force a build todo/verification ceremony unless
    // the same request also tells the coworker to perform the implementation.
    let advisory_or_research = [
        "what do you think",
        "think about how",
        "how would you",
        "architecture report",
        "integration approach",
        "research the approach",
        "investigate how",
    ]
    .iter()
    .any(|term| lower.contains(term));
    let also_requests_execution = [
        "go ahead and implement",
        "go implement",
        "then implement",
        "and implement it",
        "build it now",
        "make the changes",
        "ship it",
    ]
    .iter()
    .any(|term| lower.contains(term));
    if advisory_or_research && !also_requests_execution {
        return false;
    }
    if [
        "explain",
        "summarize",
        "review",
        "audit",
        "diagnose",
        "why does",
        "what is",
    ]
    .iter()
    .any(|term| lower.trim_start().starts_with(term))
        && !["implement", "build", "create", "fix", "change"]
            .iter()
            .any(|term| lower.contains(term))
    {
        return false;
    }

    // Plain creation requests also use "make". Recognize it as the directed
    // request, not an incidental word in an explanation, quoted future task,
    // or review brief. The artifact and substantial-scope checks below still
    // apply; native input alone never establishes creation intent.
    let direct_make = [
        "make ",
        "please make ",
        "can you make ",
        "could you make ",
        "would you make ",
        "can you please make ",
        "could you please make ",
        "would you please make ",
    ]
    .iter()
    .filter_map(|prefix| lower.trim_start().strip_prefix(*prefix))
    .any(|target| target.split_whitespace().next().is_some_and(|word|
        !matches!(word, "sure" | "no" | "nothing")));
    let action = direct_make || [
        "build",
        "implement",
        "create",
        "develop",
        "rebuild",
        "refactor",
        "fix",
        "repair",
        "change",
        "update",
        "overhaul",
        "ship",
        "integrate",
        "migrate",
        "redesign",
        "add",
        "model",
        "sculpt",
        "refine",
        "design",
    ]
    .iter()
    .any(|term| contains_word(&lower, term));
    let artifact = [
        "app",
        "application",
        "feature",
        "system",
        "workflow",
        "integration",
        "api",
        "service",
        "website",
        "frontend",
        "backend",
        "dashboard",
        "component",
        "plugin",
        "automation",
        "prototype",
        "ui",
        "tool",
        "product",
        "module",
        "library",
        "package",
        "extension",
        "cli",
        "blender",
        "3d",
        "scene",
        "sculpture",
        "illustration",
    ]
    .iter()
    .any(|term| contains_word(&lower, term));
    if !action || !artifact {
        return false;
    }

    let explicit_scope = [
        "end to end",
        "end-to-end",
        "production ready",
        "production-ready",
        "from scratch",
        "full rebuild",
        "fully rebuild",
        "complete implementation",
        "entire app",
        "entire system",
        "cross-platform",
        "architecture",
        "vertical slice",
        "multiple surfaces",
        "full ",
        "complete ",
    ]
    .iter()
    .any(|term| lower.contains(term));
    let sequence_markers = lower.matches(" and ").count()
        + lower.matches(" then ").count()
        + lower.matches(" after ").count()
        + lower.matches(',').count();
    let multi_surface = matches!(
        crate::runtime::efficiency::classify_task(request),
        crate::runtime::efficiency::EconomyTaskClass::MultiSurfaceHardTask
    );

    let detailed_visual = ["blender", "3d", "scene", "sculpture", "illustration"]
        .iter().any(|term| contains_word(&lower, term))
        && ["detailed", "polished", "realistic", "photorealistic", "premium"]
            .iter().any(|term| contains_word(&lower, term));
    explicit_scope || multi_surface || detailed_visual || contains_word(&lower, "rebuild") || sequence_markers >= 2
}

fn contains_word(haystack: &str, needle: &str) -> bool {
    haystack
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-')
        .any(|word| word == needle)
}

fn valid_todo_call(call: &RequestedToolCall) -> bool {
    call.tool_name == "todo_write"
        && call
            .input
            .get("todos")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| (3..=7).contains(&items.len()))
}

fn valid_todo_summary(summary: &str) -> bool {
    let Some((_, rest)) = summary.split_once('/') else {
        return false;
    };
    let count = rest
        .split(|ch: char| !ch.is_ascii_digit())
        .next()
        .and_then(|value| value.parse::<usize>().ok());
    count.is_some_and(|count| (3..=7).contains(&count))
}

/// Work needed to understand the task or obtain a genuine approval is safe
/// before planning. Unknown tools are intentionally not included: a new tool
/// that can mutate state should not silently bypass the build contract.
fn is_preplan_tool(call: &RequestedToolCall) -> bool {
    matches!(
        call.tool_name.as_str(),
        "read"
            | "grep"
            | "glob"
            | "list_directory"
            | "codebase_search"
            | "symbol_search"
            | "callers"
            | "callees"
            | "impact"
            | "call_path"
            | "file_symbols"
            | "web_search"
            | "web_fetch"
            | "web_scrape"
            | "web_crawl"
            | "ui_snap"
            | "image_analyze"
            | "design_reference"
            | "design_studio"
            | "skill"
            | "skill_search"
            | "memory_recall"
            | "recall"
            | "mcp_servers"
            | "composio_search"
            | "composio_schemas"
            | "browser_status"
            | "browser_state"
            | "browser_extract"
            | "browser_find_elements"
            | "browser_find_text"
            | "browser_search_page"
            | "browser_screenshot"
            | "computer_status"
            | "computer_screenshot"
            | "computer_capture_window"
            | "computer_read_text"
            | "computer_list_windows"
            | "computer_app_targets"
            | "computer_app_inspect"
            | "computer_app_locate"
            | "computer_app_read"
            | "ask_user"
            | "ask_for_login"
            | "credential_list"
    )
}

fn is_mutation_result(result: &ToolCallResult) -> bool {
    match result.tool_name.as_str() {
        "write" | "str_replace" | "image_gen" | "tools_create" | "skill_install"
        | "browser_act" | "browser_click" | "browser_type" | "browser_select"
        | "browser_upload" | "computer_act" | "computer_click" | "computer_type"
        | "computer_window_act" | "computer_key" | "computer_drag"
        | "composio_run" | "mcp_call" => true,
        "bash" => shell_summary_kind(&result.input_summary) == ShellCheckKind::Unknown,
        _ => false,
    }
}

fn is_mutation_call(call: &RequestedToolCall) -> bool {
    match call.tool_name.as_str() {
        "write" | "str_replace" | "image_gen" | "tools_create" | "skill_install"
        | "browser_act" | "browser_click" | "browser_type" | "browser_select"
        | "browser_upload" | "computer_act" | "computer_click" | "computer_type"
        | "computer_window_act" | "computer_key" | "computer_drag"
        | "composio_run" | "mcp_call" => true,
        "bash" => shell_input_kind(&call.input) == ShellCheckKind::Unknown,
        _ => false,
    }
}

fn is_verification_result(result: &ToolCallResult) -> bool {
    match result.tool_name.as_str() {
        "ui_snap"
        | "image_analyze"
        | "browser_screenshot"
        | "browser_extract"
        | "browser_state"
        | "computer_screenshot"
        | "computer_capture_window"
        | "computer_app_inspect"
        | "computer_app_read" => true,
        "bash" => shell_summary_kind(&result.input_summary) == ShellCheckKind::Verification,
        name => {
            let name = name.to_ascii_lowercase();
            name.contains("test") || name.contains("verify") || name.contains("validate")
        }
    }
}

fn is_verification_call(call: &RequestedToolCall) -> bool {
    match call.tool_name.as_str() {
        "ui_snap"
        | "image_analyze"
        | "browser_screenshot"
        | "browser_extract"
        | "browser_state"
        | "computer_screenshot"
        | "computer_capture_window"
        | "computer_app_inspect"
        | "computer_app_read" => true,
        "bash" => shell_input_kind(&call.input) == ShellCheckKind::Verification,
        name => {
            let name = name.to_ascii_lowercase();
            name.contains("test") || name.contains("verify") || name.contains("validate")
        }
    }
}

fn shell_input_kind(input: &serde_json::Value) -> ShellCheckKind {
    let input = crate::runtime::runner::normalize_tool_input("bash", input.clone());
    input.get("command").and_then(serde_json::Value::as_str)
        .map(classify_shell_check).unwrap_or(ShellCheckKind::Unknown)
}

fn shell_summary_kind(summary: &str) -> ShellCheckKind {
    // summarize_tool_input keeps at most 120 characters, adding "..." when
    // truncated. Missing full-input provenance must never validate that prefix.
    if summary.chars().count() <= 120 { classify_shell_check(summary) } else { ShellCheckKind::Unknown }
}

#[cfg(test)]
fn looks_like_verification(command: &str) -> bool {
    classify_shell_check(command) == ShellCheckKind::Verification
}

fn classify_shell_check(command: &str) -> ShellCheckKind {
    let Some(commands) = verification_command_words(command) else { return ShellCheckKind::Unknown; };
    let mut acceptance = false;
    for words in &commands {
        match verification_command_kind(words) {
            Some(check) => acceptance |= check,
            None => return ShellCheckKind::Unknown,
        }
    }
    if acceptance { ShellCheckKind::Verification } else { ShellCheckKind::Supporting }
}

/// Recognize only literal argv joined by &&. This is deliberately not a general
/// shell parser or a permission check. With &&, a successful shell receipt
/// proves every command succeeded; ;, newlines, || and pipes do not. Reject
/// expansion, redirection, escapes and other shell constructs rather than
/// guessing their effects. Quotes only group literal words (e.g. paths with
/// spaces); even quoted shell metacharacters outside this subset are refused.
fn verification_command_words(command: &str) -> Option<Vec<Vec<String>>> {
    let mut commands = Vec::new();
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote = None;
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        if matches!(ch, '\n' | '\r' | ';' | '|' | '>' | '<' | '`' | '$' | '\\')
            || (ch.is_control() && ch != '\t') {
            return None;
        }
        if let Some(delimiter) = quote {
            if ch == '&' { return None; }
            if ch == delimiter { quote = None; } else { word.push(ch); }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
            started = true;
        } else if ch == ' ' || ch == '\t' {
            if started { words.push(std::mem::take(&mut word)); started = false; }
        } else if ch == '&' {
            if chars.next() != Some('&') { return None; }
            if started { words.push(std::mem::take(&mut word)); started = false; }
            if words.is_empty() { return None; }
            commands.push(std::mem::take(&mut words));
        } else if ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':' | '=' | ',' | '+' | '%' | '@') {
            word.push(ch);
            started = true;
        } else {
            return None;
        }
    }
    if quote.is_some() { return None; }
    if started { words.push(word); }
    if words.is_empty() { return None; }
    commands.push(words);
    Some(commands)
}

fn has_check_option(args: &[String], option: &str) -> bool {
    args.iter().any(|arg| arg.split('=').next() == Some(option))
}

/// Some(true) is a test/build/check invocation; Some(false) is an ancillary
/// read-only check which may follow it. Neither validates scripts' contents or
/// artifact quality: the existing outcome audit and workflow receipts do that.
fn verification_command_kind(words: &[String]) -> Option<bool> {
    let (program, args) = words.split_first()?;
    let program = program.rsplit('/').next()?;
    if ["--help", "-h", "--version", "-V", "--dry-run", "--if-present", "--ignore-scripts",
        "--fix", "--fix-only", "--unsafe-fixes", "--updateSnapshot", "--update-snapshot",
        "--update-snapshots", "--test-update-snapshots"]
        .iter().any(|option| has_check_option(args, option)) {
        return None;
    }
    if let Some(version) = program.strip_prefix("python") {
        if !version.is_empty() && !version.split('.').all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit())) {
            return None;
        }
        let mut args = args;
        while args.first().is_some_and(|arg| matches!(arg.as_str(), "-B" | "-I" | "-E" | "-s" | "-u")) {
            args = &args[1..];
        }
        if args.first().map(String::as_str) != Some("-m") { return None; }
        let module = args.get(1)?.as_str();
        let args = &args[2..];
        return match module {
            "unittest" => Some(true),
            "pytest" => pytest_verification(args).then_some(true),
            "py_compile" => {
                let args = if args.first().map(String::as_str) == Some("-q") { &args[1..] } else { args };
                let args = if args.first().map(String::as_str) == Some("--") { &args[1..] } else { args };
                (!args.is_empty() && args.iter().all(|arg| !arg.is_empty() && !arg.starts_with('-'))).then_some(true)
            }
            _ => None,
        };
    }
    let first = args.first().map(String::as_str);
    match program {
        "cargo" => {
            let args = if args.first().is_some_and(|arg| arg.starts_with('+')) { &args[1..] } else { args };
            (matches!(args.first().map(String::as_str), Some("test" | "check" | "build" | "clippy")) && !has_check_option(args, "--fix"))
                .then_some(true)
        }
        "npm" | "pnpm" | "yarn" => {
            let script = if first == Some("run") { args.get(1).map(String::as_str) } else { first };
            matches!(script, Some("test" | "build" | "typecheck" | "lint" | "check" | "verify" | "validate"))
                .then_some(true)
        }
        "pytest" => pytest_verification(args).then_some(true),
        "node" => (first == Some("--test") || (matches!(first, Some("--check" | "-c")) && args.len() == 2))
            .then_some(true),
        "tsc" => (!has_check_option(args, "--init") && !has_check_option(args, "--watch") && !has_check_option(args, "-w"))
            .then_some(true),
        "eslint" => (!args.is_empty() && !["--fix", "--fix-dry-run", "--init"]
            .iter().any(|option| has_check_option(args, option))).then_some(true),
        "ruff" => ((first == Some("check") && !["--fix", "--fix-only", "--unsafe-fixes"]
            .iter().any(|option| has_check_option(args, option)))
            || (first == Some("format") && has_check_option(args, "--check"))).then_some(true),
        "go" | "swift" => matches!(first, Some("test" | "build")).then_some(true),
        "gradle" | "gradlew" => (matches!(first, Some("test" | "check" | "build"))
            && args.iter().all(|arg| arg.starts_with('-') || matches!(arg.as_str(), "test" | "check" | "build"))).then_some(true),
        "git" if first == Some("diff") => {
            let options = args[1..].split(|arg| arg == "--").next().unwrap_or_default();
            (has_check_option(options, "--check") && options.iter().all(|arg|
                !arg.starts_with('-') || matches!(arg.as_str(), "--check" | "--cached" | "--staged" | "--no-ext-diff" | "--no-textconv")))
                .then_some(false)
        }
        _ => None,
    }
}

fn pytest_verification(args: &[String]) -> bool {
    !["--collect-only", "--co", "--fixtures", "--fixtures-per-test", "--setup-plan"]
        .iter().any(|option| has_check_option(args, option))
}

#[cfg(test)]
mod tests {
    #[test]
    fn failed_checks_and_partial_shell_edits_invalidate_prior_verification() {
        let directory = tempfile::tempdir().unwrap();
        let artifact = directory.path().join("result.txt");
        std::fs::write(&artifact, "accepted").unwrap();
        let mut guard = BuildContractGuard::new("Build a complete command-line application with tests");
        let mut receipts = vec![result("todo_write", "0/3 todos: Build, inspect, verify", true), result("write", "result.txt", true), result("bash", "cargo test", true)];
        guard.observe_results(&receipts);
        assert!(guard.final_feedback().is_none());
        assert!(guard.completion_audit_feedback().is_some());

        // A newer failed check supersedes an earlier passing observation.
        receipts.push(result("bash", "cargo test", false));
        guard.observe_results(&receipts);
        assert!(guard.final_feedback().is_some(), "a later failed check must not retain verified status");
        receipts.push(result("bash", "cargo test", true));
        guard.observe_results(&receipts);
        assert!(guard.final_feedback().is_none());
        assert!(guard.completion_audit_feedback().is_some());

        // The shell really changes the artifact before reporting failure.
        let command = "printf changed > result.txt; exit 1";
        let run = std::process::Command::new("sh").args(["-c", command])
            .current_dir(directory.path()).output().unwrap();
        assert!(!run.status.success());
        assert_eq!(std::fs::read_to_string(&artifact).unwrap(), "changed");
        receipts.push(result("bash", command, run.status.success()));
        guard.observe_results(&receipts);
        assert!(guard.final_feedback().is_some(), "partial changes need a new check even when the command failed");
        assert!(guard.completion_audit_feedback().is_none());
    }

    #[test]
    fn python_unittest_completion_uses_real_exit_status_and_latest_change() {
        let directory = tempfile::tempdir().unwrap();
        let test_file = directory.path().join("test_result.py");
        std::fs::write(&test_file, "import unittest\nclass Result(unittest.TestCase):\n def test_result(self): self.assertEqual(1, 2)\n").unwrap();
        let run = || std::process::Command::new("python3")
            .args(["-m", "unittest", "-q"])
            .current_dir(directory.path()).output().unwrap();
        let mut guard = BuildContractGuard::new("Build a complete command-line tool and verify it");
        let mut receipts = vec![result("todo_write", "0/3 todos", true), result("write", "result.py", true)];
        let failed = run();
        assert!(!failed.status.success());
        receipts.push(result("bash", "python3 -m unittest -q", failed.status.success()));
        guard.observe_results(&receipts);
        assert!(guard.final_feedback().is_some());
        std::fs::write(&test_file, "import unittest\nclass Result(unittest.TestCase):\n def test_result(self): self.assertEqual(12, 12)\n").unwrap();
        let passed = run();
        assert!(passed.status.success(), "{}", String::from_utf8_lossy(&passed.stderr));
        receipts.push(result("bash", "python3 -m unittest -q", passed.status.success()));
        guard.observe_results(&receipts);
        assert!(guard.final_feedback().is_none(), "a successful standard-library test must release the gate");
        assert!(guard.completion_audit_feedback().is_some());
        assert!(guard.completion_audit_feedback().is_none());
        receipts.push(result("write", "later repair", true));
        guard.observe_results(&receipts);
        assert!(guard.final_feedback().is_some(), "earlier tests cannot verify a later edit");
    }

    #[test]
    fn python_runner_classification_does_not_accept_mentions_or_compound_edits() {
        for command in ["python -m unittest", "python3 -m unittest discover -v", "/usr/bin/python3.12 -B -m unittest test_totals", "python3\t-m\tunittest -q"] {
            assert!(looks_like_verification(command), "{command}");
            let tool = call("bash", serde_json::json!({"command":command}));
            assert!(is_verification_call(&tool));
            assert!(!is_mutation_call(&tool));
        }
        for command in ["echo python3 -m unittest", "python3 -m unittest_extra", "python3 -c 'unittest'", "python_helper -m unittest", "python3 -m unittest; write_changes", "python3 -m unittest && write_changes", "python3 -m unittest > output.py"] {
            assert!(!looks_like_verification(command), "{command}");
        }
    }

    const LIVE_VERIFICATION_SEQUENCE: &str = "python3 -B -m unittest discover -v && python3 -B -m py_compile totals.py test_totals.py && git diff --check";

    #[test]
    fn verification_sequences_require_literal_commands_and_all_successful_checks() {
        for command in [
            LIVE_VERIFICATION_SEQUENCE,
            "python3 -m unittest&&python3 -m py_compile totals.py",
            "python3 -u -B -m unittest discover -v",
            "'/tools with spaces/python3.12' -B -m unittest discover -s 'tests with spaces'",
            "python3 -m py_compile -q 'src/Café totals.py'",
            "python3 -m pytest -k 'refund or Unicode' && git diff --check -- totals.py",
            "cargo +stable test --lib && cargo check && git diff --cached --check",
            "npm test && npm run build && npm run typecheck",
            "pnpm run test && pnpm build",
            "yarn test && tsc --noEmit && eslint src",
            "ruff check src && ruff format --check src",
            "go test ./... && go build ./...",
            "swift test && swift build",
            "./gradlew test --no-daemon && gradle build",
            "node --test test.js && node --check app.js",
        ] {
            assert!(looks_like_verification(command), "{command}");
            for input in [serde_json::json!({"command":command}), serde_json::json!({"cmd":command}), serde_json::json!(command)] {
                let call = call("bash", input);
                assert!(is_verification_call(&call), "{command}");
                assert!(!is_mutation_call(&call), "{command}");
            }
        }
        // Diff hygiene may accompany real verification; by itself it cannot
        // replace the post-change test/build/observation requirement.
        assert!(!looks_like_verification("git diff --check"));
        let mut guard = BuildContractGuard::new("Build a complete command-line application");
        guard.observe_results(&[result("todo_write", "0/3 todos", true)]);
        let verify = call("bash", serde_json::json!({"command":LIVE_VERIFICATION_SEQUENCE}));
        assert_eq!(guard.admit_tool_batch(&[verify.clone()]), BuildAdmission::Allow);
        assert_eq!(guard.admit_tool_batch(&[call("write", serde_json::json!({"path":"totals.py"})), verify]),
            BuildAdmission::Feedback(SEQUENTIAL_VERIFY_FEEDBACK));
    }

    #[test]
    fn verification_sequences_reject_masking_shell_syntax_and_mutating_subcommands() {
        for command in [
            "", "&& python3 -m unittest", "python3 -m unittest &&", "pytest &&& cargo check",
            "echo cargo test", "printf 'pytest'", "echo 'python3 -m py_compile totals.py'",
            "python3 -c 'import unittest'", "sh -c 'cargo test'", "bash -c 'pytest'",
            "pytest; echo done", "python3 -m unittest || true", "pytest | cat", "pytest &",
            "python3 -m unittest\ngit diff --check", "pytest # no tests after this",
            "pytest > totals.py", "pytest >> totals.py", "pytest 2>&1", "pytest < input",
            "python3 - <<'PY'\nimport unittest\nPY", "pytest $(touch changed)", "pytest `touch changed`",
            "pytest \"$(touch changed)\"", "pytest \"`touch changed`\"", "pytest $TEST_ARGS",
            "pytest <(touch changed)", "! pytest", "(pytest)", "{ pytest; }", "pytest\\\n --help",
            "pytest 'unterminated", "pytest \"unterminated", "pytest\0", "pytest *.py",
            "python3 -m unittest && printf changed > totals.py",
            "python3 -m unittest && python3 rewrite.py", "python3 -m unittest && echo pytest",
            "pytest && cargo fmt", "cargo test && rm obsolete.txt", "npm test && npm install",
            "python3 -m py_compile_extra totals.py", "python3 -m py_compile", "python3 -m py_compile -",
            "python_helper -m unittest", "python3..12 -m unittest", "python3 -O -m unittest",
            "python3 -m unittest --help", "pytest --version", "pytest --collect-only", "pytest --co",
            "pytest --fixtures", "npm run test --if-present", "npm test --ignore-scripts",
            "cargo test --help", "gradle test clean", "gradle test --dry-run",
            "tsc --init", "eslint --fix src", "ruff format src", "ruff check --fix src", "cargo clippy --fix",
            "pytest && git diff --check --output=totals.py",
            "npm run lint -- --fix", "pnpm run lint --fix-only", "yarn lint --unsafe-fixes",
            "npm test -- --updateSnapshot", "pnpm test --update-snapshots",
            "node --test --test-update-snapshots", "yarn test --update-snapshot",
        ] {
            assert!(!looks_like_verification(command), "false proof: {command}");
            let call = call("bash", serde_json::json!({"command":command}));
            assert!(is_mutation_call(&call), "{command}");
            assert!(!is_verification_call(&call), "{command}");
        }
    }

    fn append_shell_receipt(guard: &mut BuildContractGuard, receipts: &mut Vec<ToolCallResult>, input: &serde_json::Value, receipt: ToolCallResult) {
        guard.observe_tool_outcome("bash", input, receipts.len(), &receipt);
        receipts.push(receipt);
        guard.observe_results(receipts);
    }

    #[test]
    fn full_shell_receipts_distinguish_equal_truncated_summaries_and_preserve_order() {
        let prefix = format!("python3 -B -m unittest discover -v{}", " ".repeat(150));
        let verified = serde_json::json!({"command":format!("{prefix}&& python3 -B -m py_compile totals.py test_totals.py && git diff --check")});
        let mutated = serde_json::json!({"command":format!("{prefix}&& printf changed > totals.py")});
        let summary = crate::runtime::summarize_tool_input("bash", &verified);
        assert_eq!(summary, crate::runtime::summarize_tool_input("bash", &mutated));
        assert!(summary.ends_with("..."));
        assert_eq!(shell_summary_kind(&summary), ShellCheckKind::Unknown, "a truncated prefix has no proof authority");

        let mut guard = BuildContractGuard::new("Build a complete command-line application");
        let mut receipts = vec![result("todo_write", "0/3 todos", true), result("write", "totals.py", true)];
        guard.observe_results(&receipts);
        let hygiene = serde_json::json!({"command":"git diff --check"});
        append_shell_receipt(&mut guard, &mut receipts, &hygiene, result("bash", "git diff --check", true));
        assert_eq!(guard.final_feedback(), Some(VERIFY_FEEDBACK), "diff hygiene alone cannot prove acceptance");
        let passed = result("bash", &summary, true);
        guard.observe_tool_outcome("bash", &verified, receipts.len(), &passed);
        assert_eq!(guard.final_feedback(), Some(VERIFY_FEEDBACK), "binding must wait for the completed receipt to be observed");
        receipts.push(passed);
        guard.observe_results(&receipts);
        assert_eq!(guard.final_feedback(), None);
        assert!(guard.completion_audit_feedback().is_some());
        guard.observe_results(&receipts);
        assert!(guard.completion_audit_feedback().is_none(), "unchanged evidence must not rearm the audit");

        append_shell_receipt(&mut guard, &mut receipts, &hygiene, result("bash", "git diff --check", true));
        assert_eq!(guard.final_feedback(), None, "a later successful diff check must preserve the test evidence");
        assert!(guard.completion_audit_feedback().is_none());
        append_shell_receipt(&mut guard, &mut receipts, &hygiene, result("bash", "git diff --check", false));
        assert_eq!(guard.final_feedback(), Some(VERIFY_FEEDBACK), "failed supporting checks invalidate earlier proof");
        append_shell_receipt(&mut guard, &mut receipts, &hygiene, result("bash", "git diff --check", true));
        assert_eq!(guard.final_feedback(), Some(VERIFY_FEEDBACK), "hygiene does not replace a new acceptance check");
        append_shell_receipt(&mut guard, &mut receipts, &verified, result("bash", &summary, true));
        assert_eq!(guard.final_feedback(), None);

        append_shell_receipt(&mut guard, &mut receipts, &mutated, result("bash", &summary, true));
        assert_eq!(guard.final_feedback(), Some(VERIFY_FEEDBACK), "same summary, different executed tail");
        append_shell_receipt(&mut guard, &mut receipts, &verified, result("bash", &summary, false));
        assert_eq!(guard.final_feedback(), Some(VERIFY_FEEDBACK), "classification alone cannot turn a failure into success");
        append_shell_receipt(&mut guard, &mut receipts, &verified, result("bash", &summary, true));
        assert_eq!(guard.final_feedback(), None);
        receipts.push(result("write", "later edit", true));
        guard.observe_results(&receipts);
        assert_eq!(guard.final_feedback(), Some(VERIFY_FEEDBACK));
        receipts.push(result("bash", &summary, true));
        guard.observe_results(&receipts);
        assert_eq!(guard.final_feedback(), Some(VERIFY_FEEDBACK), "missing provenance cannot reuse a previous full command");
        assert!(guard.shell_verifications.is_empty());
    }

    #[test]
    fn full_shell_receipts_do_not_bypass_plan_or_unresolved_workflow_gates() {
        let mut guard = BuildContractGuard::new("Build a complete command-line application");
        let input = serde_json::json!({"command":LIVE_VERIFICATION_SEQUENCE});
        let mut receipts = vec![result("write", "totals.py", true)];
        append_shell_receipt(&mut guard, &mut receipts, &input, result("bash", LIVE_VERIFICATION_SEQUENCE, true));
        assert_eq!(guard.final_feedback(), Some(PLAN_FEEDBACK));
        guard.planned = true;
        let transition = |node, state| serde_json::json!({"action":"workflow", "workflow_action":"transition", "workflow_payload":{"node_id":node, "state":state}});
        let rejected = result("work", "workflow transition", false);
        guard.observe_tool_outcome("work", &transition("required", "succeeded"), receipts.len(), &rejected);
        append_shell_receipt(&mut guard, &mut receipts, &input, result("bash", LIVE_VERIFICATION_SEQUENCE, true));
        assert_eq!(guard.final_feedback(), None);
        assert!(guard.pending_work_feedback("sequence-workflow").unwrap().contains("required"));
        assert!(guard.pending_work_feedback("sequence-workflow").is_none());
        guard.observe_tool_outcome("work", &transition("unrelated", "succeeded"), receipts.len(), &result("work", "transition", true));
        let mut response = crate::runtime::FinalResponse { summary:"done".into(), final_markdown:"Saved artifact.".into(), changes_made:vec![], verification:vec![], execution_mode:"test".into(), tool_transcript:vec![] };
        guard.preserve_pending_work("sequence-workflow", &mut response);
        assert_eq!(response.execution_mode, "incomplete_work");
        // The reply stays a human message; the unfinished state rides on the
        // execution mode and summary instead of a prepended warning.
        assert_eq!(response.final_markdown, "Saved artifact.");
    }

    #[test]
    fn compound_python_verification_uses_actual_exit_status_and_rejects_later_mutation() {
        use crate::runtime::ToolCall;
        let directory = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(directory.path());
        let root = directory.path();
        let init = std::process::Command::new("git").args(["init", "--quiet"])
            .current_dir(root).output().unwrap();
        assert!(init.status.success(), "{}", String::from_utf8_lossy(&init.stderr));
        std::fs::write(root.join("totals.py"), "value = 12\n").unwrap();
        let write_test = |expected| std::fs::write(root.join("test_totals.py"), format!(
            "import unittest\nfrom totals import value\nclass Result(unittest.TestCase):\n def test_value(self): self.assertEqual(value, {expected})\n")).unwrap();
        let executor = crate::tools::ToolExecutor::new(root).unwrap();
        let run = |command: &str| {
            let input = serde_json::json!({"command":command, "timeout_secs":10});
            let receipt = executor.execute(ToolCall { tool_name:"bash".into(), input:input.clone() });
            (input, receipt)
        };
        let mut guard = BuildContractGuard::new("Build a complete command-line application");
        let mut receipts = vec![result("todo_write", "0/3 todos", true), result("write", "totals.py", true)];

        write_test(13);
        let (input, failed) = run(LIVE_VERIFICATION_SEQUENCE);
        assert!(!failed.success, "{}", failed.output);
        append_shell_receipt(&mut guard, &mut receipts, &input, failed);
        assert_eq!(guard.final_feedback(), Some(VERIFY_FEEDBACK));

        write_test(12);
        receipts.push(result("write", "test_totals.py", true));
        let (input, passed) = run(LIVE_VERIFICATION_SEQUENCE);
        assert!(passed.success, "{}", passed.output);
        assert!(passed.output.contains("Ran 1 test"));
        append_shell_receipt(&mut guard, &mut receipts, &input, passed);
        assert_eq!(guard.final_feedback(), None);
        assert!(guard.completion_audit_feedback().is_some());

        std::fs::write(root.join("broken.py"), "def broken(\n").unwrap();
        let (input, failed_compile) = run("python3 -B -m unittest discover -v && python3 -B -m py_compile broken.py && git diff --check");
        assert!(!failed_compile.success && failed_compile.output.contains("Ran 1 test"), "{}", failed_compile.output);
        append_shell_receipt(&mut guard, &mut receipts, &input, failed_compile);
        assert_eq!(guard.final_feedback(), Some(VERIFY_FEEDBACK));

        let (input, passed) = run(LIVE_VERIFICATION_SEQUENCE);
        assert!(passed.success, "{}", passed.output);
        append_shell_receipt(&mut guard, &mut receipts, &input, passed);
        let (input, changed) = run("python3 -B -m unittest discover -v && printf changed > result.txt");
        assert!(changed.success, "{}", changed.output);
        assert_eq!(std::fs::read_to_string(root.join("result.txt")).unwrap(), "changed");
        append_shell_receipt(&mut guard, &mut receipts, &input, changed);
        assert_eq!(guard.final_feedback(), Some(VERIFY_FEEDBACK));

        // These receipts really report success, but do not establish that a
        // successful test ran. A subprocess/pipe/list can mask its failure.
        write_test(13);
        for command in [
            "echo 'cargo test'",
            "python3 -B -m unittest discover -v || true",
            "python3 -B -m unittest discover -v | cat",
            "python3 -B -m unittest discover -v; true",
            "echo \"$(python3 -B -m unittest discover -v)\"",
        ] {
            let (input, masked) = run(command);
            assert!(masked.success, "{}", masked.output);
            append_shell_receipt(&mut guard, &mut receipts, &input, masked);
            assert_eq!(guard.final_feedback(), Some(VERIFY_FEEDBACK), "{command}");
        }
    }

    #[test]
    fn workflow_completion_is_read_from_storage_after_process_restart() {
        use crate::runtime::{company::CompanyStore, workflow::*};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        let store = std::sync::Arc::new(CompanyStore::open(&path).unwrap());
        store.ensure_full_catalog_team().unwrap();
        let scheduler = DurableWorkflowScheduler::new(store.clone(), ConcurrencyPolicy::default()).unwrap();
        let request: WorkflowPlanRequest = serde_json::from_value(serde_json::json!({
            "idempotency_key":"restart-guard", "contract":{"title":"Deliver", "objective":"Verified outcome",
            "budget": WorkflowBudget::default(), "ownership":{"scope":"agent", "owner_agent_id":"coder"},
            "evidence_requirements":[{"kind":"outcome", "description":"Inspect actual result", "minimum_receipts":1,"required":true}]},
            "assignments":[{"key":"outcome", "owner_agent_id":"coder", "title":"Deliver", "outcome":"Verified outcome"}]
        })).unwrap();
        let plan = scheduler.install_plan(request).unwrap();
        let child = |pending: bool| {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "runtime::build_contract::tests::workflow_guard_restart_child", "--ignored", "--nocapture"])
                .env("PHOENIX_GUARD_DB", &path).env("PHOENIX_GUARD_RUN", &plan.run_id)
                .env("PHOENIX_GUARD_PENDING", pending.to_string()).output().unwrap();
            assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        };
        child(true);
        let (_, mut leases) = scheduler.tick(&plan.run_id, "coder", chrono::Utc::now()).unwrap();
        let lease = leases.remove(0);
        scheduler.transition_node(&lease.token.node_id, "review", Some(&lease.token), WorkflowPhase::Review,
            WorkflowNodeState::Review, RestartState::Fresh, "Review", None, None, None, None).unwrap();
        assert!(scheduler.transition_node(&lease.token.node_id, "missing", Some(&lease.token), WorkflowPhase::Commit,
            WorkflowNodeState::Succeeded, RestartState::Terminal, "Done", None, None, None, None).is_err());
        child(true);
        scheduler.record_evidence(&lease.token, EvidenceReceipt {receipt_id:"receipt_verified".into(),kind:"outcome".into(),
            summary:"Inspected output".into(),uri:None,content_hash:None,verified:true,recorded_at:chrono::Utc::now().to_rfc3339()}).unwrap();
        scheduler.transition_node(&lease.token.node_id, "committed", Some(&lease.token), WorkflowPhase::Commit,
            WorkflowNodeState::Succeeded, RestartState::Terminal, "Verified", None, None, None, None).unwrap();
        child(false);
    }

    #[test]
    #[ignore = "subprocess helper with private store and selected workflow"]
    fn workflow_guard_restart_child() {
        let store = crate::runtime::company::CompanyStore::open(std::path::Path::new(&std::env::var("PHOENIX_GUARD_DB").unwrap())).unwrap();
        let mut guard = BuildContractGuard::new("Continue");
        assert!(guard.pending_workflows_in_store(&store).is_empty(), "unselected old work cannot contaminate another turn");
        guard.bind_workflow_run(&std::env::var("PHOENIX_GUARD_RUN").unwrap());
        assert_eq!(!guard.pending_workflows_in_store(&store).is_empty(), std::env::var("PHOENIX_GUARD_PENDING").unwrap() == "true");
    }

    #[test]
    fn completed_todos_and_other_nodes_cannot_hide_a_rejected_commit() {
        let dir = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(dir.path());
        let session = "rejected-commit";
        crate::tools::todo_execute(crate::tools::TodoWriteInput { todos: vec![
            crate::tools::TodoItem { task: "Produce artifact".into(), completed: true, ..Default::default() },
        ] }, session).unwrap();
        let mut guard = BuildContractGuard::new("Produce an artifact");
        guard.observe_results(&[result("todo_write", "1/1 todos", true)]);
        let input = |node, state| serde_json::json!({"action":"workflow", "workflow_action":"transition", "workflow_payload":{"node_id":node, "state":state}});
        guard.observe_workflow_outcome("work", &input("original", "succeeded"), false, "Missing evidence");
        guard.observe_workflow_outcome("work", &input("other", "succeeded"), true, "Done");
        guard.observe_workflow_outcome("work", &input("original", "review"), true, "In review");
        assert!(guard.pending_work(session).unwrap().contains("original"));
        guard.observe_workflow_outcome("work", &input("original", "canceled"), true, "Canceled");
        assert!(guard.pending_work(session).is_some());
        guard.observe_workflow_outcome("work", &input("original", "succeeded"), true, "Done after repair");
        assert!(guard.pending_work(session).is_none());
    }

    #[test]
    fn pending_work_reminder_is_bounded_and_preserves_only_current_turn_scope() {
        use crate::tools::{TodoItem, TodoWriteInput};
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(dir.path());
        let session = "pending-check";
        let write_plan = |completed| crate::tools::todo_execute(TodoWriteInput { todos: vec![
            TodoItem { task: "Proofread Social assignment".into(), completed: true, ..Default::default() },
            TodoItem { task: "Finish activity hour plan".into(), completed, ..Default::default() },
        ] }, session).unwrap();
        write_plan(false);
        let mut guard = BuildContractGuard::new("Review the requested documents");
        assert!(guard.pending_work_feedback(session).is_none(), "old todos must not block an unrelated turn");
        guard.observe_results(&[result("todo_write", "1/2 todos", false)]);
        assert!(guard.pending_work_feedback(session).is_none(), "failed writes cannot claim this turn owns an old plan");
        guard.observe_results(&[result("todo_write", "1/2 todos", false), result("todo_write", "1/2 todos", true)]);
        let reminder = guard.pending_work_feedback(session).unwrap();
        assert!(reminder.contains("Finish activity hour plan"));
        assert!(!reminder.contains("Proofread Social assignment"));
        assert!(guard.pending_work_feedback(session).is_none(), "never loop on an unchanged blocker");
        let mut response = crate::runtime::FinalResponse { summary: "done".into(), final_markdown: "Proofreading is ready.".into(), changes_made: vec![], verification: vec![], execution_mode: "test".into(), tool_transcript: vec![] };
        guard.preserve_pending_work(session, &mut response);
        assert_eq!(response.execution_mode, "incomplete_work");
        // Open to-dos show in the To-dos panel, not appended to the reply.
        assert_eq!(response.final_markdown, "Proofreading is ready.");
        write_plan(true);
        assert!(guard.pending_work(session).is_none(), "durable completion clears the outstanding item");
    }

    #[test]
    fn group_inspection_does_not_inherit_the_builders_completion_gate() {
        let request="Build a complete Blender workflow, render a preview, and save the deliverables.";
        let call=crate::runtime::RequestedToolCall{tool_name:"bash".into(),input:serde_json::json!({"command":"blender --background baseline.blend --python inspect.py"})};
        let guard=super::BuildContractGuard::for_assignment(request,true);
        assert!(matches!(super::BuildContractGuard::for_assignment(request,false).admit_tool_batch(std::slice::from_ref(&call)),super::BuildAdmission::Feedback(_)));
        assert_eq!(guard.admit_tool_batch(&[call]),super::BuildAdmission::Allow);
        assert!(guard.final_feedback().is_none());
    }
    use super::*;

    fn call(tool: &str, input: serde_json::Value) -> RequestedToolCall {
        RequestedToolCall {
            tool_name: tool.to_string(),
            input,
        }
    }

    #[test]
    fn completion_audit_rechecks_repairs_without_repeating_unchanged_evidence() {
        let mut guard = BuildContractGuard::new("Create a detailed Blender scene with realistic peel");
        let mut results = vec![result("write", "banana.blend", true), result("image_analyze", "banana.png", true)];
        guard.observe_results(&results);
        assert!(guard.completion_audit_feedback().is_some());
        results.extend([result("image_analyze", "banana.png", true), result("write", "failed repair", false), result("todo_write", "4/4 todos", true)]);
        guard.observe_results(&results);
        assert!(guard.completion_audit_feedback().is_none(), "read-only checks and failed repairs do not restart the audit");
        results.push(result("write", "repair banana.blend", true));
        guard.observe_results(&results);
        assert!(guard.completion_audit_feedback().is_none(), "the repaired artifact first needs observation");
        results.push(result("image_analyze", "repaired banana.png", true));
        guard.observe_results(&results);
        assert!(guard.completion_audit_feedback().is_some(), "the old audit cannot accept a later repair");
        guard.observe_results(&results);
        assert!(guard.completion_audit_feedback().is_none(), "replaying the same receipts must not rearm");
    }

    fn result(tool: &str, summary: &str, success: bool) -> ToolCallResult {
        ToolCallResult {
            tool_name: tool.to_string(),
            input_summary: summary.to_string(),
            success,
            output: String::new(),
        }
    }

    #[test]
    fn native_batch_uses_its_own_completed_capture_without_a_duplicate_call() {
        let mut guard = BuildContractGuard::new("Create a complete Blender app presentation from scratch");
        let mut results = vec![result("todo_write", "0/3 todos: Model, present, verify", true)];
        guard.observe_results(&results);
        let mut captured = result("computer_window_act", "ran 3 window action(s)", true);
        captured.output = crate::tools::format_tool_output("computer_window_act", crate::tools::ToolOutput {
            summary: "ran 3 window action(s)".into(),
            content: "Screenshot saved: /tmp/owned/window.png\nWindow image: 1440x960\nCompleted 3 of 3 window action(s)".into(),
        });
        results.push(captured);
        guard.observe_results(&results);
        assert_eq!(guard.final_feedback(), None);
        for output in [
            "Completed 3 of 3 window action(s)",
            "Completed 3 of 3 window action(s)\n(end-of-batch window capture unavailable)",
            "All input actions were delivered, then focus changed",
            "Completed 3 of 3\nScreenshot saved: /tmp/text-inside-a-receipt.png",
        ] {
            let mut uncaptured = result("computer_window_act", "ran 3 window action(s)", true);
            uncaptured.output = crate::tools::format_tool_output("computer_window_act", crate::tools::ToolOutput {
                summary: "ran 3 window action(s)".into(), content: output.into(),
            });
            results.push(uncaptured);
            guard.observe_results(&results);
            assert_eq!(guard.final_feedback(), Some(VERIFY_FEEDBACK));
        }
    }

    #[test]
    fn native_window_edits_require_a_later_window_observation() {
        for tool in ["computer_window_act", "computer_key", "computer_drag"] {
            let mut guard = BuildContractGuard::new("Create a complete Blender app presentation from scratch");
            assert_eq!(guard.admit_tool_batch(&[call("computer_capture_window", serde_json::json!({"id":1}))]), BuildAdmission::Allow);
            let mut results = vec![result("todo_write", "0/3 todos: Model, present, verify", true)];
            guard.observe_results(&results);
            assert_eq!(guard.admit_tool_batch(&[
                call(tool, serde_json::json!({})),
                call("computer_capture_window", serde_json::json!({"id":1})),
            ]), BuildAdmission::Feedback(SEQUENTIAL_VERIFY_FEEDBACK));
            results.push(result(tool, "window edit", true));
            guard.observe_results(&results);
            assert_eq!(guard.final_feedback(), Some(VERIFY_FEEDBACK));
            results.push(result("computer_capture_window", "observed window", false));
            guard.observe_results(&results);
            assert_eq!(guard.final_feedback(), Some(VERIFY_FEEDBACK));
            results.push(result("computer_capture_window", "observed window", true));
            guard.observe_results(&results);
            assert_eq!(guard.final_feedback(), None);
        }
    }

    #[test]
    fn substantial_build_requires_a_standalone_compact_plan() {
        let guard = BuildContractGuard::new(
            "Build the app end to end, connect the backend, and test the real workflow",
        );
        assert_eq!(
            guard.admit_tool_batch(&[call(
                "write",
                serde_json::json!({"path":"src/app.rs", "content":"..."})
            )]),
            BuildAdmission::Feedback(PLAN_FEEDBACK)
        );
        assert_eq!(
            guard.admit_tool_batch(&[call(
                "todo_write",
                serde_json::json!({"todos":[
                    {"task":"Thin slice", "completed":false},
                    {"task":"Finish slices", "completed":false},
                    {"task":"Acceptance", "completed":false}
                ]})
            )]),
            BuildAdmission::Allow
        );
        assert!(matches!(
            guard.admit_tool_batch(&[call(
                "todo_write",
                serde_json::json!({"todos":[{"task":"Too vague", "completed":false}]})
            )]),
            BuildAdmission::Feedback(_)
        ));
    }

    #[test]
    fn mutation_needs_new_successful_verification() {
        let mut guard = BuildContractGuard::new(
            "Implement the complete backend service, then wire and test the API",
        );
        let mut results = vec![result("todo_write", "0/3 todos: Thin slice", true)];
        guard.observe_results(&results);
        assert_eq!(guard.final_feedback(), None);
        results.push(result("bash", "cargo test --lib", true));
        results.push(result("write", "src/lib.rs", true));
        guard.observe_results(&results);
        assert_eq!(guard.final_feedback(), Some(VERIFY_FEEDBACK));
        results.push(result("bash", "cargo test --lib", false));
        guard.observe_results(&results);
        assert_eq!(guard.final_feedback(), Some(VERIFY_FEEDBACK));
        results.push(result("bash", "cargo test --lib", true));
        guard.observe_results(&results);
        assert_eq!(guard.final_feedback(), None);
    }

    #[test]
    fn simple_work_and_explanations_are_not_forced_into_ceremony() {
        for request in [
            "Fix the typo in the button label",
            "Add a button",
            "Explain the architecture",
            "Review this API design",
        ] {
            let guard = BuildContractGuard::new(request);
            assert_eq!(guard.final_feedback(), None, "{request}");
        }
    }

    #[test]
    fn make_mentions_advice_and_simple_tasks_do_not_become_substantial_builds() {
        for request in [
            "Explain how to make a realistic illustration.",
            "Could you explain how to make a realistic illustration?",
            "Review a plan to make a detailed illustration.",
            "The future request is: 'Make a realistic illustration.' Explain its meaning.",
            "\"Make a realistic illustration\" is a future task, not today's request.",
            "Tomorrow I may ask you to make a realistic illustration. For now, describe the steps.",
            "Do not make a realistic illustration. Just inspect the reference.",
            "Make sure the realistic illustration looks right.",
            "Please make no changes to the realistic illustration.",
            "Could you make nothing new and inspect the realistic illustration?",
            "Make the UI label shorter.",
            "Make a cube in a scene.",
            // A bounded intent heuristic still requires a recognized artifact;
            // this repair does not claim general semantic classification.
            "Make me a realistic banana.",
        ] {
            assert!(!is_substantial_build_request(request), "{request}");
        }
    }

    #[test]
    fn read_only_or_waiting_finish_never_requires_an_invented_build_plan() {
        // Even a true build may need to yield for a user decision before any
        // mutation. The request classifier must not trap that branch in a loop.
        let mut guard = BuildContractGuard::new("Build the product end to end and verify the full application");
        guard.observe_results(&[result("ask_user", "Choose the required tolerance", true)]);
        assert_eq!(guard.final_feedback(), None);
        assert_eq!(guard.admit_tool_batch(&[call("final_answer", serde_json::json!({"final_markdown":"Waiting for your tolerance choice."}))]), BuildAdmission::Allow);
        assert_eq!(guard.admit_tool_batch(&[call("write", serde_json::json!({"path":"model.py","content":"..."}))]), BuildAdmission::Feedback(PLAN_FEEDBACK));
    }

    #[test]
    fn native_finish_cannot_bypass_post_mutation_verification() {
        let mut guard = BuildContractGuard::new("Build the product end to end and verify the full application");
        let finish = call("final_answer", serde_json::json!({"final_markdown":"Done"}));
        let edit = call("write", serde_json::json!({"path":"model.py","content":"..."}));
        assert_eq!(guard.admit_tool_batch(&[finish.clone(), edit]), BuildAdmission::Feedback(SEQUENTIAL_VERIFY_FEEDBACK));
        let mut results = vec![result("todo_write", "0/3 todos: Thin slice", true), result("write", "model.py", true)];
        guard.observe_results(&results);
        assert_eq!(guard.admit_tool_batch(&[finish.clone()]), BuildAdmission::Feedback(VERIFY_FEEDBACK));
        results.push(result("bash", "cargo test --lib", true));
        guard.observe_results(&results);
        assert_eq!(guard.admit_tool_batch(&[finish]), BuildAdmission::Allow);
    }

    #[test]
    fn future_ui_tool_architecture_question_is_not_a_build() {
        assert!(!is_substantial_build_request(
            "A visual tool framework can build custom UIs. What do you think? Look at our code and think about how to integrate it into Phoenix as a set of tools."
        ));
    }

    #[test]
    fn compound_multi_surface_build_is_substantial() {
        assert!(is_substantial_build_request(
            "Create an automation that reads the website, updates a spreadsheet, then sends email"
        ));
        assert!(is_substantial_build_request(
            "Fix the UI, preserve task history, and stop the tool loops"
        ));
    }

    #[test]
    fn setup_and_inspection_remain_available_before_the_plan() {
        let guard = BuildContractGuard::new(
            "Fully rebuild the frontend app, connect settings, and test every flow",
        );
        assert_eq!(
            guard.admit_tool_batch(&[
                call("read", serde_json::json!({"path":"src/app.tsx"})),
                call(
                    "design_reference",
                    serde_json::json!({"path":"taste/SKILL.md"})
                )
            ]),
            BuildAdmission::Allow
        );
    }

    #[test]
    fn mutation_and_its_proof_cannot_run_concurrently() {
        let mut guard = BuildContractGuard::new(
            "Build the complete backend service, wire the API, and test it end to end",
        );
        guard.observe_results(&[result("todo_write", "0/3 todos: Thin slice", true)]);
        assert_eq!(
            guard.admit_tool_batch(&[
                call(
                    "write",
                    serde_json::json!({"path":"src/lib.rs", "content":"..."})
                ),
                call("bash", serde_json::json!({"command":"cargo test --lib"}))
            ]),
            BuildAdmission::Feedback(SEQUENTIAL_VERIFY_FEEDBACK)
        );
    }

    #[test]
    fn successful_visual_observation_still_requires_original_outcome_audit_once() {
        let request = "Create a polished detailed ripe banana in Blender, with natural proportions and surface variation";
        let mut guard = BuildContractGuard::new(request);
        assert!(guard.completion_audit_feedback().is_none());
        // These are the same state transitions observed after a real mutation
        // and successful post-change observation; neither proves craft.
        guard.changed = true;
        assert!(guard.completion_audit_feedback().is_none());
        guard.verified_after_change = true;
        let audit = guard.completion_audit_feedback().expect("must audit before accepting a final");
        assert!(audit.contains(request));
        assert!(audit.contains("accepted repair does not prove the full goal"));
        assert!(audit.contains("strongest known version"));
        assert!(audit.contains("affected integration"));
        assert!(guard.completion_audit_feedback().is_none(), "bounded, not an endless self-critique loop");
        let mut inspection = BuildContractGuard::for_assignment(request, true);
        inspection.changed = true;
        inspection.verified_after_change = true;
        assert!(inspection.completion_audit_feedback().is_none(), "read-only peers do not inherit builder execution");
    }
}
