//! Stable, provider-agnostic efficiency policy for agent turns.
//!
//! The expensive failure mode this guards is not a single large prompt. It is
//! replaying that prompt for a long sequence of one-action rounds. A real
//! browser-login trace reached 146k--151k input tokens for eleven consecutive
//! rounds while advancing one desktop action at a time. The policy below keeps
//! the model fully capable while making the shortest reliable execution path
//! explicit, keeps completed tool inputs as compact receipts, and provides an
//! offline guardrail that makes the replay pattern measurable in tests and
//! diagnostics.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Small and cache-stable enough to inject into every agent role.
pub const EXECUTION_EFFICIENCY_CONTRACT: &str = r#"=== EXECUTION ECONOMY ===
Use the fewest reliable model rounds, not the fewest capabilities. Pick the
shortest safe lane in this order: an already-connected structured app/API,
then Phoenix's managed browser for websites, then desktop control only for a
non-web app or a browser-lane failure with concrete evidence. Never drive the
user's everyday browser for ordinary website work.

Batch independent actions that operate on the same observed state. One page
observation followed by one browser_act/computer_act batch is the normal shape;
do not spend one model round per click, field, key, or narration. Re-observe
only after state changed or a target became ambiguous. Reuse current tool
results and exact anchors instead of repeating a read/search/status call.

Before asking the user, silently compare the cost of continuing with the cost
of interruption. Ask only for a credential, approval, or decision Phoenix
cannot safely derive. A free account may be created through the managed
browser under the configured account policy; money always requires approval.
Stop repeating a failing strategy without changed evidence. Switch to a relevant
method or advance independent work; a failed strategy is not a task-level blocker. Routine web work should usually finish in a few
provider rounds, not a long chain of single-action rounds.
=== END EXECUTION ECONOMY ==="#;

/// Runtime task shape used for repetition guidance. This is deliberately broad:
/// it never chooses a product integration by name, it only recognizes surfaces
/// whose accidental one-click-per-round loops are especially costly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EconomyTaskClass {
    /// A stateful website task expected to produce or download an artifact.
    WebArtifact,
    /// A task whose shape spans several surfaces, deliverables, or recurring
    /// updates. Domain does not matter; the extra headroom is earned by the
    /// work graph rather than a keyword such as a product or profession.
    MultiSurfaceHardTask,
    /// Coding, research, planning, and every other task.
    General,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EconomyLimits {
    pub one_tool_streak: usize,
}

impl EconomyTaskClass {
    pub fn limits(self) -> EconomyLimits {
        match self {
            Self::WebArtifact => EconomyLimits { one_tool_streak: 3 },
            Self::MultiSurfaceHardTask => EconomyLimits { one_tool_streak: 4 },
            Self::General => EconomyLimits {
                // Repeating the exact same single-call phase three times is
                // enough to prove a stalled strategy. Legitimate dependent
                // work is unaffected because a new phase or distinct input
                // resets this counter.
                one_tool_streak: 3,
            },
        }
    }
}

pub fn classify_task(request: &str) -> EconomyTaskClass {
    let lower = request.to_ascii_lowercase();
    // Classify compound work by shape before considering the narrower website
    // artifact lane. A task that happens to download an image but also spans
    // mail, files, and a calendar needs the multi-surface repetition policy.
    let surface_categories = [
        ["website", "web site", "browser", "http://", "https://"].as_slice(),
        ["email", "inbox", "message", "chat", "thread"].as_slice(),
        ["calendar", "schedule", "reminder", "task list"].as_slice(),
        ["document", "file", "pdf", "spreadsheet", "report"].as_slice(),
        ["api", "connected app", "service", "database"].as_slice(),
        ["desktop app", "native app", "editor", "file picker"].as_slice(),
    ];
    let surfaces = surface_categories
        .iter()
        .filter(|terms| terms.iter().any(|term| lower.contains(term)))
        .count();
    let sequence_markers = lower.matches(" and ").count()
        + lower.matches(" then ").count()
        + lower.matches(" after ").count()
        + lower.matches(',').count();
    let deliverables = [
        "create",
        "prepare",
        "update",
        "send",
        "download",
        "export",
        "reconcile",
        "publish",
        "return",
    ]
    .iter()
    .filter(|term| lower.contains(**term))
    .count();
    let recurring = ["daily", "weekly", "recurring", "ongoing", "every "]
        .iter()
        .any(|term| lower.contains(term));
    if surfaces >= 3 || (surfaces >= 2 && (sequence_markers >= 2 || deliverables >= 2 || recurring))
    {
        return EconomyTaskClass::MultiSurfaceHardTask;
    }

    let web = [
        "website", "web site", "browser", "http://", "https://", "sign in", "log in", "online",
    ]
    .iter()
    .any(|term| lower.contains(term));
    let artifact_count = [
        "generate",
        "download",
        "video",
        "image",
        "export",
        "save the finished",
    ]
    .iter()
    .filter(|term| lower.contains(**term))
    .count();
    // A site named only by product can still be recognized by the expected
    // workflow shape: create an artifact, wait for it, then download/return
    // the file. No brand catalog belongs in runtime policy.
    let downloadable_artifact = artifact_count >= 2
        && (lower.contains("download")
            || lower.contains("return the file")
            || lower.contains("save the finished"));
    if (web && artifact_count > 0) || downloadable_artifact {
        return EconomyTaskClass::WebArtifact;
    }

    EconomyTaskClass::General
}

/// The concrete surface decision for a request. A structured lane wins only
/// after live discovery proves that exact service and action are connected and
/// capable; otherwise website artifacts use the managed browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreferredSurface {
    ConnectedApp,
    ManagedBrowser,
    Any,
}

