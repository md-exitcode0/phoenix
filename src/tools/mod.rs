//! Shared tools
//!
//! Tools available to orchestrator and/or sub-agents

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::providers::contracts::ToolDefinition;
use crate::runtime::{summarize_tool_input, ToolCall, ToolCallResult};
use crate::runtime::{AskUserHandler, ToolSpec};

pub mod accounts;
mod transcribe;
mod agent_control;
pub mod agent_forge;
pub mod ask_user;
mod bash;
pub mod browser_cookie_grants;
pub mod browser_cookies;
pub mod browser_native;
pub mod checkpoint;
pub mod chromium_cookies;
mod codebase_search;
mod codegraph_tools;
pub mod composio;
pub mod compress;
mod computer_use;
pub mod credentials;
mod cron_tool;
mod descriptions;
pub mod fast_apply;
pub mod react;
// pub: the prompt assembler re-materializes pinned design references
// (runtime/prompt.rs) and the session pin registry canonicalizes paths
// (session/manager.rs).
pub mod design_refs;
pub mod deferral;
pub mod design_studio;
mod glob;
mod grep;
pub mod image_analyze;
pub mod image_gen;
pub mod isolated_desktop;
mod list_directory;
pub mod local_mcp;
pub mod login_request;
pub mod passes;
pub mod mcp_client;
mod message_agent;
mod read;
pub mod recall;
pub mod remote_runner;
mod reverse_skill;
mod routine;
pub mod motion_graphics;
pub mod skills;
mod str_replace;
mod talk;
mod todo;
pub mod terminal_jobs;
pub mod ui_snap;
pub mod vital_memory;
pub mod volume_work;
pub(crate) mod web;
mod web_crawl;
mod web_fetch;
mod web_scrape;
pub(crate) mod web_search;
mod work;
mod workspace_io;
mod write;

pub fn computer_local_status_line() -> String {
    computer_use::local_status_line()
}

/// Process-cached desktop status line for the per-turn runtime-context
/// preflight (avoids a `gdbus`/`xdotool` backend probe on every turn).
pub fn computer_cached_status_line() -> String {
    computer_use::cached_status_line()
}

pub use agent_control::{AgentControlAction, AgentControlInput};
pub use message_agent::{
    is_transport_message, priority_from_transport_body, visible_transport_body, MessageAgentInput,
    MessageAttachment, MessagePriority,
};
pub use talk::{TalkInput, TalkReplyStatus, TalkResult};
#[cfg(test)]
pub(crate) use todo::execute as todo_execute;
pub(crate) use todo::snapshot as todo_snapshot;
pub(crate) use todo::snapshot_with_update_time as todo_snapshot_with_update_time;
pub use todo::{TodoItem, TodoWriteInput};
pub use volume_work::{VolumeJobInput, VolumeJobResult, VolumeWorkInput, VolumeWorkResult};

#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub summary: String,
    pub content: String,
}

/// Directories that recursive workspace tools must never descend into.
///
/// The Phoenix workspace root is the cwd, which in this repo contains gigabytes
/// of build artifacts (`target/`), VCS data (`.git/`), and dozens of full donor
/// repo clones (`life_stealing_material/`, `backups/`, `PhoenixAgent/`). Walking
/// these recursively pegs CPU and balloons memory, so every recursive tool
/// prunes them up front.
pub(crate) const IGNORED_DIR_NAMES: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".phoenix",
    "target",
    "node_modules",
    "dist",
    "build",
    "out",
    ".next",
    ".nuxt",
    "__pycache__",
    ".venv",
    "venv",
    ".mypy_cache",
    ".pytest_cache",
    ".cargo",
    ".idea",
    ".vscode",
    "life_stealing_material",
    "backups",
    "PhoenixAgent",
];

/// True when a directory name should be pruned from recursive walks.
pub(crate) fn is_ignored_dir(name: &str) -> bool {
    IGNORED_DIR_NAMES.contains(&name)
}

const TREE_MAX_DEPTH: usize = 3;
const TREE_MAX_ENTRIES: usize = 70;
const TREE_MAX_FILES_PER_DIR: usize = 12;

/// A compact, depth-limited tree of the workspace with vendored/ignored/hidden
/// dirs pruned. Injected into every agent's RUNTIME CONTEXT so it starts already
/// oriented — the way a capable engineer (or Claude Code, whose environment block
/// ships the file list up front) never burns rounds on `ls`/`find` to discover
/// where the code lives. Cheap: depth ≤3, hard-capped, no recursion into vendored
/// trees. Deterministic ordering so it stays prefix-cache-stable within a session.
pub(crate) fn workspace_tree(root: &std::path::Path) -> String {
    let mut out = String::new();
    let mut count = 0usize;
    walk_tree(root, 1, "  ", &mut out, &mut count);
    if out.is_empty() {
        return String::new();
    }
    if count >= TREE_MAX_ENTRIES {
        out.push_str("  … (layout capped — use glob/list_directory for the rest)\n");
    }
    out
}

fn walk_tree(
    dir: &std::path::Path,
    depth: usize,
    prefix: &str,
    out: &mut String,
    count: &mut usize,
) {
    if depth > TREE_MAX_DEPTH || *count >= TREE_MAX_ENTRIES {
        return;
    }
    let mut entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(_) => return,
    };
    entries.sort_by_key(|e| e.file_name());
    let (mut dirs, mut files) = (Vec::new(), Vec::new());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        match entry.file_type() {
            Ok(ft) if ft.is_dir() && !is_ignored_dir(&name) => dirs.push(name),
            Ok(ft) if ft.is_file() => files.push(name),
            _ => {}
        }
    }
    for (i, file) in files.iter().enumerate() {
        if *count >= TREE_MAX_ENTRIES {
            return;
        }
        if i >= TREE_MAX_FILES_PER_DIR {
            out.push_str(&format!("{prefix}… +{} more files\n", files.len() - i));
            break;
        }
        out.push_str(&format!("{prefix}{file}\n"));
        *count += 1;
    }
    for name in dirs {
        if *count >= TREE_MAX_ENTRIES {
            return;
        }
        out.push_str(&format!("{prefix}{name}/\n"));
        *count += 1;
        walk_tree(
            &dir.join(&name),
            depth + 1,
            &format!("{prefix}  "),
            out,
            count,
        );
    }
}

/// The user-visible permission posture for one agent turn.
///
/// Possessing a tool never implies permission to execute it. Every coworker
/// receives the complete live catalog, then this posture decides whether the
/// call can run immediately or must be presented to the user as an elevation
/// request. `FullAccess` is the product name for the old `Yolo` mode; legacy
/// serialized `"yolo"` values continue to deserialize during migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    Talk,
    #[default]
    Workspace,
    #[serde(alias = "yolo", alias = "full")]
    FullAccess,
}

impl PermissionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Talk => "talk",
            Self::Workspace => "workspace",
            Self::FullAccess => "full_access",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Talk => "Talk",
            Self::Workspace => "Workspace",
            Self::FullAccess => "Full Access",
        }
    }

    pub fn allows(self, required: Self) -> bool {
        self >= required
    }

    pub fn is_full_access(self) -> bool {
        matches!(self, Self::FullAccess)
    }
}

/// Deterministic preflight result used by every runtime path. The UI can turn
/// `RequiresApproval` into the compact approval block above the composer;
/// executors also enforce it directly so a missed UI hook cannot bypass it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ToolPermissionDecision {
    Allowed,
    RequiresApproval {
        current: PermissionMode,
        required: PermissionMode,
        reason: String,
    },
}

/// A concrete outward effect that is governed independently from the broad
/// Talk/Workspace/Full Access posture. Full Access is never permission to
/// spend money or contact another person without the configured policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GovernedEffect {
    ExternalSend,
    ExternalDelete,
    Purchase,
}

impl GovernedEffect {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExternalSend => "external_send",
            Self::ExternalDelete => "external_delete",
            Self::Purchase => "purchase",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::ExternalSend => "an external send or publication",
            Self::ExternalDelete => "an external deletion or removal",
            Self::Purchase => "a purchase or paid-plan action",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum GovernedEffectDecision {
    Allowed,
    Denied {
        effect: GovernedEffect,
        reason: String,
    },
    RequiresApproval {
        effect: GovernedEffect,
        reason: String,
    },
}

impl ToolPermissionDecision {
    pub fn required_mode(&self) -> Option<PermissionMode> {
        match self {
            Self::Allowed => None,
            Self::RequiresApproval { required, .. } => Some(*required),
        }
    }
}

/// Minimum posture for a catalog capability. This is deliberately based on
/// effects, not agent role: Leo and Nico see the same tool and receive the
/// same approval boundary for the same call.
pub fn required_permission_for_tool(tool_name: &str) -> PermissionMode {
    if tool_name.starts_with("browser_") || tool_name.starts_with("computer_") {
        return PermissionMode::FullAccess;
    }

    match tool_name {
        // Conversation, coordination, private memory, and the user's visible
        // plan remain useful even when an agent is deliberately put in Talk.
        // Coworker hiring has its own exact role-scoped user approval, and
        // provisioning is Phoenix-only plus constrained to that requested
        // record, so neither should ask for broad Full Access as a second gate.
        "ask_user" | "teach_workflow" | "ask_for_login" | "ask_for_pass" | "final_answer" | "talk"
        | "message_agent" | "agent_control" | "react" | "volume_work" | "todo_write" | "work"
        | "routine" | "recall" | "memory_recall" | "memory_save" | "vital_memory_write"
        | "create_agent" | "agent_provision" => PermissionMode::Talk,

        // Actions that can change Phoenix's global configuration, install
        // executable capability, schedule future work, or touch an external
        // account need explicit Full Access.
        "tools_create"
        | "reverse_skill"
        | "cron"
        | "skill_install"
        | "composio_run"
        | "composio_connections"
        | "credential_list"
        | "credential_generate"
        | "pass_use"
        | "account_manage"
        | "mcp_call" => PermissionMode::FullAccess,

        // Project-local filesystem/code tools and read-only network discovery
        // are the Workspace tier. Unknown future tools default here: they are
        // never silently promoted to Talk, and their own safety gates remain.
        _ => PermissionMode::Workspace,
    }
}

/// Classify one concrete call. Path-bearing tools that explicitly target
/// outside the current workspace are elevated from Workspace to Full Access
/// before path resolution, which yields a useful approval instead of the old
/// opaque `path escapes workspace` failure.
pub fn permission_decision_for_tool(
    mode: PermissionMode,
    workspace_root: &Path,
    tool_name: &str,
    input: &serde_json::Value,
) -> ToolPermissionDecision {
    let mut required = required_permission_for_tool(tool_name);
    if tool_name == "work"
        && input.get("action").and_then(serde_json::Value::as_str) == Some("workflow")
        && matches!(
            input
                .get("workflow_action")
                .and_then(serde_json::Value::as_str),
            Some("resume" | "reroute")
        )
    {
        required = PermissionMode::FullAccess;
    }
    if required == PermissionMode::Workspace
        && call_explicitly_targets_outside_workspace(workspace_root, input)
    {
        required = PermissionMode::FullAccess;
    }

    if mode.allows(required) {
        ToolPermissionDecision::Allowed
    } else {
        let reason = match required {
            PermissionMode::Talk => "This is a conversation-only action.".to_string(),
            PermissionMode::Workspace => format!(
                "`{tool_name}` needs project workspace access, while this agent is in Talk mode."
            ),
            PermissionMode::FullAccess => format!(
                "`{tool_name}` can affect the desktop, browser, Phoenix configuration, files outside the workspace, scheduled work, or an external account."
            ),
        };
        ToolPermissionDecision::RequiresApproval {
            current: mode,
            required,
            reason,
        }
    }
}

fn call_explicitly_targets_outside_workspace(
    workspace_root: &Path,
    input: &serde_json::Value,
) -> bool {
    let Ok(root) = workspace_root.canonicalize() else {
        return true;
    };
    [
        "path",
        "file_path",
        "directory",
        "cwd",
        "output_path",
        "pattern",
    ]
    .into_iter()
    .filter_map(|key| input.get(key).and_then(serde_json::Value::as_str))
    .any(|raw| {
        let path = Path::new(raw);
        // A parent-relative target is conservatively treated as outside
        // the workspace. Full Access may resolve it; Workspace mode must
        // ask before any filesystem tool crosses the project boundary.
        if path
            .components()
            .any(|component| component == std::path::Component::ParentDir)
        {
            return true;
        }
        if !path.is_absolute() {
            return false;
        }
        let resolved = if path.exists() {
            path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
        } else {
            path.to_path_buf()
        };
        !resolved.starts_with(&root)
    })
}

fn governed_effect_for_call(tool_name: &str, input: &serde_json::Value) -> Option<GovernedEffect> {
    if !matches!(tool_name, "composio_run" | "mcp_call")
        && !tool_name.starts_with("browser_")
        && !tool_name.starts_with("computer_")
    {
        return None;
    }

    // Tool slugs are the strongest signal for app actions. Browser/computer
    // calls do not have semantic schemas, so their target text and typed value
    // are included as a conservative fallback. Values are bounded before
    // inspection to keep this preflight cheap even for malformed calls.
    let mut haystack = String::new();
    if tool_name.starts_with("browser_") || tool_name.starts_with("computer_") {
        collect_effect_text(input, &mut haystack, 0);
    } else {
        collect_action_identifiers(input, &mut haystack, 0);
    }
    let haystack = haystack.to_ascii_lowercase();

    let purchase_terms = [
        "purchase",
        "checkout",
        "buy_now",
        "buy now",
        "place_order",
        "place order",
        "paid_plan",
        "paid plan",
        "subscribe_paid",
        "create_subscription",
        "upgrade_plan",
        "create_charge",
        "capture_payment",
        "confirm_payment",
        "pay_invoice",
    ];
    if purchase_terms.iter().any(|term| haystack.contains(term)) {
        return Some(GovernedEffect::Purchase);
    }

    let delete_terms = [
        "delete_",
        "remove_",
        "archive_and_delete",
        "trash_",
        "revoke_",
        "cancel_subscription",
        "delete account",
        "remove account",
    ];
    if delete_terms.iter().any(|term| haystack.contains(term)) {
        return Some(GovernedEffect::ExternalDelete);
    }

    let send_terms = [
        "send_email",
        "send_message",
        "send_mail",
        "reply_email",
        "reply_message",
        "post_message",
        "publish_post",
        "publish_page",
        "create_comment",
        "create_reply",
        "invite_user",
        "send invite",
        "send message",
        "send email",
    ];
    send_terms
        .iter()
        .any(|term| haystack.contains(term))
        .then_some(GovernedEffect::ExternalSend)
}

fn collect_action_identifiers(value: &serde_json::Value, out: &mut String, depth: usize) {
    if depth > 6 || out.len() >= 16 * 1024 {
        return;
    }
    match value {
        serde_json::Value::Array(values) => {
            for value in values.iter().take(64) {
                collect_action_identifiers(value, out, depth + 1);
            }
        }
        serde_json::Value::Object(values) => {
            for (key, value) in values.iter().take(64) {
                if matches!(
                    key.as_str(),
                    "tool_slug" | "tool" | "action" | "operation" | "method" | "name"
                ) {
                    out.push(' ');
                    out.push_str(key);
                    if let Some(value) = value.as_str() {
                        out.push(' ');
                        out.extend(value.chars().take(512));
                    }
                } else if matches!(
                    value,
                    serde_json::Value::Array(_) | serde_json::Value::Object(_)
                ) {
                    collect_action_identifiers(value, out, depth + 1);
                }
            }
        }
        _ => {}
    }
}

fn collect_effect_text(value: &serde_json::Value, out: &mut String, depth: usize) {
    if depth > 6 || out.len() >= 32 * 1024 {
        return;
    }
    match value {
        serde_json::Value::String(value) => {
            let remaining = (32usize * 1024).saturating_sub(out.len());
            out.push(' ');
            out.extend(value.chars().take(remaining));
        }
        serde_json::Value::Array(values) => {
            for value in values.iter().take(64) {
                collect_effect_text(value, out, depth + 1);
            }
        }
        serde_json::Value::Object(values) => {
            for (key, value) in values.iter().take(64) {
                out.push(' ');
                out.extend(key.chars().take(128));
                collect_effect_text(value, out, depth + 1);
            }
        }
        _ => {}
    }
}

fn call_targets_captcha(input: &serde_json::Value) -> bool {
    let mut text = String::new();
    collect_effect_text(input, &mut text, 0);
    let text = text.to_ascii_lowercase();
    ["captcha", "recaptcha", "hcaptcha", "turnstile challenge"]
        .iter()
        .any(|term| text.contains(term))
}

#[derive(Clone)]
pub struct ToolExecutor {
    workspace_root: PathBuf,
    /// Enabled only when the current acting provider can receive images.
    native_image_analysis: bool,
    /// Phoenix state root (`.phoenix/`). Holds the CodeGraph symbol index. When
    /// unset, codegraph tools fall back to `<workspace>/.phoenix`.
    state_root: Option<PathBuf>,
    mode: PermissionMode,
    interaction_mode: crate::runtime::InteractionMode,
    web: web::WebRuntime,
    ask_user_handler: Option<AskUserHandler>,
    /// Main session this executor serves — crons created by agents default to
    /// waking this session.
    session_id: Option<String>,
    /// Durable coworker browser-profile id. Browser tools route every turn for
    /// that coworker back to the same private Chrome, independent of job ids.
    browser_instance: Option<String>,
    /// Every live agent gets a distinct nested X desktop. The scope is only
    /// installed in thread-local state while a concrete tool call runs, so it
    /// never races through the process-global DISPLAY environment.
    desktop_scope: Option<isolated_desktop::DesktopScope>,
    /// One claim for the whole agent turn. Clones of this executor share the
    /// Arc, preventing an idle reaper from deleting a worker's display while
    /// its model is deliberating between tool calls.
    desktop_lease: Option<std::sync::Arc<isolated_desktop::DesktopLease>>,
    /// A disposable child browser may receive a portable-cookie snapshot from
    /// the spawning coworker's private browser. The scope carries this through
    /// to the lazy browser launch; cookie values never enter prompt/tool JSON.
    browser_parent_instance: Option<String>,
    /// Vault/account authority inherited by a disposable worker. This is
    /// intentionally separate from `browser_instance`: the latter isolates
    /// the worker's tabs and lifecycle, while this owner selects the spawning
    /// coworker's encrypted credential scopes.
    credential_agent_id: Option<String>,
    /// First-class group conversation currently hosting this turn. Used by
    /// scoped browser/credential grants; absent for direct coworker chats.
    group_id: Option<String>,
    /// Agent lane this executor serves ("frontend", "hacker", …). MCP servers
    /// carry a `route`; this is what it is matched against.
    agent: Option<String>,
    /// Lazily-opened checkpoint scope: one per executor instance (= one per
    /// mesh turn), created by the first file-mutating tool call. Shared across
    /// clones so a turn's edits land in a single rewindable scope.
    checkpoint_scope: std::sync::Arc<std::sync::OnceLock<String>>,
    /// One-call approvals minted by the turn loop. This set lives only on the
    /// executor clone used for that concrete call and is never persisted.
    approved_effects: std::collections::BTreeSet<GovernedEffect>,
}

/// Cooperative cancellation shared between the async turn and one blocking
/// tool invocation. Dropping the async wait requests cancellation; tools that
/// own an external process (notably `bash`) observe it and terminate/reap that
/// process group. Other synchronous tools acknowledge the request when their
/// handler returns, so a timeout can distinguish confirmed termination from a
/// still-running blocking action.
#[derive(Clone, Debug, Default)]
pub struct ToolCancellation {
    requested: std::sync::Arc<std::sync::atomic::AtomicBool>,
    confirmed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    unconfirmed_external_work: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ToolCancellation {
    pub(crate) fn request(&self) {
        self.requested
            .store(true, std::sync::atomic::Ordering::Release);
    }

    pub(crate) fn is_requested(&self) -> bool {
        self.requested.load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) fn confirm(&self) {
        self.confirmed
            .store(true, std::sync::atomic::Ordering::Release);
    }

    pub(crate) fn is_confirmed(&self) -> bool {
        self.confirmed.load(std::sync::atomic::Ordering::Acquire)
    }

    /// A handler returned, but could not prove that an external subprocess or
    /// side effect it owned had stopped. The provider loop must end the turn;
    /// feeding this back as an ordinary retryable tool failure could overlap a
    /// second mutation with the still-running first one.
    pub(crate) fn mark_unconfirmed(&self) {
        self.unconfirmed_external_work
            .store(true, std::sync::atomic::Ordering::Release);
    }

    pub(crate) fn has_unconfirmed_external_work(&self) -> bool {
        self.unconfirmed_external_work
            .load(std::sync::atomic::Ordering::Acquire)
    }
}

/// A bounded tool call either reached a terminal handler state or returned
/// while external work could still be running. The latter is deliberately a
/// separate type: callers must end the provider turn instead of asking the
/// model for another action and risking duplicate side effects.
#[derive(Debug)]
#[must_use = "unconfirmed tool termination must stop the provider turn"]
pub enum BoundedToolOutcome {
    Completed(ToolCallResult),
    Unconfirmed(ToolCallResult),
}

impl BoundedToolOutcome {
    pub fn is_unconfirmed(&self) -> bool {
        matches!(self, Self::Unconfirmed(_))
    }

