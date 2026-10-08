//! Phoenix CLI — focused terminal interface for the Phoenix runtime.
//!
//! Design principles:
//! - Clean startup: one line, no diagnostic dump
//! - Thinking spinner during model calls
//! - Compact tool call rows: ✔ Read(path) → 3 lines
//! - Streamed final output with subtle typewriter effect
//! - Slash commands for session control, inspection, and debugging
//! - Concise: minimize output tokens, no preamble/postamble

mod turn_events;

mod ask_prompt;
pub mod bench;
pub mod burn;
pub mod commands;
pub mod channels;
mod channel_receipts;
mod completer;
pub mod daemon;
pub mod headless_config;
mod prefs;
mod registry;
mod render;
mod style;
pub mod tui;
pub(crate) mod turn_queue;
mod session_gate;
pub mod ws_bridge;

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use rustyline::error::ReadlineError;
use tokio::sync::mpsc;

use completer::PhoenixHelper;

/// Interactive line editor with Phoenix slash-command completion + hints.
type CliEditor = rustyline::Editor<PhoenixHelper, rustyline::history::DefaultHistory>;

use crate::config::PhoenixConfig;
use crate::orchestrator;
use crate::providers::model_id::display_provider_model;
use crate::providers::scaffold::ScaffoldProvider;
use crate::runtime::{
    preview_user_request, prune_old_run_traces, save_run_trace, AgentRunner, AgentTarget, CliEvent,
    ContextItem, RunDiagnostics, RunErrorTrace, RunnerMemoryPolicy, TaskEnvelope, TraceContext,
};

use render::{
    flush_stdout, format_delegate, format_delegate_done, format_thinking_line,
    format_tool_completed, format_tool_running, print_banner, print_error, print_final_answer,
    print_footer,
};

// ── Public API for main.rs ────────────────────────────────────────────

/// Run a single task and exit (one-shot).
pub async fn run_oneshot(
    scaffold: bool,
    real: bool,
    session_id: String,
    user_request: String,
    librarian_debug: bool,
    yolo: bool,
) -> Result<()> {
    let mut state = AppState::new(scaffold, real, session_id);
    state.apply_saved_defaults(prefs::load());
    state.show_debug = state.show_debug || librarian_debug;
    state.yolo = state.yolo || yolo;
    print_banner(&state);
    if !run_interactive_turn(&mut state, user_request, true).await {
        anyhow::bail!(
            "Phoenix one-shot turn ended in a terminal failure; see the final output above."
        );
    }
    Ok(())
}

/// Run the interactive REPL.
pub async fn run_interactive(
    scaffold: bool,
    real: bool,
    session_id: String,
    librarian_debug: bool,
    yolo: bool,
) -> Result<()> {
    let mut state = AppState::new(scaffold, real, session_id);
    state.apply_saved_defaults(prefs::load());
    // Explicit flags can only force-enable on top of saved defaults.
    state.show_debug = state.show_debug || librarian_debug;
    state.yolo = state.yolo || yolo;

    // The full-screen application is the default session experience on a real
    // terminal. Diagnostic modes (scaffold/real) and PHOENIX_CLASSIC=1 keep the
    // archived line-based REPL (snapshot: backups/2026-06-10-classic-cli/).
    let classic = std::env::var("PHOENIX_CLASSIC")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if !scaffold && !real && !classic && io::stdout().is_terminal() {
        return tui::run(state).await;
    }
    print_banner(&state);

    let config = rustyline::Config::builder().build();
    let mut editor = CliEditor::with_config(config)?;
    let helper = PhoenixHelper::new();
    helper.bind_editor(&mut editor);
    editor.set_helper(Some(helper));
    let history_path = history_path()?;
    if history_path.exists() {
        let _ = editor.load_history(&history_path);
    }

    loop {
        let prompt = state.prompt_string();
        let line = match editor.readline(&prompt) {
            Ok(line) => line,
            Err(ReadlineError::Interrupted) => {
                println!("  {}  Use /quit to exit.", render::style_dim("⏎"));
                continue;
            }
            Err(ReadlineError::Eof) => {
                println!();
                break;
            }
            Err(error) => return Err(error.into()),
        };

        let input = line.trim().to_string();
        if input.is_empty() {
            continue;
        }

        if input.starts_with('/') {
            let _ = editor.add_history_entry(&input);
            if commands::handle(&input, &mut state)? {
                // Explicit end: ask the gateway to digest this session into
                // memory now (best-effort, bounded) so the next session
                // remembers where this one left off without the 30-min wait.
                let _ = tokio::time::timeout(
                    Duration::from_secs(2),
                    daemon::request_session_digest(state.session_id.clone()),
                )
                .await;
                break; // quit
            }
            continue;
        }

        // Single-line submit (Enter) or backslash continuation for multiline
        let user_request = if input.ends_with('\\') {
            let continued = read_multiline_continuation(&mut editor, &input)?;
            if continued.trim().is_empty() {
                continue;
            }
            continued
        } else {
            input
        };
        if user_request.trim().is_empty() {
            continue;
        }
        let _ = editor.add_history_entry(&user_request);
        run_interactive_turn(&mut state, user_request, false).await;
    }

    if let Some(parent) = history_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = editor.save_history(&history_path);
    Ok(())
}

// ── App State ─────────────────────────────────────────────────────────

/// How much of the working turn the feed shows. Two modes, one story: both
/// read like an operator narrating the work — the agent's narration lines
/// with one aggregated receipt ("read 3 files · made 2 edits · ran 5
/// commands") flushed at every narrative beat. Never a per-tool log.
/// - `Compact` (default): the narrated journal only.
/// - `Verbose`: the same journal, plus file edits rendered as red/green
///   diffs the moment they land.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DisplayMode {
    #[default]
    Compact,
    Verbose,
}

impl DisplayMode {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_lowercase().as_str() {
            // "detail" was retired 2026-07-04 — saved defaults migrate to
            // compact, the closest survivor.
            "compact" | "detail" | "default" => Some(Self::Compact),
            "verbose" => Some(Self::Verbose),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Verbose => "verbose",
        }
    }

    /// Cycle order for a no-arg `/display`.
    pub fn next(self) -> Self {
        match self {
            Self::Compact => Self::Verbose,
            Self::Verbose => Self::Compact,
        }
    }
}

pub struct AppState {
    pub scaffold: bool,
    pub real: bool,
    pub session_id: String,
    pub show_actions: bool,
    pub show_reasoning: bool,
    pub show_debug: bool,
    /// Feed verbosity: compact | detail | verbose. `/display` switches it.
    pub display_mode: DisplayMode,
    /// Sub-agent working steps (their tool rows) in the feed. Off = only the
    /// spawn and the colored return are shown for background agents.
    pub show_subagent_tools: bool,
    /// When true, tools run unconfined (can read/write/execute outside the
    /// launch directory). Toggled with `/yolo` and `/safe`.
    pub yolo: bool,
    /// We offer to persist toggle defaults the first time a runtime toggle is
    /// changed in an interactive session.
    pub prompted_default_offer: bool,
    pub last_prompt: Option<StoredPrompt>,
    pub last_run: Option<StoredRun>,
}

pub struct StoredPrompt {
    pub agent: String,
    pub system_prompt: String,
    pub user_prompt: String,
}

pub struct StoredRun {
    pub run_id: String,
    pub trace_path: PathBuf,
    pub reply_summary: String,
    pub token_total: u32,
    pub route: String,
}

impl AppState {
    fn new(scaffold: bool, real: bool, session_id: String) -> Self {
        Self {
            scaffold,
            real,
            session_id,
            show_actions: true,
            show_reasoning: false,
            show_debug: false,
            display_mode: DisplayMode::default(),
            show_subagent_tools: true,
            yolo: false,
            prompted_default_offer: false,
            last_prompt: None,
            last_run: None,
        }
    }

    fn permission_mode(&self) -> crate::tools::PermissionMode {
        if self.yolo {
            crate::tools::PermissionMode::FullAccess
        } else {
            crate::tools::PermissionMode::Workspace
        }
    }

    pub fn permission_label(&self) -> &'static str {
        if self.yolo {
            "yolo"
        } else {
            "workspace"
        }
    }

    pub fn permission_detail(&self) -> &'static str {
        if self.yolo {
            "unconfined"
        } else {
            "confined to workspace"
        }
    }

    /// Apply persisted toggle defaults from config (only when present).
    fn apply_saved_defaults(&mut self, defaults: prefs::CliDefaults) {
        if let Some(v) = defaults.yolo {
            self.yolo = v;
        }
        if let Some(v) = defaults.actions {
            self.show_actions = v;
        }
        if let Some(v) = defaults.thinking {
            self.show_reasoning = v;
        }
        if let Some(v) = defaults.debug {
            self.show_debug = v;
        }
        if let Some(v) = defaults.subagents {
            self.show_subagent_tools = v;
        }
        if let Some(mode) = defaults.display.as_deref().and_then(DisplayMode::parse) {
            self.display_mode = mode;
        }
    }

    /// Snapshot current toggles for persistence.
    fn current_defaults(&self) -> prefs::CliDefaults {
        prefs::CliDefaults {
            yolo: Some(self.yolo),
            actions: Some(self.show_actions),
            thinking: Some(self.show_reasoning),
            debug: Some(self.show_debug),
            subagents: Some(self.show_subagent_tools),
            display: Some(self.display_mode.label().to_string()),
        }
    }

    fn prompt_string(&self) -> String {
        format!("{} ", render::style_cyan("›"))
    }

    fn mode_label(&self) -> &'static str {
        if self.real {
            "real"
        } else if self.scaffold {
            "scaffold"
        } else {
            "provider"
        }
    }
}

// ── Interactive Turn ──────────────────────────────────────────────────

