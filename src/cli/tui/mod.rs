//! Phoenix TUI — the full-screen session application.
//!
//! `phoenix start` on a real terminal opens this instead of the line-based
//! REPL (escape hatch: `PHOENIX_CLASSIC=1`). One screen, four regions:
//!
//!   header     animated flame wordmark · provider/model · gateway dot
//!   feed       the conversation: user lines, agent activity, final answers
//!   agent bar  one chip per agent (orchestrator/coder/researcher/browser)
//!              with a breathing pulse while that agent is thinking or in a tool
//!   status bar context-usage gauge · turn timer · route · tokens · mode
//!   input      bordered prompt with history, cursor, and slash commands
//!
//! All turn execution still flows through the gateway daemon — this file is
//! pure presentation over the same `CliEvent` stream the classic CLI renders.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io;
use std::io::Read as _;
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::{DateTime, Local};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    KeyCode, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Terminal;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

use super::daemon::{self, RemoteOutcome, TurnSummary};
use super::AppState;
use crate::runtime::CliEvent;

// ── Design tokens ─────────────────────────────────────────────────────
// One warm monochrome ramp + one accent. The rule of the whole UI: color
// means ALIVE (shimmer, pulses, the accent prompt) — everything settled
// drops to the grey ramp, with at most a whisper of the owning agent's hue.
const FLAME: [Color; 5] = [
    Color::Rgb(255, 70, 0),
    Color::Rgb(255, 110, 0),
    Color::Rgb(255, 150, 0),
    Color::Rgb(255, 195, 0),
    Color::Rgb(255, 235, 90),
];
const ACCENT: Color = Color::Rgb(255, 146, 24); // phoenix orange — prompts, live turn
const TEXT: Color = Color::Rgb(232, 230, 222); // the user's words, final answers
const MID: Color = Color::Rgb(174, 172, 164); // narration, secondary text
const DIM: Color = Color::Rgb(124, 122, 116); // receipts, metadata
const SUBTLE: Color = Color::Rgb(74, 72, 68); // hairlines, idle chips, borders
const GREEN: Color = Color::Rgb(126, 202, 148);
const RED: Color = Color::Rgb(236, 106, 96);
const SURFACE: Color = Color::Rgb(30, 29, 27); // menu selection, code blocks

use crate::config::resolve_context_window;

// ── App events (input + turn stream unified) ─────────────────────────

enum AppEvent {
    Key(crossterm::event::KeyEvent),
    /// A bracketed paste delivered as one chunk (newlines and all) so a
    /// multi-line paste lands in the input buffer instead of submitting
    /// line-by-line as a burst of Enter keypresses.
    Paste(String),
    /// Mouse-wheel scroll: positive = back in history (up).
    Scroll(i16),
    Resize,
    Turn(CliEvent),
    TurnFinished(RemoteOutcome),
    TurnFailed(String),
    GatewayDown,
}

// ── Feed model ────────────────────────────────────────────────────────

enum FeedItem {
    User(String),
    /// One agent activity row: (agent, symbol, text, ok). `ok=None` → neutral.
    Activity {
        agent: String,
        symbol: &'static str,
        text: String,
        ok: Option<bool>,
    },
    Answer(String),
    /// A file-edit diff (verbose display mode): `- ` lines render red, `+ `
    /// lines green, `@@ path` header cyan.
    Diff {
        agent: String,
        diff: String,
    },
    /// A background agent's finished result — rendered as a right-aligned
    /// block in the agent's color, the visual "this came back from the side".
    AgentReturn {
        agent: String,
        subject: String,
        body: String,
        ok: bool,
    },
    Notice(String),
    Error(String),
    Blank,
}

#[derive(Clone, PartialEq)]
enum AgentMood {
    Idle,
    Thinking,
    Tool(String),
    Done,
}