    pub fn result(&self) -> &ToolCallResult {
        match self {
            Self::Completed(result) | Self::Unconfirmed(result) => result,
        }
    }

    pub fn into_result(self) -> ToolCallResult {
        match self {
            Self::Completed(result) | Self::Unconfirmed(result) => result,
        }
    }
}

struct CancelToolOnDrop {
    cancellation: ToolCancellation,
    armed: bool,
}

impl CancelToolOnDrop {
    fn new(cancellation: ToolCancellation) -> Self {
        Self {
            cancellation,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CancelToolOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.cancellation.request();
        }
    }
}

struct ConfirmToolOnReturn<'a>(&'a ToolCancellation);

impl Drop for ConfirmToolOnReturn<'_> {
    fn drop(&mut self) {
        // Record handler completion unconditionally. Otherwise a worker that
        // returns in the tiny interval immediately before the timeout requests
        // cancellation is permanently misclassified as still running.
        self.0.confirm();
    }
}

impl ToolExecutor {
    fn action_context(&self, call: &ToolCall) -> crate::runtime::extensions::ActionContext {
        let review_mode =
            crate::settings::effective_string("permissions.action_review", &self.settings_scope())
                .unwrap_or_else(|| "shadow".to_string());
        crate::runtime::extensions::ActionContext::new(
            self.session_id.clone(),
            self.agent.clone().unwrap_or_else(|| "phoenix".to_string()),
            call.tool_name.clone(),
            summarize_tool_input(&call.tool_name, &call.input),
            call.input.clone(),
            &self.workspace_root,
            self.mode.as_str().to_string(),
            "execute".to_string(),
            review_mode,
        )
    }

    pub(crate) fn settings_scope(&self) -> crate::settings::SettingsScope {
        if let Some(id) = self.group_id.as_deref() {
            crate::settings::SettingsScope::Group { id: id.to_string() }
        } else if let Some(id) = self
            .agent
            .as_deref()
            .filter(|id| !matches!(*id, "phoenix" | "orchestrator"))
        {
            crate::settings::SettingsScope::Agent { id: id.to_string() }
        } else {
            crate::settings::SettingsScope::Global
        }
    }

    fn setting_enabled(&self, key: &str) -> bool {
        crate::settings::effective_bool(key, &self.settings_scope()).unwrap_or(true)
    }

    pub fn new(workspace_root: impl Into<PathBuf>) -> Result<Self> {
        let workspace_root = workspace_root
            .into()
            .canonicalize()
            .context("failed to canonicalize tool workspace root")?;
        Ok(Self {
            workspace_root,
            native_image_analysis: false,
            state_root: None,
            mode: PermissionMode::Workspace,
            interaction_mode: crate::runtime::InteractionMode::Execute,
            web: web::WebRuntime::load(),
            ask_user_handler: None,
            session_id: None,
            browser_instance: None,
            desktop_scope: None,
            desktop_lease: None,
            browser_parent_instance: None,
            credential_agent_id: None,
            group_id: None,
            agent: None,
            checkpoint_scope: std::sync::Arc::new(std::sync::OnceLock::new()),
            approved_effects: std::collections::BTreeSet::new(),
        })
    }

    pub(crate) fn with_native_image_analysis(mut self, enabled: bool) -> Self {
        self.native_image_analysis = enabled;
        self
    }

    pub fn with_ask_user_handler(mut self, handler: AskUserHandler) -> Self {
        self.ask_user_handler = Some(handler);
        self
    }

    /// Name the main session this turn belongs to (cron tool default target).
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Key browser tools to a coworker's durable private browser profile.
    pub fn with_browser_instance(mut self, instance: Option<impl Into<String>>) -> Self {
        self.browser_instance = instance.map(Into::into);
        if let Some(scope) = self.desktop_scope.take() {
            self.desktop_scope = Some(scope.with_browser_instance(self.browser_instance.clone()));
        }
        self
    }

    /// Route GUI-capable tools through this agent's own Xephyr/Xvfb desktop.
    /// `None` retains the legacy host-desktop behavior for local CLI helpers
    /// and narrow unit tests that do not belong to a live mesh agent.
    pub fn with_desktop_scope(mut self, scope: Option<isolated_desktop::DesktopScope>) -> Self {
        self.desktop_scope = scope.map(|scope| {
            scope
                .with_browser_instance(self.browser_instance.clone())
                .with_parent_browser_instance(self.browser_parent_instance.clone())
        });
        self.desktop_lease = self
            .desktop_scope
            .as_ref()
            .map(isolated_desktop::acquire_lease);
        self
    }

    /// Set the spawning browser profile for a disposable worker. This does
    /// not copy a profile directory: browser launch takes a filtered in-memory
    /// cookie snapshot only after the child receives its own display/profile.
    pub fn with_browser_parent_instance(mut self, instance: Option<impl Into<String>>) -> Self {
        self.browser_parent_instance = instance.map(Into::into);
        if let Some(scope) = self.desktop_scope.take() {
            self.desktop_scope =
                Some(scope.with_parent_browser_instance(self.browser_parent_instance.clone()));
        }
        self
    }

    /// Inherit the spawning coworker's secret-safe vault/account authority.
    /// Secret material is never copied into the executor or model context;
    /// only the stable owner id is carried to scoped vault lookups.
    pub fn with_credential_agent_id(mut self, agent_id: Option<impl Into<String>>) -> Self {
        self.credential_agent_id = agent_id
            .map(Into::into)
            .filter(|value| !value.trim().is_empty());
        self
    }

    pub(crate) fn credential_agent_id(&self) -> anyhow::Result<String> {
        if let Some(agent_id) = self.credential_agent_id.as_deref() {
            return Ok(agent_id.to_string());
        }
        self.browser_instance
            .as_deref()
            .map(crate::tools::browser_cookie_grants::agent_id_for_profile)
            .transpose()?
            .context("credential tool has no coworker identity")
    }

    pub(crate) fn desktop_scope(&self) -> Option<&isolated_desktop::DesktopScope> {
        self.desktop_scope.as_ref()
    }

    pub(crate) fn retain_supported_desktop_tools(&self, tools: &mut Vec<ToolDefinition>) {
        let Some(scope)=self.desktop_scope.as_ref() else {return;};
        let window_supported=isolated_desktop::existing_desktops().iter()
            .any(|view|view.scope_key==scope.key()&&view.backend=="gnome"&&view.running);
        filter_private_desktop_tools(tools,window_supported);
    }

    /// Stable private-browser identity selected for this coworker. Runtime
    /// control paths use the same id as ordinary browser tools so human login
    /// and cookie import cannot accidentally land in a parallel profile.
    pub(crate) fn browser_instance_id(&self) -> Option<&str> {
        self.browser_instance.as_deref()
    }

    pub fn with_group_id(mut self, group_id: Option<impl Into<String>>) -> Self {
        self.group_id = group_id.map(Into::into);
        self
    }