async fn run_interactive_turn(
    state: &mut AppState,
    user_request: String,
    await_background: bool,
) -> bool {
    // Explicit diagnostics may run in-process, but still use the actor mesh.
    // Product turns always route through the standing gateway daemon.
    let bypass_gateway = state.scaffold || state.real;
    if bypass_gateway {
        return run_local_turn(state, user_request).await;
    }
    // Auto-start the gateway if it is down so the user never has to boot it first.
    if let Err(err) = daemon::ensure_gateway_running().await {
        print_error(&format!("{err:#}"));
        println!();
        return false;
    }
    // A one-shot must attach before submitting: detached lifecycle events and
    // the eventual auto-wake answer live on the standing Subscribe socket,
    // not on the foreground Turn socket. The subscription handshake makes
    // this ordering a real barrier rather than a scheduling assumption.
    let mut background_events = if await_background {
        match daemon::subscribe_events(state.session_id.clone()).await {
            Ok(events) => Some(events),
            Err(error) => {
                print_error(&format!(
                    "could not attach the one-shot background result stream: {error:#}"
                ));
                println!();
                return false;
            }
        }
    } else {
        None
    };
    match daemon::submit_turn(state.session_id.clone(), user_request, state.yolo).await {
        Ok((event_rx, outcome)) => {
            drive_event_loop(state, event_rx).await;
            let ok = match outcome.await {
                Ok(daemon::RemoteOutcome::Summary(mut summary)) => {
                    if summary.background_work_pending {
                        if let Some(events) = background_events.take() {
                            let (settlement_events, settlement) =
                                daemon::spawn_background_settlement(
                                    events,
                                    summary,
                                    daemon::ONE_SHOT_BACKGROUND_SETTLEMENT_TIMEOUT,
                                );
                            drive_event_loop(state, settlement_events).await;
                            match settlement.await {
                                Ok(daemon::RemoteOutcome::Summary(settled)) => summary = settled,
                                Ok(daemon::RemoteOutcome::Error(message)) => {
                                    print_error(&message);
                                    println!();
                                    return false;
                                }
                                Ok(daemon::RemoteOutcome::Lost) => {
                                    print_error(
                                        "the gateway dropped while waiting for background results",
                                    );
                                    println!();
                                    return false;
                                }
                                Err(join_err) => {
                                    print_error(&format!("internal: {join_err:#}"));
                                    println!();
                                    return false;
                                }
                            }
                        }
                    }
                    let ok = !summary_is_terminal_failure(&summary.final_markdown);
                    print_turn_summary(state, summary);
                    ok
                }
                Ok(daemon::RemoteOutcome::Error(message)) => {
                    print_error(&message);
                    false
                }
                Ok(daemon::RemoteOutcome::Lost) => {
                    print_error(
                        "the gateway dropped mid-turn — check the `phoenix` gateway terminal.",
                    );
                    false
                }
                Err(join_err) => {
                    print_error(&format!("internal: {join_err:#}"));
                    false
                }
            };
            println!();
            ok
        }
        Err(err) => {
            print_error(&format!(
                "could not reach the Phoenix gateway after autostart: {err:#}"
            ));
            println!();
            false
        }
    }
}

/// In-process turn for scaffold/real diagnostics. It still runs the actor mesh.
async fn run_local_turn(state: &mut AppState, user_request: String) -> bool {
    let saved_bytes_before = crate::tools::compress::saved_bytes_total();
    let raw_bytes_before = crate::tools::compress::raw_bytes_total();
    let (handle, event_rx) = spawn_event_loop(
        state.scaffold,
        state.real,
        state.session_id.clone(),
        user_request,
        state.permission_mode(),
        crate::runtime::InteractionMode::Execute,
        true,
        None,
        None,
        None,
        None,
        None,
        None,
        None, // no attachments from the local one-shot/interactive path
        None, // no client turn id on the local CLI path
    );
    drive_event_loop(state, event_rx).await;
    let ok = match handle.await {
        Ok(Ok(report)) => {
            let summary = daemon::summarize(&report, saved_bytes_before, raw_bytes_before);
            let ok = !summary_is_terminal_failure(&summary.final_markdown);
            print_turn_summary(state, summary);
            ok
        }
        Ok(Err(error)) => {
            print_error(&format!("{error:#}"));
            false
        }
        Err(join_err) => {
            print_error(&format!("internal: {join_err:#}"));
            false
        }
    };
    println!();
    ok
}