struct AgentChip {
    label: &'static str,
    mood: AgentMood,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BackgroundJobStatus {
    agent: String,
    subject: String,
    status: String,
    done: bool,
    /// When the spawn rendered — drives the live row's elapsed counter.
    started: Instant,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct TurnActivityStats {
    tool_calls: u32,
    failed_tool_calls: u32,
    duplicate_tool_calls: u32,
    specialist_handoffs: u32,
    background_handoffs: u32,
    // Per-category counters for the compact display mode's live aggregate
    // ("14 tools · 6 reads · 2 edits · 1 cmd").
    reads: u32,
    edits: u32,
    commands: u32,
    web: u32,
}

// The compact work journal's tally + tool bucketing moved to
// `runtime::story` (plan 015 phase 0): the daemon's story lane and the TUI
// share ONE reduction so no client can drift from the locked display taste.
use crate::runtime::story::{tool_category, CompactTally};

struct Tui {
    state: AppState,
    feed: Vec<FeedItem>,
    agents: BTreeMap<&'static str, AgentChip>,
    input: String,
    cursor: usize,
    history: Vec<String>,
    history_pos: Option<usize>,
    /// Manual scroll offset from the bottom (0 = follow latest).
    scroll_back: u16,
    turn_started: Option<Instant>,
    /// Set the instant the user presses Esc/stop: the UI stops showing work
    /// immediately (decoupled from the gateway's abort round-trip) and any
    /// trailing events from the dying turn are ignored until it truly ends.
    cancelling: bool,
    /// Live provider-stream ticker (tail of thinking/text deltas) shown under
    /// the working row — proof of life during long generations. Ephemeral.
    live_stream: String,
    /// One-line stream status note ("model streaming… 184 KB in 15s").
    stream_note: String,
    /// Messages sent while a turn was running; dispatched FIFO on turn end.
    queued_inputs: std::collections::VecDeque<String>,
    turn_activity: TurnActivityStats,
    seen_tool_inputs: std::collections::BTreeSet<String>,
    /// Compact mode's work journal: successful tool calls accumulate here per
    /// agent instead of rendering rows, and flush as one "read 3 files · ran
    /// 5 commands" receipt at the next narrative beat (narration line,
    /// handoff, completion, final). The feed reads like an operator telling
    /// the story of the work, not like a tool log.
    compact_tallies: std::collections::BTreeMap<String, CompactTally>,
    /// Live per-agent tool counts keyed by `agent_key` — NEVER flushed at
    /// beats (that's `compact_tallies`); this feeds each working agent's own
    /// shimmer row ("Canvas thinking… · read 3 files · 2 web/app calls"),
    /// turn or no turn, and clears when that agent's job returns.
    live_counts: std::collections::BTreeMap<&'static str, CompactTally>,
    background_jobs: Vec<BackgroundJobStatus>,
    turn_elapsed_last: f32,
    session_tokens: u64,
    /// Tokens (estimated) the compression layer kept out of model context
    /// this session.
    compression_saved_tokens: u64,
    /// Tokens (estimated) of raw tool output that entered the compressor
    /// this session — the denominator that turns saved into a provable ratio.
    compression_raw_tokens: u64,
    last_context_compaction: Option<String>,
    last_route: String,
    ctx_tokens_estimate: u64,
    /// Context window of the configured model (catalog/config-resolved).
    context_window: u64,
    tick: usize,
    gateway_ok: bool,
    quit_armed: bool,
    should_quit: bool,
    /// Feed length at the last snapshot save — the dirty check for the
    /// debounced feed persistence (resume restores the feed verbatim).
    feed_saved_len: usize,
    /// Main-transcript messages already represented in the feed; persisted
    /// with the snapshot so resume can append only what landed while away.
    main_msgs_seen: usize,
    /// Hovered row in the slash-command menu (visible while input is `/...`
    /// with no space yet).
    menu_sel: usize,
    /// An agent's `ask_user` popup awaiting the user's answer. Rendered as a
    /// modal over the feed; keys route here while open.
    ask_modal: Option<AskModal>,
    /// `/resume` session picker: a selectable list over the feed (↑↓ + Enter
    /// switches, Esc closes). Rows are (session id, display label).
    resume_picker: Option<ResumePicker>,
    /// Asks that arrived while another popup was open — served FIFO.
    pending_asks: std::collections::VecDeque<AskModal>,
    /// Terminal reports modified keys (kitty keyboard protocol pushed at
    /// startup). Decides whether Shift+Enter can insert a newline — legacy
    /// terminals report Shift+Enter as plain Enter, where only Alt+Enter
    /// works. Used for the honest splash hint.
    enhanced_keys: bool,
}

/// The `/resume` picker state: recent sessions, hover position.
struct ResumePicker {
    /// (session id, human label incl. age + current marker)
    items: Vec<(String, String)>,
    sel: usize,
}

/// One live ask_user popup: the questions, collected answers, and the
/// user's position within it.
struct AskModal {
    id: String,
    agent: String,
    questions: Vec<crate::tools::ask_user::AskUserQuestion>,
    current: usize,
    answers: Vec<String>,
    /// Hovered option row; == options.len() means the "type your own" row.
    sel: usize,
    /// Free-text answer buffer (active when `typing`).
    text: String,
    typing: bool,
    /// Multi-select toggles for the current question (Space flips).
    checked: Vec<bool>,
}

impl AskModal {
    /// Reset per-question cursor state when a new question becomes current.
    fn arm_current(&mut self) {
        let q = &self.questions[self.current];
        self.sel = 0;
        self.text.clear();
        // No options = the free-text row is the only row: type immediately.
        self.typing = q.options.is_empty();
        self.checked = vec![false; q.options.len()];
    }
}

/// Slash-command registry: (name, usage, description, takes_args).
/// Drives the popup menu: type `/` to open, arrows to hover, Enter to pick —
/// a hovered no-arg command runs immediately; arg commands fill the input.
const COMMANDS: [(&str, &str, &str, bool); 24] = [
    ("/help", "/help", "list commands and keys", false),
    (
        "/stop",
        "/stop",
        "stop the running turn; queued messages run next (also: Esc)",
        false,
    ),
    (
        "/clear",
        "/clear",
        "clear the screen (session unchanged)",
        false,
    ),
    ("/new", "/new", "start a fresh session", false),
    (
        "/resume",
        "/resume [id]",
        "pick a recent session from a menu (or /resume <name> directly)",
        false,
    ),
    (
        "/status",
        "/status",
        "session, models, gateway, context usage",
        false,
    ),
    (
        "/model",
        "/model",
        "show the configured per-role models",
        false,
    ),
    (
        "/memory",
        "/memory",
        "knowledge-graph memory summary",
        false,
    ),
    (
        "/burn",
        "/burn",
        "where tokens and minutes go (per agent, model, day)",
        false,
    ),
    (
        "/mcp",
        "/mcp",
        "connected MCP servers (manage: phoenix configure)",
        false,
    ),
    (
        "/compact",
        "/compact",
        "show session compaction/archive state",
        false,
    ),
    (
        "/checkpoints",
        "/checkpoints",
        "list rewindable edit checkpoints",
        false,
    ),
    (
        "/rewind",
        "/rewind",
        "undo the agent's last turn of file edits",
        false,
    ),
    (
        "/cron",
        "/cron add <when> :: <prompt> | list | rm <id>",
        "schedule wake-ups (every 10m · daily 09:30 · in 45m)",
        true,
    ),
    (
        "/yolo",
        "/yolo",
        "tools run unconfined (/safe restores)",
        false,
    ),
    (
        "/display",
        "/display [compact|verbose]",
        "feed style: narrated journal · journal + edit diffs (no arg opens the picker)",
        true,
    ),
    (
        "/view",
        "/view actions|thinking|debug|subagents",
        "toggle feed row kinds",
        true,
    ),
    (
        "/defaults",
        "/defaults save",
        "save current toggles as defaults",
        true,
    ),
    (
        "/provider",
        "/provider <id> [api-key] [model]",
        "switch LLM lane: ollama | ollama-cloud | openai-codex",
        true,
    ),
    ("/actions", "/actions", "toggle tool activity rows", false),
    (
        "/subagents",
        "/subagents",
        "toggle sub-agent working steps (spawn/return always shown)",
        false,
    ),
    ("/thinking", "/thinking", "toggle thinking rows", false),
    ("/debug", "/debug", "toggle debug rows", false),
    (
        "/quit",
        "/quit",
        "exit (a running turn finishes on the gateway)",
        false,
    ),
];

/// Stable ordering for the agent bar. Labels are the persona names
/// (`agent_persona` in runtime::delegation is the source of truth).
const AGENT_ORDER: [(&str, &str); 12] = [
    ("orchestrator", "Phoenix"),
    ("scribe", "Nico"),
    ("planner", "Maya"),
    ("finance", "Vera"),
    ("coder", "Leo"),
    ("frontend", "Iris"),
    ("researcher", "Theo"),
    ("presentation", "Elena"),
    ("critic", "Remy"),
    ("sales", "Owen"),
    ("marketing", "June"),
    ("personal_logistics", "Cleo"),
];

fn agent_key(raw: &str) -> &'static str {
    let lower = raw.to_lowercase();
    // Labels arrive in three shapes: a bare role ("coder"), a parallel-instance
    // label ("coder#2"), or a full display name ("Leo (coder)" / "Leo
    // (coder) #2"). Key by the role: the text inside the parens when present,
    // else the part before any `#`.
    let core = match (lower.find('('), lower.find(')')) {
        (Some(open), Some(close)) if close > open + 1 => &lower[open + 1..close],
        _ => lower.split('#').next().unwrap_or(""),
    };
    match core.trim() {
        "orchestrator" | "orch" | "phoenix" | "atlas" => "orchestrator",
        "scribe" | "communications" | "inbox" | "nico" => "scribe",
        "planner" | "plan" | "maya" | "compass" => "planner",
        "finance" | "purchasing" | "vera" => "finance",
        "coder" | "spark" => "coder",
        "researcher" | "research" | "theo" | "scout" => "researcher",
        "browser" | "surf" => "browser",
        "frontend" | "design" | "ui" | "iris" => "frontend",
        "database" | "db" | "vault" => "database",
        "hacker" | "cipher" => "hacker",
        "critic" | "systems" | "reliability" | "security" | "remy" | "hawk" => "critic",
        "tester" | "probe" => "tester",
        "presentation" | "documents" | "knowledge" | "elena" | "slides" | "deck" | "canvas" => {
            "presentation"
        }
        "sales" | "relationships" | "crm" | "owen" => "sales",
        "marketing" | "publishing" | "content" | "june" => "marketing",
        "personal_logistics" | "operations" | "logistics" | "cleo" => "personal_logistics",
        "computer_use" | "computeruse" | "computer" | "desktop" | "pixel" => "computer_use",
        // Unknown labels render under the orchestrator bucket (always visible)
        // — every roster agent MUST have its own arm above or its tool rows
        // get misattributed (the "planner shows as orchestrator" bug).
        _ => "orchestrator",
    }
}

/// Take the next replay chunk for `agent`, but only when its leading brief
/// actually came from `sender` — an orchestrator hand-off row must not steal
/// a chunk that a chain baton (planner/researcher/…) briefed. A non-matching
/// front chunk stays put: its own hand-off row (in a chain replay or the
/// end-of-feed pass) will claim it.
fn pop_chunk_for(
    inner: &mut HashMap<&'static str, VecDeque<Vec<crate::session::Message>>>,
    agent: &str,
    sender: &str,
) -> Option<Vec<crate::session::Message>> {
    let chunks = inner.get_mut(agent_key(agent))?;
    let matches = match chunks.front()?.first() {
        Some(crate::session::Message::Talk { from, .. }) => agent_key(from) == agent_key(sender),
        _ => true, // user-briefed or headless chunk — any hand-off row may host it
    };
    if matches {
        chunks.pop_front()
    } else {
        None
    }
}

#[cfg(test)]
mod chunk_pop_tests {
    use super::pop_chunk_for;
    use crate::session::Message;
    use std::collections::{HashMap, VecDeque};