pub fn preferred_surface(request: &str, proven_connected_capability: bool) -> PreferredSurface {
    if proven_connected_capability {
        return PreferredSurface::ConnectedApp;
    }
    match classify_task(request) {
        EconomyTaskClass::WebArtifact => PreferredSurface::ManagedBrowser,
        EconomyTaskClass::MultiSurfaceHardTask => PreferredSurface::Any,
        EconomyTaskClass::General => PreferredSurface::Any,
    }
}

/// Reject the expensive wrong-surface path before it executes. Native desktop
/// work remains untouched; website artifacts and explicit daily-browser
/// targets are prevented from wandering through `computer_*` pixel calls.
pub fn route_violation(
    _request: &str,
    tool_name: &str,
    input: &serde_json::Value,
) -> Option<String> {
    let wrong_computer_surface =
        tool_name.starts_with("computer_") && has_explicit_browser_target(input);
    if wrong_computer_surface {
        return Some(format!(
            "RUNTIME SURFACE BOUNDARY: `{tool_name}` was not executed. This is website work: use this coworker's managed `browser_*` profile. Use a connected app only after live Composio discovery proves the exact service/action; never inspect or drive a daily browser with computer use."
        ));
    }
    None
}

fn has_explicit_browser_target(input: &serde_json::Value) -> bool {
    const TARGET_FIELDS: &[&str] = &[
        "app",
        "application",
        "process",
        "window",
        "window_title",
        "target",
        "url",
        "uri",
    ];
    match input {
        serde_json::Value::Object(object) => object.iter().any(|(key, value)| {
            (TARGET_FIELDS.contains(&key.to_ascii_lowercase().as_str())
                && target_value_strings(value).any(is_browser_target))
                || has_explicit_browser_target(value)
        }),
        serde_json::Value::Array(values) => values.iter().any(has_explicit_browser_target),
        _ => false,
    }
}

fn target_value_strings(value: &serde_json::Value) -> Box<dyn Iterator<Item = &str> + '_> {
    match value {
        serde_json::Value::String(value) => Box::new(std::iter::once(value.as_str())),
        serde_json::Value::Array(values) => Box::new(values.iter().flat_map(target_value_strings)),
        serde_json::Value::Object(values) => {
            Box::new(values.values().flat_map(target_value_strings))
        }
        _ => Box::new(std::iter::empty()),
    }
}