    /// Name the agent lane this executor serves ("frontend", "hacker", …).
    /// MCP discovery uses it to honour each server's `route`. Left unset, no
    /// routing filter applies.
    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }

    /// Point CodeGraph tools at the Phoenix state root that owns
    /// `codegraph.sqlite`. Without this they default to `<workspace>/.phoenix`.
    pub fn with_state_root(mut self, state_root: impl Into<PathBuf>) -> Self {
        self.state_root = Some(state_root.into());
        self
    }

    /// Resolve the directory holding the codegraph index.
    fn codegraph_state_root(&self) -> PathBuf {
        self.state_root
            .clone()
            .unwrap_or_else(|| self.workspace_root.join(".phoenix"))
    }

    /// Set the confinement policy. Permission and path origin are deliberately
    /// separate: YOLO permits absolute paths outside the workspace, but a
    /// relative path still means "relative to this turn's workspace".
    pub fn with_permission_mode(mut self, mode: PermissionMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn with_interaction_mode(mut self, mode: crate::runtime::InteractionMode) -> Self {
        self.interaction_mode = mode;
        self
    }

    pub fn permission_mode(&self) -> PermissionMode {
        self.mode
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn permission_decision(
        &self,
        tool_name: &str,
        input: &serde_json::Value,
    ) -> ToolPermissionDecision {
        permission_decision_for_tool(self.mode, &self.workspace_root, tool_name, input)
    }

    pub fn with_effect_approved(mut self, effect: GovernedEffect) -> Self {
        self.approved_effects.insert(effect);
        self
    }

    pub fn governed_effect_decision(
        &self,
        tool_name: &str,
        input: &serde_json::Value,
    ) -> GovernedEffectDecision {
        let Some(effect) = governed_effect_for_call(tool_name, input) else {
            return GovernedEffectDecision::Allowed;
        };
        if self.approved_effects.contains(&effect) {
            return GovernedEffectDecision::Allowed;
        }
        let scope = self.settings_scope();
        match effect {
            GovernedEffect::ExternalSend => {
                match crate::settings::effective_string("permissions.external_send", &scope)
                    .as_deref()
                    .unwrap_or("ask")
                {
                    "allow" => GovernedEffectDecision::Allowed,
                    "deny" => GovernedEffectDecision::Denied {
                        effect,
                        reason: "External sends and publications are disabled for this coworker in Settings → Permissions.".into(),
                    },
                    _ => GovernedEffectDecision::RequiresApproval {
                        effect,
                        reason: "Your settings ask before anything goes out.".into(),
                    },
                }
            }
            GovernedEffect::ExternalDelete => {
                match crate::settings::effective_string("permissions.external_delete", &scope)
                    .as_deref()
                    .unwrap_or("ask")
                {
                    "allow" => GovernedEffectDecision::Allowed,
                    "deny" => GovernedEffectDecision::Denied {
                        effect,
                        reason: "External deletions are disabled for this coworker in Settings → Permissions.".into(),
                    },
                    _ => GovernedEffectDecision::RequiresApproval {
                        effect,
                        reason: "Your settings ask before anything outside Phoenix gets deleted.".into(),
                    },
                }
            }
            GovernedEffect::Purchase => {
                match crate::settings::effective_string("permissions.purchases", &scope)
                    .as_deref()
                    .unwrap_or("always_ask")
                {
                    "deny" => GovernedEffectDecision::Denied {
                        effect,
                        reason: "Purchases and paid-plan actions are disabled in Settings → Permissions.".into(),
                    },
                    _ => GovernedEffectDecision::RequiresApproval {
                        effect,
                        reason: "Money is involved, so Phoenix always checks first.".into(),
                    },
                }
            }
        }
    }

    /// The turn's checkpoint scope id, once a file-mutating tool has opened it.
    pub fn checkpoint_scope_id(&self) -> Option<&str> {
        self.checkpoint_scope.get().map(String::as_str)
    }

    /// Shadow-snapshot the target of a file-mutating tool before it runs, so
    /// `phoenix rewind` can restore the pre-turn state. Never blocks the edit:
    /// snapshot failure is logged and the tool proceeds.
    fn snapshot_for_rewind(&self, call: &ToolCall) -> Option<checkpoint::SnapshotReceipt> {
        let Some(path) = call.input.get("path").and_then(|v| v.as_str()) else {
            return None;
        };
        let abs = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.workspace_root.join(path)
        };
        let store = checkpoint::CheckpointStore::new(&self.codegraph_state_root());
        let scope = self
            .checkpoint_scope
            .get_or_init(checkpoint::CheckpointStore::new_scope_id);
        let session = self.session_id.as_deref().unwrap_or("default");
        match store.snapshot(session, scope, &self.workspace_root, &abs) {
            Ok(receipt) => Some(receipt),
            Err(error) => {
                tracing::warn!("checkpoint snapshot failed for {path}: {error:#}");
                None
            }
        }
    }

    fn discard_failed_checkpoint(&self, receipt: &checkpoint::SnapshotReceipt) {
        let store = checkpoint::CheckpointStore::new(&self.codegraph_state_root());
        match store.discard_failed_if_unchanged(receipt, &self.workspace_root) {
            Ok(true) => {
                tracing::debug!("discarded an unchanged checkpoint opened by a failed edit call")
            }
            Ok(false) => {}
            Err(error) => tracing::warn!(
                "failed edit checkpoint could not be safely discarded and was retained: {error:#}"
            ),
        }
    }

    pub fn execute(&self, call: ToolCall) -> ToolCallResult {
        let receipt = crate::runtime::extensions::begin_action(self.action_context(&call));
        if let Some(reason) = receipt.block_reason() {
            let input_summary = summarize_tool_input(&call.tool_name, &call.input);
            let result = ToolCallResult {
                tool_name: call.tool_name,
                input_summary,
                success: false,
                output: reason,
            };
            crate::runtime::extensions::finish_action(receipt, false, true, &result.output);
            return result;
        }
        let result = self.execute_with_cancellation(call, &ToolCancellation::default());
        crate::runtime::extensions::finish_action(receipt, result.success, true, &result.output);
        result
    }

    /// Run a synchronous tool on Tokio's blocking pool under a per-tool budget
    /// and, when configured, the caller's absolute turn deadline. A timed-out
    /// blocking task is never described as stopped unless the handler has
    /// actually returned (and `bash` has verified no live owned-group members
    /// and reaped its direct leader). Disposable volume workers intentionally
    /// pass no whole-turn deadline; their per-tool budget and user cancellation
    /// remain enforced.
    pub async fn execute_bounded(
        self: std::sync::Arc<Self>,
        call: ToolCall,
        budget: std::time::Duration,
        hard_deadline: Option<tokio::time::Instant>,
    ) -> BoundedToolOutcome {
        const CONFIRMATION_RESERVE: std::time::Duration = std::time::Duration::from_secs(2);
        const CONFIRMATION_POLL: std::time::Duration = std::time::Duration::from_millis(20);

        let receipt = crate::runtime::extensions::begin_action(self.action_context(&call));
        if let Some(reason) = receipt.block_reason() {
            let result = ToolCallResult {
                tool_name: call.tool_name.clone(),
                input_summary: summarize_tool_input(&call.tool_name, &call.input),
                success: false,
                output: reason,
            };
            crate::runtime::extensions::finish_action(receipt, false, true, &result.output);
            return BoundedToolOutcome::Completed(result);
        }
        let tool_name = call.tool_name.clone();
        let input_summary = summarize_tool_input(&call.tool_name, &call.input);
        let now = tokio::time::Instant::now();
        if let Some(hard_deadline) = hard_deadline {
            let remaining = hard_deadline.saturating_duration_since(now);
            if remaining.is_zero() || remaining <= CONFIRMATION_RESERVE {
                let result = ToolCallResult {
                    tool_name,
                    input_summary,
                    success: false,
                    output: "Tool execution was not started because the absolute turn deadline left no bounded cancellation window. No action was attempted by this call."
                        .to_string(),
                };
                crate::runtime::extensions::finish_action(receipt, false, true, &result.output);
                return BoundedToolOutcome::Completed(result);
            }
        }

        // Reserve a small piece of the absolute budget for cooperative process
        // termination and reaping. This is part of the turn budget, not extra
        // time appended after a timeout.
        let operation_deadline = hard_deadline
            .map(|hard_deadline| {
                std::cmp::min(
                    now + budget,
                    hard_deadline
                        .checked_sub(CONFIRMATION_RESERVE)
                        .unwrap_or(now),
                )
            })
            .unwrap_or_else(|| now + budget);
        let cancellation = ToolCancellation::default();
        let worker_cancellation = cancellation.clone();
        let mut cancel_on_drop = CancelToolOnDrop::new(cancellation.clone());
        let executor = std::sync::Arc::clone(&self);
        let started = std::time::Instant::now();
        let mut task = tokio::task::spawn_blocking(move || {
            executor.execute_with_cancellation(call, &worker_cancellation)
        });

        let outcome = match tokio::time::timeout_at(operation_deadline, &mut task).await {
            Ok(Ok(result)) => {
                cancel_on_drop.disarm();
                if cancellation.has_unconfirmed_external_work() {
                    BoundedToolOutcome::Unconfirmed(result)
                } else {
                    BoundedToolOutcome::Completed(result)
                }
            }
            Ok(Err(join_error)) => {
                cancel_on_drop.disarm();
                BoundedToolOutcome::Unconfirmed(ToolCallResult {
                    tool_name,
                    input_summary,
                    success: false,
                    output: format!(
                        "Tool execution panicked: {join_error}. External termination is NOT CONFIRMED; the turn must stop before another action is attempted."
                    ),
                })
            }
            Err(_) => {
                cancellation.request();
                let confirmation_deadline = hard_deadline
                    .map(|hard_deadline| {
                        std::cmp::min(
                            hard_deadline,
                            tokio::time::Instant::now() + CONFIRMATION_RESERVE,
                        )
                    })
                    .unwrap_or_else(|| tokio::time::Instant::now() + CONFIRMATION_RESERVE);
                while !cancellation.is_confirmed()
                    && tokio::time::Instant::now() < confirmation_deadline
                {
                    tokio::time::sleep(CONFIRMATION_POLL).await;
                }
                let confirmed = cancellation.is_confirmed();
                cancel_on_drop.disarm();
                let status = if confirmed {
                    "Cancellation was requested and termination was CONFIRMED. The handler returned; bash cancellation also verifies that its owned process group has no live members and reaps its direct leader. Partial side effects from before cancellation may still exist, so verify state before retrying."
                } else if hard_deadline.is_some() {
                    "Cancellation was requested but termination was NOT CONFIRMED before the absolute deadline. The underlying blocking action may still complete; verify current state and do not repeat the call blindly."
                } else {
                    "Cancellation was requested but termination was NOT CONFIRMED within the bounded cancellation reserve. The underlying blocking action may still complete; verify current state and do not repeat the call blindly."
                };
                let result = ToolCallResult {
                    tool_name,
                    input_summary,
                    success: false,
                    output: format!(
                        "Tool exceeded its bounded execution window after {:.1}s. {status}",
                        started.elapsed().as_secs_f64()
                    ),
                };
                if confirmed && !cancellation.has_unconfirmed_external_work() {
                    BoundedToolOutcome::Completed(result)
                } else {
                    BoundedToolOutcome::Unconfirmed(result)
                }
            }
        };
        let confirmed = !outcome.is_unconfirmed();
        let result = outcome.result();
        crate::runtime::extensions::finish_action(
            receipt,
            result.success,
            confirmed,
            &result.output,
        );
        outcome
    }

    fn execute_with_cancellation(
        &self,
        call: ToolCall,
        cancellation: &ToolCancellation,
    ) -> ToolCallResult {
        let desktop_scope = self.desktop_scope.clone();
        isolated_desktop::with_scope(desktop_scope, || {
            self.execute_with_cancellation_in_scope(call, cancellation)
        })
    }

    fn execute_with_cancellation_in_scope(
        &self,
        call: ToolCall,
        cancellation: &ToolCancellation,
    ) -> ToolCallResult {
        let input_summary = summarize_tool_input(&call.tool_name, &call.input);
        let _confirm_on_return = ConfirmToolOnReturn(cancellation);
        if cancellation.is_requested() {
            return ToolCallResult {
                tool_name: call.tool_name,
                input_summary,
                success: false,
                output: "Tool cancellation was requested before execution began; no action was attempted by this call."
                    .to_string(),
            };
        }
        let permission_decision = self.permission_decision(&call.tool_name, &call.input);
        if let ToolPermissionDecision::RequiresApproval {
            current,
            required,
            reason,
        } = &permission_decision
        {
            let receipt = serde_json::to_string(&permission_decision).unwrap_or_else(|_| {
                format!(
                    "{{\"status\":\"requires_approval\",\"current\":\"{}\",\"required\":\"{}\"}}",
                    current.as_str(),
                    required.as_str()
                )
            });
            return ToolCallResult {
                tool_name: call.tool_name.clone(),
                input_summary,
                success: false,
                output: format!(
                    "Permission required: `{}` needs {} (current: {}). {reason}\nPHOENIX_PERMISSION_REQUEST={receipt}",
                    call.tool_name,
                    required.display_name(),
                    current.display_name(),
                ),
            };
        }
        match self.governed_effect_decision(&call.tool_name, &call.input) {
            GovernedEffectDecision::Allowed => {}
            GovernedEffectDecision::Denied { reason, .. } => {
                return ToolCallResult {
                    tool_name: call.tool_name,
                    input_summary,
                    success: false,
                    output: reason,
                };
            }
            GovernedEffectDecision::RequiresApproval { effect, reason } => {
                let decision = GovernedEffectDecision::RequiresApproval {
                    effect,
                    reason: reason.clone(),
                };
                let receipt = serde_json::to_string(&decision).unwrap_or_else(|_| {
                    format!(
                        "{{\"status\":\"requires_approval\",\"effect\":\"{}\"}}",
                        effect.as_str()
                    )
                });
                return ToolCallResult {
                    tool_name: call.tool_name,
                    input_summary,
                    success: false,
                    output: format!(
                        "Action approval required for {}. {reason}\nPHOENIX_ACTION_APPROVAL_REQUEST={receipt}",
                        effect.display_name()
                    ),
                };
            }
        }
        // Relative paths always originate at the workspace. YOLO changes only
        // whether an explicitly absolute/escaping path is allowed.
        let fs_root = &self.workspace_root;
        // Full Access lifts the command guardrails too — bash runs at full
        // OS-user power. Narrow destructive-operation guards remain active.
        let confined = !matches!(self.mode, PermissionMode::FullAccess);
        // Browser tools (donor: browser-use) drive a native Rust Chromium session.
        if browser_native::is_browser_tool(&call.tool_name) {
            if !self.setting_enabled("browser.enabled") {
                return ToolCallResult {
                    tool_name: call.tool_name,
                    input_summary,
                    success: false,
                    output: "Browser tools are disabled for this coworker in Settings → Browser & Accounts."
                        .to_string(),
                };
            }
            if !self.setting_enabled("browser.auto_solve_captcha")
                && call_targets_captcha(&call.input)
            {
                return ToolCallResult {
                    tool_name: call.tool_name,
                    input_summary,
                    success: false,
                    output: "Automatic CAPTCHA handling is disabled for this coworker. Open the embedded login handoff and ask the user to complete the challenge."
                        .to_string(),
                };
            }
            return match browser_native::execute(
                &call.tool_name,
                call.input,
                self.browser_instance.as_deref(),
                self.session_id.as_deref(),
                self.credential_agent_id.as_deref(),
                self.group_id.as_deref(),
            ) {
                Ok(output) => ToolCallResult {
                    output: format_tool_output(&call.tool_name, output),
                    tool_name: call.tool_name,
                    input_summary,
                    success: true,
                },
                Err(error) => ToolCallResult {
                    tool_name: call.tool_name,
                    input_summary,
                    success: false,
                    // `{:#}` keeps the whole cause chain; `.to_string()` kept
                    // only the outer context and hid root causes from the
                    // agent (live 2026-07-06: "failed to launch Chromium"
                    // with the disk-full reason silently dropped).
                    output: format!("{error:#}"),
                },
            };
        }
        // ── "read before edit" guard (Claude Code safety feature) ──────────
        // In confined mode, ban edits to an EXISTING file unless it was read
        // via the `read` tool in this session. YOLO deliberately bypasses this
        // workflow gate. New file creation is always allowed. The guard is
        // per-session (shared across all agents in the same session).
        if confined && matches!(call.tool_name.as_str(), "write" | "str_replace") {
            if let Some(path_str) = call.input.get("path").and_then(|v| v.as_str()) {
                // Resolve the path the same way the tool will — workspace-relative
                // join + canonicalize for existing files.
                if let Ok(resolved) = resolve_workspace_path(fs_root, path_str, false, confined) {
                    // Only existing files need the guard. New files are allowed.
                    if resolved.exists() {
                        if let Some(canonical) = resolved.canonicalize().ok() {
                            if !crate::runtime::read_files::was_read(
                                self.session_id.as_deref(),
                                &canonical,
                            ) {
                                return ToolCallResult {
                                    tool_name: call.tool_name,
                                    input_summary,
                                    success: false,
                                    output: format!(
                                        "You must read this file before editing it. \
                                         Use the read tool on `{path_str}` first."
                                    ),
                                };
                            }
                        }
                    }
                }
            }
        }

        // Open the first-write-wins recovery point only after generic policy
        // gates have accepted the edit. Tool-specific validation still happens
        // below; a failed call hands this receipt back so an unchanged snapshot
        // can be removed without ever discarding evidence of a possible write.
        let checkpoint_receipt = if matches!(call.tool_name.as_str(), "write" | "str_replace") {
            self.snapshot_for_rewind(&call)
        } else {
            None
        };

        let result = match call.tool_name.as_str() {
            "routine" => {
                if !self.setting_enabled("workflows.enabled") {
                    return ToolCallResult {
                        tool_name: call.tool_name,
                        input_summary,
                        success: false,
                        output: "Saved workflows are disabled for this coworker in Settings → Workflows & Routines."
                            .to_string(),
                    };
                }
                let agent = self.agent.clone();
                let group_id = self.group_id.clone();
                parse_and_run(call.input, move |input| {
                    routine::execute(input, agent.as_deref(), group_id.as_deref())
                })
            }
            "work" => {
                let workspace = self.workspace_root.clone();
                let session_id = self.session_id.clone();
                let agent = self.agent.clone();
                parse_and_run(call.input, move |input| {
                    work::execute(&workspace, input, session_id.as_deref(), agent.as_deref())
                })
            }
            "talk" if !self.setting_enabled("agents.collaboration_enabled") => {
                Err(anyhow::anyhow!(
                    "Agent collaboration is disabled in Settings → Agents & Groups"
                ))
            }
            "volume_work" => Err(anyhow::anyhow!(
                "volume_work is an async agent runtime operation and cannot run through the synchronous tool executor"
            )),
            "recall" | "memory_recall" | "memory_save" | "vital_memory_write"
                if !self.setting_enabled("memory.enabled") =>
            {
                Err(anyhow::anyhow!(
                    "Long-term memory is disabled in Settings → Memory"
                ))
            }
            "read" => {
                let session_id = self.session_id.clone();
                let agent = self.agent.clone();
                let fs_root_clone = fs_root.clone();
                parse_and_run::<read::ReadInput>(call.input, move |input| {
                    let path_str = input.path.clone();
                    let question = input.question.clone().unwrap_or_else(|| "general orientation".to_string());
                    let result = read::execute_with_receipt(&fs_root_clone, confined, input);
                    // Record the read on success so the edit guard allows
                    // subsequent writes/str_replaces to this file.
                    if let Ok(execution) = &result {
                        if let Ok(resolved) =
                            resolve_workspace_path(&fs_root_clone, &path_str, false, confined)
                        {
                            if let Ok(canonical) = resolved.canonicalize() {
                                if let Some(sid) = session_id.as_deref() {
                                    crate::runtime::read_files::record(
                                        sid,
                                        &canonical,
                                        &execution.sha256,
                                        execution.bytes,
                                    );
                                    crate::runtime::company::mirror_read(
                                        sid,
                                        agent.as_deref().unwrap_or("orchestrator"),
                                        &canonical,
                                        &question,
                                        &execution.output.summary,
                                    );
                                }
                            }
                        }
                    }
                    result.map(|execution| execution.output)
                })
            }
            "write" => {
                let session_id = self.session_id.clone();
                parse_and_run(call.input, |input| {
                    write::execute_for_session(fs_root, confined, input, session_id.as_deref())
                })
            }
            "grep" => parse_and_run(call.input, |input| grep::execute(fs_root, confined, input)),
            "glob" => parse_and_run(call.input, |input| glob::execute(fs_root, confined, input)),
            "codebase_search" => {
                parse_and_run(call.input, |input| {
                    codebase_search::execute(fs_root, confined, input)
                })
            }
            "index_codebase" => {
                let state = self.codegraph_state_root();
                let workspace = self.workspace_root.clone();
                parse_and_run(call.input, move |input| {
                    codegraph_tools::index_codebase(&state, &workspace, input)
                })
            }
            "symbol_search" => {
                let state = self.codegraph_state_root();
                let workspace = self.workspace_root.clone();
                parse_and_run(call.input, move |input| {
                    codegraph_tools::symbol_search(&state, &workspace, input)
                })
            }
            "callers" => {
                let state = self.codegraph_state_root();
                let workspace = self.workspace_root.clone();
                parse_and_run(call.input, move |input| {
                    codegraph_tools::callers(&state, &workspace, input)
                })
            }
            "callees" => {
                let state = self.codegraph_state_root();
                let workspace = self.workspace_root.clone();
                parse_and_run(call.input, move |input| {
                    codegraph_tools::callees(&state, &workspace, input)
                })
            }
            "impact" => {
                let state = self.codegraph_state_root();
                let workspace = self.workspace_root.clone();
                parse_and_run(call.input, move |input| {
                    codegraph_tools::impact(&state, &workspace, input)
                })
            }
            "file_symbols" => {
                let state = self.codegraph_state_root();
                let workspace = self.workspace_root.clone();
                parse_and_run(call.input, move |input| {
                    codegraph_tools::file_symbols(&state, &workspace, input)
                })
            }
            "call_path" => {
                let state = self.codegraph_state_root();
                let workspace = self.workspace_root.clone();
                parse_and_run(call.input, move |input| {
                    codegraph_tools::call_path(&state, &workspace, input)
                })
            }
            "bash" => parse_and_run(call.input, |input| {
                bash::execute_cancellable(fs_root, confined, input, cancellation)
            }),
            "motion_graphics" => parse_and_run(call.input, |input| {
                motion_graphics::execute_cancellable(fs_root, confined, input, cancellation)
            }),
            "background_terminal" => parse_and_run(call.input, |input: bash::BashInput| {
                bash::validate_input(fs_root, confined, &input)?;
                let session=self.session_id.as_deref().context("background terminal needs an owning conversation")?;
                let actor=self.agent.as_deref().unwrap_or("phoenix");
                let executor=self.clone();
                let label=input.command.lines().next().unwrap_or("Terminal task").chars().take(160).collect::<String>();
                let record=terminal_jobs::start(session,actor,&label,move|token| {
                    isolated_desktop::with_scope(executor.desktop_scope.clone(),||
                        bash::execute_cancellable(&executor.workspace_root,confined,input,&token))
                })?;
                Ok(ToolOutput {summary:format!("Started {} in the background; completion is pending.",record.job_id),
                    content:serde_json::to_string(&record)?})
            }),
            "terminal_job" => {
                #[derive(Deserialize)]
                struct Input {job_id:String, action:String}
                parse_and_run(call.input, |input:Input| {
                    let session=self.session_id.as_deref().context("terminal job needs an owning conversation")?;
                    let actor=self.agent.as_deref().unwrap_or("phoenix");
                    let record=match input.action.as_str() {
                        "status"=>terminal_jobs::status(session,actor,&input.job_id)?,
                        "cancel"=>terminal_jobs::cancel(session,actor,&input.job_id)?,
                        _=>anyhow::bail!("terminal_job action must be status or cancel"),
                    };
                    Ok(ToolOutput {summary:format!("{}: {:?}",record.job_id,record.status),content:serde_json::to_string(&record)?})
                })
            },
            "remote_runner" => remote_runner::summary_json().map(|content| ToolOutput {
                summary: "Listed trusted remote runners.".to_string(),
                content: content.to_string(),
            }),
            "design_reference" => parse_and_run(call.input, design_refs::execute),
            "design_studio" => parse_and_run(call.input, design_studio::execute),
            // The turn loop owns this call: it opens the design controller.
            "design_website" => Err(anyhow::anyhow!("design_website is started by Iris's turn, not the general executor")),
            "ui_snap" => {
                let workspace = self.workspace_root.clone();
                run_async(call.input, move |input: ui_snap::UiSnapInput| async move {
                    ui_snap::execute(&workspace, input).await
                })
            }
            "image_gen" => {
                let workspace = self.workspace_root.clone();
                run_async(call.input, move |input: image_gen::ImageGenInput| async move {
                    let content = image_gen::execute(&workspace, input).await?;
                    Ok(ToolOutput {
                        summary: content.lines().next().unwrap_or("image generated").to_string(),
                        content,
                    })
                })
            }
            "image_analyze" => {
                let workspace = self.workspace_root.clone();
                let native = self.native_image_analysis;
                run_async(
                    call.input,
                    move |input: image_analyze::ImageAnalyzeInput| async move {
                        let content = if native {
                            image_analyze::prepare_native(&workspace, input)?
                        } else {
                            image_analyze::execute(&workspace, input).await?
                        };
                        Ok(ToolOutput {
                            summary: content
                                .lines()
                                .nth(1)
                                .unwrap_or("image analyzed")
                                .chars()
                                .take(120)
                                .collect(),
                            content,
                        })
                    },
                )
            }
            "skill" => {
                let workspace = self.workspace_root.clone();
                parse_and_run(call.input, move |input| skills::execute(&workspace, input))
            }
            "skill_install" => run_async(call.input, skills::install),
            "skill_search" => run_async(call.input, skills::search),
            // Composio "For You" via MCP (the user's connected apps; ck_ key).
            // The legacy Platform REST lane is archived (_archive/composio-rest-2026-07-02).
            "composio_search" => run_async(call.input, composio::mcp_search),
            "composio_schemas" => run_async(call.input, composio::mcp_schemas),
            "composio_run" => run_async(call.input, composio::mcp_run),
            "composio_connections" => run_async(call.input, composio::mcp_connections),
            // Local (stdio) MCP servers — external tool providers like T3MP3ST,
            // registered under [[profile.mcp_server]]. Discover then call.
            "mcp_servers" => {
                let agent = self.agent.clone();
                run_async(call.input, move |input| async move {
                    local_mcp::servers(input, agent.as_deref()).await
                })
            }
            "mcp_call" => {
                let agent = self.agent.clone();
                run_async(call.input, move |input| async move {
                    local_mcp::call(input, agent.as_deref()).await
                })
            }
            "tools_create" => run_async(call.input, agent_forge::execute_tools_create),
            "reverse_skill" => {
                let workspace = self.workspace_root.clone();
                let actor = self
                    .agent
                    .clone()
                    .unwrap_or_else(|| "orchestrator".to_string());
                run_async(call.input, move |input| async move {
                    reverse_skill::execute(&workspace, &actor, input).await
                })
            }
            "computer_status" => parse_and_run(call.input, computer_use::status),
            "computer_screenshot" => parse_and_run(call.input, computer_use::screenshot),
            "computer_move" => parse_and_run(call.input, computer_use::move_cursor),
            "computer_locate" => parse_and_run(call.input, computer_use::locate),
            "computer_read_text" => parse_and_run(call.input, computer_use::read_text),
            "computer_list_windows" => parse_and_run(call.input, computer_use::list_windows),
            "computer_focus_window" => parse_and_run(call.input, computer_use::focus_window),
            "computer_capture_window" => parse_and_run(call.input, computer_use::capture_window),
            "computer_window_act" => parse_and_run(call.input, computer_use::window_act),
            "computer_lower_window" => parse_and_run(call.input, computer_use::lower_window),
            "computer_app_targets" => parse_and_run(call.input, computer_use::app_targets),
            "computer_app_inspect" => parse_and_run(call.input, computer_use::app_inspect),
            "computer_app_locate" => parse_and_run(call.input, computer_use::app_locate),
            "computer_app_read" => parse_and_run(call.input, computer_use::app_read),
            "computer_click" => parse_and_run(call.input, computer_use::click),
            "computer_drag" => parse_and_run(call.input, computer_use::drag),
            "computer_scroll" => parse_and_run(call.input, computer_use::scroll),
            "computer_type" => parse_and_run(call.input, computer_use::type_text),
            "computer_key" => parse_and_run(call.input, computer_use::key),
            "computer_act" => parse_and_run(call.input, computer_use::act),
            "computer_open" => parse_and_run(call.input, computer_use::open),
            "computer_wait" => parse_and_run(call.input, computer_use::wait),
            "str_replace" => {
                let session_id = self.session_id.clone();
                parse_and_run(call.input, |input| {
                    str_replace::execute_for_session(
                        fs_root,
                        confined,
                        input,
                        session_id.as_deref(),
                    )
                })
            }
            "list_directory" => {
                parse_and_run(call.input, |input| {
                    list_directory::execute(fs_root, confined, input)
                })
            }
            "web_search" => {
                let web = self.web.clone();
                run_async(call.input, move |input| web_search::execute(input, web))
            }
            "transcribe_audio" => {
                let root = fs_root.clone();
                run_async(call.input, move |input: transcribe::TranscribeInput| async move {
                    let path = resolve_workspace_path(&root, &input.path, true, confined)?;
                    transcribe::execute(path).await
                })
            }
            "web_fetch" => {
                let web = self.web.clone();
                run_async(call.input, move |input| web_fetch::execute(input, web))
            }
            "web_scrape" => {
                let web = self.web.clone();
                run_async(call.input, move |input| web_scrape::execute(input, web))
            }
            "web_crawl" => {
                let web = self.web.clone();
                run_async(call.input, move |input| web_crawl::execute(input, web))
            }
            "cron" => {
                if !self.setting_enabled("schedules.enabled") {
                    return ToolCallResult {
                        tool_name: call.tool_name,
                        input_summary,
                        success: false,
                        output: "Scheduled work is disabled in Settings → Schedules."
                            .to_string(),
                    };
                }
                let session = self
                    .session_id
                    .clone()
                    .unwrap_or_else(|| "main-session".to_string());
                parse_and_run(call.input, move |input| cron_tool::execute(input, &session))
            }
            "credential_list" => {
                let agent_id = self.credential_agent_id();
                let group_id = self.group_id.clone();
                match agent_id {
                    Ok(agent_id) => parse_and_run(call.input, move |input| {
                        credentials::list(input, &agent_id, group_id.as_deref())
                    }),
                    Err(error) => Err(error),
                }
            }
            "credential_generate" => {
                let agent_id = self.credential_agent_id();
                let group_id = self.group_id.clone();
                match agent_id {
                    Ok(agent_id) => parse_and_run(call.input, move |input| {
                        credentials::generate(input, &agent_id, group_id.as_deref())
                    }),
                    Err(error) => Err(error),
                }
            }
            "pass_use" => {
                let agent_id = self.credential_agent_id();
                let group_id = self.group_id.clone();
                let browser_instance = self.browser_instance.clone();
                let session_id = self.session_id.clone();
                let browser_enabled = self.setting_enabled("browser.enabled");
                match agent_id {
                    Ok(agent_id) => parse_and_run(call.input, move |input: passes::PassUseInput| {
                        anyhow::ensure!(
                            browser_enabled || !input.target.starts_with("browser"),
                            "browser tools are disabled for this coworker in Settings → Browser & Accounts"
                        );
                        passes::use_pass(
                            input,
                            &agent_id,
                            group_id.as_deref(),
                            browser_instance.as_deref(),
                            session_id.as_deref(),
                        )
                    }),
                    Err(error) => Err(error),
                }
            }
            "ask_for_pass" => Err(anyhow::anyhow!(
                "ask_for_pass opens an inline popup and is only available inside a Phoenix conversation. Ask the user to add the pass in Settings → Passes, then use credential_list."
            )),
            "account_manage" => {
                let agent_id = self.credential_agent_id();
                let group_id = self.group_id.clone();
                match agent_id {
                    Ok(agent_id) => parse_and_run(call.input, move |input| {
                        accounts::execute(input, &agent_id, group_id.as_deref())
                    }),
                    Err(error) => Err(error),
                }
            }
            "ask_for_login" => {
                let agent_id = self.credential_agent_id();
                let result = agent_id.and_then(|agent_id| {
                    anyhow::ensure!(
                        self.setting_enabled("browser.enabled"),
                        "browser login is disabled for this coworker in Settings → Browser & Accounts"
                    );
                    let mut input: login_request::LoginRequestInput =
                        serde_json::from_value(call.input).context("invalid ask_for_login input")?;
                    input = input.validate_and_normalize()?;
                    let credential_scope = crate::tools::browser_cookie_grants::authorized_scope(
                        &input.scope,
                        &agent_id,
                        self.group_id.as_deref(),
                    )?;
                    let settings_scope = self.settings_scope();
                    let import_policy = crate::settings::effective_string(
                        "permissions.login_import",
                        &settings_scope,
                    )
                    .unwrap_or_else(|| "ask".into());
                    let account_policy = crate::settings::effective_string(
                        "permissions.account_creation",
                        &settings_scope,
                    )
                    .unwrap_or_else(|| "ask".into());
                    let account_enabled = crate::settings::effective_bool(
                        "browser.account_creation",
                        &settings_scope,
                    )
                    .unwrap_or(true);
                    input.methods.retain(|method| match method.as_str() {
                        "import_cookies" => import_policy != "deny",
                        "create_account" => account_enabled && account_policy != "deny",
                        _ => true,
                    });
                    anyhow::ensure!(
                        !input.methods.is_empty(),
                        "every requested login route is disabled in Settings"
                    );
                    let saved_credential = login_request::saved_credential_for_site(
                        &agent_id,
                        self.group_id.as_deref(),
                        &input.site,
                    )
                    .ok()
                    .flatten();
                    let mut decision = if saved_credential.is_some() {
                        login_request::LoginDecision::SavedCredential
                    } else if let Some(decision) =
                        login_request::automatic_decision(
                            &input,
                            &import_policy,
                            &account_policy,
                            account_enabled,
                        )
                    {
                        decision
                    } else {
                        let (ask, options) = input.to_ask(&agent_id, self.group_id.as_deref());
                        let answer = ask_user::execute(ask, self.ask_user_handler.as_ref())?;
                        login_request::decision_from_answer(&answer.content, &options)
                    };
                    let mut decision_result = login_request::decision_result(&input, decision);
                    if let Some(credential) = saved_credential.as_ref() {
                        decision_result["credential_id"] =
                            serde_json::json!(credential.credential_id);
                        decision_result["username_hint"] =
                            serde_json::json!(credential.username);
                        decision_result["credential_site"] =
                            serde_json::json!(credential.site);
                    }
                    if decision == login_request::LoginDecision::ImportCookies {
                        let imported = login_request::resolve_cookie_source().map(|source| {
                            browser_native::execute(
                                "browser_import_cookies",
                                serde_json::json!({
                                    "source": source,
                                    "site": input.site,
                                    "scope": input.scope,
                                }),
                                self.browser_instance.as_deref(),
                                self.session_id.as_deref(),
                                self.credential_agent_id.as_deref(),
                                self.group_id.as_deref(),
                            )
                            .map(|output| (source, output))
                        });
                        match imported.transpose() {
                            Ok(Some((source, imported))) => {
                                decision_result["import_receipt"] =
                                    serde_json::json!(imported.content);
                                decision_result["next_step"] = serde_json::json!(
                                    "Cookie import completed in this coworker's private browser. Verify once. If the site still asks for login, request only create_account and user_login next; do not retry the same import."
                                );
                                decision_result["source"] = serde_json::json!(source);
                            }
                            failure
                                if login_request::fallback_after_import_failure(
                                    &input,
                                    &account_policy,
                                    account_enabled,
                                ) == Some(login_request::LoginDecision::CreateAccount) =>
                            {
                                decision = login_request::LoginDecision::CreateAccount;
                                decision_result = login_request::decision_result(&input, decision);
                                decision_result["automatic_fallback"] = serde_json::json!(
                                    failure.err().map(|error| error.to_string()).unwrap_or_else(||
                                        "No supported browser cookie source was available.".to_string()
                                    )
                                );
                            }
                            Ok(None) => anyhow::bail!(
                                "no browser cookie source is available; choose one in Browser & Accounts or allow free account creation"
                            ),
                            Err(error) => return Err(error),
                        }
                    }
                    if decision == login_request::LoginDecision::CreateAccount
                        && account_policy != "allow_free"
                    {
                        let approval_id = accounts::grant_creation_approval(
                            &input.site,
                            credential_scope,
                            &agent_id,
                        )?;
                        decision_result["approval_id"] = serde_json::json!(approval_id);
                    }
                    Ok(ToolOutput {
                        summary: format!("login decision: {}", decision.as_str()),
                        content: decision_result.to_string(),
                    })
                });
                return match result {
                    Ok(output) => ToolCallResult {
                        tool_name: call.tool_name,
                        input_summary,
                        success: true,
                        output: format_tool_output("ask_for_login", output),
                    },
                    Err(error) => ToolCallResult {
                        tool_name: call.tool_name,
                        input_summary,
                        success: false,
                        output: format!("{error:#}"),
                    },
                };
            }
            "react" => {
                // A reaction is an acknowledgement, so a malformed one is not
                // worth failing a turn over — it degrades to no reaction.
                match serde_json::from_value::<react::ReactInput>(call.input)
                    .map_err(anyhow::Error::from)
                    .and_then(react::execute)
                {
                    Ok(output) => Ok(output),
                    Err(error) => Ok(ToolOutput {
                        summary: format!("react skipped: {error}"),
                        content: format!("react skipped: {error}"),
                    }),
                }
            }
            "ask_user" | "teach_workflow" => {
                let tool_name = call.tool_name.clone();
                let mut input: ask_user::AskUserInput = if tool_name == "teach_workflow" {
                    match serde_json::from_value::<ask_user::TeachWorkflowInput>(call.input)
                        .map_err(anyhow::Error::from)
                        .and_then(ask_user::TeachWorkflowInput::into_ask)
                    {
                        Ok(input) => input,
                        Err(error) => {
                            return ToolCallResult {
                                tool_name,
                                input_summary,
                                success: false,
                                output: format!("invalid teach_workflow input: {error:#}"),
                            }
                        }
                    }
                } else {
                    match serde_json::from_value(call.input) {
                        Ok(input) => input,
                        Err(error) => {
                            return ToolCallResult {
                                tool_name,
                                input_summary,
                                success: false,
                                output: format!("invalid ask_user input: {error}"),
                            }
                        }
                    }
                };
                ask_user::bind_runtime_context(
                    &mut input,
                    self.agent.as_deref().unwrap_or("phoenix"),
                    self.group_id.as_deref(),
                );
                let approval = input.approval.clone();
                let approval_was_presented = approval
                    .as_ref()
                    .is_some_and(|request| request.is_presented_in(&input.questions));
                let result = ask_user::execute(input, self.ask_user_handler.as_ref());
                if let (Ok(output), Some(approval), Some(session)) =
                    (&result, approval, self.session_id.as_deref())
                {
                    if approval.action == "permanent_agent"
                        && approval_was_presented
                        && approval.confirmed_by(&output.content)
                    {
                        if let Err(error) =
                            agent_forge::grant_permanent_hire(session, &approval.subject)
                        {
                            return ToolCallResult {
                                tool_name: call.tool_name,
                                input_summary,
                                success: false,
                                output: format!(
                                    "The user approved this hire, but Phoenix could not durably save the one-use approval receipt: {error:#}. No coworker was created."
                                ),
                            };
                        }
                    }
                }
                return match result {
                    Ok(output) => ToolCallResult {
                        tool_name: tool_name.clone(),
                        input_summary,
                        success: true,
                        output: format_tool_output(&tool_name, output),
                    },
                    Err(error) => ToolCallResult {
                        tool_name,
                        input_summary,
                        success: false,
                        output: format!("{error:#}"),
                    },
                };
            }
            "todo_write" => {
                let parsed: TodoWriteInput = match serde_json::from_value(call.input) {
                    Ok(p) => p,
                    Err(e) => {
                        return ToolCallResult {
                            tool_name: call.tool_name,
                            input_summary,
                            success: false,
                            output: format!("invalid todo_write input: {e}"),
                        }
                    }
                };
                todo::execute(parsed, self.session_id.as_deref().unwrap_or("main"))
            }
            "create_agent" => {
                let parsed: agent_forge::CreateAgentInput =
                    match serde_json::from_value(call.input) {
                        Ok(p) => p,
                        Err(e) => {
                            return ToolCallResult {
                                tool_name: call.tool_name,
                                input_summary,
                                success: false,
                                output: format!("invalid create_agent input: {e}"),
                            }
                        }
                    };
                if !agent_forge::permanent_hire_is_granted(
                    self.session_id.as_deref(),
                    &parsed.role,
                ) {
                    if let (Some(handler), Some(session_id)) =
                        (self.ask_user_handler.as_ref(), self.session_id.as_deref())
                    {
                        let ask = match agent_forge::permanent_hire_ask(&parsed) {
                            Ok(ask) => ask,
                            Err(error) => {
                                return ToolCallResult {
                                    tool_name: call.tool_name,
                                    input_summary,
                                    success: false,
                                    output: format!("invalid create_agent input: {error:#}"),
                                }
                            }
                        };
                        let approval = ask.approval.clone().expect("hire ask is typed");
                        let presented = approval.is_presented_in(&ask.questions);
                        let answer = match ask_user::execute(ask, Some(handler)) {
                            Ok(output) => output.content,
                            Err(error) => {
                                return ToolCallResult {
                                    tool_name: call.tool_name,
                                    input_summary,
                                    success: false,
                                    output: format!("The hiring question could not be completed: {error:#}"),
                                }
                            }
                        };
                        if !(presented && approval.confirmed_by(&answer)) {
                            return ToolCallResult {
                                tool_name: call.tool_name,
                                input_summary,
                                success: false,
                                output: "The user did not approve this permanent hire. No coworker was created."
                                    .to_string(),
                            };
                        }
                        if let Err(error) =
                            agent_forge::grant_permanent_hire(session_id, &approval.subject)
                        {
                            return ToolCallResult {
                                tool_name: call.tool_name,
                                input_summary,
                                success: false,
                                output: format!(
                                    "The hire was approved, but Phoenix could not save its one-use receipt: {error:#}. No coworker was created."
                                ),
                            };
                        }
                    }
                }
                return agent_forge::execute_create_agent(parsed, self.session_id.as_deref());
            }
            "agent_provision" => {
                let parsed: agent_forge::AgentProvisionInput =
                    match serde_json::from_value(call.input) {
                        Ok(p) => p,
                        Err(e) => {
                            return ToolCallResult {
                                tool_name: call.tool_name,
                                input_summary,
                                success: false,
                                output: format!("invalid agent_provision input: {e}"),
                            }
                        }
                    };
                return agent_forge::execute_agent_provision(parsed, self.agent.as_deref());
            }
            "vital_memory_write" => {
                let parsed: vital_memory::VitalMemoryWriteInput =
                    match serde_json::from_value(call.input) {
                        Ok(p) => p,
                        Err(e) => {
                            return ToolCallResult {
                                tool_name: call.tool_name,
                                input_summary,
                                success: false,
                                output: format!("invalid vital_memory_write input: {e}"),
                            }
                        }
                    };
                vital_memory::execute(parsed)
            }
            "memory_recall" => {
                return ToolCallResult {
                    tool_name: call.tool_name,
                    input_summary,
                    success: false,
                    output: "memory_recall is executed by the runtime (async graph search) — it cannot run through the sync tool executor. If you see this, the mesh interception is broken.".to_string(),
                }
            }
            "memory_save" => {
                return ToolCallResult {
                    tool_name: call.tool_name,
                    input_summary,
                    success: false,
                    output: "memory_save is executed by the runtime (async graph ingest) — it cannot run through the sync tool executor. If you see this, the mesh interception is broken.".to_string(),
                }
            }
            "recall" => {
                let parsed: recall::RecallInput = match serde_json::from_value(call.input) {
                    Ok(p) => p,
                    Err(e) => {
                        return ToolCallResult {
                            tool_name: call.tool_name,
                            input_summary,
                            success: false,
                            output: format!("invalid recall input: {e}"),
                        }
                    }
                };
                let archive_dir = self.codegraph_state_root().join("sessions");
                let session = self
                    .session_id
                    .clone()
                    .unwrap_or_else(|| "default".to_string());
                recall::execute(parsed, &archive_dir, &session)
            }
            other => Err(anyhow::anyhow!("unknown or non-executable tool: {other}")),
        };

        if result.is_err() {
            if let Some(receipt) = checkpoint_receipt.as_ref() {
                self.discard_failed_checkpoint(receipt);
            }
        }

        match result {
            Ok(output) => ToolCallResult {
                output: format_tool_output(&call.tool_name, output),
                tool_name: call.tool_name,
                input_summary,
                success: true,
            },
            Err(error) => ToolCallResult {
                tool_name: call.tool_name,
                input_summary,
                success: false,
                // Full cause chain — see the browser arm above.
                output: format!("{error:#}"),
            },
        }
    }
}