    fn brief(from: &str, to: &str) -> Message {
        Message::Talk {
            from: from.into(),
            to: to.into(),
            subject: "brief".into(),
            body: "do the thing".into(),
            reply_expected: false,
            handoff_id: String::new(),
            reply_to: None,
            causation_id: None,
            status: String::new(),
        }
    }

    fn inner_with(chunks: Vec<Vec<Message>>) -> HashMap<&'static str, VecDeque<Vec<Message>>> {
        HashMap::from([("coder", chunks.into_iter().collect::<VecDeque<_>>())])
    }

    #[test]
    fn orchestrator_row_cannot_steal_a_chain_briefed_chunk() {
        // The 4daeb15c regression: coder chunk 1 was briefed by researcher (a
        // chain baton); the orchestrator's later hand-off row must skip it and
        // leave it for the chain replay, not splice the wrong work.
        let mut inner = inner_with(vec![
            vec![brief("researcher", "coder")],
            vec![brief("orchestrator", "coder")],
        ]);
        assert!(pop_chunk_for(&mut inner, "coder", "orchestrator").is_none());
        assert!(pop_chunk_for(&mut inner, "coder", "researcher").is_some());
        // With the baton chunk claimed, the orchestrator row gets its own.
        assert!(pop_chunk_for(&mut inner, "coder", "orchestrator").is_some());
    }

