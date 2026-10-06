//! Canonical runtime contracts for Phoenix.
//!
//! These types are intentionally generic and small. They define the shared
//! envelopes between orchestrator, sub-agents, tools, and provider-backed
//! runtimes before any rich agent behavior is implemented.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::providers::{CompletionRequest, CompletionResponse};
use crate::session::SubAgentType;
use runner::PersistenceStatus;

pub(crate) const GATEWAY_LOG_MAX_BYTES: u64 = 20 * 1024 * 1024;

pub mod action_ledger;
pub mod compaction;
pub mod company_directory;
pub mod compressor;
pub mod design_contract;
pub mod efficiency;
pub mod extensions;
pub(crate) mod iris_design;
pub mod read_files;
pub(crate) mod tool_failure_guard;

/// Times the stretch between a turn arriving and its first provider call, and
/// makes whichever step ate it name itself.
///
/// "Why do I wait ~20s before it even starts thinking" was, until this existed,
/// unanswerable without guessing. The gateway log timestamps `turn start`,
/// `Resumed`, `pruned` and `thinking`, so the *gap* was visible — but half a
/// dozen distinct things happen inside it (loading every session off disk,
/// a bounded Composio network refresh, the project-brain digest over every
/// other thread, prompt assembly, the durable-session prune and its re-save),
/// and the log could not tell them apart. Every diagnosis was therefore a
/// hypothesis about which one was slow, including several confident ones that
/// measurement then contradicted.
///
/// Only steps that actually cost something are logged (see [`SLOW_STEP`]), so a
/// healthy turn stays exactly as quiet as it was and a slow one names its own
/// culprit in the file the user already reads.
pub struct TurnPhase {
    started: std::time::Instant,
    last: std::time::Instant,
    session: String,
}

/// Below this, a step is noise. The whole pre-thinking stretch should be well
/// under a second, so anything reaching this threshold is worth a line.
const SLOW_STEP: std::time::Duration = std::time::Duration::from_millis(150);

impl TurnPhase {
    pub fn start(session: &str) -> Self {
        let now = std::time::Instant::now();
        TurnPhase {
            started: now,
            last: now,
            session: session.to_string(),
        }
    }

    /// Close the step that just finished, logging it only if it was slow.
    pub fn step(&mut self, name: &str) {
        let took = self.last.elapsed();
        self.last = std::time::Instant::now();
        if took >= SLOW_STEP {
            gwlog(&format!(
                "  slow turn-start step [{}] {name} took {:.2}s (t+{:.2}s)",
                self.session,
                took.as_secs_f64(),
                self.started.elapsed().as_secs_f64(),
            ));
        }
    }

    /// Total pre-thinking latency — the number the user actually feels.
    pub fn total_secs(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }
}

/// One timestamped line appended to `~/.phoenix/gateway.log`, file-only —
/// never stdout (runtime code also runs inside the interactive CLI process,
/// where a stray println corrupts the TUI's in-place redraws). The daemon's
/// `glog` covers foreground turns; this is for runtime lanes with no daemon
/// handle — background specialists, whose entire tool activity (including
/// the asks behind "why did that coworker ask me this?") was invisible in the log
/// until 2026-07-07.
pub fn gwlog(line: &str) {
    if crate::config::test_isolated_from_live_home() {
        return;
    }
    let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let _ = append_gateway_log_line(&format!("[{stamp}] {line}\n"));
}

/// Append an already-rendered gateway-log record through the one shared
/// permission, rotation, and locking gate.
///
/// The target-derived advisory lock is the same lock used by the daemon's
/// foreground logger, so a background specialist or browser event cannot
/// race a rotation onto the old inode. The process mutex supplies the same
/// guarantee on platforms without `flock` and avoids needless lock-file
/// contention between local runtime threads.
pub(crate) fn append_gateway_log_line(rendered: &str) -> std::io::Result<()> {
    if crate::config::test_isolated_from_live_home() {
        return Ok(());
    }
    let path = crate::config::phoenix_home().join("gateway.log");
    append_private_rotating_log_at(&path, rendered.as_bytes(), GATEWAY_LOG_MAX_BYTES)
}

pub(crate) fn append_private_rotating_log_at(
    path: &std::path::Path,
    rendered: &[u8],
    max_bytes: u64,
) -> std::io::Result<()> {
    if max_bytes == 0 || rendered.len() as u64 > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "gateway log record is {} bytes; expected at most {max_bytes}",
                rendered.len()
            ),
        ));
    }

    static PROCESS_LOG_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    let _process_guard = PROCESS_LOG_LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    crate::config::private_io::with_private_lock(path, || {
        let mut file = open_rotating_private_log_append(path, rendered.len() as u64, max_bytes)?;
        use std::io::Write;
        file.write_all(rendered)?;
        Ok(())
    })
    .map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("failed to append private gateway log: {error:#}"),
        )
    })
}

fn rotated_log_path(path: &std::path::Path) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".1");
    std::path::PathBuf::from(name)
}

fn validate_private_log_metadata(
    path: &std::path::Path,
    metadata: &std::fs::Metadata,
) -> std::io::Result<()> {
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("refusing non-regular Phoenix log {}", path.display()),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.nlink() != 1 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "refusing Phoenix log {} with unsafe owner/link count",
                    path.display()
                ),
            ));
        }
    }
    Ok(())
}