const KNOWN_TOOLS: &[(&str, &str)] = &[
    ("ask_user", "Ask the user for clarification or approval."),
    (
        "react",
        "Acknowledge a short request with a single emoji instead of a written reply.",
    ),
    (
        "teach_workflow",
        "Invite the user to demonstrate a browser workflow in this coworker's embedded private browser.",
    ),
    ("bash", "Run a shell command inside the workspace."),
    ("background_terminal", "Start a managed background shell job and resume this conversation when it actually completes."),
    ("terminal_job", "Inspect or cancel a managed terminal job owned by this conversation."),
    (
        "remote_runner",
        "List user-configured, host-key-pinned remote execution targets.",
    ),
    (
        "create_agent",
        "Request a NEW permanent responsibility owner under ~/.phoenix/agents/ (rare, approval-gated).",
    ),
    (
        "agent_provision",
        "Phoenix-only refinement and atomic publication of a requested coworker.",
    ),
    (
        "tools_create",
        "Build a NEW capability as a local MCP server in an agent's tools/ dir — scaffolded, registered, live-probed (orchestrator only).",
    ),
    (
        "reverse_skill",
        "Observe, review, canary, publish, or roll back a real trace-derived skill candidate.",
    ),
    (
        "cron",
        "Schedule a wake-up: at the set time the prompt arrives in this session as a user message.",
    ),
    (
        "credential_list",
        "List saved Passes (logins, cards, API keys, tokens) as metadata only — never secret values.",
    ),
    (
        "credential_generate",
        "Generate and store a strong password without exposing it to the model.",
    ),
    (
        "ask_for_pass",
        "Ask the user to save a login, card, API key, token, verification code, or secret through a secure inline popup.",
    ),
    (
        "pass_use",
        "Use a saved pass by id: Phoenix fills it into a browser field or an HTTP header without exposing it.",
    ),
    (
        "account_manage",
        "Track scoped account creation, verification, readiness, and blockers without storing secrets.",
    ),
    (
        "ask_for_login",
        "Ask the user how to establish site access using a structured login block.",
    ),
    (
        "computer_status",
        "Desktop control backend status (agent cursor bridge or X11).",
    ),
    (
        "computer_screenshot",
        "Capture the desktop to a file (vision caption rides along).",
    ),
    (
        "computer_move",
        "Glide the agent's overlay cursor to screen coordinates.",
    ),
    (
        "computer_click",
        "Click at coordinates (or current agent-cursor position).",
    ),
    ("computer_drag", "Drag from one point to another (desktop)."),
    ("computer_scroll", "Scroll at the agent-cursor position."),
    ("computer_type", "Type text into the focused window."),
    (
        "computer_key",
        "Send a key or combo (e.g. ctrl+l, enter) to the desktop.",
    ),
    (
        "computer_act",
        "Run an ordered batch of UI actions in one call, then screenshot once.",
    ),
    (
        "computer_open",
        "Open a non-browser desktop application, local file, or directory; web URLs stay in Phoenix's managed browser.",
    ),
    ("computer_wait", "Wait up to 10s for the desktop to settle."),
    (
        "computer_read_text",
        "OCR — transcribe all on-screen text verbatim (exact strings, dialogs, tables).",
    ),
    (
        "computer_list_windows",
        "List open windows (id, title, app, geometry) to target one specifically.",
    ),
    (
        "computer_focus_window",
        "Activate/raise a window by id (un-minimizes, switches workspace).",
    ),
    (
        "computer_capture_window",
        "Capture ONE window's live content by id — works even when fully covered by other windows.",
    ),
    (
        "computer_window_act",
        "Run window-relative actions on a window in the background (focus/pointer restored after).",
    ),
    (
        "computer_lower_window",
        "Push a window below the others so the agent's workspace never blocks the user.",
    ),
    (
        "computer_app_targets",
        "List apps reachable via accessibility (AT-SPI) to act on without mouse/focus.",
    ),
    (
        "computer_app_inspect",
        "Read a running app's UI elements + on-screen bounds via AT-SPI (perception; act with the mouse).",
    ),
    (
        "computer_app_locate",
        "Find elements by visible text (one query or a whole list) and get (cx,cy) for the mouse — a11y-grounded click targeting; misses report the closest visible texts.",
    ),
    (
        "computer_app_read",
        "Read EVERYTHING visible in an app in one call — the window's text in document order (AT-SPI).",
    ),
    (
        "computer_locate",
        "Vision-ground a described element to (x, y) coordinates.",
    ),
    (
        "browser_act",
        "Run a sequence of browser actions in one call (multi-act; state attaches once).",
    ),
    ("browser_click", "Click an element by [index] (browser)."),
    ("browser_close", "Close a browser tab by id."),
    (
        "browser_console",
        "Read captured console output and JS exceptions (browser).",
    ),
    (
        "browser_dropdown_options",
        "List a dropdown's options (browser).",
    ),
    (
        "browser_evaluate",
        "Run JavaScript on the page (browser, last resort).",
    ),
    (
        "browser_extract",
        "Extract the whole page's visible text (browser, large output).",
    ),
    (
        "browser_find_elements",
        "Query the DOM with a CSS selector (browser, free).",
    ),
    (
        "browser_find_text",
        "Scroll to the first occurrence of text (browser).",
    ),
    ("browser_go_back", "Go back one page in history (browser)."),
    (
        "browser_input",
        "Type text into a field by [index] (browser).",
    ),
    (
        "browser_input_credential",
        "Fill a site-bound secret from the encrypted vault without exposing it to the model.",
    ),
    ("browser_navigate", "Open a URL in the browser."),
    (
        "browser_save_as_pdf",
        "Save the current page as PDF (browser).",
    ),
    (
        "browser_download",
        "Download a URL to disk through the page's session (the only working download path headless).",
    ),
    (
        "browser_screenshot",
        "Capture a screenshot to a file (browser).",
    ),
    (
        "browser_scroll",
        "Scroll the page or a container (browser).",
    ),
    ("browser_search", "Search the web inside the browser."),
    (
        "browser_search_page",
        "Find text/regex on the current page (browser, free).",
    ),
    (
        "browser_select_dropdown",
        "Select a dropdown option by text (browser).",
    ),
    (
        "browser_send_keys",
        "Send keyboard keys/shortcuts (browser).",
    ),
    (
        "browser_status",
        "Inspect browser/CDP health and profile diagnostics.",
    ),
    (
        "browser_state",
        "Fresh browser state: URL, tabs, [index] elements.",
    ),
    (
        "browser_upload_file",
        "Attach a local file to a file input by [index] (browser).",
    ),
    (
        "browser_import_cookies",
        "Import only one site's portable cookies from a local browser with agent, group, or company scope.",
    ),
    ("browser_switch", "Switch to another tab by id (browser)."),
    ("browser_wait", "Wait for the page to settle (browser)."),
    (
        "callees",
        "List the indexed symbols a function or method calls.",
    ),
    (
        "callers",
        "List who calls an indexed symbol before you change it.",
    ),
    (
        "codebase_search",
        "Semantic-style code search over workspace chunks.",
    ),
    (
        "index_codebase",
        "Refresh the local codebase index once before exploration or after a completed coding task.",
    ),
    (
        "design_reference",
        "Read Phoenix's embedded visual-design contract and task references.",
    ),
    ("design_studio", "Offline semantic palettes, varied typography candidates, page-blueprint and copy checks."),
    ("design_website", "Start the staged website design workflow in a site folder (Iris decides when)."),
    (
        "image_gen",
        "Generate an image from a text prompt into artifacts/images/ (configured image model).",
    ),
    (
        "ui_snap",
        "Screenshot a URL (your dev server) with an isolated throwaway browser — the design lane's own eye.",
    ),
    (
        "image_analyze",
        "Read/understand any image file with the vision model — caption, verbatim text, or answer a question.",
    ),
    (
        "motion_graphics",
        "Make motion graphics, animation and video with the bundled motionmaxxing workflow: guide, render, and scripted gates.",
    ),
    (
        "skill",
        "List or load installed SKILL.md skills (Agent Skills standard).",
    ),
    (
        "skill_install",
        "Install a SKILL.md skill from skills.sh, GitHub, or a local path.",
    ),
    (
        "skill_search",
        "Search the skills.sh registry for installable skills.",
    ),
    (
        "composio_search",
        "Search the user's connected Composio apps for tools by use-case (start here).",
    ),
    (
        "composio_schemas",
        "Get exact input schemas for Composio tool slugs found by composio_search.",
    ),
    (
        "composio_run",
        "Execute one or more Composio app tools (found via composio_search).",
    ),
    (
        "composio_connections",
        "List or connect the user's Composio app accounts.",
    ),
    (
        "mcp_servers",
        "Discover the user's connected MCP servers (local or remote) and the tools each exposes. Start here before mcp_call.",
    ),
    (
        "mcp_call",
        "Invoke a tool on a connected MCP server: { server, tool, arguments }. Discover names/args via mcp_servers first.",
    ),
    (
        "file_symbols",
        "Compact indexed symbol outline of one file.",
    ),
    (
        "call_path",
        "Shortest indexed static call path between two symbols.",
    ),
    (
        "final_answer",
        "Finish the turn with a structured, validated final answer.",
    ),
    ("glob", "Find files by glob pattern."),
    ("grep", "Search workspace contents by pattern."),
    (
        "impact",
        "Transitive indexed callers affected if a symbol changes.",
    ),
    ("list_directory", "List files and directories at a path."),
    (
        "recall",
        "Search this session's compacted-out history for a fact, finding, or value instead of re-deriving it.",
    ),
    ("read", "Read a file from the workspace."),
    (
        "str_replace",
        "Replace exact text in a file with a targeted edit.",
    ),
    (
        "symbol_search",
        "Find symbols by meaning or name across the codebase index.",
    ),
    ("agent_control", "Inspect, steer, stop, or resume detached coworker jobs."),
    ("message_agent", "Send a non-blocking message, priority correction, or attachment to coworkers or a group."),
    ("talk", "Send a structured task or reply between agents."),
    (
        "volume_work",
        "Run a bounded batch of independent jobs through ephemeral generic workers.",
    ),
    (
        "routine",
        "Find, load, start, and record outcomes for human-taught reusable workflows.",
    ),
    ("work", "Inspect and update the team's durable work graph, evidence, and decisions."),
    (
        "memory_recall",
        "Search the long-term knowledge graph for remembered context.",
    ),
    (
        "memory_save",
        "Write one durable note into the long-term knowledge graph.",
    ),
    ("todo_write", "Create or update a structured task list."),
    (
        "transcribe_audio",
        "Transcribe speech in a local audio or video file (voice notes, recordings, meetings) to text.",
    ),
    (
        "vital_memory_write",
        "Remember a durable user preference, goal, boundary, or standing instruction in Phoenix's always-on vital memory (VITALS.md).",
    ),
    (
        "web_crawl",
        "Crawl a site via configured crawl provider (Firecrawl/Tavily).",
    ),
    (
        "web_fetch",
        "Fetch a URL (uses scrape provider when configured, else HTTP).",
    ),
    (
        "web_scrape",
        "Scrape a single URL via configured scrape provider.",
    ),
    (
        "web_search",
        "Search the web via configured search provider.",
    ),
    ("write", "Create or overwrite a file in the workspace."),
];