    #[test]
    fn display_name_senders_still_match() {
        let mut inner = inner_with(vec![vec![brief("Theo (researcher)", "Leo (coder)")]]);
        assert!(pop_chunk_for(&mut inner, "coder", "researcher").is_some());
    }
}

impl Tui {
    fn new(state: AppState, enhanced_keys: bool) -> Self {
        let mut agents = BTreeMap::new();
        for (key, label) in AGENT_ORDER {
            agents.insert(
                key,
                AgentChip {
                    label,
                    mood: AgentMood::Idle,
                },
            );
        }
        let history = match super::history_path() {
            Ok(path) => match load_history(&path) {
                Ok(history) => history,
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %format_args!("{error:#}"),
                        "TUI history was not loaded"
                    );
                    Vec::new()
                }
            },
            Err(error) => {
                tracing::warn!(error = %format_args!("{error:#}"), "TUI history path unavailable");
                Vec::new()
            }
        };
        let mut tui = Self {
            state,
            feed: Vec::new(),
            agents,
            input: String::new(),
            cursor: 0,
            history,
            history_pos: None,
            scroll_back: 0,
            turn_started: None,
            cancelling: false,
            live_stream: String::new(),
            stream_note: String::new(),
            queued_inputs: std::collections::VecDeque::new(),
            turn_activity: TurnActivityStats::default(),
            seen_tool_inputs: std::collections::BTreeSet::new(),
            compact_tallies: std::collections::BTreeMap::new(),
            live_counts: std::collections::BTreeMap::new(),
            background_jobs: Vec::new(),
            turn_elapsed_last: 0.0,
            session_tokens: 0,
            compression_saved_tokens: 0,
            compression_raw_tokens: 0,
            last_context_compaction: None,
            last_route: "—".to_string(),
            ctx_tokens_estimate: 0,
            context_window: resolve_context_window(),
            tick: 0,
            gateway_ok: true,
            quit_armed: false,
            should_quit: false,
            menu_sel: 0,
            ask_modal: None,
            resume_picker: None,
            pending_asks: std::collections::VecDeque::new(),
            enhanced_keys,
            feed_saved_len: 0,
            main_msgs_seen: 0,
        };
        tui.refresh_ctx_estimate();
        tui.splash();
        tui.replay_session();
        tui
    }

    /// Resume context. Snapshot first: the feed persisted while the session
    /// was live reloads VERBATIM — the same rows, in the same order, exactly
    /// as the user left the screen; only messages that landed after the
    /// snapshot (background work while the TUI was closed) append at the
    /// bottom, where they happened. Transcript reconstruction below survives
    /// solely as the fallback for sessions that predate snapshots.
    fn replay_session(&mut self) {
        if self.restore_feed_snapshot() {
            return;
        }
        let Some(main) = read_session(&session_file(&self.state.session_id)) else {
            return; // brand-new session — nothing to replay
        };
        self.feed.push(FeedItem::Notice(format!(
            "resumed session {} — {} message(s)",
            main.id,
            main.messages.len(),
        )));

        // Specialist transcripts, chunked per delegated turn: each chunk
        // replays under the hand-off row that briefed it — an orchestrator row
        // in the main transcript, or a chain baton inside another specialist's
        // work. Every roster agent replays (planner included: its chain batons
        // are how researcher/coder work gets anchored).
        let verbose = self.state.display_mode == crate::cli::DisplayMode::Verbose;
        let mut inner: HashMap<&'static str, VecDeque<Vec<crate::session::Message>>> =
            HashMap::new();
        if verbose {
            for (agent, _) in AGENT_ORDER {
                if agent == "orchestrator" {
                    continue;
                }
                let Some(agent_type) = crate::runtime::delegation::specialist_from_talk_name(agent)
                else {
                    continue;
                };
                // Use the same bounded derivation as the runtime. Building
                // this filename by concatenation made verbose replay miss
                // specialists for a valid 192-byte main session id.
                let path = session_file(&crate::session::specialist_session_id(
                    &self.state.session_id,
                    agent_type,
                ));
                let Some(session) = read_session(&path) else {
                    continue;
                };
                let chunks = chunk_specialist_turns(&session.messages, agent);
                if !chunks.is_empty() {
                    inner.insert(agent_key(agent), chunks);
                }
            }
        }

        self.feed.push(FeedItem::Blank);
        let skipped = main.messages.len().saturating_sub(300);
        if skipped > 0 {
            self.feed.push(FeedItem::Notice(format!(
                "({skipped} earlier message(s) folded — the transcript on disk has everything)"
            )));
        }
        self.replay_messages(&main.messages[skipped..], "orchestrator", &mut inner);
        self.main_msgs_seen = main.messages.len();
        // Inner work whose hand-off fell outside the replay window (or whose
        // chain parent transcript is missing) still shows after the
        // conversation — under its OWN hand-off row, read from the chunk's
        // leading brief, never a placeholder banner.
        let mut agents: Vec<&'static str> = inner.keys().copied().collect();
        agents.sort_unstable();
        for agent in agents {
            let mut first = true;
            loop {
                let Some(chunk) = inner.get_mut(agent).and_then(|c| c.pop_front()) else {
                    break;
                };
                if first {
                    self.feed.push(FeedItem::Blank);
                    first = false;
                }
                if let Some(crate::session::Message::Talk {
                    from, to, subject, ..
                }) = chunk.first()
                {
                    self.push_activity(
                        from,
                        "⧉",
                        format!("{from} → {to}: {}", truncate(subject, 90)),
                        None,
                    );
                }
                self.replay_inner_chunk(&chunk, agent, &mut inner);
            }
        }
        self.feed.push(FeedItem::Blank);
        // Land at the latest exchange; scroll up for the rest.
        self.scroll_back = 0;
    }

    /// Reload the persisted feed snapshot for this session, exactly as saved.
    /// Returns false when no usable snapshot exists (fall back to transcript
    /// reconstruction). Messages that landed after the snapshot — background
    /// returns while the TUI was closed — append at the bottom via the same
    /// replay path, chronologically where they belong.
    fn restore_feed_snapshot(&mut self) -> bool {
        let Some(snapshot) = feedstore::load(&self.state.session_id) else {
            return false;
        };
        // The snapshot IS the screen the user left, splash included — replace
        // the fresh splash instead of stacking a second banner on top.
        self.feed = snapshot.items.into_iter().map(FeedItem::from).collect();
        self.feed.push(FeedItem::Blank);
        self.feed.push(FeedItem::Notice(format!(
            "resumed session {} — restored exactly as you left it",
            self.state.session_id
        )));
        self.main_msgs_seen = snapshot.main_watermark;
        if let Some(main) = read_session(&session_file(&self.state.session_id)) {
            // A compacted transcript can shrink below the watermark; there is
            // no honest delta to draw then, and the snapshot already has the
            // conversation — skip rather than duplicate.
            if main.messages.len() > snapshot.main_watermark {
                let fresh = &main.messages[snapshot.main_watermark..];
                self.feed.push(FeedItem::Notice(format!(
                    "({} message(s) landed while you were away)",
                    fresh.len()
                )));
                // No inner-chunk splicing here: those specialists' rows were
                // either already on screen when the user left, or belong to
                // work that returns as its own card below.
                self.replay_messages(fresh, "orchestrator", &mut HashMap::new());
                self.main_msgs_seen = main.messages.len();
            }
        }
        self.feed.push(FeedItem::Blank);
        self.scroll_back = 0;
        true
    }

    /// Persist the rendered feed when it changed since the last save — called
    /// from the ticker (debounced) so a crash or oomd kill loses at most a
    /// second of journal, never the session view.
    fn persist_feed_if_dirty(&mut self) {
        if self.feed.len() != self.feed_saved_len {
            self.persist_feed_now();
        }
    }

    /// Unconditional snapshot save under the CURRENT session id — also the
    /// pre-switch hook for /new and /resume so the outgoing session keeps
    /// its exact final screen.
    fn persist_feed_now(&mut self) {
        match feedstore::save(&self.state.session_id, self.main_msgs_seen, &self.feed) {
            Ok(()) => self.feed_saved_len = self.feed.len(),
            Err(error) => {
                tracing::warn!("{error:#}");
                let message = format!(
                    "journal snapshot was not saved; the session transcript is intact, but this exact screen layout is not durable ({error:#})"
                );
                if !matches!(self.feed.last(), Some(FeedItem::Error(last)) if last == &message) {
                    self.feed.push(FeedItem::Error(message));
                }
                // Avoid retrying every ticker frame. The next real feed change
                // retries, and explicit turn-end/quit saves still try again.
                self.feed_saved_len = self.feed.len();
            }
        }
    }

    /// Refresh the main-transcript watermark from disk (one parse) — done at
    /// turn end and quit, not per save, so the debounced saver stays cheap.
    fn refresh_feed_watermark(&mut self) {
        if let Some(main) = read_session(&session_file(&self.state.session_id)) {
            self.main_msgs_seen = main.messages.len();
        }
    }

    /// Render the main transcript into the feed in order, splicing each
    /// specialist's next work chunk under its hand-off row.
    fn replay_messages(
        &mut self,
        messages: &[crate::session::Message],
        owner: &str,
        inner: &mut HashMap<&'static str, VecDeque<Vec<crate::session::Message>>>,
    ) {
        // Replay journals tool calls the same way the live feed does:
        // successes accumulate and land as one receipt at the next beat.
        let mut tally = CompactTally::default();
        for message in messages {
            match message {
                crate::session::Message::User { content } => {
                    self.flush_replay_tally(owner, &mut tally);
                    self.feed.push(FeedItem::User(truncate(content, 600)));
                }
                crate::session::Message::Assistant { content } => {
                    self.flush_replay_tally(owner, &mut tally);
                    self.replay_assistant(owner, content, true);
                }
                crate::session::Message::Talk {
                    from,
                    to,
                    subject,
                    body,
                    reply_expected,
                    ..
                } => {
                    self.flush_replay_tally(owner, &mut tally);
                    let is_return =
                        !reply_expected && agent_key(to) == "orchestrator" && from != to;
                    if is_return {
                        // A specialist's report-back IS its result — replay it
                        // as the same return card the live feed drew, instead
                        // of a truncated activity row that buries the output.
                        let failed = body.starts_with("BACKGROUND JOB FAILED");
                        self.feed.push(FeedItem::AgentReturn {
                            agent: from.clone(),
                            subject: subject.clone(),
                            body: body.clone(),
                            ok: !failed,
                        });
                    } else {
                        // A hand-off send — same ⧉ hand-off row as live spawns,
                        // then that specialist's work rides directly under it,
                        // exactly where it happened.
                        self.push_activity(
                            from,
                            "⧉",
                            format!("{from} → {to}: {}", truncate(subject, 90)),
                            None,
                        );
                        if let Some(chunk) = pop_chunk_for(inner, to, from) {
                            self.replay_inner_chunk(&chunk, to, inner);
                        }
                    }
                }
                crate::session::Message::GroupContribution {
                    display_name,
                    subject,
                    body,
                    ..
                } => {
                    self.flush_replay_tally(owner, &mut tally);
                    self.push_activity(
                        display_name,
                        "◆",
                        format!("{}: {}", subject, truncate(body, 180)),
                        None,
                    );
                }
                crate::session::Message::ToolResult {
                    tool_name,
                    input,
                    success,
                    output,
                } => {
                    self.replay_tool_result(owner, &mut tally, tool_name, input, *success, output);
                }
            }
        }
        self.flush_replay_tally(owner, &mut tally);
    }

    /// One delegated specialist turn, replayed inline under its hand-off row:
    /// narration + aggregated receipts + verbose diffs. Incoming briefs and
    /// returns are skipped — the hand-off row and the return card already
    /// carry them in the conversation — but an outgoing chain baton (this
    /// agent handing work to another specialist) renders its own ⧉ row and
    /// splices that agent's next chunk right there, exactly like the live
    /// feed drew the chain.
    fn replay_inner_chunk(
        &mut self,
        chunk: &[crate::session::Message],
        owner: &str,
        inner: &mut HashMap<&'static str, VecDeque<Vec<crate::session::Message>>>,
    ) {
        let mut tally = CompactTally::default();
        for message in chunk {
            match message {
                crate::session::Message::Assistant { content } => {
                    self.flush_replay_tally(owner, &mut tally);
                    self.replay_assistant(owner, content, false);
                }
                crate::session::Message::ToolResult {
                    tool_name,
                    input,
                    success,
                    output,
                } => {
                    self.replay_tool_result(owner, &mut tally, tool_name, input, *success, output);
                }
                crate::session::Message::Talk {
                    from, to, subject, ..
                } => {
                    let baton = agent_key(from) == agent_key(owner)
                        && agent_key(to) != "orchestrator"
                        && agent_key(to) != agent_key(owner);
                    if baton {
                        self.flush_replay_tally(owner, &mut tally);
                        self.push_activity(
                            from,
                            "⧉",
                            format!("{from} → {to}: {}", truncate(subject, 90)),
                            None,
                        );
                        if let Some(next) = pop_chunk_for(inner, to, from) {
                            self.replay_inner_chunk(&next, to, inner);
                        }
                    }
                }
                _ => {}
            }
        }
        self.flush_replay_tally(owner, &mut tally);
    }

    /// The narration brief rides in EVERY display mode, exactly like the live
    /// feed (/thinking widens it); `with_final` additionally renders a
    /// final_answer's markdown as the Answer block (main conversation only).
    fn replay_assistant(&mut self, owner: &str, content: &str, with_final: bool) {
        if let Some(reasoning) = extract_reasoning(content) {
            let cap = if self.state.show_reasoning { 400 } else { 140 };
            let brief = first_sentence(&reasoning, cap);
            if !brief.is_empty() {
                self.push_activity(owner, "∴", brief, None);
            }
        }
        if with_final {
            if let Some(markdown) = extract_final_markdown(content) {
                self.feed.push(FeedItem::Blank);
                self.feed.push(FeedItem::Answer(markdown));
            }
        }
    }

    /// Successes journal into the aggregated receipt; failures render
    /// individually in every mode; verbose additionally rebuilds the red/green
    /// edit diff from the stored tool input — an edit must never resume as
    /// nothing but a "did N things" receipt.
    fn replay_tool_result(
        &mut self,
        owner: &str,
        tally: &mut CompactTally,
        tool_name: &str,
        input: &str,
        success: bool,
        output: &str,
    ) {
        if success {
            tally.add(tool_name);
            if self.state.display_mode == crate::cli::DisplayMode::Verbose {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(input) {
                    if let Some(diff) =
                        crate::runtime::mesh::edit_display_diff(tool_name, &json, None)
                    {
                        self.feed.push(FeedItem::Diff {
                            agent: owner.to_string(),
                            diff,
                        });
                    }
                }
            }
        } else {
            self.push_activity(
                owner,
                "✘",
                format!(
                    "{tool_name}({}) → {}",
                    truncate(input, 70),
                    truncate(output, 110)
                ),
                Some(false),
            );
        }
    }

    fn flush_replay_tally(&mut self, owner: &str, tally: &mut CompactTally) {
        if !tally.is_empty() {
            self.push_activity(owner, "·", tally.receipt(), None);
            *tally = CompactTally::default();
        }
    }

    fn splash(&mut self) {
        self.feed.push(FeedItem::Notice(
            "Phoenix — your whole-computer agent inbox.".to_string(),
        ));
        let cwd = std::env::current_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "?".to_string());
        self.feed.push(FeedItem::Notice(format!(
            "permissions: {} ({}) · workspace: {}",
            self.state.permission_label(),
            self.state.permission_detail(),
            cwd
        )));
        let newline_hint = if self.enhanced_keys {
            "Shift+Enter for a new line"
        } else {
            // This terminal doesn't report Shift+Enter (no kitty keyboard
            // protocol) — it arrives as plain Enter and would submit.
            "Alt+Enter for a new line"
        };
        self.feed.push(FeedItem::Notice(format!(
            "Type a request and press Enter — {newline_hint}. /help for commands, /quit to exit."
        )));
        self.feed.push(FeedItem::Blank);
    }

    fn turn_running(&self) -> bool {
        self.turn_started.is_some()
    }

    /// Approximate context usage from the durable session transcript on disk
    /// (~4 chars per token). Honest label: the gauge says "approx".
    fn refresh_ctx_estimate(&mut self) {
        let path = session_file(&self.state.session_id);
        self.ctx_tokens_estimate = session_file_len(&path).map(|len| len / 4).unwrap_or(0);
    }

    /// Sub-agent working steps obey the `/subagents` toggle; the orchestrator
    /// row always shows. Spawn/return events are exempt — they render
    /// regardless, so the user always sees agents come and go.
    fn subagent_row_visible(&self, agent: &str) -> bool {
        self.state.show_subagent_tools || matches!(agent_key(agent), "orchestrator")
    }

    fn push_activity(&mut self, agent: &str, symbol: &'static str, text: String, ok: Option<bool>) {
        self.feed.push(FeedItem::Activity {
            agent: agent.to_string(),
            symbol,
            text,
            ok,
        });
    }

    fn toggle_view(&mut self, target: &str) {
        let (label, value) = match target {
            "actions" => {
                self.state.show_actions = !self.state.show_actions;
                ("tool activity rows", self.state.show_actions)
            }
            "thinking" => {
                self.state.show_reasoning = !self.state.show_reasoning;
                ("thinking rows", self.state.show_reasoning)
            }
            "debug" => {
                self.state.show_debug = !self.state.show_debug;
                ("debug rows", self.state.show_debug)
            }
            "subagents" => {
                self.state.show_subagent_tools = !self.state.show_subagent_tools;
                ("sub-agent working steps", self.state.show_subagent_tools)
            }
            _ => return,
        };
        self.feed.push(FeedItem::Notice(format!(
            "{label}: {}",
            if value { "on" } else { "off" }
        )));
    }

    fn set_mood(&mut self, raw_agent: &str, mood: AgentMood) {
        if let Some(chip) = self.agents.get_mut(agent_key(raw_agent)) {
            chip.mood = mood;
        }
    }

    fn all_idle(&mut self) {
        for chip in self.agents.values_mut() {
            chip.mood = AgentMood::Idle;
        }
    }
}