/// Render the streamed runtime events (spinner, tool rows, librarian passes)
/// until `Done` — shared by local and gateway-backed turns.
async fn drive_event_loop(state: &mut AppState, mut event_rx: mpsc::Receiver<CliEvent>) {
    let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    // Only animate the spinner on a real TTY. When stdout is captured/piped (or a
    // log line interleaves), the `\r` overwrite doesn't collapse and every frame
    // becomes its own line — the "30 repeated Search lines" bloat. On non-TTY we
    // stay silent and let the ✓/✗ result lines carry the story.
    let animate = io::stdout().is_terminal();
    let mut thinking = false;
    let mut tick = 0usize;
    let mut tool_in_progress: Option<String> = None;
    loop {
        tokio::select! {
            event = event_rx.recv() => {
                match event {
                    Some(CliEvent::TerminalFailure { message }) => {
                        if animate { print!("\r\x1b[2K"); }
                        eprintln!("{message}");
                        return;
                    }
                    Some(CliEvent::StreamDelta { .. }) => {
                        // One-shot mode renders no live ticker; the spinner
                        // already shows life and the final text arrives whole.
                    }
                    Some(CliEvent::BackgroundAgentSpawned { agent, subject, .. }) => {
                        println!("{}", render::style_dim(&format!("⧉ {agent} spawned in background: {subject}")));
                    }
                    Some(CliEvent::BackgroundAgentReturned { agent, ok, summary, .. }) => {
                        let mark = if ok { "◆" } else { "◆ FAILED" };
                        println!("{}", render::style_dim(&format!("{mark} {agent} returned: {summary}")));
                    }
                    Some(CliEvent::BackgroundResultsAbsorbed { .. }) => {
                        // One-shot settlement barrier; intentionally no UI row.
                    }
                    Some(CliEvent::Thinking) => {
                        thinking = true;
                        tick = 0;
                        tool_in_progress = None;
                        if animate {
                            print!(
                                "\r\x1b[2K{}",
                                format_thinking_line(spinner[0], "Thinking…")
                            );
                            flush_stdout();
                        }
                    }
                    Some(CliEvent::ToolCallStarted { agent, tool_name, input_summary }) => {
                        if !state.show_actions && !agent.starts_with("Librarian") {
                            continue;
                        }
                        if !thinking {
                            thinking = true;
                            tick = 0;
                        }
                        tool_in_progress = Some(format_tool_running(&agent, &tool_name, &input_summary));
                    }
                    Some(CliEvent::ToolCallCompleted { agent, tool_name, input_summary, success, output_summary, diff }) => {
                        if !state.show_actions && !agent.starts_with("Librarian") {
                            continue;
                        }
                        clear_thinking(&mut thinking, &mut tool_in_progress);
                        let (line, detail) = format_tool_completed(
                            &agent, &tool_name, &input_summary, success, &output_summary,
                        );
                        println!("{line}");
                        if let Some(d) = detail {
                            println!("{d}");
                        }
                        if state.display_mode == DisplayMode::Verbose {
                            if let Some(diff) = diff {
                                render::print_diff(&diff);
                            }
                        }
                    }
                    Some(CliEvent::SpecialistDelegated { agent, subject }) => {
                        if !state.show_actions { continue; }
                        clear_thinking(&mut thinking, &mut tool_in_progress);
                        println!("{}", format_delegate(&agent, &subject));
                    }
                    // Disposable volume workers surface as compact live chips
                    // in the desktop Environment panel. Do not turn one batch
                    // into a wall of CLI handoff rows.
                    Some(CliEvent::VolumeWorkerLifecycle { .. }) => {}
                    Some(CliEvent::AgentHandoff { from, to, subject, background, .. }) => {
                        if !state.show_actions { continue; }
                        clear_thinking(&mut thinking, &mut tool_in_progress);
                        let mode = if background { " (background)" } else { "" };
                        println!(
                            "  {} {} → {}: {}{}",
                            render::style_cyan("⇄"),
                            render::style_bold(&from),
                            render::style_bold(&to),
                            subject,
                            render::style_dim(mode),
                        );
                    }
                    Some(CliEvent::GroupMessage { agent_name, round, markdown, .. }) => {
                        clear_thinking(&mut thinking, &mut tool_in_progress);
                        println!(
                            "  {} {} · round {}\n{}",
                            render::style_cyan("◆"),
                            render::style_bold(&agent_name),
                            round,
                            markdown
                        );
                    }
                    Some(CliEvent::GroupMemberStatus {
                        agent_name,
                        state: member_state,
                        detail,
                        ..
                    }) => {
                        if !state.show_actions { continue; }
                        clear_thinking(&mut thinking, &mut tool_in_progress);
                        println!(
                            "  {} {} · {} — {}",
                            render::style_cyan("◌"),
                            render::style_bold(&agent_name),
                            member_state,
                            render::style_dim(&detail),
                        );
                    }
                    Some(CliEvent::SpecialistQueued { agent, subject, status }) => {
                        if !state.show_actions { continue; }
                        clear_thinking(&mut thinking, &mut tool_in_progress);
                        println!(
                            "  {} {} queued: {} ({})",
                            render::style_cyan("⇢"),
                            render::style_bold(&agent),
                            subject,
                            render::style_dim(&status)
                        );
                    }
                    Some(CliEvent::SpecialistCompleted { agent, ok, summary }) => {
                        if !state.show_actions { continue; }
                        if ok {
                            println!("{}", format_delegate_done(&agent));
                        } else {
                            println!("{}", render::style_dim(&format!("✖ {agent} failed: {summary}")));
                        }
                    }
                    Some(CliEvent::SpecialistOutput { agent: _, summary }) => {
                        if !state.show_actions { continue; }
                        let preview: String = summary
                            .lines()
                            .find(|l| !l.trim().is_empty() && !l.starts_with('#'))
                            .unwrap_or(&summary)
                            .chars()
                            .take(120)
                            .collect();
                        println!("      {}", render::style_dim(&preview));
                    }
                    Some(CliEvent::Routing { target, rationale }) => {
                        if !state.show_actions { continue; }
                        clear_thinking(&mut thinking, &mut tool_in_progress);
                        println!("  {} {}", render::style_cyan("→"), render::style_bold(&target));
                        if state.show_reasoning && !rationale.trim().is_empty() {
                            render::print_thinking_block("routing", rationale.trim());
                        }
                    }
                    // The deterministic memory recall (Cognee) still runs in the
                    // runtime, but the librarian is no longer a teammate — don't
                    // print a "Librarian ·preload" pass line for it.
                    Some(CliEvent::LibrarianPass { .. }) => {}
                    Some(CliEvent::Reasoning(text)) => {
                        clear_thinking(&mut thinking, &mut tool_in_progress);
                        let trimmed = text.trim();
                        if state.show_reasoning && !trimmed.is_empty() {
                            render::print_thinking_block("model", trimmed);
                        }
                    }
                    Some(CliEvent::AgentThinking { agent, text }) => {
                        clear_thinking(&mut thinking, &mut tool_in_progress);
                        let trimmed = text.trim();
                        if state.show_reasoning && !trimmed.is_empty() {
                            render::print_thinking_block(&agent, trimmed);
                        }
                    }
                    Some(CliEvent::AgentMessage { agent, text }) => {
                        clear_thinking(&mut thinking, &mut tool_in_progress);
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            render::print_thinking_block(&agent, trimmed);
                        }
                    }
                    Some(CliEvent::PromptAssembled { agent, system_prompt, user_prompt }) => {
                        state.last_prompt = Some(StoredPrompt { agent, system_prompt, user_prompt });
                    }
                    Some(CliEvent::SessionResolved { scope, session_id, status }) => {
                        clear_thinking(&mut thinking, &mut tool_in_progress);
                        println!(
                            "  {} Session {:<6} {} ({})",
                            render::style_dim("◦"),
                            scope,
                            session_id,
                            status,
                        );
                    }
                    Some(CliEvent::GatewayNotice(text)) => {
                        clear_thinking(&mut thinking, &mut tool_in_progress);
                        println!("  {} {}", render::style_dim("◦"), render::style_dim(&text));
                    }
                    Some(CliEvent::WakeTurn { prompt, .. }) => {
                        clear_thinking(&mut thinking, &mut tool_in_progress);
                        let shown: String = prompt.chars().take(600).collect();
                        println!("  {} {}", render::style_cyan("❯"), render::style_bold(&shown));
                    }
                    Some(CliEvent::WatcherCard { from, subject, body, .. }) => {
                        // Watcher rulings and goal milestones read like a
                        // specialist return: a barred header, the body quiet.
                        clear_thinking(&mut thinking, &mut tool_in_progress);
                        println!(
                            "  {} {} — {subject}",
                            render::style_cyan("▌"),
                            render::style_bold(&from)
                        );
                        for line in body.lines() {
                            println!("  {} {}", render::style_cyan("▌"), render::style_dim(line));
                        }
                    }
                    Some(CliEvent::SteerQueued { from, to, body, .. }) => {
                        // A mid-task message parked for a working agent reads
                        // like the handoff it is: who it is from, who it is
                        // for, then the message.
                        clear_thinking(&mut thinking, &mut tool_in_progress);
                        println!(
                            "  {} {} → {to}",
                            render::style_cyan("⇐"),
                            render::style_bold(&from)
                        );
                        for line in body.lines() {
                            println!("  {} {}", render::style_cyan("⇐"), render::style_dim(line));
                        }
                    }
                    Some(CliEvent::SteerDelivered { to, .. }) => {
                        clear_thinking(&mut thinking, &mut tool_in_progress);
                        println!(
                            "  {} {}",
                            render::style_dim("✓"),
                            render::style_dim(&format!("{to} picked up your message"))
                        );
                    }
                    Some(CliEvent::AskUser {
                        id,
                        agent,
                        questions,
                        approval,
                    }) => {
                        clear_thinking(&mut thinking, &mut tool_in_progress);
                        let answer = if io::stdout().is_terminal() {
                            let input = crate::tools::ask_user::AskUserInput {
                                questions,
                                approval,
                            };
                            println!("  {} {agent} needs your input", render::style_dim("◆"));
                            tokio::task::spawn_blocking(move || {
                                crate::cli::ask_prompt::collect_answers(&input)
                            })
                            .await
                            .ok()
                            .and_then(|r| r.ok())
                            .unwrap_or_else(|| {
                                "The user cancelled the question. Proceed with your best \
                                 judgment and state the assumption you made."
                                    .to_string()
                            })
                        } else {
                            "Non-interactive run: the user cannot answer. Proceed with your \
                             best judgment and state the assumption you made in your final \
                             answer."
                                .to_string()
                        };
                        // Local turns share this process; gateway turns need the socket.
                        if !crate::runtime::asks::answer(&id, answer.clone()) {
                            let _ = daemon::answer_ask(id, answer).await;
                        }
                    }
                    Some(
                        CliEvent::FinalOutput(_)
                        | CliEvent::TextChunk(_)
                        | CliEvent::PermissionCheck { .. }
                        | CliEvent::ContextUsage { .. }
                        | CliEvent::ContextCompaction { .. },
                    ) => {
                        // Informational events — not rendered inline
                    }
                    Some(CliEvent::Done) => break,
                    // Item 7: cross-session answer notification — one-shot mode ignores it.
                    Some(CliEvent::CrossAnswer { .. }) => {}
                    None => break,
                }
            }
            _ = async {
                if thinking {
                    tokio::time::sleep(Duration::from_millis(80)).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                if thinking && animate {
                    tick = (tick + 1) % spinner.len();
                    let label = tool_in_progress.as_deref().unwrap_or("thinking…");
                    print!("\r\x1b[2K{}", format_thinking_line(spinner[tick], label));
                    flush_stdout();
                }
            }
        }
    }

    clear_thinking(&mut thinking, &mut tool_in_progress);
}

/// Print the final answer + footer and record the run on the app state.
fn print_turn_summary(state: &mut AppState, summary: daemon::TurnSummary) {
    let trace_path = PathBuf::from(&summary.trace_path);
    state.last_run = Some(StoredRun {
        run_id: summary.run_id.clone(),
        trace_path: trace_path.clone(),
        reply_summary: summary.final_markdown.clone(),
        token_total: summary.total_tokens,
        route: summary.route.clone(),
    });
    state.session_id = summary.main_session_id.clone();

    print_final_answer(summary.final_markdown.trim());
    print_footer(
        summary.total_tokens,
        summary.orchestrator_tokens,
        summary.coder_tokens,
        &relative_path(&trace_path),
    );
}

fn clear_thinking(thinking: &mut bool, tool_in_progress: &mut Option<String>) {
    if *thinking {
        // Only emit the clear-line escape on a real TTY; on captured/piped output
        // it would just litter the transcript with stray control codes.
        if io::stdout().is_terminal() {
            print!("\r\x1b[2K");
            let _ = io::stdout().flush();
        }
        *thinking = false;
        *tool_in_progress = None;
    }
}

fn summary_is_terminal_failure(markdown: &str) -> bool {
    let trimmed = markdown.trim();
    crate::runtime::user_error::runtime_failure_summary(trimmed).is_some()
        || trimmed.contains(" agent could not complete its turn:")
        || trimmed
            .contains(" agent hit an internal error (a panic) and could not finish this turn:")
        || trimmed.starts_with("Phoenix stopped this agent at a hard runtime boundary:")
        || trimmed.starts_with(
            "The provider became unavailable after this agent had already performed work.",
        )
        || trimmed.contains("Phoenix rejected it during runtime validation.")
        || trimmed.contains("reached Phoenix's bounded provider-loop limit after")
}

// ── Turn Execution ────────────────────────────────────────────────────

pub struct TurnReport {
    pub run_id: String,
    pub trace_path: PathBuf,
    pub execution: crate::runtime::RuntimeExecution,
    pub mode: &'static str,
    pub orchestrator_model: Option<String>,
    pub coder_model: Option<String>,
    pub orchestrator_tokens: Option<(u32, u32)>,
    /// Peak single-call input (context-window usage) — the gauge divides THIS by
    /// the window, never `orchestrator_tokens.0` (the multi-round sum).
    pub orchestrator_peak_input: Option<u32>,
    pub coder_tokens: Option<(u32, u32)>,
    pub trace_diagnostics: Option<RunDiagnostics>,
}

impl TurnReport {
    fn total_tokens(&self) -> u32 {
        self.orchestrator_tokens.map(|(i, o)| i + o).unwrap_or(0)
            + self.coder_tokens.map(|(i, o)| i + o).unwrap_or(0)
    }

    fn route_description(&self) -> String {
        use crate::runtime::AgentTarget;
        match &self.execution.decision.target {
            AgentTarget::Orchestrator => "direct".into(),
            AgentTarget::Specialist(st) => format!("→ {:?}", st).to_lowercase(),
        }
    }
}