/// Build native tool definitions for an agent based on its allowlist.
/// These are passed directly to the provider in the tool-calling API format.
fn filter_private_desktop_tools(tools: &mut Vec<ToolDefinition>, window_supported: bool) {
    tools.retain(|tool|!tool.name.starts_with("computer_app_") &&
        (window_supported||!matches!(tool.name.as_str(),"computer_capture_window"|"computer_window_act"|"computer_lower_window")));
    for tool in tools.iter_mut().filter(|tool|tool.name=="computer_act") {
        if let Some(variants)=tool.parameters["properties"]["actions"]["items"]["anyOf"].as_array_mut() {
            variants.retain(|variant|!matches!(variant["properties"]["type"]["enum"][0].as_str(),Some("click_element"|"type_into")));
        }
    }
}

pub fn tool_definitions_for_agent(allowlist: &[String]) -> Vec<ToolDefinition> {
    let mut definitions = allowlist
        .iter()
        .filter_map(|name| tool_definition(name.as_str()))
        .collect::<Vec<_>>();
    if !allowlist.iter().any(|name| name == "work") {
        if let Some(definition) = tool_definition("work") {
            definitions.push(definition);
        }
    }
    definitions
}

fn tool_definition(name: &str) -> Option<ToolDefinition> {
    descriptions::tool_definition(name)
}