mod commands;
mod events;
mod feedstore;
mod input;
mod render;
mod support;

use support::*;

struct TurnRequest {
    session_id: String,
    user_request: String,
    yolo: bool,
}

// ── Event loop ────────────────────────────────────────────────────────

/// Blocking thread that forwards crossterm input into the app channel.
fn spawn_input_reader(tx: mpsc::Sender<AppEvent>) {
    std::thread::spawn(move || {
        loop {
            match crossterm::event::read() {
                Ok(Event::Key(key)) => {
                    // With the kitty protocol pushed, a terminal may report key
                    // RELEASE events too — acting on both press and release would
                    // double every keystroke. Presses (and repeats) only.
                    if key.kind == KeyEventKind::Release {
                        continue;
                    }
                    if tx.blocking_send(AppEvent::Key(key)).is_err() {
                        break;
                    }
                }
                Ok(Event::Mouse(mouse)) => {
                    let delta = match mouse.kind {
                        MouseEventKind::ScrollUp => 3,
                        MouseEventKind::ScrollDown => -3,
                        _ => 0,
                    };
                    if delta != 0 && tx.blocking_send(AppEvent::Scroll(delta)).is_err() {
                        break;
                    }
                }
                Ok(Event::Resize(_, _)) => {
                    if tx.blocking_send(AppEvent::Resize).is_err() {
                        break;
                    }
                }
                Ok(Event::Paste(text)) => {
                    if tx.blocking_send(AppEvent::Paste(text)).is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });
}

/// Turn executor task: receives submissions, streams gateway events back.
fn spawn_turn_executor(mut rx: mpsc::Receiver<TurnRequest>, tx: mpsc::Sender<AppEvent>) {
    tokio::spawn(async move {
        while let Some(request) = rx.recv().await {
            // One ensure+retry on connect failure so a down gateway auto-starts
            // without looping forever if spawn itself is broken.
            let mut submit = daemon::submit_turn(
                request.session_id.clone(),
                request.user_request.clone(),
                request.yolo,
            )
            .await;
            if submit.is_err() {
                match daemon::ensure_gateway_running().await {
                    Ok(()) => {
                        submit = daemon::submit_turn(
                            request.session_id,
                            request.user_request,
                            request.yolo,
                        )
                        .await;
                    }
                    Err(_) => {
                        // Fall through to GatewayDown with the original connect error.
                    }
                }
            }
            match submit {
                Ok((mut event_rx, outcome)) => {
                    while let Some(event) = event_rx.recv().await {
                        if tx.send(AppEvent::Turn(event)).await.is_err() {
                            return;
                        }
                    }
                    let finished = match outcome.await {
                        Ok(outcome) => AppEvent::TurnFinished(outcome),
                        Err(join_error) => {
                            AppEvent::TurnFailed(format!("internal: {join_error:#}"))
                        }
                    };
                    if tx.send(finished).await.is_err() {
                        return;
                    }
                }
                Err(_) => {
                    if tx.send(AppEvent::GatewayDown).await.is_err() {
                        return;
                    }
                }
            }
        }
    });
}

/// Hold a live Subscribe stream to the gateway for the current session,
/// forwarding its events into the app loop. Reconnects (with a short pause)
/// when the stream drops — a gateway restart — and re-points immediately when
/// the watched session id changes.
fn spawn_background_subscription(
    mut session_rx: tokio::sync::watch::Receiver<String>,
    tx: mpsc::Sender<AppEvent>,
) {
    tokio::spawn(async move {
        loop {
            let session_id = session_rx.borrow_and_update().clone();
            match daemon::subscribe_events(session_id.clone()).await {
                Ok(mut events) => loop {
                    tokio::select! {
                        event = events.recv() => match event {
                            Some(event) => {
                                if tx.send(AppEvent::Turn(event)).await.is_err() {
                                    return; // app gone
                                }
                            }
                            None => break, // stream dropped — resubscribe
                        },
                        changed = session_rx.changed() => {
                            if changed.is_err() {
                                return; // app gone
                            }
                            break; // session switched — resubscribe
                        }
                    }
                },
                Err(_) => {
                    // Gateway not up (yet). Retry quietly unless the session
                    // switches first.
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                        changed = session_rx.changed() => {
                            if changed.is_err() {
                                return;
                            }
                        }
                    }
                }
            }
        }
    });
}

/// Run the full-screen Phoenix application. Returns when the user quits.
pub async fn run(state: AppState) -> Result<()> {
    enable_raw_mode()?;
    // Shift+Enter-as-newline needs the kitty keyboard protocol: a legacy
    // terminal reports Shift+Enter as plain Enter, which SUBMITS. Probe
    // support (the query needs raw mode, hence after enable) and push the
    // disambiguate flag so modified Enter arrives with its modifiers. On
    // terminals without it, Alt+Enter still works (ESC-prefixed Enter) and
    // the splash hint names the right combo.
    let enhanced_keys = supports_keyboard_enhancement().unwrap_or(false);
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    if enhanced_keys {
        execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal, state, enhanced_keys).await;

    // Teardown in push-reverse order, best-effort: a failed pop must not
    // leave the terminal in raw mode.
    if enhanced_keys {
        let _ = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
    }
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    result
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: AppState,
    enhanced_keys: bool,
) -> Result<()> {
    let (app_tx, mut app_rx) = mpsc::channel::<AppEvent>(512);
    let (turn_tx, turn_rx) = mpsc::channel::<TurnRequest>(8);
    spawn_input_reader(app_tx.clone());
    spawn_turn_executor(turn_rx, app_tx.clone());
    // Standing background-event subscription: spawns/returns/working-steps of
    // detached agents arrive here the moment they happen, turn or no turn.
    // The watch channel re-points the stream when the user switches sessions.
    let (session_watch_tx, session_watch_rx) =
        tokio::sync::watch::channel(state.session_id.clone());
    spawn_background_subscription(session_watch_rx, app_tx.clone());

    // Probe the gateway once at startup so the header dot is honest.
    // If down, auto-start it so the user never has to boot `phoenix` first.
    let mut app = Tui::new(state, enhanced_keys);
    app.gateway_ok = tokio::net::UnixStream::connect(daemon::socket_path())
        .await
        .is_ok();
    if !app.gateway_ok {
        match daemon::ensure_gateway_running().await {
            Ok(()) => {
                app.gateway_ok = true;
                app.feed.push(FeedItem::Notice(
                    "gateway started automatically".to_string(),
                ));
            }
            Err(err) => {
                app.feed.push(FeedItem::Error(format!(
                    "the Phoenix gateway is not running — autostart failed: {err:#}"
                )));
            }
        }
    }

    let mut ticker = tokio::time::interval(Duration::from_millis(80));
    loop {
        terminal.draw(|frame| app.render(frame))?;
        tokio::select! {
            _ = ticker.tick() => {
                app.tick = app.tick.wrapping_add(1);
                // Debounced feed snapshot (~1s): resume shows this exact
                // screen even after a crash or oomd kill.
                if app.tick % 12 == 0 {
                    app.persist_feed_if_dirty();
                }
            }
            event = app_rx.recv() => {
                let Some(event) = event else { break };
                match event {
                    AppEvent::Key(key) => {
                        app.on_key(key, &turn_tx);
                        // /new and /resume change the session — re-point the
                        // background subscription at the new one.
                        if *session_watch_tx.borrow() != app.state.session_id {
                            let _ = session_watch_tx.send(app.state.session_id.clone());
                        }
                    }
                    AppEvent::Paste(text) => app.on_paste(&text),
                    AppEvent::Scroll(delta) => {
                        if delta > 0 {
                            app.scroll_back = app.scroll_back.saturating_add(delta as u16);
                        } else {
                            app.scroll_back = app.scroll_back.saturating_sub((-delta) as u16);
                        }
                    }
                    AppEvent::Resize => {}
                    AppEvent::Turn(event) => app.on_turn_event(event),
                    AppEvent::TurnFinished(outcome) => {
                        app.gateway_ok = true;
                        app.on_turn_finished(outcome);
                        // Turn boundary: sync the snapshot watermark to the
                        // transcript on disk and save the settled feed.
                        app.refresh_feed_watermark();
                        app.persist_feed_now();
                        app.run_next_queued(&turn_tx);
                    }
                    AppEvent::TurnFailed(message) => {
                        app.turn_started = None;
                        let was_cancelling = std::mem::take(&mut app.cancelling);
                        app.all_idle();
                        if !was_cancelling {
                            app.feed.push(FeedItem::Error(message));
                        }
                        app.run_next_queued(&turn_tx);
                    }
                    AppEvent::GatewayDown => {
                        app.gateway_ok = false;
                        app.turn_started = None;
                        app.all_idle();
                        app.feed.push(FeedItem::Error(
                            "the Phoenix gateway is not running — autostart failed; check gateway-autostart.log or run `phoenix`".to_string(),
                        ));
                    }
                }
            }
        }
        if app.should_quit {
            break;
        }
    }

    // Final feed snapshot: the next `phoenix start` restores THIS screen,
    // exactly as it looks right now.
    app.refresh_feed_watermark();
    app.persist_feed_now();

    // Explicit end (any quit path, /quit or Ctrl-C twice): ask the gateway to
    // digest this session into memory NOW — the daemon acks immediately and
    // digests detached, so this costs milliseconds — instead of leaving
    // continuity to the 30-min idle timer. Bounded so a dead gateway can
    // never hold the terminal hostage on exit.
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        super::daemon::request_session_digest(app.state.session_id.clone()),
    )
    .await;

    // Persist input history alongside the classic CLI's history file.
    match super::history_path() {
        Ok(path) => {
            if let Err(error) = persist_history(&path, &app.history) {
                tracing::warn!(
                    path = %path.display(),
                    error = %format_args!("{error:#}"),
                    "TUI history was not saved"
                );
            }
        }
        Err(error) => {
            tracing::warn!(error = %format_args!("{error:#}"), "TUI history path unavailable");
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "../tui_stop_tests.rs"]
mod tui_stop_tests;