fn runner_memory_policy(scaffold: bool, real: bool) -> RunnerMemoryPolicy {
    match (scaffold, real) {
        (true, false) => RunnerMemoryPolicy::DisabledDiagnostic,
        _ if !crate::settings::effective_bool(
            "memory.enabled",
            &crate::settings::SettingsScope::Global,
        )
        .unwrap_or(true) =>
        {
            RunnerMemoryPolicy::DisabledDiagnostic
        }
        _ => RunnerMemoryPolicy::Enabled,
    }
}

/// Bind every turn entry point to the canonical conversation owner. A missing
/// client target means "the owner of this thread", never "Phoenix". This is
/// deliberately below the daemon/wake/UI producers so forgetting identity in
/// any one of them cannot start Phoenix inside Avery's (or a group's) thread.
pub(crate) fn resolve_canonical_turn_route(
    session_id: &str,
    requested_agent: Option<String>,
    requested_group: Option<String>,
) -> Result<(Option<String>, Option<String>)> {
    let persisted = crate::session::SessionStore::read_one_from_disk(
        &crate::config::phoenix_home().join("sessions"),
        session_id,
    )?;
    let snapshot = crate::runtime::company::global()?.directory_snapshot()?;
    resolve_canonical_turn_route_from(
        &snapshot,
        session_id,
        persisted.as_ref().map(|session| &session.kind),
        requested_agent,
        requested_group,
    )
}