pub fn shared_tool_specs() -> Vec<ToolSpec> {
    KNOWN_TOOLS
        .iter()
        .map(|(name, description)| ToolSpec {
            name: (*name).to_string(),
            description: (*description).to_string(),
        })
        .collect()
}

/// Every Phoenix coworker can reach the complete company capability catalog.
/// Role manifests influence expertise and preferred ordering, not whether a
/// tool exists. Runtime permission modes, approval requests, credential scope,
/// and tool-specific safety gates still decide whether an individual call may
/// execute. Retired hidden-agent fan-out tools are not part of the catalog;
/// parallel browser work is ordinary coworker/group delegation.
pub fn universal_agent_tool_names() -> Vec<String> {
    KNOWN_TOOLS
        .iter()
        .map(|(name, _)| (*name).to_string())
        .collect()
}

/// Preserve an agent's role-optimized tool order, then append every remaining
/// company capability exactly once. This avoids stale exported manifests
/// silently freezing a coworker out of tools added by a later Phoenix build.
pub fn merge_with_universal_tools(preferred: Vec<String>) -> Vec<String> {
    let mut merged = Vec::new();
    for tool in preferred
        .into_iter()
        .chain(universal_agent_tool_names().into_iter())
    {
        if !matches!(
            tool.as_str(),
            "browser_swarm"
                | "agent_pipeline"
                | "tools_assign"
                | "browser_visibility"
                | "browser_login_handoff"
        ) && !merged.iter().any(|existing| existing == &tool)
        {
            merged.push(tool);
        }
    }
    merged
}

pub fn tool_spec(name: &str) -> Option<ToolSpec> {
    KNOWN_TOOLS
        .iter()
        .find(|(tool_name, _)| *tool_name == name)
        .map(|(tool_name, description)| ToolSpec {
            name: (*tool_name).to_string(),
            description: (*description).to_string(),
        })
}

pub fn validate_tool_allowlist(tool_names: &[String]) -> Result<(), String> {
    let unknown = tool_names
        .iter()
        .filter(|name| tool_spec(name).is_none())
        .cloned()
        .collect::<Vec<_>>();

    if unknown.is_empty() {
        Ok(())
    } else {
        Err(format!("unknown Phoenix tools: {}", unknown.join(", ")))
    }
}

fn parse_and_run<T>(
    input: serde_json::Value,
    run: impl FnOnce(T) -> Result<ToolOutput>,
) -> Result<ToolOutput>
where
    T: serde::de::DeserializeOwned,
{
    let parsed = serde_json::from_value(input).context("invalid tool input")?;
    run(parsed)
}

fn run_async<T, F, Fut>(input: serde_json::Value, run: F) -> Result<ToolOutput>
where
    T: serde::de::DeserializeOwned + Send + 'static,
    F: FnOnce(T) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<ToolOutput>> + Send,
{
    let parsed: T = serde_json::from_value(input).context("invalid tool input")?;
    let handle = tokio::runtime::Handle::try_current()
        .map_err(|_| anyhow::anyhow!("async tool requires a tokio runtime"))?;
    // Async tools run on a helper OS thread so their thread-local desktop
    // scope would otherwise disappear. Carry it explicitly: local MCP,
    // generated-tool probes, and any future GUI-capable async helper inherit
    // the same private X11 environment as synchronous tools.
    let desktop_scope = isolated_desktop::current_scope();
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                isolated_desktop::with_scope(desktop_scope, || handle.block_on(run(parsed)))
            })
            .join()
            .unwrap()
    })
}

fn resolve_workspace_path(
    workspace_root: &Path,
    input: &str,
    must_exist: bool,
    confined: bool,
) -> Result<PathBuf> {
    let root = workspace_root
        .canonicalize()
        .context("failed to canonicalize workspace root")?;
    let raw = Path::new(input);
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        root.join(raw)
    };

    let resolved = if joined.exists() {
        joined.canonicalize()?
    } else {
        if must_exist {
            bail!("path does not exist: {input}");
        }
        // The target doesn't exist yet (a write). The OLD code required the
        // IMMEDIATE parent to already exist, so writing `accounts/creds.txt`
        // into a workspace with no `accounts/` dir failed at resolution —
        // before the write tool's own create_dir_all ever ran. A browser task
        // hit this repeatedly and wasted rounds (2026-07-15). Instead, walk up to the
        // nearest EXISTING ancestor (the workspace root always exists),
        // canonicalize THAT, and re-append the not-yet-created tail. The
        // escape check below still guarantees the result stays inside root.
        let mut ancestor = joined.clone();
        let mut tail: Vec<std::ffi::OsString> = Vec::new();
        loop {
            let name = ancestor
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("path has no parent: {input}"))?
                .to_os_string();
            let parent = ancestor
                .parent()
                .ok_or_else(|| anyhow::anyhow!("path has no parent: {input}"))?
                .to_path_buf();
            tail.push(name);
            if parent.exists() {
                let mut resolved = parent
                    .canonicalize()
                    .with_context(|| format!("cannot resolve {input}"))?;
                for name in tail.iter().rev() {
                    resolved.push(name);
                }
                break resolved;
            }
            ancestor = parent;
        }
    };

    if confined && !resolved.starts_with(&root) {
        bail!("path escapes workspace: {input}");
    }

    Ok(resolved)
}

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