fn is_browser_target(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return true;
    }
    ["zen", "firefox", "chrome", "chromium", "brave", "edge"]
        .iter()
        .any(|browser| {
            lower == *browser
                || lower.starts_with(&format!("{browser} "))
                || lower.ends_with(&format!(" {browser}"))
                || lower.contains(&format!(" {browser} "))
                || lower.contains(&format!("mozilla {browser}"))
                || lower.contains(&format!("google {browser}"))
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EconomyAdmission {
    Allow,
    Feedback(String),
    Stop(String),
}

/// Stateful, provider-independent efficiency guide shared by both production
/// loops. Context pressure belongs exclusively to the anchored compactor; this
/// guard only corrects repeated calls and records whole-turn telemetry.
#[derive(Debug, Clone)]
pub struct ExecutionEconomyGuard {
    request: String,
    class: EconomyTaskClass,
    limits: EconomyLimits,
    turn_provider_rounds: usize,
    turn_tool_calls: usize,
    turn_replay_input_tokens: u64,
    one_tool_streak: usize,
    streak_feedback_issued: bool,
    last_single_phase: Option<&'static str>,
    last_single_call_fingerprint: Option<String>,
    progress_since_admission: bool,
    processed_results: usize,
    successes: usize,
    failures: usize,
    successful_call_fingerprints: HashSet<String>,
    weekly_used_at_start: u64,
    weekly_budget: u64,
    weekly_warning_percent: u64,
    weekly_hard_stop: bool,
    weekly_notice_emitted: bool,
}

impl ExecutionEconomyGuard {
    pub fn new(request: &str) -> Self {
        let class = classify_task(request);
        Self {
            request: request.to_string(),
            class,
            limits: class.limits(),
            turn_provider_rounds: 0,
            turn_tool_calls: 0,
            turn_replay_input_tokens: 0,
            one_tool_streak: 0,
            streak_feedback_issued: false,
            last_single_phase: None,
            last_single_call_fingerprint: None,
            progress_since_admission: false,
            processed_results: 0,
            successes: 0,
            failures: 0,
            successful_call_fingerprints: HashSet::new(),
            weekly_used_at_start: 0,
            weekly_budget: 0,
            weekly_warning_percent: 70,
            weekly_hard_stop: false,
            weekly_notice_emitted: false,
        }
    }

    pub fn with_weekly_policy(
        mut self,
        used: u64,
        budget: u64,
        warning_percent: u64,
        hard_stop: bool,
    ) -> Self {
        self.weekly_used_at_start = used;
        self.weekly_budget = budget;
        self.weekly_warning_percent = warning_percent.clamp(1, 100);
        self.weekly_hard_stop = hard_stop;
        self
    }

    pub fn take_weekly_notice(&mut self) -> Option<String> {
        if self.weekly_notice_emitted || self.weekly_budget == 0 {
            return None;
        }
        let used = self
            .weekly_used_at_start
            .saturating_add(self.turn_replay_input_tokens);
        let warning = self
            .weekly_budget
            .saturating_mul(self.weekly_warning_percent)
            / 100;
        // Do not toast on every turn after the threshold. `/burn` and
        // Settings remain the passive always-available surface; the runtime
        // notice fires only on the turn that crosses the configured line.
        if self.weekly_used_at_start >= warning || used < warning {
            return None;
        }
        self.weekly_notice_emitted = true;
        Some(format!(
            "Local 7-day provider-input estimate is {:.1}% of the configured {}M-token planning budget. This is a Phoenix replay guard, not a vendor quota percentage; provider subscription units are dynamic and unpublished.",
            100.0 * used as f64 / self.weekly_budget.max(1) as f64,
            self.weekly_budget / 1_000_000
        ))
    }

    pub fn class(&self) -> EconomyTaskClass {
        self.class
    }

    pub fn limits(&self) -> EconomyLimits {
        self.limits
    }

    pub fn before_provider_round(&mut self) -> EconomyAdmission {
        if self.weekly_hard_stop
            && self.weekly_budget > 0
            && self
                .weekly_used_at_start
                .saturating_add(self.turn_replay_input_tokens)
                >= self.weekly_budget
        {
            return EconomyAdmission::Stop(format!(
                "configured local seven-day input budget reached ({} tokens). This is a Phoenix token-replay boundary, not a claim about the provider's unpublished subscription quota; raise or disable the hard stop in Settings → Advanced to continue",
                self.weekly_budget
            ));
        }
        // Provider-round and replay totals are telemetry, never a reason to
        // erase context or restart a turn. The context-aware compactor owns
        // rollover and preserves an anchored summary plus recent verbatim work.
        self.turn_provider_rounds += 1;
        EconomyAdmission::Allow
    }

    pub fn record_provider_usage(&mut self, input_tokens: u32) {
        self.turn_replay_input_tokens = self
            .turn_replay_input_tokens
            .saturating_add(u64::from(input_tokens));
    }

    /// Synchronize completed results without coupling this module to a
    /// particular loop's internal result type.
    pub fn observe_results<'a>(
        &mut self,
        results: impl IntoIterator<Item = (&'a str, &'a str, bool, &'a str)>,
    ) {
        let mut saw_new = false;
        let mut saw_progress = false;
        let mut newly_processed = 0usize;
        for (index, (tool, input, success, output)) in results.into_iter().enumerate() {
            if index < self.processed_results {
                continue;
            }
            saw_new = true;
            newly_processed = newly_processed.saturating_add(1);
            if success {
                self.successes += 1;
                // Animations, clocks, and generated DOM ids can change an
                // observation payload without advancing the task. Distinct
                // call intent is the stable progress signal; legitimate
                // navigate -> observe -> act transitions are recognized by
                // the semantic phase check in `admit_tool_batch`.
                let _ = output;
                saw_progress |= self
                    .successful_call_fingerprints
                    .insert(format!("{tool}|{}", take_chars(input, 240)));
            } else {
                self.failures += 1;
            }
        }
        if saw_new {
            self.progress_since_admission = saw_progress;
        }
        self.processed_results = self.processed_results.saturating_add(newly_processed);
    }

    pub fn admit_tool_batch(&mut self, calls: &[(String, serde_json::Value)]) -> EconomyAdmission {
        let work = calls
            .iter()
            .filter(|(name, _)| name != "final_answer")
            .collect::<Vec<_>>();
        if work.is_empty() {
            return EconomyAdmission::Allow;
        }
        for (tool, input) in &work {
            if let Some(feedback) = route_violation(&self.request, tool, input) {
                return EconomyAdmission::Feedback(feedback);
            }
        }
        if work.len() == 1 {
            let phase = semantic_tool_phase(&work[0].0);
            let call_fingerprint =
                format!("{}|{}", work[0].0, take_chars(&work[0].1.to_string(), 240));
            if self.last_single_phase != Some(phase)
                || self.last_single_call_fingerprint.as_deref() != Some(call_fingerprint.as_str())
                || self.progress_since_admission
            {
                self.one_tool_streak = 1;
                self.streak_feedback_issued = false;
            } else {
                self.one_tool_streak += 1;
            }
            self.last_single_phase = Some(phase);
            self.last_single_call_fingerprint = Some(call_fingerprint);
            self.progress_since_admission = false;
            if self.one_tool_streak > self.limits.one_tool_streak {
                let (tool, _) = work[0];
                if self.streak_feedback_issued {
                    return EconomyAdmission::Feedback(format!(
                        "EXECUTION ECONOMY: `{tool}` is still the same one-tool step after the earlier correction and has produced no new intent or evidence across {} rounds. Do not call it again. Reuse the latest result, choose a materially different route, batch the remaining known work, or finish now.",
                        self.one_tool_streak
                    ));
                }
                self.streak_feedback_issued = true;
                return EconomyAdmission::Feedback(format!(
                    "EXECUTION ECONOMY: `{tool}` was not executed because this would repeat the same one-tool step for round {}. Reuse the latest state and emit one batch for all known same-state actions, switch to a proven connected-app/managed-browser lane, or finish. Do not retry the identical call: it will provide no new state and must be abandoned.",
                    self.one_tool_streak
                ));
            }
        } else {
            self.one_tool_streak = 0;
            self.streak_feedback_issued = false;
            self.last_single_phase = None;
            self.last_single_call_fingerprint = None;
            self.progress_since_admission = false;
        }
        self.turn_tool_calls += work.len();
        EconomyAdmission::Allow
    }

    pub fn provider_rounds(&self) -> usize {
        self.turn_provider_rounds
    }

    pub fn tool_calls(&self) -> usize {
        self.turn_tool_calls
    }

    pub fn replay_input_tokens(&self) -> u64 {
        self.turn_replay_input_tokens
    }

    pub fn turn_provider_rounds(&self) -> usize {
        self.turn_provider_rounds
    }

    pub fn demonstrated_progress(&self) -> bool {
        self.successes >= 2
            && self.successful_call_fingerprints.len() * 4 >= self.successes.saturating_mul(3)
            && self.failures <= 2
    }
}

fn semantic_tool_phase(tool: &str) -> &'static str {
    match tool {
        "browser_navigate" | "browser_search" | "browser_go_back" => "web_navigate",
        "browser_state"
        | "browser_extract"
        | "browser_screenshot"
        | "browser_find_text"
        | "browser_search_page" => "web_observe",
        "browser_download" => "web_deliver",
        "browser_click"
        | "browser_input"
        | "browser_send_keys"
        | "browser_select_dropdown"
        | "browser_upload_file"
        | "browser_act" => "web_act",
        "computer_status"
        | "computer_app_targets"
        | "computer_app_read"
        | "computer_app_inspect"
        | "computer_list_windows"
        | "computer_screenshot"
        | "computer_capture_window"
        | "computer_locate"
        | "computer_read_text" => "desktop_observe",
        "computer_click"
        | "computer_type"
        | "computer_key"
        | "computer_act"
        | "computer_window_act"
        | "computer_focus_window" => "desktop_act",
        "composio_search" | "composio_schemas" | "composio_connections" => "structured_discover",
        "composio_run" => "structured_execute",
        "ask_for_login" | "ask_for_pass" | "pass_use" | "account_manage" | "credential_generate" => "authentication",
        _ => "other",
    }
}

pub fn guard_for_turn(request: &str, state_root: &Path) -> ExecutionEconomyGuard {
    let scope = crate::settings::SettingsScope::Global;
    let budget_millions =
        crate::settings::effective_u64("efficiency.weekly_input_budget_millions", &scope)
            .unwrap_or(20);
    let warning =
        crate::settings::effective_u64("efficiency.weekly_warning_percent", &scope).unwrap_or(70);
    let hard =
        crate::settings::effective_bool("efficiency.weekly_hard_stop", &scope).unwrap_or(false);
    let usage = weekly_usage(state_root, chrono::Utc::now());
    ExecutionEconomyGuard::new(request).with_weekly_policy(
        usage.input_tokens,
        budget_millions.saturating_mul(1_000_000),
        warning,
        hard,
    )
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WeeklyUsage {
    pub rounds: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub estimated_rounds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WeeklyTelemetryGeneration {
    root: std::path::PathBuf,
    minute_bucket: i64,
    files: Vec<Option<(u64, u64, u32)>>,
}

#[derive(Debug, Clone)]
struct WeeklyUsageCache {
    generation: WeeklyTelemetryGeneration,
    usage: WeeklyUsage,
}

static WEEKLY_USAGE_CACHE: std::sync::OnceLock<std::sync::Mutex<Option<WeeklyUsageCache>>> =
    std::sync::OnceLock::new();

fn weekly_telemetry_generation(
    state_root: &Path,
    now: chrono::DateTime<chrono::Utc>,
) -> WeeklyTelemetryGeneration {
    let files = ["round_timings.prev.jsonl", "round_timings.jsonl"]
        .iter()
        .map(|name| {
            let path = state_root.join("runs").join(name);
            let metadata = std::fs::symlink_metadata(path).ok()?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return None;
            }
            let modified = metadata
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?;
            Some((metadata.len(), modified.as_secs(), modified.subsec_nanos()))
        })
        .collect();
    WeeklyTelemetryGeneration {
        root: state_root.to_path_buf(),
        // The seven-day cutoff moves with time even when the files do not.
        // Minute granularity avoids stale threshold crossings without parsing
        // an unchanged 32MB rotation for every provider turn.
        minute_bucket: now.timestamp().div_euclid(60),
        files,
    }
}

/// Local seven-day provider-token ledger derived from Phoenix's durable round
/// telemetry. It is an honest consumption proxy, not a claim about a vendor's
/// unpublished/dynamic subscription quota units.
pub fn weekly_usage(state_root: &Path, now: chrono::DateTime<chrono::Utc>) -> WeeklyUsage {
    const MAX_BYTES: usize = 16 * 1024 * 1024;
    const MAX_LINE: usize = 64 * 1024;
    let generation = weekly_telemetry_generation(state_root, now);
    let cache = WEEKLY_USAGE_CACHE.get_or_init(|| std::sync::Mutex::new(None));
    if let Ok(cached) = cache.lock() {
        if let Some(cached) = cached
            .as_ref()
            .filter(|cached| cached.generation == generation)
        {
            return cached.usage.clone();
        }
    }

    let cutoff = now - chrono::Duration::days(7);
    let mut usage = WeeklyUsage::default();
    for name in ["round_timings.prev.jsonl", "round_timings.jsonl"] {
        let path = state_root.join("runs").join(name);
        let Ok(Some(bytes)) =
            crate::config::private_io::read_private_file_limited(&path, MAX_BYTES)
        else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        for line in text.lines().filter(|line| line.len() <= MAX_LINE) {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let Some(ts) = value
                .get("ts")
                .and_then(serde_json::Value::as_str)
                .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
                .map(|ts| ts.with_timezone(&chrono::Utc))
            else {
                continue;
            };
            if ts < cutoff || ts > now + chrono::Duration::minutes(5) {
                continue;
            }
            usage.rounds += 1;
            let reported = value
                .get("input_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            if reported > 0 {
                usage.input_tokens = usage.input_tokens.saturating_add(reported);
            } else {
                let estimated = value
                    .get("estimated_input_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .or_else(|| {
                        value
                            .get("request_chars")
                            .and_then(serde_json::Value::as_u64)
                            .map(|chars| chars / 4)
                    })
                    .unwrap_or(0);
                usage.input_tokens = usage.input_tokens.saturating_add(estimated);
                usage.estimated_rounds += 1;
            }
            usage.output_tokens = usage.output_tokens.saturating_add(
                value
                    .get("output_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
            );
        }
    }
    if let Ok(mut cached) = cache.lock() {
        *cached = Some(WeeklyUsageCache {
            generation,
            usage: usage.clone(),
        });
    }
    usage
}

const RECEIPT_MAX_CHARS: usize = 640;
const RECEIPT_VALUE_MAX_CHARS: usize = 180;
const REQUEST_ECHO_MAX_CHARS: usize = 640;
const REQUEST_ECHO_TRANSCRIPT_CHARS: usize = 64_000;

/// The native tool catalog is authoritative. Repeating a very large allowlist
/// as prose spends context without adding capability.
pub fn compact_allowlist(tools: &[String]) -> String {
    const MAX_INLINE_TOOLS: usize = 20;
    if tools.len() <= MAX_INLINE_TOOLS {
        return tools.join(", ");
    }
    format!(
        "{} tools available through the native catalog; choose the shortest reliable lane",
        tools.len()
    )
}

/// Stable exact-content dedupe for prompt sections assembled from overlapping
/// memory/indexer sources. First occurrence wins so ordering and provenance do
/// not churn the provider's prefix cache.
pub fn join_unique(values: impl IntoIterator<Item = String>, separator: &str) -> String {
    let mut seen = std::collections::HashSet::new();
    values
        .into_iter()
        .filter(|value| {
            let key = value.split_whitespace().collect::<Vec<_>>().join(" ");
            !key.is_empty() && seen.insert(key)
        })
        .collect::<Vec<_>>()
        .join(separator)
}

/// Echo the request at the recency edge only when a long transcript can bury
/// it. Short turns already carry the full request near the top; duplicating it
/// there is pure input burn.
pub fn request_echo(request: &str, transcript_chars: usize) -> Option<String> {
    (transcript_chars >= REQUEST_ECHO_TRANSCRIPT_CHARS).then(|| {
        let trimmed = request.trim();
        if trimmed.chars().count() <= REQUEST_ECHO_MAX_CHARS {
            trimmed.to_string()
        } else {
            format!(
                "{} …(full request is authoritative above)",
                take_chars(trimmed, REQUEST_ECHO_MAX_CHARS)
            )
        }
    })
}

/// Render a completed tool invocation as a compact, non-secret receipt.
///
/// Full inputs remain durable in [`crate::session::Message::ToolResult`]. This
/// function only shapes the projection re-fed to a model. Large write bodies,
/// replacement strings, typed text, credentials, cookie blobs, and opaque
/// runtime ids therefore stop riding every later provider round.
pub fn compact_tool_input(tool_name: &str, raw: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        let collapsed = redact_opaque_tokens(&raw.split_whitespace().collect::<Vec<_>>().join(" "));
        return if collapsed.chars().count() <= RECEIPT_VALUE_MAX_CHARS {
            collapsed
        } else {
            format!("input omitted ({} chars)", raw.chars().count())
        };
    };
    let Some(object) = value.as_object() else {
        return format!("input recorded ({} chars)", raw.chars().count());
    };

    let mut fields: BTreeMap<&str, String> = BTreeMap::new();
    for key in [
        "action",
        "app",
        "command",
        "combo",
        "file_path",
        "index",
        "limit",
        "max",
        "model",
        "offset",
        "path",
        "pattern",
        "provider",
        "query",
        "scope",
        "site",
        "status",
        "tool_slug",
        "url",
    ] {
        if let Some(value) = object.get(key) {
            if let Some(rendered) = safe_scalar(value) {
                fields.insert(key, rendered);
            }
        }
    }

    for key in [
        "content", "new", "old", "text", "password", "token", "cookies",
    ] {
        if let Some(value) = object.get(key) {
            fields.insert(key, format!("<{} chars>", value_chars(value)));
        }
    }

    if let Some(actions) = object.get("actions").and_then(serde_json::Value::as_array) {
        let labels = actions
            .iter()
            .take(12)
            .filter_map(|action| {
                let action = action.as_object()?;
                let kind = action
                    .get("action")
                    .or_else(|| action.get("type"))?
                    .as_str()?;
                let target = action
                    .get("query")
                    .or_else(|| action.get("url"))
                    .and_then(serde_json::Value::as_str)
                    .map(|value| format!(" {}", take_chars(value, 48)))
                    .unwrap_or_default();
                Some(format!("{kind}{target}"))
            })
            .collect::<Vec<_>>();
        let suffix = (actions.len() > labels.len())
            .then(|| format!(" +{} more", actions.len() - labels.len()))
            .unwrap_or_default();
        fields.insert(
            "actions",
            format!("{} [{}]{}", actions.len(), labels.join(", "), suffix),
        );
    }

    // Unknown tools still get an honest receipt without leaking opaque ids or
    // recursively serializing arbitrary payloads. Safe short scalar keys are
    // useful anchors; *_id, secrets, and transport metadata are not.
    if fields.is_empty() {
        for (key, value) in object {
            let lower = key.to_ascii_lowercase();
            if lower == "id"
                || lower.ends_with("_id")
                || lower.contains("token")
                || lower.contains("secret")
                || lower.contains("password")
                || lower.contains("cookie")
                || lower.contains("hash")
            {
                continue;
            }
            if let Some(rendered) = safe_scalar(value) {
                fields.insert(key, rendered);
            }
            if fields.len() >= 8 {
                break;
            }
        }
    }

    let rendered = fields
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(", ");
    let rendered = if rendered.is_empty() {
        format!("{tool_name} input recorded")
    } else {
        rendered
    };
    if rendered.chars().count() <= RECEIPT_MAX_CHARS {
        rendered
    } else {
        format!("{} …", take_chars(&rendered, RECEIPT_MAX_CHARS))
    }
}

fn safe_scalar(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => Some(take_chars(text, RECEIPT_VALUE_MAX_CHARS)),
        serde_json::Value::Bool(value) => Some(value.to_string()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn redact_opaque_tokens(value: &str) -> String {
    value
        .split_whitespace()
        .map(|token| {
            let trimmed = token.trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_');
            let lower = trimmed.to_ascii_lowercase();
            let prefixed = ["queued_", "epoch_", "job_", "call_", "request_", "req_"]
                .iter()
                .any(|prefix| lower.starts_with(prefix));
            let long_hex = trimmed.len() >= 24
                && trimmed.chars().all(|ch| ch.is_ascii_hexdigit())
                && trimmed.chars().any(|ch| ch.is_ascii_alphabetic())
                && trimmed.chars().any(|ch| ch.is_ascii_digit());
            if prefixed || long_hex {
                "<internal-reference>"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn value_chars(value: &serde_json::Value) -> usize {
    value
        .as_str()
        .map(|text| text.chars().count())
        .unwrap_or_else(|| {
            serde_json::to_string(value)
                .map(|text| text.chars().count())
                .unwrap_or(0)
        })
}

fn take_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

/// One row from `runs/round_timings.jsonl`, reduced to the fields needed for
/// quota and loop analysis. Unknown fields remain forward-compatible.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoundCostSample {
    #[serde(default)]
    pub session: String,
    #[serde(default)]
    pub round: usize,
    #[serde(default)]
    pub request_chars: usize,
    #[serde(default)]
    pub input_tokens: u32,
    #[serde(default)]
    pub output_tokens: u32,
    #[serde(default)]
    pub cache_read_tokens: Option<u32>,
    #[serde(default)]
    pub provider_error: bool,
    #[serde(default)]
    pub tools: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EfficiencyReport {
    pub rounds: usize,
    pub total_input_tokens: u64,
    pub effective_input_tokens: u64,
    pub total_output_tokens: u64,
    pub max_round_input_tokens: u32,
    pub one_tool_rounds: usize,
    pub longest_one_tool_streak: usize,
    pub provider_errors: usize,
    pub replay_budget_passes: bool,
}

/// Detect the exact quota-drain shape seen in the historical website trace. A cache hit
/// can reduce billed compute, but twenty sequential model decisions still
/// consume subscription capacity and latency, so the replay guard considers
/// round count and per-round input in addition to uncached tokens.
pub fn analyze_round_costs(samples: &[RoundCostSample]) -> EfficiencyReport {
    let mut longest_streak = 0usize;
    let mut streak = 0usize;
    let mut previous_round: Option<usize> = None;
    for sample in samples {
        let consecutive = previous_round.is_none_or(|round| sample.round == round + 1);
        if sample.tools.len() == 1 && consecutive {
            streak += 1;
        } else if sample.tools.len() == 1 {
            streak = 1;
        } else {
            streak = 0;
        }
        longest_streak = longest_streak.max(streak);
        previous_round = Some(sample.round);
    }
    let total_input_tokens = samples.iter().map(|s| u64::from(s.input_tokens)).sum();
    let effective_input_tokens = samples
        .iter()
        .map(|s| {
            u64::from(
                s.input_tokens
                    .saturating_sub(s.cache_read_tokens.unwrap_or(0)),
            )
        })
        .sum();
    let max_round_input_tokens = samples.iter().map(|s| s.input_tokens).max().unwrap_or(0);
    let one_tool_rounds = samples.iter().filter(|s| s.tools.len() == 1).count();
    let replay_budget_passes = !(max_round_input_tokens >= 100_000 && longest_streak >= 4)
        && !(samples.len() >= 12 && one_tool_rounds * 4 >= samples.len() * 3);
    EfficiencyReport {
        rounds: samples.len(),
        total_input_tokens,
        effective_input_tokens,
        total_output_tokens: samples.iter().map(|s| u64::from(s.output_tokens)).sum(),
        max_round_input_tokens,
        one_tool_rounds,
        longest_one_tool_streak: longest_streak,
        provider_errors: samples.iter().filter(|s| s.provider_error).count(),
        replay_budget_passes,
    }
}

pub fn analyze_round_timing_jsonl(raw: &str) -> EfficiencyReport {
    let samples = raw
        .lines()
        .filter_map(|line| serde_json::from_str::<RoundCostSample>(line).ok())
        .collect::<Vec<_>>();
    analyze_round_costs(&samples)
}

/// Produce independent reports per durable session. This is the preferred
/// diagnostic for a whole timing log. Rows stay in append order because round
/// numbers restart at zero for each new turn inside the same durable session;
/// sorting them would falsely join unrelated one-tool streaks.
pub fn analyze_round_timing_jsonl_by_session(raw: &str) -> BTreeMap<String, EfficiencyReport> {
    let mut grouped: BTreeMap<String, Vec<RoundCostSample>> = BTreeMap::new();
    for sample in raw
        .lines()
        .filter_map(|line| serde_json::from_str::<RoundCostSample>(line).ok())
    {
        grouped
            .entry(sample.session.clone())
            .or_default()
            .push(sample);
    }
    grouped
        .into_iter()
        .map(|(session, samples)| (session, analyze_round_costs(&samples)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str) -> (String, serde_json::Value) {
        (name.to_string(), serde_json::json!({}))
    }

    #[test]
    fn dreamina_is_managed_browser_first_and_desktop_is_rejected() {
        let request = "Generate a video in Dreamina and return the downloaded file";
        assert_eq!(classify_task(request), EconomyTaskClass::WebArtifact);
        assert_eq!(
            preferred_surface(request, false),
            PreferredSurface::ManagedBrowser
        );
        assert_eq!(
            preferred_surface(request, true),
            PreferredSurface::ConnectedApp
        );
        assert!(route_violation(
            request,
            "computer_capture_window",
            &serde_json::json!({"app":"Zen"})
        )
        .is_some());
        assert!(route_violation(request, "browser_navigate", &serde_json::json!({})).is_none());
        assert!(route_violation(request, "composio_search", &serde_json::json!({})).is_none());
    }

    #[test]
    fn compound_web_artifact_work_gets_multi_surface_headroom() {
        let request = "Download an image from the website, attach it to an email, update the PDF report, then schedule a calendar reminder";
        assert_eq!(
            classify_task(request),
            EconomyTaskClass::MultiSurfaceHardTask
        );
    }

    #[test]
    fn desktop_route_gate_reads_targets_not_payload_prose() {
        let request = "Sign in and prepare an image";
        assert!(route_violation(
            request,
            "computer_app_read",
            &serde_json::json!({"app":"Photoshop", "instruction":"sign in and edit image"})
        )
        .is_none());
        assert!(route_violation(
            request,
            "computer_type",
            &serde_json::json!({"text":"Mention Chrome in the document"})
        )
        .is_none());
        assert!(route_violation(
            request,
            "computer_app_read",
            &serde_json::json!({"app":"Google Chrome"})
        )
        .is_some());
        assert!(route_violation(
            request,
            "computer_open",
            &serde_json::json!({"url":"https://example.com"})
        )
        .is_some());
    }

    #[test]
    fn weekly_cache_generation_tracks_append_and_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let runs = dir.path().join("runs");
        std::fs::create_dir_all(&runs).unwrap();
        let current = runs.join("round_timings.jsonl");
        std::fs::write(&current, "one\n").unwrap();
        let now = chrono::Utc::now();
        let first = weekly_telemetry_generation(dir.path(), now);
        assert_eq!(first, weekly_telemetry_generation(dir.path(), now));

        std::fs::write(&current, "one\ntwo\n").unwrap();
        let appended = weekly_telemetry_generation(dir.path(), now);
        assert_ne!(first, appended);

        std::fs::rename(&current, runs.join("round_timings.prev.jsonl")).unwrap();
        std::fs::write(&current, "fresh\n").unwrap();
        let rotated = weekly_telemetry_generation(dir.path(), now);
        assert_ne!(appended, rotated);
    }

    #[test]
    fn repeated_one_tool_pattern_keeps_correcting_without_runtime_stop() {
        let mut guard = ExecutionEconomyGuard::new(
            "Generate a video in Dreamina and return the downloaded file",
        );
        for _ in 0..3 {
            assert_eq!(
                guard.admit_tool_batch(&[call("browser_state")]),
                EconomyAdmission::Allow
            );
        }
        assert!(matches!(
            guard.admit_tool_batch(&[call("browser_state")]),
            EconomyAdmission::Feedback(message) if message.contains("round 4")
        ));
        assert!(matches!(
            guard.admit_tool_batch(&[call("browser_state")]),
            EconomyAdmission::Feedback(message) if message.contains("Do not call it again")
        ));
        assert_eq!(guard.tool_calls(), 3);
    }

    #[test]
    fn valid_batched_multi_surface_work_has_real_headroom() {
        let mut guard = ExecutionEconomyGuard::new(
            "Inspect the website, reconcile the spreadsheet, prepare a PDF, and update the calendar",
        );
        assert_eq!(guard.class(), EconomyTaskClass::MultiSurfaceHardTask);
        for _ in 0..8 {
            assert_eq!(guard.before_provider_round(), EconomyAdmission::Allow);
            let batch = vec![call("browser_act"), call("write")];
            assert_eq!(guard.admit_tool_batch(&batch), EconomyAdmission::Allow);
            guard.record_provider_usage(45_000);
        }
        assert_eq!(guard.provider_rounds(), 8);
        assert_eq!(guard.tool_calls(), 16);
        assert_eq!(guard.replay_input_tokens(), 360_000);
    }

    #[test]
    fn distinct_dependent_browser_phases_do_not_look_like_a_stalled_loop() {
        let request = "Generate a video in Dreamina and return the downloaded file";
        let mut guard = ExecutionEconomyGuard::new(request);
        let phases = [
            "browser_navigate",
            "browser_state",
            "browser_act",
            "browser_extract",
            "browser_act",
            "browser_download",
        ];
        let mut results: Vec<(String, String, bool, String)> = Vec::new();
        for (index, tool) in phases.into_iter().enumerate() {
            assert_eq!(guard.before_provider_round(), EconomyAdmission::Allow);
            guard.record_provider_usage(40_000);
            guard.observe_results(results.iter().map(|(tool, input, ok, output)| {
                (tool.as_str(), input.as_str(), *ok, output.as_str())
            }));
            assert_eq!(
                guard.admit_tool_batch(&[call(tool)]),
                EconomyAdmission::Allow,
                "phase {tool} should be admitted"
            );
            results.push((
                tool.to_string(),
                format!("step-{index}"),
                true,
                format!("new-state-{index}"),
            ));
        }
        assert_eq!(guard.tool_calls(), 6);
        assert_eq!(guard.provider_rounds(), 6);
        assert_eq!(guard.replay_input_tokens(), 240_000);
    }

    #[test]
    fn multi_surface_task_can_use_a_native_pdf_editor_but_not_zen() {
        let request =
            "Inspect the website, prepare a PDF in a native editor, and update the calendar";
        assert!(route_violation(
            request,
            "computer_open",
            &serde_json::json!({"target":"/tmp/assignment.pdf"})
        )
        .is_none());
        assert!(route_violation(
            request,
            "computer_app_read",
            &serde_json::json!({"app":"Zen"})
        )
        .is_some());
    }

    #[test]
    fn large_replay_never_creates_a_context_erasing_checkpoint() {
        let mut guard = ExecutionEconomyGuard::new(
            "Generate a video in Dreamina and return the downloaded file",
        );
        for _ in 0..12 {
            assert_eq!(guard.before_provider_round(), EconomyAdmission::Allow);
            guard.record_provider_usage(149_000);
        }
        assert_eq!(guard.replay_input_tokens(), 1_788_000);
        assert_eq!(guard.provider_rounds(), 12);
    }

    #[test]
    fn provider_usage_is_one_monotonic_whole_turn_total() {
        let mut guard = ExecutionEconomyGuard::new("Generate and download an image on a website");
        for _ in 0..16 {
            assert_eq!(guard.before_provider_round(), EconomyAdmission::Allow);
            guard.record_provider_usage(149_000);
        }
        assert_eq!(guard.provider_rounds(), 16);
        assert_eq!(guard.replay_input_tokens(), 2_384_000);
    }

    #[test]
    fn advancing_work_keeps_context_until_the_anchored_compactor_runs() {
        let mut guard = ExecutionEconomyGuard::new(
            "Use the website to generate an image, download it, and return the file",
        );
        let rounds = 12usize;
        let mut results: Vec<(String, String, bool, String)> = Vec::new();
        for index in 0..rounds {
            assert_eq!(guard.before_provider_round(), EconomyAdmission::Allow);
            guard.record_provider_usage(20_000);
            guard.observe_results(results.iter().map(|(tool, input, ok, output)| {
                (tool.as_str(), input.as_str(), *ok, output.as_str())
            }));
            let input = serde_json::json!({"actions":[{"action":"click","index":index}]});
            assert_eq!(
                guard.admit_tool_batch(&[("browser_act".to_string(), input.clone())]),
                EconomyAdmission::Allow
            );
            results.push((
                "browser_act".to_string(),
                input.to_string(),
                true,
                format!("completed phase {index}"),
            ));
        }
        guard.observe_results(results.iter().map(|(tool, input, ok, output)| {
            (tool.as_str(), input.as_str(), *ok, output.as_str())
        }));
        assert_eq!(guard.before_provider_round(), EconomyAdmission::Allow);
        assert_eq!(guard.replay_input_tokens(), rounds as u64 * 20_000);
        assert_eq!(guard.turn_provider_rounds(), rounds + 1);
    }

    #[test]
    fn weekly_warning_fires_only_on_the_crossing_turn() {
        let mut crossing =
            ExecutionEconomyGuard::new("ordinary work").with_weekly_policy(69, 100, 70, false);
        assert!(crossing.take_weekly_notice().is_none());
        crossing.record_provider_usage(1);
        assert!(crossing.take_weekly_notice().is_some());
        assert!(crossing.take_weekly_notice().is_none());

        let mut already_above =
            ExecutionEconomyGuard::new("next turn").with_weekly_policy(75, 100, 70, false);
        already_above.record_provider_usage(5);
        assert!(
            already_above.take_weekly_notice().is_none(),
            "passive burn/settings surface replaces repetitive per-turn toasts"
        );
    }

    #[test]
    fn huge_write_input_becomes_a_small_non_secret_receipt() {
        let body = "x".repeat(80_000);
        let raw = serde_json::json!({
            "path": "reports/assignment.md",
            "content": body,
            "request_id": "queued_c6d468483ccc45d694f986166d7921b3",
            "token": "never-render-this"
        })
        .to_string();
        let receipt = compact_tool_input("write", &raw);
        assert!(receipt.contains("path=reports/assignment.md"));
        assert!(receipt.contains("content=<80000 chars>"));
        assert!(!receipt.contains("queued_"));
        assert!(!receipt.contains("never-render-this"));
        assert!(receipt.len() < 160, "receipt was {} bytes", receipt.len());
    }

    #[test]
    fn browser_batch_receipt_keeps_actions_not_typed_secrets() {
        let raw = serde_json::json!({
            "actions": [
                {"action":"input", "index":3, "text":"private@example.com"},
                {"action":"input", "index":5, "text":"secret"},
                {"action":"click", "index":7}
            ]
        })
        .to_string();
        let receipt = compact_tool_input("browser_act", &raw);
        assert!(receipt.contains("actions=3 [input, input, click]"));
        assert!(!receipt.contains("private@example.com"));
        assert!(!receipt.contains("secret"));
    }

    #[test]
    fn opaque_transport_references_never_render_from_plain_inputs() {
        let receipt = compact_tool_input(
            "legacy",
            "queued_c6d468483ccc45d694f986166d7921b3 is starting",
        );
        assert_eq!(receipt, "<internal-reference> is starting");
    }

    #[test]
    fn request_echo_is_not_paid_twice_on_short_turns() {
        assert_eq!(request_echo("Do the thing", 4_000), None);
        assert_eq!(
            request_echo("Do the thing", 80_000).as_deref(),
            Some("Do the thing")
        );
    }

    #[test]
    fn dreamina_replay_fixture_fails_the_efficiency_budget() {
        let inputs = [
            141_447, 142_013, 142_130, 142_356, 142_658, 142_789, 143_151, 143_442, 143_540,
            144_007, 144_744, 144_994, 145_108, 145_106, 145_278, 145_465, 145_695, 146_048,
            146_432, 147_100, 149_011, 149_042, 148_845, 149_320, 149_811, 150_067, 150_706, 0,
        ];
        let samples = inputs
            .iter()
            .enumerate()
            .map(|(index, input)| RoundCostSample {
                session: "dreamina".into(),
                round: index,
                request_chars: 473_171 + index * 1_120,
                input_tokens: *input,
                output_tokens: 200,
                cache_read_tokens: Some(140_672),
                provider_error: false,
                tools: (index < 26)
                    .then(|| vec![serde_json::json!({"tool":"computer_click"})])
                    .unwrap_or_default(),
            })
            .collect::<Vec<_>>();
        let report = analyze_round_costs(&samples);
        assert_eq!(report.rounds, 28);
        assert_eq!(report.one_tool_rounds, 26);
        assert_eq!(report.longest_one_tool_streak, 26);
        assert_eq!(report.total_input_tokens, 3_930_305);
        assert!(!report.replay_budget_passes);
    }

    #[test]
    fn semantic_batches_pass_and_cut_replayed_input_by_over_eighty_percent() {
        let samples = (0..3)
            .map(|round| RoundCostSample {
                session: "batched".into(),
                round,
                request_chars: 120_000,
                input_tokens: 42_000,
                output_tokens: 200,
                cache_read_tokens: Some(36_000),
                provider_error: false,
                tools: vec![serde_json::json!({"tool":"browser_act", "actions": 4})],
            })
            .collect::<Vec<_>>();
        let report = analyze_round_costs(&samples);
        assert!(report.replay_budget_passes);
        assert_eq!(report.total_input_tokens, 126_000);
        assert!(report.total_input_tokens * 10 < 3_930_305);
    }
}