/// Open an append-only Phoenix log without ever exposing its first bytes via
/// the process umask. Existing logs are repaired as well: credentials and
/// user/browser context can reach gateway.log, so a historical 0644 inode is
/// not safe merely because new installs use a stricter umask.
fn open_private_log_append(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    crate::config::private_io::prepare_private_parent(path).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("refusing unsafe Phoenix log parent: {error:#}"),
        )
    })?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    validate_private_log_metadata(path, &metadata)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

/// Return an append handle for the current log inode, rotating first when the
/// incoming record would cross the bound. Must be called while holding the
/// target-derived private lock.
fn open_rotating_private_log_append(
    path: &std::path::Path,
    incoming_bytes: u64,
    max_bytes: u64,
) -> std::io::Result<std::fs::File> {
    let file = open_private_log_append(path)?;
    let current_bytes = file.metadata()?.len();
    let needs_rotation = current_bytes >= max_bytes
        || (current_bytes > 0 && current_bytes.saturating_add(incoming_bytes) > max_bytes);
    if !needs_rotation {
        return Ok(file);
    }
    drop(file);

    let rotated = rotated_log_path(path);
    match std::fs::symlink_metadata(&rotated) {
        Ok(metadata) => {
            validate_private_log_metadata(&rotated, &metadata)?;
            std::fs::remove_file(&rotated)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::fs::rename(path, &rotated)?;
    if let Some(parent) = path.parent() {
        if let Ok(directory) = std::fs::File::open(parent) {
            let _ = directory.sync_all();
        }
    }
    open_private_log_append(path)
}

/// Provider stream ownership follows the execution's existing channel. There
/// is no mutable session-global slot and no cross-execution cleanup operation.
pub(crate) fn stream_observer(
    tx: tokio::sync::mpsc::Sender<CliEvent>,
) -> crate::providers::contracts::StreamObserver {
    crate::providers::contracts::StreamObserver::new(move |kind, text| {
        let _ = tx.try_send(CliEvent::StreamDelta {
            kind: kind.to_string(), text: text.to_string(),
        });
    })
}

#[cfg(test)]
mod live_ticker_tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn private_log_opener_creates_private_parent_and_repairs_existing_mode() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("phoenix-state/logs/gateway.log");
        drop(open_private_log_append(&path).unwrap());
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        drop(open_private_log_append(&path).unwrap());
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600,
            "opening a historical log must repair its mode"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rotating_log_append_preserves_complete_records_and_private_modes() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("phoenix-state/logs/gateway.log");
        append_private_rotating_log_at(&path, b"first-line\n", 12).unwrap();
        append_private_rotating_log_at(&path, b"next\n", 12).unwrap();

        let rotated = rotated_log_path(&path);
        assert_eq!(std::fs::read(&rotated).unwrap(), b"first-line\n");
        assert_eq!(std::fs::read(&path).unwrap(), b"next\n");
        for private_path in [&path, &rotated] {
            assert_eq!(
                std::fs::metadata(private_path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert!(append_private_rotating_log_at(&path, b"too-large", 4).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"next\n");
    }

    #[cfg(unix)]
    #[test]
    fn private_log_opener_rejects_special_files_and_shared_inodes() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let root = tempfile::tempdir().unwrap();
        let fifo = root.path().join("gateway.fifo");
        let raw = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(raw.as_ptr(), 0o600) }, 0);
        let started = std::time::Instant::now();
        assert!(open_private_log_append(&fifo).is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));

        let path = root.path().join("gateway.log");
        let alias = root.path().join("gateway-alias.log");
        std::fs::write(&path, b"existing\n").unwrap();
        std::fs::hard_link(&path, &alias).unwrap();
        assert!(open_private_log_append(&path).is_err());
        assert_eq!(std::fs::read(&alias).unwrap(), b"existing\n");
    }

    #[tokio::test]
    async fn execution_owned_stream_cancellation_closes_only_its_channel() {
        let (tx_a, mut rx_a) = tokio::sync::mpsc::channel::<CliEvent>(8);
        let (tx_b, mut rx_b) = tokio::sync::mpsc::channel::<CliEvent>(8);
        let a = stream_observer(tx_a);
        let b = stream_observer(tx_b);
        let task = tokio::spawn(async move {
            a.emit("text", "Iris");
            std::future::pending::<()>().await;
            drop(a);
        });
        assert!(matches!(rx_a.recv().await, Some(CliEvent::StreamDelta { text, .. }) if text == "Iris"));
        assert!(rx_b.try_recv().is_err());
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(tokio::time::timeout(std::time::Duration::from_secs(1), rx_a.recv()).await.unwrap().is_none(),
            "aborted execution must release its own channel without global cleanup");
        let retry = b.clone();
        drop(b);
        crate::providers::stream_progress_observed(Some("same-conversation"), Some(&retry), "Theo still running");
        assert!(matches!(rx_b.recv().await, Some(CliEvent::StreamDelta { kind, text }) if kind == "progress" && text == "Theo still running"));
        // Ticker backpressure must not stall provider response assembly.
        for _ in 0..1000 { retry.emit("text", "bounded"); }
        assert_eq!(rx_b.len(), 8);
        drop(retry);
        while rx_b.recv().await.is_some() {}
    }
}
pub mod agent_conversation;
pub mod asks;
pub mod build_contract;
pub(crate) mod user_error;
pub mod company;
pub mod company_activity;
pub mod company_control;
pub mod context_compiler;
pub(crate) mod default_goal;
pub mod delegation;
pub mod gateway;
pub mod group_conversation;
pub mod journal;
pub mod limits;
pub mod r#loop;
pub mod mailbox;
pub mod memory_beat;
pub mod memory_hooks;
pub mod mesh;
pub mod postbox;
pub mod project_brain;
pub mod prompt;
pub mod reverse_skill;
pub mod round_timing;
pub mod runner;
pub mod session_digest;
pub mod shared_contract;
mod spec;
pub mod story;
pub mod trace;
pub mod wire_history;
pub mod turn_anchor;
mod turns;
pub mod vision;
pub(crate) mod visual_progress;
pub mod workflow;
pub mod workflow_teaching;