pub(crate) fn format_tool_output(_tool_name: &str, output: ToolOutput) -> String {
    let raw = if output.content.trim().is_empty() {
        output.summary
    } else {
        format!("{}\n{}", output.summary, output.content)
    };
    // Deterministic prune-before-context: every tool result is compressed
    // (lossless) before it becomes model context.
    compress::prune_lossless_for_ingestion(&raw)
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Truncate `s` to at most `max_bytes`, snapping DOWN to a UTF-8 char boundary
/// so the cut never lands mid-character. `String::truncate` panics on a
/// non-boundary index — and tool output routinely contains multibyte UTF-8
/// (accents, CJK, emoji), so a raw byte cut is a latent crash. No-op if already
/// within budget.
pub(crate) fn truncate_bytes(s: &mut String, max_bytes: usize) {
    if s.len() <= max_bytes {
        return;
    }
    let mut cut = max_bytes;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
}

#[cfg(test)]
mod truncate_bytes_tests {
    use super::truncate_bytes;

    #[test]
    fn never_splits_a_multibyte_char() {
        // 'é' is 2 bytes; cutting at byte 5 lands mid-char with a raw truncate.
        let mut s = "aaaaé tail".to_string();
        truncate_bytes(&mut s, 5);
        assert!(s.len() <= 5);
        assert!(s.is_char_boundary(s.len()));
        assert_eq!(s, "aaaa"); // snapped back off the half 'é'
    }

    #[test]
    fn leaves_short_strings_alone() {
        let mut s = "short".to_string();
        truncate_bytes(&mut s, 100);
        assert_eq!(s, "short");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn private_desktop_catalog_hides_unsupported_tools_and_action_variants() {
        let original=super::tool_definitions_for_agent(&super::universal_agent_tool_names());
        assert!(original.iter().any(|tool|tool.name=="computer_app_read"));
        for native in [false,true] {
            let mut offered=original.clone();
            super::filter_private_desktop_tools(&mut offered,native);
            assert!(!offered.iter().any(|tool|tool.name.starts_with("computer_app_")));
            for name in ["computer_capture_window","computer_window_act","computer_lower_window"] {
                assert_eq!(offered.iter().any(|tool|tool.name==name),native);
            }
            for name in ["computer_open","computer_screenshot","computer_act","read","recall"] {
                assert!(offered.iter().any(|tool|tool.name==name),"missing {name}");
            }
            let batch=offered.iter().find(|tool|tool.name=="computer_act").unwrap();
            let actions=batch.parameters["properties"]["actions"]["items"]["anyOf"].as_array().unwrap();
            assert_eq!(actions.len(),8);
            assert!(!actions.iter().any(|a|matches!(a["properties"]["type"]["enum"][0].as_str(),Some("click_element"|"type_into"))));
        }
    }
    use super::*;
    use crate::config::test_env::PhoenixHomeGuard;
    use tempfile::tempdir;

    #[test]
    fn workspace_tree_skips_vendored_and_hidden_and_respects_depth() {
        let root = tempdir().unwrap();
        let p = root.path();
        std::fs::write(p.join("Cargo.toml"), "x").unwrap();
        std::fs::create_dir_all(p.join("src/tools")).unwrap();
        std::fs::write(p.join("src/tools/bash.rs"), "x").unwrap();
        std::fs::create_dir_all(p.join("life_stealing_material/donor")).unwrap();
        std::fs::write(p.join("life_stealing_material/donor/huge.rs"), "x").unwrap();
        std::fs::create_dir_all(p.join("target/debug")).unwrap();
        std::fs::create_dir_all(p.join(".git/objects")).unwrap();

        let tree = workspace_tree(p);
        assert!(tree.contains("Cargo.toml"), "{tree}");
        assert!(tree.contains("src/"), "{tree}");
        assert!(tree.contains("bash.rs"), "depth-3 file shown: {tree}");
        // Vendored / build / VCS dirs are pruned entirely.
        assert!(!tree.contains("life_stealing_material"), "{tree}");
        assert!(!tree.contains("target"), "{tree}");
        assert!(!tree.contains(".git"), "{tree}");
    }

    #[test]
    fn tool_definitions_returns_definitions_for_allowlist() {
        let allowlist = vec![
            "read".to_string(),
            "write".to_string(),
            "grep".to_string(),
            "glob".to_string(),
            "bash".to_string(),
            "str_replace".to_string(),
            "list_directory".to_string(),
            "talk".to_string(),
        ];
        let defs = tool_definitions_for_agent(&allowlist);
        assert_eq!(defs.len(), 9);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"read"));
        assert!(names.contains(&"talk"));
        assert!(names.contains(&"work"));
        assert!(names.contains(&"str_replace"));
    }

    #[test]
    fn tool_definition_has_valid_json_schema() {
        let def = tool_definition("read").unwrap();
        assert_eq!(def.name, "read");
        assert!(def.description.contains("When NOT"));
        assert!(def.parameters.get("properties").is_some());
        let props = &def.parameters["properties"];
        assert!(props.get("path").is_some());
    }

    #[test]
    fn one_shot_login_dispatch_returns_the_same_typed_decision_contract() {
        let root = tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let handler: AskUserHandler = std::sync::Arc::new(|request| {
            assert_eq!(
                request.approval.as_ref().map(|value| value.action.as_str()),
                Some("login_request")
            );
            Ok("A: Create an account".to_string())
        });
        let executor = ToolExecutor::new(root.path())
            .unwrap()
            .with_browser_instance(Some("agent-phoenix"))
            .with_ask_user_handler(handler);
        let result = executor.execute(ToolCall {
            tool_name: "ask_for_login".to_string(),
            input: serde_json::json!({
                "site": "https://example.com/login",
                "reason": "finish the approved setup",
                "scope": "agent"
            }),
        });
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("\"decision\":\"create_account\""));
        assert!(result.output.contains("\"site\":\"example.com\""));
        assert!(result.output.contains("Generate a vault password"));
    }

    #[test]
    fn create_agent_owns_its_typed_approval_round_trip() {
        let root = tempdir().unwrap();
        let _home = PhoenixHomeGuard::set_private(root.path());
        let handler: AskUserHandler = std::sync::Arc::new(|request| {
            let approval = request.approval.as_ref().expect("typed hire approval");
            assert_eq!(approval.action, "permanent_agent");
            assert_eq!(approval.subject, "school_coach");
            assert_eq!(approval.approved_option, "Approve permanent hire");
            assert!(approval.is_presented_in(&request.questions));
            Ok("A: Approve permanent hire".to_string())
        });
        let result = ToolExecutor::new(root.path())
            .unwrap()
            .with_permission_mode(PermissionMode::FullAccess)
            .with_session_id("hire-round-trip-session")
            .with_ask_user_handler(handler)
            .execute(ToolCall {
                tool_name: "create_agent".to_string(),
                input: serde_json::json!({
                    "role": "school_coach",
                    "persona": "Avery",
                    "description": "Keeps coursework, deadlines, and study plans organized.",
                    "mission": "Protect the user's time through reliable school operations."
                }),
            });
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("Setting up"));
        assert!(root
            .path()
            .join("agents/school_coach/provisioning.json")
            .is_file());
    }

    #[test]
    fn declining_the_runtime_hire_card_creates_nothing() {
        let root = tempdir().unwrap();
        let _home = PhoenixHomeGuard::set_private(root.path());
        let handler: AskUserHandler = std::sync::Arc::new(|_| Ok("A: Do not hire".to_string()));
        let result = ToolExecutor::new(root.path())
            .unwrap()
            .with_permission_mode(PermissionMode::FullAccess)
            .with_session_id("hire-decline-session")
            .with_ask_user_handler(handler)
            .execute(ToolCall {
                tool_name: "create_agent".to_string(),
                input: serde_json::json!({
                    "role": "school_coach",
                    "persona": "Avery",
                    "description": "Keeps coursework organized."
                }),
            });
        assert!(!result.success);
        assert!(result.output.contains("did not approve"));
        assert!(!root.path().join("agents/school_coach").exists());
    }

    #[test]
    fn design_reference_dispatches_through_executor() {
        let dir = tempdir().unwrap();
        let exec = ToolExecutor::new(dir.path()).unwrap();
        // Index (no path) and a nested file, through the real dispatch path.
        let index = exec.execute(ToolCall {
            tool_name: "design_reference".to_string(),
            input: serde_json::json!({}),
        });
        assert!(index.success, "{}", index.output);
        assert!(index.output.contains("SKILL.md"));
        let file = exec.execute(ToolCall {
            tool_name: "design_reference".to_string(),
            input: serde_json::json!({"path": "references/slop-test.md"}),
        });
        assert!(file.success, "{}", file.output);
        assert!(file.output.to_lowercase().contains("gate"));
    }

    #[test]
    fn design_studio_dispatches_without_models_or_file_mutation() {
        let dir = tempdir().unwrap();
        let exec = ToolExecutor::new(dir.path()).unwrap();
        let result = exec.execute(ToolCall {
            tool_name: "design_studio".into(),
            input: serde_json::json!({"action":"typography","seed":"Phoenix landing"}),
        });
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("candidates"));
        assert!(tool_definition("design_studio").is_some());
        assert!(!crate::runtime::vision::invalidates_native_observation("design_studio"));
        let bad = exec.execute(ToolCall { tool_name:"design_studio".into(), input:serde_json::json!({"action":"execute","command":"touch forbidden"}) });
        assert!(!bad.success);
        assert!(!dir.path().join("forbidden").exists());
    }

    #[test]
    fn talk_schema_and_runtime_cover_the_whole_roster() {
        // `to` must remain OPEN: custom agents are created at runtime and a
        // provider-side enum would reject them before Phoenix can resolve the
        // registry. Runtime validation still rejects unknown/disabled names.
        let def = tool_definition("talk").unwrap();
        let to_schema = &def.parameters["properties"]["to"];
        assert_eq!(to_schema["type"], "string");
        assert!(
            to_schema.get("enum").is_none(),
            "talk targets must not be a closed enum"
        );

        for role in crate::sub_agents::registry::BUILTIN_ROLES {
            let agent = crate::runtime::delegation::specialist_from_talk_name(role)
                .unwrap_or_else(|| panic!("delegation cannot resolve built-in `{role}`"));
            assert!(
                crate::runtime::delegation::specialist_is_executable(agent),
                "built-in `{role}` resolves but is not executable"
            );
            assert_eq!(
                crate::runtime::delegation::specialist_label(agent),
                role,
                "registry label and talk resolver drifted for `{role}`"
            );
            let persona = crate::runtime::delegation::agent_persona(role)
                .unwrap_or_else(|| panic!("built-in `{role}` has no persona"));
            assert_eq!(
                crate::runtime::delegation::specialist_from_talk_name(persona),
                Some(agent),
                "persona `{persona}` does not route to built-in `{role}`"
            );
        }
        for (role, _) in crate::sub_agents::registry::production_custom_roster() {
            let agent = crate::runtime::delegation::specialist_from_talk_name(&role)
                .unwrap_or_else(|| panic!("production custom agent `{role}` is not talk-routable"));
            assert!(
                crate::runtime::delegation::specialist_is_executable(agent),
                "enabled custom agent `{role}` is not executable"
            );
        }
        let runtime_targets = crate::runtime::delegation::valid_talk_target_names();
        assert!(runtime_targets.iter().any(|name| name == "orchestrator"));
        assert!(runtime_targets.iter().any(|name| name == "user"));
    }

    #[test]
    fn design_reference_has_native_tool_definition() {
        let def = tool_definition("design_reference").unwrap();
        assert!(def.description.contains("When NOT"));
        assert!(def.parameters["properties"].get("path").is_some());
    }

    #[test]
    fn bash_definition_includes_cwd_not_cd_in_command() {
        let def = tool_definition("bash").unwrap();
        assert!(def.description.contains("cwd"));
        assert!(def.description.contains("not code browsing"));
        assert!(def.description.contains("read, grep, glob, list_directory"));
        assert!(def.parameters["properties"].get("cwd").is_some());
    }

    #[test]
    fn workspace_mode_confines_reads_to_launch_dir() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("inside.txt"), "hello").unwrap();
        let outside = tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "nope").unwrap();

        let exec = ToolExecutor::new(dir.path()).unwrap(); // Workspace by default
                                                           // Inside: allowed.
        let ok = exec.execute(ToolCall {
            tool_name: "read".to_string(),
            input: serde_json::json!({ "path": "inside.txt" }),
        });
        assert!(
            ok.success,
            "in-workspace read should succeed: {}",
            ok.output
        );
        // Outside via absolute path: rejected.
        let escaped = exec.execute(ToolCall {
            tool_name: "read".to_string(),
            input: serde_json::json!({ "path": outside.path().join("secret.txt") }),
        });
        assert!(!escaped.success);
        assert!(
            escaped.output.contains("Permission required")
                && escaped.output.contains("Full Access"),
            "{}",
            escaped.output
        );
    }

    #[test]
    fn legacy_plan_wire_value_no_longer_changes_execution_behavior() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("inside.txt"), "hello").unwrap();
        let legacy_mode: crate::runtime::InteractionMode =
            serde_json::from_str("\"plan\"").unwrap();
        let exec = ToolExecutor::new(dir.path())
            .unwrap()
            .with_interaction_mode(legacy_mode);

        let read = exec.execute(ToolCall {
            tool_name: "read".to_string(),
            input: serde_json::json!({ "path": "inside.txt" }),
        });
        assert!(
            read.success,
            "read-only discovery must work: {}",
            read.output
        );

        let write = exec.execute(ToolCall {
            tool_name: "write".to_string(),
            input: serde_json::json!({ "path": "changed.txt", "content": "no" }),
        });
        assert!(write.success, "legacy plan value must normalize to the default behavior: {}", write.output);
        assert_eq!(std::fs::read_to_string(dir.path().join("changed.txt")).unwrap(), "no");
    }

    #[test]
    fn full_access_mode_lifts_confinement() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "hello-from-outside").unwrap();

        let exec = ToolExecutor::new(dir.path())
            .unwrap()
            .with_permission_mode(PermissionMode::FullAccess);
        assert!(exec.permission_mode().is_full_access());
        let result = exec.execute(ToolCall {
            tool_name: "read".to_string(),
            input: serde_json::json!({ "path": secret }),
        });
        assert!(
            result.success,
            "full-access read outside should succeed: {}",
            result.output
        );
        assert!(result.output.contains("hello-from-outside"));
    }

    #[test]
    fn full_access_keeps_relative_paths_workspace_relative() {
        let workspace = tempdir().unwrap();
        let exec = ToolExecutor::new(workspace.path())
            .unwrap()
            .with_permission_mode(PermissionMode::FullAccess);

        let result = exec.execute(ToolCall {
            tool_name: "write".to_string(),
            input: serde_json::json!({
                "path": "artifacts/receipt.md",
                "content": "done",
                "purpose": "deliverable"
            }),
        });

        assert!(
            result.success,
            "relative Full Access write failed: {}",
            result.output
        );
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("artifacts/receipt.md")).unwrap(),
            "done"
        );
    }

    #[test]
    fn permission_modes_accept_legacy_yolo_and_use_product_names() {
        assert_eq!(
            serde_json::from_str::<PermissionMode>("\"yolo\"").unwrap(),
            PermissionMode::FullAccess
        );
        assert_eq!(
            serde_json::to_string(&PermissionMode::FullAccess).unwrap(),
            "\"full_access\""
        );
        assert_eq!(PermissionMode::Talk.display_name(), "Talk");
    }

    #[test]
    fn talk_workspace_and_full_access_are_effect_based() {
        let root = tempdir().unwrap();
        assert_eq!(
            permission_decision_for_tool(
                PermissionMode::Talk,
                root.path(),
                "memory_recall",
                &serde_json::json!({})
            ),
            ToolPermissionDecision::Allowed
        );
        for purpose_built_hire_tool in ["create_agent", "agent_provision"] {
            assert_eq!(
                permission_decision_for_tool(
                    PermissionMode::Talk,
                    root.path(),
                    purpose_built_hire_tool,
                    &serde_json::json!({})
                ),
                ToolPermissionDecision::Allowed
            );
        }
        assert!(matches!(
            permission_decision_for_tool(
                PermissionMode::Talk,
                root.path(),
                "read",
                &serde_json::json!({ "path": "src/lib.rs" })
            ),
            ToolPermissionDecision::RequiresApproval {
                required: PermissionMode::Workspace,
                ..
            }
        ));
        assert!(matches!(
            permission_decision_for_tool(
                PermissionMode::Workspace,
                root.path(),
                "browser_click",
                &serde_json::json!({ "index": 4 })
            ),
            ToolPermissionDecision::RequiresApproval {
                required: PermissionMode::FullAccess,
                ..
            }
        ));
        assert!(matches!(
            permission_decision_for_tool(
                PermissionMode::Workspace,
                root.path(),
                "glob",
                &serde_json::json!({ "pattern": "../Doorquoter/**/*.md" })
            ),
            ToolPermissionDecision::RequiresApproval {
                required: PermissionMode::FullAccess,
                ..
            }
        ));
        assert_eq!(
            permission_decision_for_tool(
                PermissionMode::FullAccess,
                root.path(),
                "glob",
                &serde_json::json!({ "pattern": "../Doorquoter/**/*.md" })
            ),
            ToolPermissionDecision::Allowed
        );
        assert!(matches!(
            permission_decision_for_tool(
                PermissionMode::Workspace,
                root.path(),
                "work",
                &serde_json::json!({
                    "action": "workflow",
                    "workflow_action": "reroute"
                })
            ),
            ToolPermissionDecision::RequiresApproval {
                required: PermissionMode::FullAccess,
                ..
            }
        ));
        assert_eq!(
            permission_decision_for_tool(
                PermissionMode::FullAccess,
                root.path(),
                "composio_run",
                &serde_json::json!({})
            ),
            ToolPermissionDecision::Allowed
        );
    }

    #[test]
    fn outward_sends_and_money_have_independent_runtime_gates() {
        let root = tempdir().unwrap();
        let executor = ToolExecutor::new(root.path())
            .unwrap()
            .with_permission_mode(PermissionMode::FullAccess);
        let send = serde_json::json!({
            "tools": [{
                "tool_slug": "GMAIL_SEND_EMAIL",
                "arguments": {"recipient": "person@example.com", "body": "hello"}
            }]
        });
        assert!(matches!(
            executor.governed_effect_decision("composio_run", &send),
            GovernedEffectDecision::RequiresApproval {
                effect: GovernedEffect::ExternalSend,
                ..
            }
        ));

        let purchase = serde_json::json!({
            "action": "click",
            "text": "Place order"
        });
        assert!(matches!(
            executor.governed_effect_decision("browser_click", &purchase),
            GovernedEffectDecision::RequiresApproval {
                effect: GovernedEffect::Purchase,
                ..
            }
        ));

        let approved = executor
            .clone()
            .with_effect_approved(GovernedEffect::ExternalSend);
        assert_eq!(
            approved.governed_effect_decision("composio_run", &send),
            GovernedEffectDecision::Allowed
        );
        assert_eq!(
            executor.governed_effect_decision(
                "composio_run",
                &serde_json::json!({
                    "tools": [{"tool_slug": "GMAIL_LIST_MESSAGES", "arguments": {}}]
                })
            ),
            GovernedEffectDecision::Allowed
        );
        assert!(matches!(
            executor.governed_effect_decision(
                "composio_run",
                &serde_json::json!({
                    "tools": [{"tool_slug": "NOTION_DELETE_PAGE", "arguments": {}}]
                })
            ),
            GovernedEffectDecision::RequiresApproval {
                effect: GovernedEffect::ExternalDelete,
                ..
            }
        ));
    }

    #[test]
    fn scoped_settings_change_real_external_effect_decisions() {
        with_isolated_home(|| {
            let root = tempdir().unwrap();
            let send = serde_json::json!({
                "tools": [{
                    "tool_slug": "GMAIL_SEND_EMAIL",
                    "arguments": {"recipient": "person@example.com", "body": "hello"}
                }]
            });
            let set = |key: &str,
                       value: serde_json::Value,
                       scope: crate::settings::SettingsScope,
                       revision: u64| {
                match crate::settings::execute(crate::settings::SettingsCommand::Set {
                    key: key.into(),
                    value,
                    scope,
                    expected_revision: Some(revision),
                })
                .unwrap()
                {
                    crate::settings::SettingsReply::Snapshot { snapshot } => snapshot.revision,
                    _ => unreachable!(),
                }
            };

            let revision = set(
                "permissions.external_send",
                serde_json::json!("deny"),
                crate::settings::SettingsScope::Global,
                0,
            );
            let revision = set(
                "permissions.external_send",
                serde_json::json!("allow"),
                crate::settings::SettingsScope::Agent { id: "iris".into() },
                revision,
            );
            set(
                "permissions.external_send",
                serde_json::json!("ask"),
                crate::settings::SettingsScope::Group {
                    id: "launch-room".into(),
                },
                revision,
            );

            let phoenix = ToolExecutor::new(root.path()).unwrap();
            assert!(matches!(
                phoenix.governed_effect_decision("composio_run", &send),
                GovernedEffectDecision::Denied { .. }
            ));

            let iris = ToolExecutor::new(root.path()).unwrap().with_agent("iris");
            assert_eq!(
                iris.governed_effect_decision("composio_run", &send),
                GovernedEffectDecision::Allowed
            );

            let launch_room = ToolExecutor::new(root.path())
                .unwrap()
                .with_agent("iris")
                .with_group_id(Some("launch-room"));
            assert!(matches!(
                launch_room.governed_effect_decision("composio_run", &send),
                GovernedEffectDecision::RequiresApproval { .. }
            ));
        });
    }

    #[test]
    fn capability_settings_stop_tools_before_their_handlers_run() {
        with_isolated_home(|| {
            let root = tempdir().unwrap();
            let executor = ToolExecutor::new(root.path())
                .unwrap()
                .with_permission_mode(PermissionMode::FullAccess);
            let mut revision = 0;
            for (key, tool, expected) in [
                (
                    "browser.enabled",
                    "browser_navigate",
                    "Browser tools are disabled",
                ),
                (
                    "workflows.enabled",
                    "routine",
                    "Saved workflows are disabled",
                ),
                (
                    "agents.collaboration_enabled",
                    "talk",
                    "Agent collaboration is disabled",
                ),
                ("memory.enabled", "recall", "Long-term memory is disabled"),
                ("schedules.enabled", "cron", "Scheduled work is disabled"),
            ] {
                revision = match crate::settings::execute(crate::settings::SettingsCommand::Set {
                    key: key.into(),
                    value: serde_json::json!(false),
                    scope: crate::settings::SettingsScope::Global,
                    expected_revision: Some(revision),
                })
                .unwrap()
                {
                    crate::settings::SettingsReply::Snapshot { snapshot } => snapshot.revision,
                    _ => unreachable!(),
                };
                let result = executor.execute(ToolCall {
                    tool_name: tool.into(),
                    input: serde_json::json!({}),
                });
                assert!(!result.success, "{tool} unexpectedly ran");
                assert!(
                    result.output.contains(expected),
                    "{tool} returned the wrong policy explanation: {}",
                    result.output
                );
            }
        });
    }

    #[test]
    fn denied_talk_mode_write_never_reaches_the_filesystem() {
        let root = tempdir().unwrap();
        let executor = ToolExecutor::new(root.path())
            .unwrap()
            .with_permission_mode(PermissionMode::Talk);
        let result = executor.execute(ToolCall {
            tool_name: "write".to_string(),
            input: serde_json::json!({
                "path": "must-not-exist.txt",
                "content": "unsafe"
            }),
        });
        assert!(!result.success);
        assert!(result.output.contains("PHOENIX_PERMISSION_REQUEST="));
        assert!(!root.path().join("must-not-exist.txt").exists());
    }

    #[test]
    fn tool_definition_unknown_returns_none() {
        assert!(tool_definition("nonexistent_tool").is_none());
        assert!(tool_definition("delegate").is_none());
    }

    #[test]
    fn codebase_search_definition_present() {
        let def = tool_definition("codebase_search").unwrap();
        assert_eq!(def.name, "codebase_search");
        assert!(def.description.contains("grep"));
    }

    #[test]
    fn validates_known_tool_allowlists() {
        let allowlist = vec![
            "read".to_string(),
            "write".to_string(),
            "bash".to_string(),
            "str_replace".to_string(),
            "list_directory".to_string(),
        ];
        assert!(validate_tool_allowlist(&allowlist).is_ok());
    }

    #[test]
    fn every_coworker_receives_the_complete_live_tool_catalog() {
        let merged = merge_with_universal_tools(vec![
            "talk".to_string(),
            "read".to_string(),
            "talk".to_string(),
            "browser_swarm".to_string(),
            "browser_visibility".to_string(),
            "browser_login_handoff".to_string(),
        ]);
        let expected = universal_agent_tool_names()
            .into_iter()
            .filter(|name| {
                !matches!(
                    name.as_str(),
                    "browser_swarm" | "agent_pipeline" | "tools_assign"
                )
            })
            .collect::<std::collections::BTreeSet<_>>();
        let actual = merged
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(actual, expected);
        assert_eq!(
            merged.iter().filter(|name| name.as_str() == "talk").count(),
            1
        );
        assert!(!merged.iter().any(|name| name == "browser_visibility"));
        assert!(!merged.iter().any(|name| name == "browser_login_handoff"));
        assert!(tool_definitions_for_agent(&[
            "browser_visibility".to_string(),
            "browser_login_handoff".to_string(),
        ])
        .iter()
        .all(|definition| {
            !matches!(
                definition.name.as_str(),
                "browser_visibility" | "browser_login_handoff"
            )
        }));
        for required in [
            "browser_navigate",
            "browser_input_credential",
            "credential_generate",
            "routine",
            "mcp_call",
            "talk",
        ] {
            assert!(
                merged.iter().any(|name| name == required),
                "missing {required}"
            );
        }
    }

    #[test]
    fn rejects_unknown_tools() {
        let allowlist = vec!["read".to_string(), "invented".to_string()];
        let error = validate_tool_allowlist(&allowlist).unwrap_err();
        assert!(error.contains("invented"));
    }

    #[test]
    fn executes_read_and_requests_full_access_for_path_escape() {
        let root_dir = tempdir().unwrap();
        let root = root_dir.path().to_path_buf();
        std::fs::write(root.join("sample.txt"), "Phoenix tool test").unwrap();
        let executor = ToolExecutor::new(&root).unwrap();

        let result = executor.execute(ToolCall {
            tool_name: "read".to_string(),
            input: serde_json::json!({ "path": "sample.txt" }),
        });
        assert!(result.success);
        assert!(result.output.contains("Phoenix tool test"));

        let escaped = executor.execute(ToolCall {
            tool_name: "read".to_string(),
            input: serde_json::json!({ "path": "../outside.txt" }),
        });
        assert!(!escaped.success);
        assert!(escaped.output.contains("Permission required"));
        assert!(escaped.output.contains("Full Access"));
        assert!(escaped.output.contains("outside the workspace"));
        assert!(escaped.output.contains("PHOENIX_PERMISSION_REQUEST"));
    }

    #[test]
    fn shared_tool_boundary_persists_structured_start_and_finish_receipts() {
        let home = tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let workspace = home.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("sample.txt"), "Phoenix ledger test").unwrap();
        let executor = ToolExecutor::new(&workspace)
            .unwrap()
            .with_session_id("ledger-session")
            .with_agent("iris");

        let result = executor.execute(ToolCall {
            tool_name: "read".to_string(),
            input: serde_json::json!({ "path": "sample.txt" }),
        });
        assert!(result.success);
        let rows = crate::runtime::action_ledger::recent(10)
            .unwrap()
            .into_iter()
            .filter(|row| {
                row.session_id.as_deref() == Some("ledger-session")
                    && row.agent_id == "iris"
                    && row.tool_name == "read"
            })
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].phase,
            crate::runtime::action_ledger::ActionPhase::Started
        );
        assert_eq!(
            rows[1].phase,
            crate::runtime::action_ledger::ActionPhase::Finished
        );
        assert_eq!(rows[1].session_id.as_deref(), Some("ledger-session"));
        assert_eq!(rows[1].agent_id, "iris");
        assert_eq!(rows[1].tool_name, "read");
        assert_eq!(rows[1].success, Some(true));
        assert_eq!(rows[1].execution_confirmed, Some(true));
    }

    #[test]
    fn write_requires_explicit_overwrite() {
        let root_dir = tempdir().unwrap();
        let root = root_dir.path().to_path_buf();
        std::fs::write(root.join("sample.txt"), "old").unwrap();
        let executor = ToolExecutor::new(&root).unwrap();

        let blocked = executor.execute(ToolCall {
            tool_name: "write".to_string(),
            input: serde_json::json!({ "path": "sample.txt", "content": "new" }),
        });
        assert!(!blocked.success);

        let written = executor.execute(ToolCall {
            tool_name: "write".to_string(),
            input: serde_json::json!({ "path": "sample.txt", "content": "new", "overwrite": true }),
        });
        assert!(written.success);
        assert_eq!(
            std::fs::read_to_string(root.join("sample.txt")).unwrap(),
            "new"
        );
    }

    #[test]
    fn bash_rejects_destructive_commands() {
        let root_dir = tempdir().unwrap();
        let root = root_dir.path().to_path_buf();
        let executor = ToolExecutor::new(&root).unwrap();

        let result = executor.execute(ToolCall {
            tool_name: "bash".to_string(),
            input: serde_json::json!({ "command": "git reset --hard" }),
        });

        assert!(!result.success);
        assert!(result.output.contains("unsafe command"));
    }

    #[test]
    fn bash_nonzero_exit_is_a_failed_tool_result() {
        let root_dir = tempdir().unwrap();
        let executor = ToolExecutor::new(root_dir.path()).unwrap();
        let result = executor.execute(ToolCall {
            tool_name: "bash".to_string(),
            input: serde_json::json!({
                "command": "printf diagnostic >&2; exit 9",
                "timeout_secs": 5
            }),
        });

        assert!(
            !result.success,
            "nonzero bash exit must not be evidence of success"
        );
        assert!(result.output.contains("FAILED with exit code 9"));
        assert!(result.output.contains("diagnostic"));
    }

    #[test]
    fn bounded_outcome_keeps_unconfirmed_work_typed() {
        let outcome = BoundedToolOutcome::Unconfirmed(ToolCallResult {
            tool_name: "mcp_call".to_string(),
            input_summary: "mutating call".to_string(),
            success: false,
            output: "termination not confirmed".to_string(),
        });
        assert!(outcome.is_unconfirmed());
        assert!(!outcome.into_result().success);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bounded_bash_confirms_process_group_termination_before_reporting_stop() {
        let root_dir = tempdir().unwrap();
        let root = root_dir.path().to_path_buf();
        let executor = std::sync::Arc::new(ToolExecutor::new(&root).unwrap());

        let outcome = executor
            .execute_bounded(
                ToolCall {
                    tool_name: "bash".to_string(),
                    input: serde_json::json!({
                        "command": "sleep 20; printf landed > late.txt",
                        "timeout_secs": 30
                    }),
                },
                std::time::Duration::from_millis(100),
                Some(tokio::time::Instant::now() + std::time::Duration::from_secs(3)),
            )
            .await;
        assert!(
            !outcome.is_unconfirmed(),
            "bash cancellation was not confirmed"
        );
        let result = outcome.into_result();

        assert!(!result.success);
        assert!(
            result.output.contains("termination was CONFIRMED"),
            "{}",
            result.output
        );
        assert!(
            !root.join("late.txt").exists(),
            "cancelled command must not resume its pending mutation"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropping_bounded_wait_cancels_mutating_process_group() {
        let root_dir = tempdir().unwrap();
        let root = root_dir.path().to_path_buf();
        let executor = std::sync::Arc::new(ToolExecutor::new(&root).unwrap());
        let task = tokio::spawn(executor.execute_bounded(
            ToolCall {
                tool_name: "bash".to_string(),
                input: serde_json::json!({
                    "command": "sleep 20 & child=$!; printf '%s' \"$child\" > child.pid; wait \"$child\"; printf landed > late.txt",
                    "timeout_secs": 30
                }),
            },
            std::time::Duration::from_secs(20),
            Some(tokio::time::Instant::now() + std::time::Duration::from_secs(30)),
        ));

        let pid_path = root.join("child.pid");
        for _ in 0..50 {
            if pid_path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let pid: i32 = std::fs::read_to_string(&pid_path)
            .expect("bash child published its pid")
            .trim()
            .parse()
            .unwrap();
        task.abort();
        let _ = task.await;

        let still_running = |pid: i32| {
            // SAFETY: signal 0 performs a liveness check and sends no signal.
            if unsafe { libc::kill(pid, 0) } != 0 {
                return false;
            }
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
            let state = stat
                .rsplit_once(") ")
                .and_then(|(_, rest)| rest.as_bytes().first().copied());
            !matches!(state, Some(b'Z' | b'X'))
        };
        for _ in 0..100 {
            if !still_running(pid) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(!still_running(pid), "child process remained live");
        assert!(
            !root.join("late.txt").exists(),
            "aborting the turn must cancel the pending mutation"
        );
    }

    fn with_isolated_home<T>(test: impl FnOnce() -> T) -> T {
        let home = tempdir().expect("isolated Phoenix home");
        let _home_guard = PhoenixHomeGuard::set_private(home.path());
        test()
    }

    /// Read a file, then edit it → should work (read recorded).
    #[test]
    fn read_before_edit_allows_edit_after_read() {
        with_isolated_home(|| {
            let root_dir = tempdir().unwrap();
            let root = root_dir.path().to_path_buf();
            std::fs::write(root.join("target.rs"), "old content").unwrap();
            let executor = ToolExecutor::new(&root)
                .unwrap()
                .with_session_id("test-session-1");

            // Read the file first.
            let read_result = executor.execute(ToolCall {
                tool_name: "read".to_string(),
                input: serde_json::json!({ "path": "target.rs" }),
            });
            assert!(read_result.success, "read should succeed");

            // Now str_replace should work.
            let replace_result = executor.execute(ToolCall {
                tool_name: "str_replace".to_string(),
                input: serde_json::json!({
                    "path": "target.rs",
                    "old_str": "old",
                    "new_str": "new"
                }),
            });
            assert!(
                replace_result.success,
                "str_replace after read should succeed: {}",
                replace_result.output
            );
            assert_eq!(
                std::fs::read_to_string(root.join("target.rs")).unwrap(),
                "new content"
            );

            // And write with overwrite should work too.
            let write_result = executor.execute(ToolCall {
                tool_name: "write".to_string(),
                input: serde_json::json!({
                    "path": "target.rs",
                    "content": "overwritten",
                    "overwrite": true
                }),
            });
            assert!(
                write_result.success,
                "write after read should succeed: {}",
                write_result.output
            );
        });
    }

    /// Edit an existing file without reading it first → should be rejected.
    #[test]
    fn read_before_edit_rejects_edit_without_read() {
        with_isolated_home(|| {
            let root_dir = tempdir().unwrap();
            let root = root_dir.path().to_path_buf();
            std::fs::write(root.join("unread.rs"), "original").unwrap();
            let executor = ToolExecutor::new(&root)
                .unwrap()
                .with_session_id("test-session-2");

            // str_replace without reading first → rejected.
            let replace_result = executor.execute(ToolCall {
                tool_name: "str_replace".to_string(),
                input: serde_json::json!({
                    "path": "unread.rs",
                    "old_str": "original",
                    "new_str": "modified"
                }),
            });
            assert!(!replace_result.success);
            assert!(
                replace_result
                    .output
                    .contains("must read this file before editing"),
                "should tell agent to read first: {}",
                replace_result.output
            );

            // write with overwrite without reading first → rejected.
            let write_result = executor.execute(ToolCall {
                tool_name: "write".to_string(),
                input: serde_json::json!({
                    "path": "unread.rs",
                    "content": "overwritten",
                    "overwrite": true
                }),
            });
            assert!(!write_result.success);
            assert!(
                write_result
                    .output
                    .contains("must read this file before editing"),
                "should tell agent to read first: {}",
                write_result.output
            );

            // File should be unchanged.
            assert_eq!(
                std::fs::read_to_string(root.join("unread.rs")).unwrap(),
                "original"
            );
        });
    }

    #[test]
    fn read_before_edit_rejects_a_stale_content_version() {
        with_isolated_home(|| {
            let root_dir = tempdir().unwrap();
            let root = root_dir.path().to_path_buf();
            std::fs::write(root.join("shared.rs"), "version one").unwrap();
            let executor = ToolExecutor::new(&root)
                .unwrap()
                .with_session_id("test-session-stale-read");

            let read_result = executor.execute(ToolCall {
                tool_name: "read".to_string(),
                input: serde_json::json!({ "path": "shared.rs" }),
            });
            assert!(read_result.success, "{}", read_result.output);

            // Simulate the user or another process editing after the receipt.
            std::fs::write(root.join("shared.rs"), "version two").unwrap();
            let write_result = executor.execute(ToolCall {
                tool_name: "write".to_string(),
                input: serde_json::json!({
                    "path": "shared.rs",
                    "content": "stale replacement",
                    "overwrite": true
                }),
            });
            assert!(
                !write_result.success,
                "stale overwrite unexpectedly succeeded"
            );
            assert!(
                write_result.output.contains("changed since it was read"),
                "{}",
                write_result.output
            );
            assert_eq!(
                std::fs::read_to_string(root.join("shared.rs")).unwrap(),
                "version two"
            );
        });
    }

    /// New file creation (file doesn't exist) → allowed without read.
    #[test]
    fn read_before_edit_allows_new_file_creation() {
        with_isolated_home(|| {
            let root_dir = tempdir().unwrap();
            let root = root_dir.path().to_path_buf();
            let executor = ToolExecutor::new(&root)
                .unwrap()
                .with_session_id("test-session-3");

            // Write a new file (doesn't exist) → should work.
            let write_result = executor.execute(ToolCall {
                tool_name: "write".to_string(),
                input: serde_json::json!({
                    "path": "new_file.rs",
                    "content": "brand new"
                }),
            });
            assert!(
                write_result.success,
                "new file creation should succeed: {}",
                write_result.output
            );
            assert_eq!(
                std::fs::read_to_string(root.join("new_file.rs")).unwrap(),
                "brand new"
            );
        });
    }

    #[test]
    fn failed_new_file_edit_does_not_leave_a_future_delete_checkpoint() {
        with_isolated_home(|| {
            let root_dir = tempdir().unwrap();
            let root = root_dir.path().to_path_buf();
            let executor = ToolExecutor::new(&root)
                .unwrap()
                .with_session_id("test-failed-new-file");

            // Missing `content` fails tool-input validation after the recovery
            // pre-image was opened. The executor must discard that unchanged
            // `existed:false` entry before a user later creates the same path.
            let failed = executor.execute(ToolCall {
                tool_name: "write".to_string(),
                input: serde_json::json!({ "path": "later.txt" }),
            });
            assert!(!failed.success);

            let store = checkpoint::CheckpointStore::new(&root.join(".phoenix"));
            assert!(store.scopes_result().unwrap().is_empty());
            std::fs::write(root.join("later.txt"), "user-created later").unwrap();
            assert!(store.rewind_latest(&root).unwrap().is_none());
            assert_eq!(
                std::fs::read_to_string(root.join("later.txt")).unwrap(),
                "user-created later"
            );
        });
    }

    /// Path normalization: read with relative path, edit with absolute path
    /// (or vice versa) → should work (same canonical path).
    #[test]
    fn read_before_edit_path_normalization() {
        with_isolated_home(|| {
            let root_dir = tempdir().unwrap();
            let root = root_dir.path().to_path_buf();
            std::fs::write(root.join("norm.rs"), "content").unwrap();
            let executor = ToolExecutor::new(&root)
                .unwrap()
                .with_session_id("test-session-4");

            // Read with relative path.
            let read_result = executor.execute(ToolCall {
                tool_name: "read".to_string(),
                input: serde_json::json!({ "path": "norm.rs" }),
            });
            assert!(read_result.success);

            // Edit with absolute path → should work (same canonical path).
            let abs_path = root.join("norm.rs");
            let replace_result = executor.execute(ToolCall {
                tool_name: "str_replace".to_string(),
                input: serde_json::json!({
                    "path": abs_path.to_string_lossy().to_string(),
                    "old_str": "content",
                    "new_str": "modified"
                }),
            });
            assert!(
                replace_result.success,
                "edit with absolute path after read with relative path should succeed: {}",
                replace_result.output
            );
        });
    }

    /// No session_id → guard is skipped (backward compatible).
    #[test]
    fn read_before_edit_skipped_without_session_id() {
        with_isolated_home(|| {
            let root_dir = tempdir().unwrap();
            let root = root_dir.path().to_path_buf();
            std::fs::write(root.join("no_session.rs"), "original").unwrap();
            // No with_session_id → session_id is None → guard skipped.
            let executor = ToolExecutor::new(&root).unwrap();

            let write_result = executor.execute(ToolCall {
                tool_name: "write".to_string(),
                input: serde_json::json!({
                    "path": "no_session.rs",
                    "content": "overwritten",
                    "overwrite": true
                }),
            });
            assert!(
                write_result.success,
                "write without session_id should succeed (guard skipped): {}",
                write_result.output
            );
        });
    }

    /// Full Access means normal unconfined file access: an existing file may be edited
    /// immediately even when session read tracking is active.
    #[test]
    fn full_access_skips_read_before_edit_gate() {
        with_isolated_home(|| {
            let root_dir = tempdir().unwrap();
            let root = root_dir.path().to_path_buf();
            std::fs::write(root.join("unread.rs"), "original").unwrap();
            let executor = ToolExecutor::new(&root)
                .unwrap()
                .with_session_id("test-session-yolo")
                .with_permission_mode(PermissionMode::FullAccess);

            let result = executor.execute(ToolCall {
                tool_name: "str_replace".to_string(),
                input: serde_json::json!({
                    "path": "unread.rs",
                    "old_str": "original",
                    "new_str": "modified"
                }),
            });

            assert!(
                result.success,
                "Full Access edit was gated: {}",
                result.output
            );
            assert_eq!(
                std::fs::read_to_string(root.join("unread.rs")).unwrap(),
                "modified"
            );
        });
    }

    #[test]
    fn disposable_worker_uses_parent_vault_owner_not_synthetic_browser_id() {
        with_isolated_home(|| {
            let root = tempdir().unwrap();
            let vault = crate::security::vault::Vault::open_default();
            vault
                .initialize("a sufficiently long test master password")
                .unwrap();
            let parent_credential = vault
                .put(
                    crate::security::vault::CredentialScope::agent("school_coach"),
                    "https://vvs-moodle.pembinahills.ca/login/index.php",
                    "VVS Moodle",
                    Some("student@example.test".to_string()),
                    "password",
                    "{}",
                    "never-return-this-test-secret",
                )
                .unwrap();

            let isolated = ToolExecutor::new(root.path())
                .unwrap()
                .with_permission_mode(PermissionMode::FullAccess)
                .with_browser_instance(Some("volume-worker-volume-batch-1"));
            let before = isolated.execute(ToolCall {
                tool_name: "credential_list".to_string(),
                input: serde_json::json!({"site": "vvs-moodle.pembinahills.ca"}),
            });
            assert!(before.success, "{}", before.output);
            assert!(
                !before.output.contains(&parent_credential.credential_id),
                "a synthetic worker identity must not accidentally receive an unrelated vault"
            );

            let executor = ToolExecutor::new(root.path())
                .unwrap()
                .with_permission_mode(PermissionMode::FullAccess)
                .with_browser_instance(Some("volume-worker-volume-batch-1"))
                .with_browser_parent_instance(Some("agent-school_coach"))
                .with_credential_agent_id(Some("school_coach"));

            assert_eq!(
                executor.browser_instance_id(),
                Some("volume-worker-volume-batch-1")
            );
            assert_eq!(executor.credential_agent_id().unwrap(), "school_coach");
            let after = executor.execute(ToolCall {
                tool_name: "credential_list".to_string(),
                input: serde_json::json!({"site": "vvs-moodle.pembinahills.ca"}),
            });
            assert!(after.success, "{}", after.output);
            assert!(after.output.contains(&parent_credential.credential_id));
            assert!(!after.output.contains("never-return-this-test-secret"));
        });
    }
}