fn resolve_canonical_turn_route_from(
    snapshot: &crate::runtime::company_directory::DirectorySnapshot,
    session_id: &str,
    persisted_kind: Option<&crate::session::SessionKind>,
    requested_agent: Option<String>,
    requested_group: Option<String>,
) -> Result<(Option<String>, Option<String>)> {
    anyhow::ensure!(
        requested_agent.is_none() || requested_group.is_none(),
        "a turn cannot target both an agent and a group"
    );
    let owner = crate::runtime::agent_conversation::resolve_canonical_owner(snapshot, session_id)?;
    match owner {
        Some(crate::runtime::agent_conversation::CanonicalConversationOwner::Agent(context)) => {
            anyhow::ensure!(
                requested_group.is_none(),
                "{} owns canonical session `{session_id}`; it cannot run as a group",
                context.display_name
            );
            if let Some(requested) = requested_agent.as_deref() {
                let requested_context =
                    crate::runtime::agent_conversation::resolve_agent_turn(snapshot, requested)?;
                anyhow::ensure!(
                    requested_context.agent_id == context.agent_id,
                    "{} owns canonical session `{session_id}`; requested target `{requested}` is a different coworker",
                    context.display_name
                );
            }
            if let Some(kind) = persisted_kind {
                let expected = if context.internal_role == "phoenix"
                    || context.internal_role == "orchestrator"
                {
                    crate::session::SessionKind::Main
                } else {
                    let agent = crate::runtime::delegation::specialist_from_talk_name(
                        &context.internal_role,
                    )
                    .with_context(|| {
                        format!(
                            "coworker `{}` has unavailable runtime role `{}`",
                            context.display_name, context.internal_role
                        )
                    })?;
                    crate::session::SessionKind::SubAgent(agent)
                };
                anyhow::ensure!(
                    *kind == expected,
                    "canonical session `{session_id}` is persisted as {kind:?}, not its owner {:?}",
                    expected
                );
            }
            Ok((Some(context.agent_id), None))
        }
        Some(crate::runtime::agent_conversation::CanonicalConversationOwner::Group {
            group_id,
        }) => {
            anyhow::ensure!(
                requested_agent.is_none(),
                "group `{group_id}` owns canonical session `{session_id}`; it cannot run as a direct agent"
            );
            if let Some(kind) = persisted_kind {
                anyhow::ensure!(
                    *kind == crate::session::SessionKind::Main,
                    "canonical group session `{session_id}` is persisted as {kind:?}, not a group transcript"
                );
            }
            if let Some(requested) = requested_group {
                anyhow::ensure!(
                    requested == group_id,
                    "group `{requested}` does not own canonical session `{session_id}`"
                );
                Ok((None, Some(requested)))
            } else {
                Ok((None, Some(group_id)))
            }
        }
        None => {
            anyhow::ensure!(
                requested_group.is_none(),
                "group `{}` does not own canonical session `{session_id}`",
                requested_group.as_deref().unwrap_or_default()
            );
            match persisted_kind {
                None => {
                    anyhow::ensure!(
                        requested_agent.is_none(),
                        "unowned session `{session_id}` cannot be assigned to an agent by the caller"
                    );
                    // A genuinely new, untargeted session is Phoenix's main
                    // conversation. Any specialist/group must first have a
                    // durable directory owner or a legacy persisted kind.
                    Ok((None, None))
                }
                Some(crate::session::SessionKind::Main) => {
                    if let Some(requested) = requested_agent.as_deref() {
                        anyhow::ensure!(
                            matches!(
                                requested.trim().trim_start_matches('@').to_ascii_lowercase().as_str(),
                                "phoenix" | "orchestrator"
                            ),
                            "Phoenix owns persisted main session `{session_id}`; requested target `{requested}` is a different coworker"
                        );
                    }
                    Ok((None, None))
                }
                Some(crate::session::SessionKind::SubAgent(expected)) => {
                    if let Some(requested) = requested_agent.as_deref() {
                        let resolved =
                            crate::runtime::delegation::specialist_from_talk_name(requested)
                                .with_context(|| format!("unknown target_agent: {requested:?}"))?;
                        anyhow::ensure!(
                            resolved == *expected,
                            "persisted session `{session_id}` belongs to `{}`; requested target `{requested}` is a different coworker",
                            crate::runtime::delegation::specialist_label(*expected)
                        );
                    }
                    Ok((
                        Some(crate::runtime::delegation::specialist_label(*expected).to_string()),
                        None,
                    ))
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ComposerAttachmentKind {
    Image,
    File,
    Folder,
}

fn composer_attachment_kind(path: &std::path::Path) -> ComposerAttachmentKind {
    if path.is_dir() {
        return ComposerAttachmentKind::Folder;
    }
    let extension = path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if ["png", "jpg", "jpeg", "webp", "gif", "avif"].contains(&extension.as_str()) {
        ComposerAttachmentKind::Image
    } else {
        ComposerAttachmentKind::File
    }
}

fn request_with_composer_attachments(
    user_request: &str,
    attachments: Option<&Vec<String>>,
) -> String {
    let Some(paths) = attachments else {
        return user_request.to_string();
    };
    let live = paths
        .iter()
        .take(32)
        .filter_map(|raw| {
            let trimmed = raw.trim();
            let path = std::path::Path::new(trimmed);
            (!trimmed.is_empty() && trimmed.chars().take(4_097).count() <= 4_096 && path.exists())
                .then(|| (trimmed, composer_attachment_kind(path)))
        })
        .collect::<Vec<_>>();
    if live.is_empty() {
        return user_request.to_string();
    }

    let list = live
        .iter()
        .map(|(path, kind)| {
            let label = match kind {
                ComposerAttachmentKind::Image => "image",
                ComposerAttachmentKind::File => "file",
                ComposerAttachmentKind::Folder => "folder",
            };
            let encoded = serde_json::to_string(path).expect("a Rust string always serializes");
            format!("- {label}: {encoded}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let has_images = live
        .iter()
        .any(|(_, kind)| *kind == ComposerAttachmentKind::Image);
    let has_files = live
        .iter()
        .any(|(_, kind)| *kind == ComposerAttachmentKind::File);
    let has_folders = live
        .iter()
        .any(|(_, kind)| *kind == ComposerAttachmentKind::Folder);
    let mut guidance = Vec::new();
    if has_images {
        guidance.push("inspect images with `image_analyze`");
    }
    if has_files {
        guidance.push("inspect files with `read` or the appropriate workspace tool");
    }
    if has_folders {
        guidance.push("inspect folders with `list_directory` and targeted workspace searches");
    }
    format!(
        "{user_request}\n\n[The user attached the following item(s) to THIS message. {} before answering; do not treat files or folders as images:\n{list}]",
        guidance.join(", and ")
    )
}

/// Carry actual composer image attachments separately from prose. The design
/// controller must never discover authoritative references by parsing text a
/// user, page or coworker could have written. The original engine validates
/// the bytes and snapshots each reference before accepting it.
fn composer_design_reference_context(
    attachments: Option<&Vec<String>>,
    workspace: &std::path::Path,
) -> Option<crate::runtime::ContextItem> {
    let paths = attachments?.iter().take(32).filter_map(|raw| {
        let raw = raw.trim();
        if raw.is_empty() || raw.len() > 4096 { return None; }
        let path = std::path::Path::new(raw);
        let path = if path.is_absolute() { path.to_path_buf() } else { workspace.join(path) };
        (path.is_file() && composer_attachment_kind(&path) == ComposerAttachmentKind::Image)
            .then(|| path.to_string_lossy().into_owned())
    }).collect::<Vec<_>>();
    (!paths.is_empty()).then(|| crate::runtime::ContextItem {
        label: "phoenix_design_reference_paths".into(),
        content: serde_json::to_string(&paths).expect("image attachment paths serialize"),
    })
}

async fn execute_inner(
    scaffold: bool,
    real: bool,
    session_id: String,
    user_request: String,
    event_tx: Option<mpsc::Sender<CliEvent>>,
    permission_mode: crate::tools::PermissionMode,
    interaction_mode: crate::runtime::InteractionMode,
    interactive: bool,
    workspace_root: std::path::PathBuf,
    target_agent: Option<String>,
    target_group: Option<String>,
    group_activation: Option<crate::runtime::group_conversation::GroupActivationIntent>,
    sticky_notes: Option<Vec<crate::cli::daemon::StickyNoteData>>,
    viewport: Option<crate::cli::daemon::ViewportData>,
    attachments: Option<Vec<String>>,
    client_turn_id: Option<String>,
) -> Result<TurnReport> {
    let provider_factory = crate::providers::ProviderFactory::new();

    let config = if scaffold || real {
        PhoenixConfig::load().ok()
    } else {
        Some(
            PhoenixConfig::load()
                .context("No Phoenix config found. Run `phoenix onboard` first.")?,
        )
    };
    let orchestrator_model = config
        .as_ref()
        .map(|c| c.profile.llm.orchestrator())
        .unwrap_or_else(|| "phoenix-scaffold-model".to_string());

    let trace_diagnostics = config.as_ref().map(|c| {
        let llm = &c.profile.llm;
        RunDiagnostics {
            session_id: Some(session_id.clone()),
            user_request_preview: Some(preview_user_request(&user_request, 500)),
            provider_id: Some(llm.provider.clone()),
            orchestrator_model: Some(llm.orchestrator()),
            specialist_model: Some(llm.specialist()),
            librarian_model: Some(llm.librarian()),
            timeout_seconds: Some(llm.timeout_seconds),
            configured_max_tokens: llm.max_tokens,
            response_model: None,
            specialist_response_model: None,
        }
    });

    let (target_agent, target_group) =
        resolve_canonical_turn_route(&session_id, target_agent, target_group)?;

    // A direct coworker turn binds the editable directory identity to its one
    // canonical endless transcript. Old registry-only agents still resolve as
    // a migration fallback until onboarding imports them into the directory.
    let (resolved_target, agent_context) = match target_agent.as_deref() {
        None => (AgentTarget::Orchestrator, None),
        Some(name) => {
            let directory_name = if name.eq_ignore_ascii_case("orchestrator") {
                "phoenix"
            } else {
                name
            };
            let company = crate::runtime::company::global()?;
            let mut snapshot = company.directory_snapshot()?;
            let normalized = |value: &str| {
                value
                    .trim()
                    .trim_start_matches('@')
                    .to_ascii_lowercase()
                    .replace([' ', '-'], "_")
            };
            let wanted = normalized(directory_name);
            let matching_id = snapshot.agents.iter().find_map(|agent| {
                [
                    agent.profile.agent_id.as_str(),
                    agent.profile.internal_role.as_str(),
                    agent.profile.display_name.as_str(),
                ]
                .into_iter()
                .any(|candidate| normalized(candidate) == wanted)
                .then(|| agent.profile.agent_id.clone())
            });
            if let Some(agent_id) = matching_id {
                company.ensure_agent_canonical_session(&agent_id)?;
                snapshot = company.directory_snapshot()?;
                let context =
                    crate::runtime::agent_conversation::resolve_agent_turn(&snapshot, &agent_id)?;
                anyhow::ensure!(
                    context.canonical_session_id == session_id,
                    "{} uses canonical session `{}` (received `{session_id}`)",
                    context.display_name,
                    context.canonical_session_id
                );
                let target = if context.internal_role == "phoenix" {
                    AgentTarget::Orchestrator
                } else {
                    let specialist = crate::runtime::delegation::specialist_from_talk_name(
                        &context.internal_role,
                    )
                    .with_context(|| {
                        format!(
                            "coworker `{}` has unavailable runtime role `{}`",
                            context.display_name, context.internal_role
                        )
                    })?;
                    AgentTarget::Specialist(specialist)
                };
                (target, Some(context))
            } else {
                let specialist = crate::runtime::delegation::specialist_from_talk_name(name)
                    .with_context(|| {
                        format!("unknown target_agent: {name:?} — not a recognized coworker")
                    })?;
                (AgentTarget::Specialist(specialist), None)
            }
        }
    };

    // Build context items from sticky notes (if any) so the agent can read
    // and reference the user's pinned canvas notes. When a viewport is
    // provided, notes are spatially scoped: nearby notes get full text,
    // distant notes get a one-line summary, far notes are omitted.
    let mut context_items: Vec<ContextItem> = Vec::new();
    if let Some(ref notes) = sticky_notes {
        let non_empty: Vec<_> = notes.iter().filter(|n| !n.text.trim().is_empty()).collect();
        if !non_empty.is_empty() {
            if let Some(ref vp) = viewport {
                // Spatial scoping: split notes by distance from viewport center.
                let max_dist = ((vp.w + vp.h) / 2.0) * 2.0; // 2x viewport "radius"
                let mut nearby = Vec::new();
                let mut distant = Vec::new();
                for note in &non_empty {
                    let dx = note.x - vp.x;
                    let dy = note.y - vp.y;
                    let dist = (dx * dx + dy * dy).sqrt();
                    if dist < max_dist {
                        nearby.push(*note);
                    } else {
                        distant.push(*note);
                    }
                }

                if !nearby.is_empty() {
                    let content = nearby
                        .iter()
                        .enumerate()
                        .map(|(i, n)| format!("### Note {}\n{}", i + 1, n.text.trim()))
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    context_items.push(ContextItem {
                        label: "canvas_sticky_notes_nearby".into(),
                        content: format!(
                            "## Phoenix Notes (near your current view)\n\n{content}\n"
                        ),
                    });
                }

                if !distant.is_empty() {
                    let summary = distant
                        .iter()
                        .map(|n| n.text.chars().take(30).collect::<String>())
                        .collect::<Vec<_>>()
                        .join(", ");
                    context_items.push(ContextItem {
                        label: "canvas_sticky_notes_distant".into(),
                        content: format!("## Other Phoenix notes (not in current view): {summary}"),
                    });
                }
            } else {
                // No viewport — include all notes (existing behavior).
                let mut block = String::from("## Phoenix Notes\n\n");
                for (i, note) in non_empty.iter().enumerate() {
                    block.push_str(&format!("### Note {}\n{}\n\n", i + 1, note.text.trim()));
                }
                context_items.push(ContextItem {
                    label: "canvas_sticky_notes".to_string(),
                    content: block.trim_end().to_string(),
                });
            }
        }
    }

    // Composer attachments are typed by their real filesystem shape. The note
    // MUST ride on task.user_request itself (not a context item): the mesh path
    // seeds the orchestrator's user message straight from user_request and
    // ignores task.context. Appending it keeps the session title (first line)
    // clean while preventing a file/folder selection from being mislabeled as
    // an image and sent to the vision lane.
    let request_for_task = request_with_composer_attachments(&user_request, attachments.as_ref());
    if let Some(references) = composer_design_reference_context(attachments.as_ref(), &workspace_root) {
        context_items.push(references);
    }

    let (group, effective_group_activation) = match target_group.as_deref() {
        Some(group_id) => {
            let company = crate::runtime::company::global()?;
            let canonical_session_id = company.ensure_group_canonical_session(group_id)?;
            anyhow::ensure!(
                canonical_session_id == session_id,
                "group `{group_id}` uses canonical session `{canonical_session_id}` (received `{session_id}`)"
            );
            let snapshot = company.directory_snapshot()?;
            let intent = match group_activation.as_ref() {
                Some(intent) => {
                    anyhow::ensure!(
                        intent.group_id == group_id,
                        "group activation belongs to `{}`, not `{group_id}`",
                        intent.group_id
                    );
                    intent.clone()
                }
                None => crate::runtime::group_conversation::preview_group_activation(
                    &snapshot,
                    group_id,
                    &request_for_task,
                )?
                .intent(),
            };
            let context = crate::runtime::group_conversation::resolve_group_turn_from_activation(
                &snapshot, &intent,
            )?;
            (Some(context), Some(intent))
        }
        None => {
            anyhow::ensure!(
                group_activation.is_none(),
                "group activation was supplied without a group route"
            );
            (None, None)
        }
    };
    debug_assert!(group
        .as_ref()
        .is_none_or(|group| group.canonical_session_id == session_id));

    let task_id = task_envelope_id(client_turn_id);
    if let (Some(group), Some(intent)) = (&group, &effective_group_activation) {
        let active_participants = group.explicitly_pinged().cloned().collect::<Vec<_>>();
        // Save the authored boundary before creating queued member states.
        // A transcript failure must not leave a never-started room looking
        // queued forever. Boundary retries are keyed by the same turn id.
        crate::runtime::runner::persist_group_user_boundary(
            &crate::config::phoenix_home(),
            group,
            &task_id,
            &request_for_task,
            &orchestrator_model,
        )?;
        crate::runtime::company::global()?.reserve_group_turn(
            &group.canonical_session_id,
            &task_id,
            &request_for_task,
            intent,
            &active_participants,
        )?;
    }

    let task = TaskEnvelope {
        id: task_id,
        session_id: session_id.clone(),
        target_agent: resolved_target,
        title: summarize_title(&user_request),
        user_request: request_for_task,
        context: context_items,
        reply_expected: true,
        interaction_mode,
        agent: agent_context,
        group,
    };

    let orch = orchestrator::Orchestrator::new(
        &orchestrator_model,
        orchestrator::ORCHESTRATOR_SYSTEM_PROMPT,
    );

    // Phoenix-owned state (memory, sessions, cache) lives in the unified
    // `~/.phoenix` home, independent of cwd; the workspace is the CALLER's
    // directory (the daemon threads the client's cwd through — its own
    // process cwd is meaningless).
    let memory_root = crate::config::phoenix_home().join("memory");
    let memory_policy = runner_memory_policy(scaffold, real);

    let (execution, mode, orch_model, coder_model, orch_tokens, coder_tokens) = if real {
        let mut runner = AgentRunner::with_workspace(
            memory_root.clone(),
            workspace_root.clone(),
            Arc::new(ScaffoldProvider),
        )
        .with_permission_mode(permission_mode)
        .with_memory_policy(memory_policy);
        if let Some(ref tx) = event_tx {
            runner = runner.with_event_channel(tx.clone());
        }
        let exec = runner.execute_tool_backed_slice(&orch, &task).await?;
        (exec, "real", None, None, None, None)
    } else if scaffold {
        let mut runner = AgentRunner::with_workspace(
            memory_root.clone(),
            workspace_root.clone(),
            Arc::new(ScaffoldProvider),
        )
        .with_permission_mode(permission_mode)
        .with_memory_policy(memory_policy);
        if let Some(ref tx) = event_tx {
            runner = runner.with_event_channel(tx.clone());
        }
        let exec = runner.execute_first_slice(&orch, &task).await?;
        (exec, "scaffold", None, None, None, None)
    } else {
        let config = config.as_ref().expect("provider-backed has config");
        let llm = &config.profile.llm;
        let fallback_enabled = crate::settings::effective_bool(
            "models.fallback_enabled",
            &crate::settings::SettingsScope::Global,
        )
        .unwrap_or(true);
        let individual_agent_models = crate::settings::effective_string(
            "agents.model_assignment",
            &crate::settings::SettingsScope::Global,
        )
        .is_none_or(|policy| policy != "inherit");
        let no_fallbacks: Vec<String> = Vec::new();
        // A dead main account (expired token, exhausted quota) must not skip
        // the fallback chain below — build it as a `Result`, not `?`, and
        // let `lane_with_fallback_chain` treat a missing first link as just
        // that. `build_base` re-resolves it on demand: a role with no
        // provider override shares the main account, so its default input
        // is the same build, not a clone (`anyhow::Error` isn't `Clone`).
        // `[profile.llm.auth_by_lane]` pins WHICH stored account is a lane's
        // primary. Without it a provider holding five accounts resolves its
        // primary alphabetically (`nvidia:2` beats `nvidia:6` because "2" < "6"
        // in ASCII) — nobody chose that, and it made "make this one the main"
        // unexpressible in the dashboard. `with_lane_pin` is a no-op when the
        // lane has no pin or the pin names another provider's account.
        let orchestrator_llm = llm.with_lane_pin("orchestrator", &llm.provider);
        let build_base = || provider_factory.build_llm_provider(&orchestrator_llm);
        // Per-role provider overrides (brain/executor split). A broken role
        // provider falls back to the main one with a visible warning — the
        // turn must never die because a secondary provider is misconfigured.
        let role_provider =
            |id: &Option<String>, role: &str| -> Option<Arc<dyn crate::providers::LLMProvider>> {
                let configured = id.as_deref().map(str::trim).filter(|id| !id.is_empty());
                let target = configured.unwrap_or(llm.provider.as_str());
                // A role sharing the main provider normally shares its build
                // too — UNLESS it pins a different ACCOUNT of that provider,
                // which is the whole point of `auth_by_lane` and needs its own
                // resolution.
                let pinned = llm.lane_pin(role, target).is_some();
                if configured.map_or(true, |id| id == llm.provider) && !pinned {
                    return None;
                }
                // Pin AFTER probe_for_provider: that helper drops a pin aimed
                // at another provider, and this one is aimed at `target`.
                let profile = llm.probe_for_provider(target).with_lane_pin(role, target);
                match provider_factory.build_llm_provider(&profile) {
                    Ok(p) => Some(p),
                    Err(error) => {
                        eprintln!(
                        "warning: {role} primary account for `{target}` unavailable; resolving its account pool ({error:#})"
                    );
                        None
                    }
                }
            };
        let specialist_primary = role_provider(&llm.specialist_provider, "specialist");
        let librarian_primary = role_provider(&llm.librarian_provider, "librarian");
        // Account-fallback chains: each role's provider gets wrapped with its
        // configured chain of extra accounts, rotated automatically when the
        // active account exhausts (quota, rate limit, expired auth).
        let role_provider_id = |id: &Option<String>| -> String {
            id.as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .unwrap_or(&llm.provider)
                .to_string()
        };
        let specialist_pid = role_provider_id(&llm.specialist_provider);
        let librarian_pid = role_provider_id(&llm.librarian_provider);
        // The orchestrator lane is load-bearing: if nothing in it can be
        // built (main dead AND every fallback account dead), the turn really
        // cannot proceed, so this is the one call site that still propagates.
        let provider = provider_factory.lane_with_fallback_chain(
            build_base(),
            llm,
            "orchestrator",
            &llm.provider,
            if fallback_enabled {
                &llm.fallback.orchestrator
            } else {
                &no_fallbacks
            },
        )?;
        let specialist_provider = if !fallback_enabled {
            specialist_primary.clone()
        } else {
            let primary = match &specialist_primary {
                Some(p) => Ok(p.clone()),
                None => provider_factory.build_llm_provider(&llm.probe_for_provider(&specialist_pid).with_lane_pin("specialist", &specialist_pid)),
            };
            Some(provider_factory.lane_with_fallback_chain(
                primary, llm, "specialist", &specialist_pid, &llm.fallback.specialist,
            )?)
        };
        let librarian_provider = if !fallback_enabled {
            librarian_primary
        } else {
            let primary = match &librarian_primary {
                Some(p) => Ok(p.clone()),
                None => provider_factory.build_llm_provider(&llm.probe_for_provider(&librarian_pid).with_lane_pin("librarian", &librarian_pid)),
            };
            Some(provider_factory.lane_with_fallback_chain(
                primary, llm, "librarian", &librarian_pid, &llm.fallback.librarian,
            )?)
        };
        // Agents with their own chains ("give coder these four Codex
        // accounts") get dedicated providers, checked before the shared
        // specialist one.
        let mut agent_providers: std::collections::HashMap<
            String,
            Arc<dyn crate::providers::LLMProvider>,
        > = std::collections::HashMap::new();
        // Agents reachable here: those with a chain, plus those that only pin
        // an account (`[profile.llm.auth_by_lane].coder`) — a pin with no chain
        // is still a real choice and must not be silently dropped. Role names
        // in that table belong to the lanes above, not to an agent.
        const PINNED_ROLE_KEYS: [&str; 5] =
            ["orchestrator", "specialist", "librarian", "vision", "image"];
        let mut agent_keys: Vec<String> = if fallback_enabled {
            llm.fallback
                .agents
                .keys()
                .filter(|lane| individual_agent_models || lane.as_str() == "volume_worker")
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        if individual_agent_models || llm.auth_by_lane.contains_key("volume_worker") {
            for lane in llm.auth_by_lane.keys() {
                if !PINNED_ROLE_KEYS.contains(&lane.as_str())
                    && (individual_agent_models || lane == "volume_worker")
                    && !agent_keys.contains(lane)
                {
                    agent_keys.push(lane.clone());
                }
            }
        }
        let empty_chain: Vec<String> = Vec::new();
        for agent in &agent_keys {
            let chain = if fallback_enabled {
                llm.fallback.agents.get(agent).unwrap_or(&empty_chain)
            } else {
                &empty_chain
            };
            /* An agent pin may deliberately cross the shared specialist
            provider boundary (one coworker on Codex while the rest ride
            OpenRouter). The dashboard has always allowed that shape, but
            this builder used to ask only "is the pin owned by
            specialist_pid?", silently ignore a Codex pin, and then send
            the agent's Codex model id through OpenRouter. Resolve the
            pinned account's owner first; that owner defines this agent's
            primary provider. */
            let pinned_provider = llm
                .auth_by_lane
                .get(agent)
                .map(String::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .and_then(|id| id.split_once(':').map(|(provider, _)| provider))
                .filter(|provider| !provider.is_empty());
            let agent_pid = pinned_provider.unwrap_or(&specialist_pid);
            let agent_pin = llm.lane_pin(agent, agent_pid);
            if chain.is_empty() && agent_pin.is_none() {
                continue;
            }
            // An agent that pins its own account resolves its own primary;
            // otherwise it rides the shared specialist lane, as before.
            let primary = if agent_pin.is_some() {
                provider_factory.build_llm_provider(
                    &llm.probe_for_provider(agent_pid)
                        .with_lane_pin(agent, agent_pid),
                )
            } else {
                match &specialist_primary {
                    Some(p) => Ok(p.clone()),
                    None => build_base(),
                }
            };
            let provider = provider_factory.lane_with_fallback_chain(primary, llm, agent, agent_pid, chain)?;
            agent_providers.insert(agent.clone(), provider);
        }
        let ask_handler: crate::runtime::AskUserHandler = if interactive {
            std::sync::Arc::new(|input| crate::cli::ask_prompt::collect_answers(input))
        } else {
            std::sync::Arc::new(|_input| {
                anyhow::bail!(
                    "ask_user is unavailable in a gateway session (no terminal to prompt). Decide with best judgment and surface the open question in your final answer."
                )
            })
        };
        // Vision is optional grounding — a broken vision config degrades to a
        // warning, never a failed turn. But the warning must reach the USER:
        // this went to the daemon's invisible stderr for days while agents
        // reported "no vision model configured" against a config that HAD one
        // (openai-codex auth broke when codex left the chains, 2026-07-08).
        let vision = match crate::runtime::vision::vision_config(llm) {
            Ok(v) => v,
            Err(error) => {
                eprintln!("warning: vision disabled ({error:#})");
                static VISION_DOWN_NOTICED: std::sync::atomic::AtomicBool =
                    std::sync::atomic::AtomicBool::new(false);
                if !VISION_DOWN_NOTICED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    crate::runtime::postbox::forward_to_active(
                        crate::runtime::CliEvent::GatewayNotice(format!(
                            "vision is DOWN, not unconfigured — the configured vision \
                             provider failed to initialize ({error:#}). Agents will report \
                             'no vision model configured' until it is fixed: `phoenix \
                             configure` → Model roles → Vision (pick a working lane or \
                             re-login the provider)."
                        )),
                    );
                }
                None
            }
        };
        let mut agent_models: std::collections::HashMap<String, String> = individual_agent_models
            .then(|| llm.agent_models.clone().into_iter().collect())
            .unwrap_or_default();
        if let Some(model) = llm.agent_models.get("volume_worker") {
            agent_models.insert("volume_worker".to_string(), model.clone());
        }
        let role_efforts = llm
            .efforts
            .iter()
            .filter(|(lane, _)| {
                individual_agent_models
                    || PINNED_ROLE_KEYS.contains(&lane.as_str())
                    || lane.as_str() == "volume_worker"
            })
            .map(|(lane, effort)| (lane.clone(), effort.clone()))
            .collect();
        let mut role_context_windows: std::collections::HashMap<String, u64> = llm
            .context_windows
            .iter()
            .map(|(lane, window)| (lane.clone(), *window))
            .collect();
        if let Some(window) = llm.context_window {
            role_context_windows
                .entry("orchestrator".to_string())
                .or_insert(window);
        }
        let mut runner =
            AgentRunner::with_workspace(memory_root.clone(), workspace_root.clone(), provider)
                .with_permission_mode(permission_mode)
                .with_memory_policy(memory_policy)
                .with_specialist_model(llm.specialist())
                .with_librarian_model(llm.librarian())
                .with_specialist_provider(specialist_provider)
                .with_agent_providers(agent_providers)
                .with_librarian_provider(librarian_provider)
                .with_reasoning_effort(llm.reasoning_effort.clone())
                .with_agent_models(agent_models)
                .with_role_efforts(role_efforts)
                .with_role_context_windows(role_context_windows)
                .with_vision(vision)
                .with_native_vision(llm.native_vision)
                .with_ask_user_handler(ask_handler);
        if let Some(ref tx) = event_tx {
            runner = runner.with_event_channel(tx.clone());
        }
        let timeout_seconds = llm.timeout_seconds;
        let provider_model = display_provider_model(Some(&llm.provider), &llm.orchestrator());
        // The actor mesh is the only supported runtime: gateway loop,
        // per-coworker canonical sessions, and `talk` as a durable yield.
        let exec = runner
            .execute_mesh_slice(&orch, &task)
            .await
            .map_err(|error| {
                let detail = error.to_string();
                let hint = if timeout_seconds == 0 {
                    "No provider idle-read timeout is configured; this failure was not caused by Phoenix imposing a provider wall-clock limit.".to_string()
                } else if detail.to_ascii_lowercase().contains("timeout")
                    || detail.to_ascii_lowercase().contains("timed out")
                {
                    format!(
                        "Configured provider idle-read timeout is {timeout_seconds}s; switch models or adjust `timeout_seconds` in .phoenix/config.toml if the provider can legitimately stay silent longer."
                    )
                } else {
                    format!("Configured provider idle-read timeout is {timeout_seconds}s; this failure was not necessarily a timeout.")
                };
                error.context(format!(
                    "Provider-backed Phoenix turn failed for {provider_model}. {hint}"
                ))
            })?;

        let orch_tokens = exec
            .outcome
            .provider_response
            .as_ref()
            .map(|t| (t.input_tokens, t.output_tokens));
        let coder_tokens = exec
            .specialist_outcome
            .provider_response
            .as_ref()
            .map(|t| (t.input_tokens, t.output_tokens));
        let orch_model = exec
            .outcome
            .provider_response
            .as_ref()
            .map(|t| t.request_model.clone());
        let coder_model = exec
            .specialist_outcome
            .provider_response
            .as_ref()
            .map(|t| t.request_model.clone());

        (
            exec,
            "provider-backed",
            orch_model,
            coder_model,
            orch_tokens,
            coder_tokens,
        )
    };

    let mut trace_diagnostics = trace_diagnostics;
    if let Some(ref mut diag) = trace_diagnostics {
        diag.response_model = orch_model.clone();
        diag.specialist_response_model = coder_model.clone();
    }

    // Peak single-call input for the context-window gauge (never the sum).
    let orch_peak = execution
        .outcome
        .provider_response
        .as_ref()
        .map(|t| t.peak_input_tokens);

    Ok(TurnReport {
        run_id: String::new(),
        trace_path: PathBuf::new(),
        execution,
        mode,
        orchestrator_model: orch_model,
        coder_model,
        orchestrator_tokens: orch_tokens,
        orchestrator_peak_input: orch_peak,
        coder_tokens,
        trace_diagnostics,
    })
}

/// Spawn execution in a background task with an event channel.
/// `interactive` gates terminal prompts (ask_user); the gateway daemon passes
/// false because it has no terminal to prompt on.
pub(crate) fn spawn_event_loop(
    scaffold: bool,
    real: bool,
    session_id: String,
    user_request: String,
    permission_mode: crate::tools::PermissionMode,
    interaction_mode: crate::runtime::InteractionMode,
    interactive: bool,
    // The workspace the USER is in. The daemon passes the client's cwd here;
    // None = this process's own cwd (local one-shot/interactive paths).
    workspace: Option<std::path::PathBuf>,
    // Route directly to a specialist instead of the orchestrator.
    // None or "orchestrator" = existing behavior.
    target_agent: Option<String>,
    // Route to a first-class company group rather than one coworker.
    target_group: Option<String>,
    // Stable-id activation authority already validated at the wire/queue
    // boundary. Execution revalidates it against its final directory snapshot.
    group_activation: Option<crate::runtime::group_conversation::GroupActivationIntent>,
    // Canvas sticky notes injected into the agent's context.
    sticky_notes: Option<Vec<crate::cli::daemon::StickyNoteData>>,
    // The canvas viewport — where the user is looking. Used for spatial
    // context scoping of sticky notes. None = no spatial filtering.
    viewport: Option<crate::cli::daemon::ViewportData>,
    // Image files the user attached to this message (absolute paths on disk).
    attachments: Option<Vec<String>>,
    // Stable identity for a client-authored turn. Internal wakes omit it and
    // keep their existing generated task identity.
    client_turn_id: Option<String>,
) -> (
    tokio::task::JoinHandle<Result<TurnReport>>,
    mpsc::Receiver<CliEvent>,
) {
    let (tx, rx) = mpsc::channel::<CliEvent>(256);
    // Provider observers own clones of this execution's channel. Aborting
    // this future drops its senders without touching another execution.
    let handle = tokio::spawn(async move {
        let workspace = workspace
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let trace_context = TraceContext::new(
            workspace.clone(),
            crate::config::phoenix_home().join("runs"),
        );
        let _ = prune_old_run_traces(&trace_context.runs_dir, trace_context.created_at);

        let fallback_diagnostics = PhoenixConfig::load().ok().map(|c| {
            let llm = &c.profile.llm;
            RunDiagnostics {
                session_id: Some(session_id.clone()),
                user_request_preview: Some(preview_user_request(&user_request, 500)),
                provider_id: Some(llm.provider.clone()),
                orchestrator_model: Some(llm.orchestrator()),
                specialist_model: Some(llm.specialist()),
                librarian_model: Some(llm.librarian()),
                timeout_seconds: Some(llm.timeout_seconds),
                configured_max_tokens: llm.max_tokens,
                response_model: None,
                specialist_response_model: None,
            }
        });

        let result = execute_inner(
            scaffold,
            real,
            session_id,
            user_request,
            Some(tx),
            permission_mode,
            interaction_mode,
            interactive,
            workspace,
            target_agent,
            target_group,
            group_activation,
            sticky_notes,
            viewport,
            attachments,
            client_turn_id,
        )
        .await;

        match result {
            Ok(report) => {
                let trace = trace_context.success_trace(
                    report.mode,
                    match report.mode {
                        "scaffold" => "built-in scaffold provider",
                        "real" => "local tool executor",
                        _ => "configured provider",
                    },
                    report.execution.clone(),
                    report.trace_diagnostics.clone(),
                );
                let trace_path = save_run_trace(&trace, &trace_context.runs_dir)?;
                Ok(TurnReport {
                    run_id: trace_context.run_id,
                    trace_path,
                    ..report
                })
            }
            Err(error) => {
                let mode = if real {
                    "real"
                } else if scaffold {
                    "scaffold"
                } else {
                    "provider-backed"
                };
                let mut trace =
                    trace_context.error_trace(mode, "", error.to_string(), fallback_diagnostics);
                if let Some(ref mut err) = trace.error {
                    *err = RunErrorTrace::from_anyhow(&error);
                }
                let trace_path = save_run_trace(&trace, &trace_context.runs_dir)?;
                Err(error).with_context(|| format!("Trace saved: {}", relative_path(&trace_path)))
            }
        }
    });
    (handle, rx)
}

// ── Helpers ───────────────────────────────────────────────────────────

/// Read multiline input when the first line ends with `\`.
fn read_multiline_continuation(editor: &mut CliEditor, first_line: &str) -> Result<String> {
    let mut lines = vec![first_line.trim_end_matches('\\').to_string()];
    loop {
        let line = match editor.readline("\\ ") {
            Ok(line) => line,
            Err(ReadlineError::Interrupted) => return Ok(String::new()),
            Err(ReadlineError::Eof) => break,
            Err(error) => return Err(error.into()),
        };
        if line.trim().is_empty() {
            // Blank line = done with multiline input
            break;
        }
        if line.ends_with('\\') {
            lines.push(line.trim_end_matches('\\').to_string());
        } else {
            lines.push(line);
            break; // Non-continuation line = last line
        }
    }
    Ok(lines.join("\n"))
}

pub(crate) fn summarize_title(prompt: &str) -> String {
    prompt
        .split_whitespace()
        .take(8)
        .collect::<Vec<_>>()
        .join(" ")
}

fn task_envelope_id(client_turn_id: Option<String>) -> String {
    client_turn_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

pub(crate) fn history_path() -> Result<PathBuf> {
    Ok(crate::config::phoenix_home().join("history"))
}

pub(crate) fn relative_path(path: &PathBuf) -> String {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| path.strip_prefix(cwd).ok().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| path.clone())
        .display()
        .to_string()
}

pub use render::{style_bold, style_dim};

#[cfg(test)]
mod tests {
    use super::{
        request_with_composer_attachments, resolve_canonical_turn_route_from, runner_memory_policy,
        summary_is_terminal_failure, task_envelope_id,
    };
    use crate::runtime::company_directory::{
        role_catalog_profiles, AgentRecord, DirectorySnapshot, GroupProfile, GroupRecord,
        LifecycleState,
    };
    use crate::runtime::RunnerMemoryPolicy;
    use crate::session::{SessionKind, SubAgentType};

    fn routing_snapshot() -> DirectorySnapshot {
        let agents = role_catalog_profiles(true)
            .into_iter()
            .map(|mut profile| {
                profile.canonical_session_id = Some(format!("agent-{}", profile.agent_id));
                AgentRecord {
                    profile,
                    archived_at: None,
                    delete_after: None,
                    created_at: "now".into(),
                    updated_at: "now".into(),
                    as_of_seq: 1,
                }
            })
            .collect();
        let groups = vec![GroupRecord {
            profile: GroupProfile {
                group_id: "launch".into(),
                name: "Launch".into(),
                description: String::new(),
                color: "#ff7a45".into(),
                icon_seed: "launch".into(),
                lifecycle: LifecycleState::Active,
                pinned: false,
                sort_order: 0,
                canonical_session_id: Some("group-launch".into()),
                metadata_json: "{}".into(),
                leader_agent_id: None,
            },
            archived_at: None,
            delete_after: None,
            created_at: "now".into(),
            updated_at: "now".into(),
            as_of_seq: 1,
        }];
        DirectorySnapshot {
            agents,
            groups,
            ..DirectorySnapshot::default()
        }
    }

    #[test]
    fn client_turn_id_is_the_stable_task_envelope_id() {
        let id = "turn_canvas_0123456789".to_string();
        assert_eq!(task_envelope_id(Some(id.clone())), id);
        assert!(!task_envelope_id(None).is_empty());
    }

    #[test]
    fn composer_attachments_keep_image_file_and_folder_tooling_distinct() {
        let root = tempfile::tempdir().expect("attachment fixture");
        let image = root.path().join("shot.png");
        let file = root.path().join("notes.txt");
        let folder = root.path().join("sources");
        std::fs::write(&image, b"not decoded in this routing test").expect("image fixture");
        std::fs::write(&file, b"hello").expect("file fixture");
        std::fs::create_dir(&folder).expect("folder fixture");
        let paths = vec![
            image.display().to_string(),
            file.display().to_string(),
            folder.display().to_string(),
            root.path().join("missing.pdf").display().to_string(),
        ];

        let request = request_with_composer_attachments("Review these.", Some(&paths));
        assert!(request.contains(&format!("- image: {:?}", image.display().to_string())));
        assert!(request.contains(&format!("- file: {:?}", file.display().to_string())));
        assert!(request.contains(&format!("- folder: {:?}", folder.display().to_string())));
        assert!(!request.contains("missing.pdf"));
        assert!(request.contains("images with `image_analyze`"));
        assert!(request.contains("files with `read`"));
        assert!(request.contains("folders with `list_directory`"));
        assert!(request.contains("do not treat files or folders as images"));
    }

    #[test]
    fn canonical_direct_route_rejects_other_agents_groups_and_aliases() {
        let snapshot = routing_snapshot();
        assert_eq!(
            resolve_canonical_turn_route_from(
                &snapshot,
                "agent-finance",
                Some(&SessionKind::SubAgent(SubAgentType::Finance)),
                None,
                None,
            )
            .unwrap(),
            (Some("finance".into()), None)
        );
        assert!(resolve_canonical_turn_route_from(
            &snapshot,
            "agent-finance",
            Some(&SessionKind::SubAgent(SubAgentType::Finance)),
            Some("code".into()),
            None,
        )
        .is_err());
        assert!(resolve_canonical_turn_route_from(
            &snapshot,
            "agent-finance",
            Some(&SessionKind::SubAgent(SubAgentType::Finance)),
            Some("scribe".into()),
            None,
        )
        .is_err());
        assert!(resolve_canonical_turn_route_from(
            &snapshot,
            "agent-finance",
            Some(&SessionKind::SubAgent(SubAgentType::Finance)),
            None,
            Some("launch".into()),
        )
        .is_err());
    }

    #[test]
    fn canonical_group_route_rejects_direct_and_foreign_group_targets() {
        let snapshot = routing_snapshot();
        assert_eq!(
            resolve_canonical_turn_route_from(
                &snapshot,
                "group-launch",
                Some(&SessionKind::Main),
                None,
                None,
            )
            .unwrap(),
            (None, Some("launch".into()))
        );
        assert!(resolve_canonical_turn_route_from(
            &snapshot,
            "group-launch",
            Some(&SessionKind::Main),
            Some("finance".into()),
            None,
        )
        .is_err());
        assert!(resolve_canonical_turn_route_from(
            &snapshot,
            "group-launch",
            Some(&SessionKind::Main),
            None,
            Some("other".into()),
        )
        .is_err());
    }

    #[test]
    fn unowned_routes_require_a_matching_persisted_kind() {
        let snapshot = routing_snapshot();
        assert_eq!(
            resolve_canonical_turn_route_from(
                &snapshot,
                "legacy-finance",
                Some(&SessionKind::SubAgent(SubAgentType::Finance)),
                Some("money".into()),
                None,
            )
            .unwrap(),
            (Some("finance".into()), None)
        );
        assert!(resolve_canonical_turn_route_from(
            &snapshot,
            "legacy-finance",
            Some(&SessionKind::SubAgent(SubAgentType::Finance)),
            Some("code".into()),
            None,
        )
        .is_err());
        assert!(resolve_canonical_turn_route_from(
            &snapshot,
            "new-unowned",
            None,
            Some("finance".into()),
            None,
        )
        .is_err());
        assert!(resolve_canonical_turn_route_from(
            &snapshot,
            "new-unowned",
            None,
            None,
            Some("launch".into()),
        )
        .is_err());
    }

    #[test]
    fn canonical_owner_and_persisted_session_kind_must_agree() {
        let snapshot = routing_snapshot();
        assert!(resolve_canonical_turn_route_from(
            &snapshot,
            "agent-finance",
            Some(&SessionKind::Main),
            None,
            None,
        )
        .is_err());
        assert!(resolve_canonical_turn_route_from(
            &snapshot,
            "group-launch",
            Some(&SessionKind::SubAgent(SubAgentType::Finance)),
            None,
            None,
        )
        .is_err());
    }

    #[test]
    fn scaffold_alone_disables_runner_memory() {
        assert_eq!(
            runner_memory_policy(true, false),
            RunnerMemoryPolicy::DisabledDiagnostic
        );
        assert_eq!(
            runner_memory_policy(false, true),
            RunnerMemoryPolicy::Enabled
        );
        assert_eq!(
            runner_memory_policy(false, false),
            RunnerMemoryPolicy::Enabled
        );
    }

    #[test]
    fn one_shot_exit_detects_runtime_authored_terminal_failures() {
        assert!(summary_is_terminal_failure(
            "The `orchestrator` agent could not complete its turn: usage_limit_reached"
        ));
        assert!(summary_is_terminal_failure(
            "The `coder` agent hit an internal error (a panic) and could not finish this turn: boom"
        ));
        assert!(summary_is_terminal_failure(
            "Phoenix stopped this agent at a hard runtime boundary: round limit"
        ));
        assert!(summary_is_terminal_failure(
            "The provider became unavailable after this agent had already performed work. Phoenix preserved the completed tool evidence instead of discarding the turn."
        ));
        assert!(summary_is_terminal_failure(
            "## Result\nPhoenix rejected it during runtime validation.\n\nReason: malformed envelope"
        ));
        assert!(summary_is_terminal_failure(
            "## Result\nPhoenix reached Phoenix's bounded provider-loop limit after 48 rounds."
        ));
    }

    #[test]
    fn one_shot_exit_keeps_successful_answers_zero() {
        assert!(!summary_is_terminal_failure(
            "## Done\nBuild passes and the app is running."
        ));
        assert!(!summary_is_terminal_failure("message delivered to Phoenix"));
    }
}


#[cfg(test)]
mod composer_design_reference_tests {
    use super::composer_design_reference_context;

    #[test]
    fn only_actual_composer_image_entries_become_typed_design_references() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("reference with spaces.PNG"), b"fixture; bytes validated by engine").unwrap();
        std::fs::write(dir.path().join("notes.md"), b"/private/claimed.png").unwrap();
        std::fs::create_dir(dir.path().join("folder.png")).unwrap();
        let attachments = vec!["reference with spaces.PNG".into(), "notes.md".into(), "folder.png".into(), "missing.jpg".into()];
        let context = composer_design_reference_context(Some(&attachments), dir.path()).unwrap();
        assert_eq!(context.label, "phoenix_design_reference_paths");
        let paths: Vec<String> = serde_json::from_str(&context.content).unwrap();
        assert_eq!(paths, vec![dir.path().join("reference with spaces.PNG").to_string_lossy().into_owned()]);
        assert!(composer_design_reference_context(None, dir.path()).is_none());
        assert!(composer_design_reference_context(Some(&vec!["notes.md".into()]), dir.path()).is_none());
    }
}