pub use runner::{AgentRunner, RunnerMemoryPolicy, RuntimeExecution};
pub use spec::{
    AgentSpec, AgentTargetSpec, ExecutionStyle, OutputContract, PermissionProfile, WorkflowContract,
};
pub use trace::{
    preview_user_request, prune_old_run_traces, save_run_trace, RunDiagnostics, RunErrorTrace,
    TraceContext,
};
pub use turns::{
    parse_agent_turn_response, AgentTurnResponse, FinalResponse, RequestedToolCall,
    TurnToolTranscriptEntry,
};

/// Legacy wire compatibility for turns authored before Phoenix moved to one
/// default execution behavior. New product surfaces no longer expose a mode.
/// Plan remains deserializable only so old durable queue rows can be read.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum InteractionMode {
    #[default]
    Execute,
}

impl<'de> Deserialize<'de> for InteractionMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        match value.as_str() {
            // "plan" is accepted only to recover historical queue/session
            // records. It has no separate runtime behavior anymore.
            "execute" | "plan" => Ok(Self::Execute),
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["execute", "plan"],
            )),
        }
    }
}

/// Stable provenance for an authored turn that did not originate in the
/// composer.  Keep this typed all the way to the story lane: parsing a magic
/// prompt prefix in the UI cannot distinguish two executions of the same
/// recurring schedule.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TurnOrigin {
    GroupContinuation {
        original_turn_id: String,
        display: String,
    },
    Routine {
        routine_id: String,
        scheduled_for: String,
        schedule: String,
    },
    /// A detached `ask_user` card was answered after its asking turn had
    /// already settled. The runtime executes an internal continuation prompt,
    /// but Canvas must render the user's actual answer as the new turn
    /// boundary instead of exposing that internal envelope or making the
    /// coworker appear to restart on its own.
    AskAnswer {
        ask_id: String,
        #[serde(default)]
        agent_id: Option<String>,
        display: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEnvelope {
    pub id: String,
    pub session_id: String,
    pub target_agent: AgentTarget,
    pub title: String,
    pub user_request: String,
    pub context: Vec<ContextItem>,
    pub reply_expected: bool,
    #[serde(default)]
    pub interaction_mode: InteractionMode,
    /// Present for a direct first-class coworker conversation, including
    /// Phoenix when explicitly addressed. This binds the runtime role to the
    /// editable identity and the coworker's canonical endless transcript.
    #[serde(default)]
    pub agent: Option<crate::runtime::agent_conversation::AgentTurnContext>,
    /// Present when the user addressed a first-class company group rather
    /// than Phoenix or one coworker.
    #[serde(default)]
    pub group: Option<crate::runtime::group_conversation::GroupTurnContext>,
}

impl TaskEnvelope {
    pub fn new(
        session_id: impl Into<String>,
        target_agent: AgentTarget,
        title: impl Into<String>,
        user_request: impl Into<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.into(),
            target_agent,
            title: title.into(),
            user_request: user_request.into(),
            context: Vec::new(),
            reply_expected: true,
            interaction_mode: InteractionMode::Execute,
            agent: None,
            group: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentTarget {
    Orchestrator,
    Specialist(SubAgentType),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextItem {
    pub label: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentArtifact {
    pub kind: ArtifactKind,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ArtifactKind {
    Reply,
    Plan,
    Finding,
    CitationBundle,
    FilePatch,
    ToolTranscript,
    MemoryCandidate,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeCompletion {
    #[default]
    Unknown,
    Completed,
    Incomplete,
}

impl OutcomeCompletion {
    pub(crate) fn from_execution_mode(mode: &str) -> Self {
        match mode {
            "provider_failure_with_preserved_evidence" | "provider_error_fallback"
            | "runtime_boundary_with_preserved_evidence" | "no_progress_repeat_guard"
            | "incomplete_work" | "provider_validation_fallback" => Self::Incomplete,
            _ => Self::Completed,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentOutcome {
    #[serde(default)]
    pub completion: OutcomeCompletion,
    pub agent: AgentTarget,
    pub summary: String,
    pub artifacts: Vec<AgentArtifact>,
    pub tool_results: Vec<ToolCallResult>,
    pub provider_response: Option<ProviderTurn>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderTurn {
    pub request_model: String,
    pub output_text: String,
    /// TOTAL input tokens this turn. For a single provider call this is that
    /// call's prompt; for a MESH turn it is the SUM across every round — a burn
    /// metric (what the CLI footer and /burn report). It can exceed the context
    /// window on a multi-round turn (5 rounds × ~870k = 4.3M) and MUST NOT be
    /// divided by the window to get "how full is the context" — that is what
    /// `peak_input_tokens` is for.
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// PEAK single-call input this turn (max over rounds), NOT the sum. This is
    /// the real "context window usage" number: one request is bounded by the
    /// window, so peak/window is always ≤ 100%. The window gauge uses THIS; the
    /// sum above is only for cost. (Single-call turns: peak == input_tokens.)
    #[serde(default)]
    pub peak_input_tokens: u32,
}

impl From<CompletionResponse> for ProviderTurn {
    fn from(value: CompletionResponse) -> Self {
        let output_text = if value.content.trim().is_empty() && !value.tool_calls.is_empty() {
            let calls = value
                .tool_calls
                .iter()
                .map(|call| format!("{}({})", call.tool_name, call.arguments))
                .collect::<Vec<_>>()
                .join(", ");
            format!("Provider requested tool call(s): {calls}")
        } else {
            value.content
        };

        Self {
            request_model: value.model,
            output_text,
            input_tokens: value.usage.input_tokens,
            output_tokens: value.usage.output_tokens,
            // A single provider call: its peak IS its own input.
            peak_input_tokens: value.usage.input_tokens,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub tool_name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallResult {
    pub tool_name: String,
    pub input_summary: String,
    pub success: bool,
    pub output: String,
}

pub fn summarize_tool_input(tool_name: &str, input: &serde_json::Value) -> String {
    let value = |key: &str| input.get(key).and_then(|value| value.as_str());
    let summary = match tool_name {
        "bash" => value("command").unwrap_or_default().to_string(),
        "read" => {
            let path = value("path")
                .or_else(|| value("file_path"))
                .unwrap_or_default();
            match (
                input.get("offset").and_then(serde_json::Value::as_u64),
                input.get("limit").and_then(serde_json::Value::as_u64),
            ) {
                (Some(offset), Some(limit)) => {
                    format!("{path} from line {} for {limit} lines", offset.max(1))
                }
                (Some(offset), None) => format!("{path} from line {}", offset.max(1)),
                (None, Some(limit)) => format!("{path}, first {limit} lines"),
                (None, None) => path.to_string(),
            }
        }
        "write" => match (
            value("path"),
            input.get("overwrite").and_then(|value| value.as_bool()),
        ) {
            (Some(path), Some(true)) => format!("overwrite {path}"),
            (Some(path), _) => path.to_string(),
            _ => String::new(),
        },
        "str_replace" => match (
            value("path"),
            input
                .get("allow_multiple")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
        ) {
            (Some(path), true) => format!("replace-all in {path}"),
            (Some(path), false) => format!("replace in {path}"),
            _ => String::new(),
        },
        "grep" => match (value("pattern"), value("path")) {
            (Some(pattern), Some(path)) => format!("{pattern} in {path}"),
            (Some(pattern), None) => pattern.to_string(),
            _ => String::new(),
        },
        "glob" => value("pattern").unwrap_or_default().to_string(),
        "codebase_search" => {
            let q = value("query").unwrap_or_default();
            match input
                .get("target_directories")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
            {
                Some(n) if n > 0 => format!("{q} ({n} dirs)"),
                _ => q.to_string(),
            }
        }
        "list_directory" => value("path").unwrap_or_default().to_string(),
        "talk" => {
            let to = value("to").unwrap_or("?");
            let subject = value("subject").unwrap_or("?");
            let body_len = value("body").map(|b| b.chars().count()).unwrap_or(0);
            format!("{to}: {subject} ({body_len} chars body)")
        }
        "receive_specialist_result" => value("from").unwrap_or("specialist").to_string(),
        "web_search" => value("query").unwrap_or_default().to_string(),
        "web_fetch" | "web_scrape" => value("url").unwrap_or_default().to_string(),
        "web_crawl" => value("url").unwrap_or_default().to_string(),
        "ask_user" => {
            let count = input
                .get("questions")
                .and_then(|q| q.as_array())
                .map(|arr| arr.len())
                .unwrap_or(0);
            let first_header = input
                .get("questions")
                .and_then(|q| q.as_array())
                .and_then(|arr| arr.first())
                .and_then(|item| item.get("header"))
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if first_header.is_empty() {
                format!("{count} question(s)")
            } else {
                format!("{count} question(s): {first_header}")
            }
        }
        "todo_write" => {
            let count = input
                .get("todos")
                .and_then(|t| t.as_array())
                .map(|arr| arr.len())
                .unwrap_or(0);
            let done = input
                .get("todos")
                .and_then(|t| t.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter(|item| {
                            (item.get("completed").and_then(|c| c.as_bool()).unwrap_or(false)
                                || item.get("status").and_then(|s| s.as_str()) == Some("completed"))
                        })
                        .count()
                })
                .unwrap_or(0);
            let next_open = input
                .get("todos")
                .and_then(|t| t.as_array())
                .and_then(|arr| {
                    arr.iter().find_map(|item| {
                        let completed = item
                            .get("completed")
                            .and_then(|c| c.as_bool())
                            .unwrap_or(false)
                            || matches!(item.get("status").and_then(|s| s.as_str()), Some("completed" | "cancelled"));
                        if completed {
                            None
                        } else {
                            item.get("task").and_then(|t| t.as_str())
                        }
                    })
                })
                .unwrap_or("");
            if next_open.is_empty() {
                format!("{done}/{count} todos")
            } else {
                format!("{done}/{count} todos: {next_open}")
            }
        }
        _ => {
            if input.is_null() {
                String::new()
            } else {
                let compact = input.to_string();
                if compact.chars().count() > 120 {
                    format!("{}...", compact.chars().take(120).collect::<String>())
                } else {
                    compact
                }
            }
        }
    };

    if summary.chars().count() > 120 {
        format!("{}...", summary.chars().take(120).collect::<String>())
    } else {
        summary
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SessionScope {
    Main,
    Specialist(SubAgentType),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum LibrarianPhase {
    Preload,
    Prune,
    Save,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryBundle {
    pub session_scope: SessionScope,
    pub task_frame: String,
    pub loaded_memory_paths: Vec<String>,
    pub loaded_knowledge_paths: Vec<String>,
    pub ranked_context_items: Vec<String>,
    pub omitted_items: Vec<String>,
    pub grounding_receipts: Vec<String>,
    pub completion_state: String,
    pub open_questions: Vec<String>,
    pub recommended_next_agent_or_tool: Option<String>,
    pub context_budget_used: usize,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibrarianPassRecord {
    pub session_scope: SessionScope,
    pub phase: LibrarianPhase,
    pub memory_paths: Vec<String>,
    pub knowledge_paths: Vec<String>,
    pub saved_memory_paths: Vec<String>,
    pub omitted_items: Vec<String>,
    pub receipts: Vec<String>,
    pub context_budget_used: usize,
    pub pruned_message_count: usize,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DelegationMode {
    StayLocal,
    Handoff,
    Parallel,
}

/// Lifecycle state for one disposable `volume_work` item. These workers are
/// intentionally not company coworkers and never create handoff/postbox
/// ownership rows; the desktop uses this lightweight signal solely for a live
/// working-count/chip representation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VolumeWorkerLifecycleStatus {
    Started,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorDecision {
    pub mode: DelegationMode,
    pub target: AgentTarget,
    pub rationale: String,
}

/// Events emitted by the runtime during execution for real-time CLI rendering.
/// Serializable so the gateway daemon can stream them to CLI clients over the
/// unix socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CliEvent {
    /// Model is generating (show spinner)
    Thinking,
    /// Live token-stream fragment from the provider (kind: "thinking" |
    /// "text" | "progress") — the TUI's proof-of-life ticker while a long
    /// generation streams. Ephemeral: never persisted to the session.
    StreamDelta { kind: String, text: String },
    /// Legacy provider commentary event. Kept for wire compatibility with
    /// older runners; the story lane presents it as visible work commentary,
    /// never as private chain-of-thought.
    Reasoning(String),
    /// Provider reasoning summary for one agent. Transient: clients show it
    /// only as the live "thinking" status while the agent works, never as a
    /// transcript row.
    AgentThinking { agent: String, text: String },
    /// A short user-facing message the agent wrote alongside its tool calls
    /// (a progress update, an early answer). Durable: it is part of the
    /// conversation, rendered between tool rows.
    AgentMessage { agent: String, text: String },
    /// Live context-window usage for one agent: prompt tokens sent vs the
    /// model's window. Emitted once per provider call so every agent's window
    /// can show a real context gauge while the turn is still running.
    ContextUsage {
        agent: String,
        used: u32,
        limit: u64,
        /// Tokens this agent has spent so far in the current turn (input +
        /// output across every round), for the turn block's running total.
        #[serde(default)]
        spent: u64,
    },
    /// Durable, agent-attributed context folding lifecycle. Unlike the old
    /// generic gateway notice this belongs inside the agent's ordered work
    /// trace, so clients can animate `started` and retain the final receipt.
    ContextCompaction {
        agent: String,
        status: String,
        before_tokens: u64,
        after_tokens: u64,
        folded_messages: usize,
        limit: u64,
    },
    /// A tool call was initiated by an agent
    ToolCallStarted {
        agent: String,
        tool_name: String,
        input_summary: String,
    },
    /// A tool call completed
    ToolCallCompleted {
        agent: String,
        tool_name: String,
        input_summary: String,
        success: bool,
        output_summary: String,
        /// Display diff for file edits (write/str_replace): `- ` removed and
        /// `+ ` added lines, rendered red/green by verbose-mode UIs. Never
        /// model context — a pure UI artifact. `#[serde(default)]` keeps the
        /// Subscribe-socket wire format compatible with older clients.
        #[serde(default)]
        diff: Option<String>,
    },
    /// Orchestrator delegated to a specialist
    SpecialistDelegated { agent: String, subject: String },
    /// One disposable volume worker changed lifecycle state. This is a live
    /// UI signal, not a specialist delegation or durable handoff receipt.
    VolumeWorkerLifecycle {
        batch_id: String,
        worker_id: String,
        label: String,
        item_id: String,
        status: VolumeWorkerLifecycleStatus,
    },
    /// One named agent handed work directly to another.
    AgentHandoff {
        #[serde(default)]
        handoff_id: String,
        from: String,
        to: String,
        subject: String,
        background: bool,
        #[serde(default)]
        requester: String,
        #[serde(default)]
        receiver: String,
        #[serde(default)]
        status: String,
        #[serde(default)]
        causation_id: Option<String>,
        /// Full peer result when this event settles an inline handoff. Normal
        /// queued/working handoff beats leave it empty.
        #[serde(default)]
        body: Option<String>,
        /// Original handoff lifecycle this completed.
        #[serde(default)]
        reply_to: Option<String>,
    },
    /// One coworker's contribution to a first-class group thread. Group UI
    /// renders these as individual chat messages, not a Phoenix relay block.
    GroupMessage {
        #[serde(default)]
        message_id: String,
        group_id: String,
        round: u8,
        agent_id: String,
        agent_name: String,
        markdown: String,
        #[serde(default)]
        reply_to: Option<String>,
        #[serde(default)]
        causation_id: Option<String>,
    },
    /// Durable execution state for one explicitly activated group member.
    /// Clients render this as a compact lifecycle line, never as an authored
    /// coworker reply.
    GroupMemberStatus {
        turn_id: String,
        group_id: String,
        agent_id: String,
        agent_name: String,
        state: String,
        detail: String,
    },
    /// Work was delivered to a mailbox/planned specialist but no live execution started.
    SpecialistQueued {
        agent: String,
        subject: String,
        status: String,
    },
    /// Specialist finished and handed back. `ok=false` marks a failed/panicked
    /// turn so a queued job is never left silently "running" forever.
    SpecialistCompleted {
        agent: String,
        ok: bool,
        summary: String,
    },
    /// Specialist produced output — show what they wrote
    SpecialistOutput { agent: String, summary: String },
    /// A chunk of text output from streaming
    TextChunk(String),
    /// The final output text is ready
    FinalOutput(String),
    /// Orchestrator routing decision
    Routing { target: String, rationale: String },
    /// Librarian pass completed
    LibrarianPass {
        phase: String,
        scope: String,
        summary: String,
        loaded_count: usize,
        saved_count: usize,
        pruned_count: usize,
        receipts: Vec<String>,
    },
    /// Permission posture or permission decision for a tool execution
    PermissionCheck {
        tool_name: String,
        input_summary: String,
        status: String,
        detail: String,
    },
    /// Prompt assembly saved (for /prompt inspection)
    PromptAssembled {
        agent: String,
        system_prompt: String,
        user_prompt: String,
    },
    /// Session resolved at run start
    SessionResolved {
        scope: String,
        session_id: String,
        status: String,
    },
    /// Gateway-level notice for the client (e.g. queued behind a running turn)
    GatewayNotice(String),
    /// The gateway could not confirm the terminal result after execution.
    /// This ends the owned attempt without turning its answer preview into a
    /// confirmed delivery or permission to repeat work.
    TerminalFailure { message: String },
    /// An agent needs the user's input mid-task (`ask_user`): the TUI renders
    /// a modal popup with the questions/options and sends the answer back via
    /// the daemon's `answer-ask` verb keyed by `id`. Canvas agents continue
    /// independent work; the answer enters as a durable continuation.
    AskUser {
        id: String,
        agent: String,
        questions: Vec<crate::tools::ask_user::AskUserQuestion>,
        /// Machine-readable approval contract. Old clients ignore it; the new
        /// composer renders tool elevation/login/hiring as purpose-built blocks.
        #[serde(default)]
        approval: Option<crate::tools::ask_user::ApprovalRequest>,
    },
    /// A specialist was spawned as a detached background job (mode-2 talk) —
    /// it keeps working across turns while the user converses.
    BackgroundAgentSpawned {
        agent: String,
        subject: String,
        #[serde(default)]
        handoff_id: String,
        #[serde(default)]
        requester: String,
        #[serde(default)]
        receiver: String,
        #[serde(default)]
        status: String,
        #[serde(default)]
        causation_id: Option<String>,
    },
    /// A background specialist finished. `body` is the full result markdown
    /// the TUI renders as the agent-colored return block; the orchestrator
    /// absorbs the same body into its context via the postbox.
    BackgroundAgentReturned {
        agent: String,
        subject: String,
        ok: bool,
        summary: String,
        body: String,
        #[serde(default)]
        handoff_id: String,
        #[serde(default)]
        requester: String,
        #[serde(default)]
        receiver: String,
        #[serde(default)]
        status: String,
        #[serde(default)]
        reply_to: Option<String>,
        #[serde(default)]
        causation_id: Option<String>,
    },
    /// The orchestrator atomically drained `count` completed background
    /// results from the postbox into its next provider turn. This is a quiet
    /// lifecycle barrier for one-shot clients: a `Returned` event means the
    /// work finished, while this event proves that the result is actually in
    /// the integration turn whose later `FinalOutput`/`Done` can be terminal.
    BackgroundResultsAbsorbed { count: usize },
    /// A hidden watcher spoke — a Judge/Oracle ruling (sweep correction,
    /// halt enforcement, completion-gate bounce) or a goal-lane milestone
    /// (goal recognized / met / failed). Rendered as the SAME return card a
    /// specialist report-back draws: the live feed must look exactly as
    /// clean as a resumed transcript (which reconstructs these Talks as
    /// return cards), never as dim plumbing text.
    WatcherCard {
        from: String,
        subject: String,
        body: String,
        ok: bool,
    },
    /// A mid-task message was PARKED in an agent's steer lane — emitted by
    /// `postbox::steer` the instant the note is queued, before the target has
    /// read anything. A steer is a handoff (work handed to a working agent),
    /// so it renders as the same inbound card a delegation draws, in the
    /// ADDRESSED agent's window, with a live "waiting for their next step"
    /// foot. Without this a user's message vanished into the lane and only
    /// watcher rulings ever drew a card (`turn_loop` gated the card on
    /// `note.watcher`) — the user's "why did my queued message not come" and
    /// "why do steers not look like agent handoffs" (2026-07-30).
    SteerQueued {
        from: String,
        to: String,
        subject: String,
        body: String,
    },
    /// The addressed agent DRAINED a parked steer at its round top — the note
    /// is in its next model call. Settles the live card `SteerQueued` drew,
    /// exactly as a specialist's return settles a delegation card.
    SteerDelivered { to: String, subject: String },
    /// A daemon-initiated turn's driving prompt (goal heartbeat wake). The
    /// prompt lands in the transcript as a User message, so resume shows it
    /// as the bold ❯ row — live must show the same row when the wake fires,
    /// not start a turn's events out of nowhere.
    WakeTurn {
        prompt: String,
        /// One occurrence, not one routine definition. Re-delivery of the same
        /// occurrence keeps this id; tomorrow's run receives a different id.
        #[serde(default)]
        turn_id: Option<String>,
        #[serde(default)]
        origin: Option<TurnOrigin>,
    },
    /// A cross-session answer signal, broadcast so a different open
    /// conversation can show an ephemeral owner-aware completion toast.
    CrossAnswer {
        session_id: String,
        #[serde(default)]
        owner_kind: Option<String>,
        #[serde(default)]
        owner_id: Option<String>,
        project_name: String,
        summary: String,
    },
    /// Execution is complete
    Done,
}

/// A function that checks whether a risky command should be permitted.
pub type PermissionCheck = std::sync::Arc<dyn Fn(&str, &str) -> bool + Send + Sync>;

/// Collect interactive answers for `ask_user` (CLI wires this; tests omit it).
pub type AskUserHandler = std::sync::Arc<
    dyn Fn(&crate::tools::ask_user::AskUserInput) -> anyhow::Result<String> + Send + Sync,
>;

#[async_trait]
pub trait AgentRuntime: Send + Sync {
    fn agent_type(&self) -> AgentTarget;
    fn tools(&self) -> &[ToolSpec];

    async fn execute(
        &self,
        task: &TaskEnvelope,
        provider_request: CompletionRequest,
    ) -> anyhow::Result<AgentOutcome>;
}

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

static TASK_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Generate a unique task ID for async specialist delegation.
pub fn next_task_id(prefix: &str) -> String {
    let id = TASK_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}-{:03}", prefix, id)
}

/// Result of a completed background specialist task.
#[derive(Debug, Clone)]
pub struct SpecialistFinish {
    pub task_id: String,
    pub outcome: AgentOutcome,
    pub save_record: LibrarianPassRecord,
    pub prune_record: Option<LibrarianPassRecord>,
    pub specialist_session_id: String,
    pub status: PersistenceStatus,
    pub body: String,
}

/// A pending background specialist task.
pub struct PendingTask {
    pub task_id: String,
    pub target: SubAgentType,
    pub started_at: Instant,
    pub join: tokio::task::JoinHandle<anyhow::Result<SpecialistFinish>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSpecialistStatus {
    pub task_id: String,
    pub target: SubAgentType,
    pub age_ms: u128,
}

/// Registry of background specialist tasks for the current orchestrator turn.
pub struct PendingSpecialists {
    tasks: Vec<PendingTask>,
    pub completion_rx: tokio::sync::mpsc::UnboundedReceiver<SpecialistFinish>,
    completion_tx: tokio::sync::mpsc::UnboundedSender<SpecialistFinish>,
}

impl PendingSpecialists {
    pub fn new() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            tasks: Vec::new(),
            completion_rx: rx,
            completion_tx: tx,
        }
    }

    pub fn completion_sender(&self) -> tokio::sync::mpsc::UnboundedSender<SpecialistFinish> {
        self.completion_tx.clone()
    }

    pub fn spawn_task(
        &mut self,
        task_id: String,
        target: SubAgentType,
        join: tokio::task::JoinHandle<anyhow::Result<SpecialistFinish>>,
    ) {
        self.tasks.push(PendingTask {
            task_id,
            target,
            started_at: Instant::now(),
            join,
        });
    }

    /// Non-blocking drain of completed tasks. Removes matching tasks from the registry.
    pub fn drain_completed(&mut self) -> Vec<SpecialistFinish> {
        let mut finished = Vec::new();
        while let Ok(finish) = self.completion_rx.try_recv() {
            self.tasks.retain(|t| t.task_id != finish.task_id);
            finished.push(finish);
        }
        finished
    }

    /// Check if any tasks are still running.
    pub fn has_pending(&self) -> bool {
        !self.tasks.is_empty()
    }

    pub fn status_snapshot(&self) -> Vec<PendingSpecialistStatus> {
        let now = Instant::now();
        self.tasks
            .iter()
            .map(|task| PendingSpecialistStatus {
                task_id: task.task_id.clone(),
                target: task.target,
                age_ms: now.saturating_duration_since(task.started_at).as_millis(),
            })
            .collect()
    }

    /// Await all pending tasks with a timeout.
    pub async fn await_all(mut self, timeout: std::time::Duration) -> Vec<SpecialistFinish> {
        let deadline = Instant::now() + timeout;
        let mut results = Vec::new();

        // First drain what's already completed
        results.extend(self.drain_completed());

        // Then wait for remaining tasks
        for task in self.tasks.drain(..) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, task.join).await {
                Ok(Ok(Ok(finish))) => results.push(finish),
                Ok(Ok(Err(e))) => {
                    tracing::warn!("Specialist task {} failed: {}", task.task_id, e);
                }
                Ok(Err(e)) => {
                    tracing::warn!("Specialist task {} panicked: {}", task.task_id, e);
                }
                Err(_) => {
                    tracing::warn!(
                        "Specialist task {} timed out after {:?}",
                        task.task_id,
                        timeout
                    );
                }
            }
        }

        // Drain any remaining completions
        results.extend(self.drain_completed());
        results
    }
}

#[cfg(test)]
mod provider_turn_tests {
    use super::*;

    #[test]
    fn peak_input_is_a_single_call_never_the_multiround_sum() {
        // The window gauge divides `peak_input_tokens` by the window, so it must
        // stay ≤ the window even when a multi-round turn's SUM blows past it.
        // A 3-round turn on a 1M window: three ~870k calls sum to 2.6M (259% —
        // the old bug) but the peak single call is 870k (87% — correct).
        let window: u32 = 1_000_000;
        let turn = ProviderTurn {
            request_model: "glm-5.2:cloud".into(),
            output_text: String::new(),
            input_tokens: 2_610_000,    // SUM across 3 rounds (burn)
            peak_input_tokens: 870_000, // MAX single call (window usage)
            output_tokens: 4_000,
        };
        // Burn can exceed the window; window usage never does.
        assert!(turn.input_tokens > window, "sum is a burn metric");
        assert!(
            turn.peak_input_tokens <= window,
            "peak is one request — always ≤ window (this is what the gauge uses)"
        );
        assert!(turn.peak_input_tokens < turn.input_tokens);
    }

    #[test]
    fn provider_turn_from_response_sets_peak_to_its_own_input() {
        use crate::providers::{CompletionResponse, TokenUsage};
        let resp = CompletionResponse {
            content: "done".into(),
            model: "glm-5.2:cloud".into(),
            usage: TokenUsage::new(123_456, 789),
            reasoning: None,
            stop_reason: None,
            tool_calls: vec![],
            provider_replay: None,
        };
        let turn = ProviderTurn::from(resp);
        // A single provider call: peak == input.
        assert_eq!(turn.input_tokens, 123_456);
        assert_eq!(turn.peak_input_tokens, 123_456);
    }
}

#[cfg(test)]
mod pending_specialist_tests {
    use super::*;

    #[tokio::test]
    async fn pending_specialists_expose_running_task_status() {
        let mut pending = PendingSpecialists::new();
        let join = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            anyhow::bail!("not used by status snapshot")
        });

        pending.spawn_task("coder-001".to_string(), SubAgentType::Coder, join);

        let snapshot = pending.status_snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].task_id, "coder-001");
        assert_eq!(snapshot[0].target, SubAgentType::Coder);
        assert!(snapshot[0].age_ms < 5_000);
        assert!(pending.has_pending());
    }

    #[tokio::test]
    async fn pending_specialists_drain_removes_finished_tasks_from_status() {
        let mut pending = PendingSpecialists::new();
        let sender = pending.completion_sender();
        let join = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            anyhow::bail!("test task should be drained by channel before join resolves")
        });
        pending.spawn_task("coder-002".to_string(), SubAgentType::Coder, join);

        let outcome = AgentOutcome {
            completion: OutcomeCompletion::Completed,
            agent: AgentTarget::Specialist(SubAgentType::Coder),
            summary: "done".to_string(),
            artifacts: vec![],
            tool_results: vec![],
            provider_response: None,
        };
        let save_record = LibrarianPassRecord {
            session_scope: SessionScope::Specialist(SubAgentType::Coder),
            phase: LibrarianPhase::Save,
            memory_paths: vec![],
            knowledge_paths: vec![],
            saved_memory_paths: vec![],
            omitted_items: vec![],
            receipts: vec![],
            context_budget_used: 0,
            pruned_message_count: 0,
            summary: "saved".to_string(),
        };
        sender
            .send(SpecialistFinish {
                task_id: "coder-002".to_string(),
                outcome,
                save_record,
                prune_record: None,
                specialist_session_id: "main__coder".to_string(),
                status: PersistenceStatus::Resumed,
                body: "done".to_string(),
            })
            .unwrap();

        let finished = pending.drain_completed();
        assert_eq!(finished.len(), 1);
        assert!(!pending.has_pending());
        assert!(pending.status_snapshot().is_empty());
    }
}
