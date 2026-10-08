//! The story lane (plan 015 phase 0) — the daemon-side journal reduction.
//!
//! Every UI face (TUI, canvas, mobile) must tell the SAME calm story: narration
//! interleaved with one aggregated receipt per stretch of work, handoffs,
//! return cards, watcher cards — never a raw per-tool event storm. The TUI
//! pioneered this reduction (the LOCKED display taste in `cli/tui/`); this
//! module lifts its rules to the source so new clients subscribe to a
//! `story` lane over the wire (`WireRequest::SubscribeJournal`) instead of
//! re-implementing (and drifting from) the reduction client-side.
//!
//! The raw `CliEvent` lane stays available (`WireRequest::Subscribe`) for
//! zoomed-in views (a specific agent's live window wants per-tool detail).

use serde::{Deserialize, Serialize};

use crate::runtime::CliEvent;

/// Per-agent counts of successful tool calls since the last narrative beat,
/// flushed as one human receipt line. Shared by the TUI's compact journal and
/// the daemon's story lane — one implementation, zero drift.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CompactTally {
    pub reads: u32,
    pub edits: u32,
    pub commands: u32,
    pub web: u32,
    pub other: u32,
}

impl CompactTally {
    pub fn add(&mut self, tool_name: &str) {
        match tool_category(tool_name) {
            Some("read") => self.reads += 1,
            Some("edit") => self.edits += 1,
            Some("command") => self.commands += 1,
            Some("web") => self.web += 1,
            _ => self.other += 1,
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// "read 3 files · ran 2 commands · 5 web/app calls" — plain verbs,
    /// singular/plural handled, only non-zero parts.
    pub fn receipt(&self) -> String {
        fn part(count: u32, one: &str, many: &str) -> Option<String> {
            match count {
                0 => None,
                1 => Some(one.to_string()),
                n => Some(many.replace("{n}", &n.to_string())),
            }
        }
        let parts: Vec<String> = [
            part(self.reads, "read 1 file", "read {n} files"),
            part(self.edits, "made 1 edit", "made {n} edits"),
            part(self.commands, "ran 1 command", "ran {n} commands"),
            part(self.web, "1 web/app call", "{n} web/app calls"),
            part(self.other, "1 other action", "{n} other actions"),
        ]
        .into_iter()
        .flatten()
        .collect();
        parts.join(" · ")
    }
}

/// Bucket a tool into the compact-mode aggregate categories.
pub fn tool_category(tool_name: &str) -> Option<&'static str> {
    match tool_name {
        "read" | "grep" | "glob" | "list_directory" | "codebase_search" | "index_codebase"
        | "symbol_search" | "file_symbols" | "callers" | "callees" | "impact" | "call_path" => {
            Some("read")
        }
        "write" | "str_replace" => Some("edit"),
        "bash" => Some("command"),
        name if name.starts_with("web_")
            || name.starts_with("browser_")
            || name.starts_with("composio_") =>
        {
            Some("web")
        }
        _ => None,
    }
}

/// A summary of one context item loaded for a turn — part of a `Context`
/// story row showing the user what context the agent is working with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextItemSummary {
    /// The kind of context: "sticky_note", "file", "memory", etc.
    pub kind: String,
    /// A short human-readable label (the file path, the note title, etc.).
    pub label: String,
    /// A brief preview of the content (truncated).
    pub preview: String,
}

/// One calm journal row. The variants ARE the display vocabulary: a client
/// renders each kind its own way (the TUI's feed items, the canvas's cards)
/// but the story — what appears, in what order — is decided here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoryEvent {
    /// A driving prompt (user message echo / goal-heartbeat wake).
    User {
        text: String,
        #[serde(default)]
        turn_id: Option<String>,
        #[serde(default)]
        origin: Option<crate::runtime::TurnOrigin>,
    },
    /// An agent narrating its work ("build running, fixing the parse next").
    Narration { agent: String, text: String },
    /// Plain, user-visible work commentary between tool calls. This is not a
    /// private reasoning trace and clients must not label it as one.
    Commentary { agent: String, text: String },
    /// The agent's current reasoning summary. Transient live status only:
    /// clients show it beside the working indicator while it is current and
    /// never keep it as a transcript row.
    Thinking { agent: String, text: String },
    /// A tool call just STARTED — the client lights a live, shimmering work dot
    /// for it and stops shimmering when the matching `Tool` lands.
    ToolStart {
        agent: String,
        tool: String,
        target: String,
    },
    /// One completed tool call, in order. These are the rows behind a compact
    /// receipt: expand "read 3 files · 2 edits" and you get exactly these, one
    /// per line, ✓ or ✗, in the order they ran.
    Tool {
        agent: String,
        tool: String,
        target: String,
        ok: bool,
        detail: String,
        /// Red/green diff when this tool edited a file.
        diff: Option<String>,
    },
    /// Aggregated work receipt at a narrative beat (the compact summary row).
    Receipt { agent: String, text: String },
    /// The full brief an agent received when work was handed to it — rendered
    /// as the green prompt block in the RECEIVING agent's window.
    Brief { agent: String, text: String },
    /// Live context-window usage for one agent (used, limit) in tokens.
    Usage {
        agent: String,
        used: u32,
        limit: u64,
        #[serde(default)]
        spent: u64,
    },
    /// One visible row in the agent's ordered work wave. `started` is live;
    /// `completed` and `failed` settle the same row and survive replay.
    ContextCompaction {
        agent: String,
        status: String,
        before_tokens: u64,
        after_tokens: u64,
        folded_messages: usize,
        limit: u64,
    },
    /// Work handed to a specialist (background = detached job).
    Handoff {
        from: String,
        to: String,
        subject: String,
        background: bool,
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
    /// Lightweight lifecycle for an anonymous volume worker. It drives the
    /// Environment panel's compact worker chips and is deliberately not a
    /// conversation handoff, return, or durable transcript row.
    SubagentLifecycle {
        batch_id: String,
        worker_id: String,
        label: String,
        item_id: String,
        status: crate::runtime::VolumeWorkerLifecycleStatus,
    },
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
    GroupMemberStatus {
        turn_id: String,
        group_id: String,
        agent_id: String,
        agent_name: String,
        state: String,
        detail: String,
    },
    /// A specialist's result came home — the return card.
    Return {
        agent: String,
        subject: String,
        body: String,
        ok: bool,
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
    /// A watcher ruling / goal milestone — same card chrome as a return.
    Card {
        from: String,
        subject: String,
        body: String,
        ok: bool,
    },
    /// A mid-task message parked for a WORKING agent (a user redirect, an
    /// orchestrator `talk`, a watcher correction). Rendered as the
    /// same inbound card a handoff draws, in the addressed agent's window,
    /// with a live foot until `SteerDelivered` settles it — a steer IS a
    /// handoff into someone already working, and must read like one.
    Steer {
        from: String,
        to: String,
        subject: String,
        body: String,
    },
    /// The addressed agent picked the steer up — settles the live `Steer` card.
    SteerDelivered { to: String, subject: String },
    /// A failure the user must see (failed tool call, failed specialist).
    Failure { agent: String, text: String },
    /// This agent's turn is OVER — stop every live-work animation for it
    /// (shimmer, spinner, work group, thinking box). Emitted for EVERY
    /// specialist completion, success or failure.
    ///
    /// Why this exists: a successful completion used to emit nothing at all
    /// ("the return card already tells it"), and the return card only fires
    /// when the whole chain finishes — and only for the first agent. So a
    /// background specialist that finished, or any chain member whose chain
    /// later died, span forever (live 2026-07-20: "hawk and coder and iris are
    /// done, why are they shown as active, why do they all shimmer"). Settled
    /// is the one signal that always arrives.
    Settled { agent: String, ok: bool },
    /// Context items loaded for a turn (sticky notes, files, memory) —
    /// shows the user what context the agent is working with.
    Context {
        agent: String,
        items: Vec<ContextItemSummary>,
    },
    /// A red/green edit diff the moment it lands — the TUI's verbose-mode
    /// row, needed for the canvas agent terminals to look like the real TUI.
    Diff { agent: String, diff: String },
    /// An agent asked the user (`ask_user`) — the client renders the answer UI
    /// while independent work continues and replies over `AnswerAsk` by `id`.
    /// `questions` is authoritative. `question` and `options` remain for
    /// older canvas clients and already-persisted story rows.
    AskPending {
        id: String,
        agent: String,
        question: String,
        options: Vec<String>,
        #[serde(default)]
        questions: Vec<crate::tools::ask_user::AskUserQuestion>,
        #[serde(default)]
        approval: Option<crate::tools::ask_user::ApprovalRequest>,
    },
    /// The turn's final answer, markdown.
    Answer { markdown: String },
    /// Terminal boundary for one owned execution, after its final candidate.
    /// An error invalidates delivery of that candidate; it never means the
    /// work had no effects. Old successful boundaries retain their JSON shape.
    ExecutionEnded {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// A cross-session answer notification, broadcast to every session box so
    /// a different conversation can show an owner-aware completion card.
    CrossAnswer {
        session_id: String,
        #[serde(default)]
        owner_kind: Option<String>,
        #[serde(default)]
        owner_id: Option<String>,
        project_name: String,
        summary: String,
    },
    /// A gateway-level notice (compaction, queueing, alarms).
    Notice { text: String },
}

/// Reduces the raw `CliEvent` stream into `StoryEvent` rows, one subscriber
/// at a time (per-connection state: the tallies are the reduction's memory).
#[derive(Default)]
pub struct StoryReducer {
    tallies: std::collections::BTreeMap<String, CompactTally>,
}

/// Overlapping executions never share tool tallies or completion flushes,
/// even when they use the same coworker label in one conversation.
#[derive(Default)]
pub struct OwnedStoryReducer {
    legacy: StoryReducer,
    tasks: std::collections::HashMap<super::postbox::ExecutionScope, StoryReducer>,
}

impl OwnedStoryReducer {
    pub fn push(&mut self, entry: &super::postbox::JournalEvent) -> Vec<StoryEvent> {
        let Some(scope) = &entry.scope else { return self.legacy.push(&entry.event) };
        let mut rows = self.tasks.entry(scope.clone()).or_default().push(&entry.event);
        match &entry.event {
            CliEvent::Done => {
                self.tasks.remove(scope);
                rows.push(StoryEvent::ExecutionEnded { error: None });
            }
            CliEvent::TerminalFailure { message } => {
                self.tasks.remove(scope);
                rows.push(StoryEvent::ExecutionEnded { error: Some(message.clone()) });
            }
            _ => {}
        }
        rows
    }
}

impl StoryReducer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Consume one raw event; return the journal rows it produces (usually
    /// zero or one — beats that flush receipts return several).
    pub fn push(&mut self, event: &CliEvent) -> Vec<StoryEvent> {
        let mut rows = Vec::new();
        match event {
            CliEvent::ToolCallStarted {
                agent,
                tool_name,
                input_summary,
            } => {
                // Lights the live work dot in that agent's window immediately —
                // the user sees work START, not just its aggregate afterwards.
                rows.push(StoryEvent::ToolStart {
                    agent: agent.clone(),
                    tool: tool_name.clone(),
                    target: tool_target(tool_name, input_summary),
                });
            }
            CliEvent::ContextUsage { agent, used, limit, spent } => {
                rows.push(StoryEvent::Usage {
                    agent: agent.clone(),
                    used: *used,
                    limit: *limit,
                    spent: *spent,
                });
            }
            CliEvent::ContextCompaction {
                agent,
                status,
                before_tokens,
                after_tokens,
                folded_messages,
                limit,
            } => {
                rows.push(StoryEvent::ContextCompaction {
                    agent: agent.clone(),
                    status: status.clone(),
                    before_tokens: *before_tokens,
                    after_tokens: *after_tokens,
                    folded_messages: *folded_messages,
                    limit: *limit,
                });
            }
            CliEvent::ToolCallCompleted {
                agent,
                tool_name,
                input_summary,
                success,
                output_summary,
                diff,
            } => {
                let detailed_receipts = crate::settings::effective_bool(
                    "advanced.tool_receipts",
                    &crate::settings::SettingsScope::Global,
                )
                .unwrap_or(true);
                // EVERY tool call is journaled in order. The compact receipt row
                // stays the default view; these are what it expands into.
                rows.push(StoryEvent::Tool {
                    agent: agent.clone(),
                    tool: tool_name.clone(),
                    target: tool_target(tool_name, input_summary),
                    ok: *success,
                    detail: detailed_receipts
                        .then(|| truncate(output_summary, 400))
                        .unwrap_or_default(),
                    // Diffs keep their newlines: flattened to one line they
                    // counted +0 −0 in Review and rendered as one squashed row.
                    diff: detailed_receipts
                        .then(|| diff.as_ref().map(|d| truncate_multiline(d, 8000)))
                        .flatten(),
                });
                if *success {
                    // NOTE: the diff rides on the Tool row above — it is no
                    // longer pushed a second time as a standalone Diff event.
                    // The client shows it when the receipt row is expanded.
                    //
                    // Successes journal, never log: they accumulate and land
                    // as one receipt at the next narrative beat.
                    let tally = self.tallies.entry(agent.clone()).or_default();
                    tally.add(tool_name);
                    // A quiet worker (a model that narrates rarely) must not
                    // leave its window BLANK between beats — the 2026-07-10
                    // canvas run showed specialists as "nothing but errors"
                    // because only failures surfaced. Every 8 calls without a
                    // beat, the tally lands as an interim receipt row.
                    let total =
                        tally.reads + tally.edits + tally.commands + tally.web + tally.other;
                    if total >= 8 {
                        let text = tally.receipt();
                        *tally = CompactTally::default();
                        rows.push(StoryEvent::Receipt {
                            agent: agent.clone(),
                            text,
                        });
                    }
                }
                // A failed call is already a red, humanized Tool row. Emitting
                // a second full Failure card duplicated the same error and made
                // ordinary recoverable misses look like two fatal incidents.
                // Specialist/turn failures still use StoryEvent::Failure below.
            }
            // Older runners emitted this bare event. Preserve wire/history
            // compatibility, but present it as user-visible commentary rather
            // than falsely labeling a provider summary as chain-of-thought.
            CliEvent::Reasoning(text) => {
                if !is_plumbing_reasoning(text) && !is_compaction_text(text) {
                    rows.push(StoryEvent::Thinking {
                        agent: "orchestrator".to_string(),
                        text: truncate(text, REASONING_CAP),
                    });
                }
            }
            // Reasoning summaries are live status, not conversation. They do
            // not flush the receipt tally: they are not a narrative beat.
            CliEvent::AgentThinking { agent, text } => {
                if !is_plumbing_reasoning(text) && !is_compaction_text(text) {
                    rows.push(StoryEvent::Thinking {
                        agent: agent.clone(),
                        text: truncate(text, REASONING_CAP),
                    });
                }
            }
            // What the agent says to the user between tool calls lands in ITS
            // OWN window as a message row.
            CliEvent::AgentMessage { agent, text } => {
                self.flush_into(&mut rows);
                if !is_plumbing_reasoning(text) && !is_compaction_text(text) {
                    rows.push(StoryEvent::Commentary {
                        agent: agent.clone(),
                        text: truncate(text, REASONING_CAP),
                    });
                }
            }
            // The assembled user prompt IS the brief the agent was handed —
            // the receiving window shows it as the green block.
            CliEvent::PromptAssembled {
                agent, user_prompt, ..
            } => {
                if !user_prompt.trim().is_empty() && !is_compaction_text(user_prompt) {
                    rows.push(StoryEvent::Brief {
                        agent: agent.clone(),
                        text: truncate(user_prompt, 4000),
                    });
                }
            }
            CliEvent::SpecialistOutput { agent, summary } => {
                if !is_compaction_text(summary) {
                    rows.push(StoryEvent::Narration {
                        agent: agent.clone(),
                        text: truncate(summary, 200),
                    });
                }
            }
            CliEvent::SpecialistDelegated { agent, subject } => {
                self.flush_into(&mut rows);
                rows.push(StoryEvent::Handoff {
                    from: "orchestrator".to_string(),
                    to: agent.clone(),
                    subject: truncate(subject, 120),
                    background: false,
                    handoff_id: String::new(),
                    requester: "orchestrator".to_string(),
                    receiver: agent.clone(),
                    status: "queued".to_string(),
                    causation_id: None,
                });
            }
            // Volume workers are deliberately represented by compact live
            // chips in the Environment surface, not as a sequence of durable
            // subagent handoff rows in the conversation story.
            CliEvent::VolumeWorkerLifecycle {
                batch_id,
                worker_id,
                label,
                item_id,
                status,
            } => rows.push(StoryEvent::SubagentLifecycle {
                batch_id: batch_id.clone(),
                worker_id: worker_id.clone(),
                label: label.clone(),
                item_id: item_id.clone(),
                status: *status,
            }),
            CliEvent::AgentHandoff {
                handoff_id,
                from,
                to,
                subject,
                background,
                requester,
                receiver,
                status,
                causation_id,
                body,
                reply_to,
            } => {
                self.flush_into(&mut rows);
                let requester = if requester.is_empty() {
                    from.clone()
                } else {
                    requester.clone()
                };
                let receiver = if receiver.is_empty() {
                    to.clone()
                } else {
                    receiver.clone()
                };
                let state = if status.is_empty() {
                    "queued".to_string()
                } else {
                    status.clone()
                };
                if let Some(body) = body.as_ref().filter(|body| !body.trim().is_empty()) {
                    rows.push(StoryEvent::Return {
                        agent: receiver.clone(),
                        subject: subject.clone(),
                        body: body.clone(),
                        ok: !matches!(state.as_str(), "blocked" | "failed" | "error"),
                        handoff_id: handoff_id.clone(),
                        requester,
                        receiver,
                        status: state,
                        reply_to: reply_to.clone(),
                        causation_id: causation_id.clone(),
                    });
                } else {
                    rows.push(StoryEvent::Handoff {
                        from: from.clone(),
                        to: to.clone(),
                        subject: truncate(subject, 120),
                        background: *background,
                        handoff_id: handoff_id.clone(),
                        requester,
                        receiver,
                        status: state,
                        causation_id: causation_id.clone(),
                    });
                }
            }
            CliEvent::GroupMessage {
                message_id,
                group_id,
                round,
                agent_id,
                agent_name,
                markdown,
                reply_to,
                causation_id,
            } => {
                self.flush_into(&mut rows);
                rows.push(StoryEvent::GroupMessage {
                    message_id: message_id.clone(),
                    group_id: group_id.clone(),
                    round: *round,
                    agent_id: agent_id.clone(),
                    agent_name: agent_name.clone(),
                    // Group replies are rendered immediately as Markdown.
                    // The generic `truncate` helper intentionally flattens
                    // newlines for one-line activity labels; using it here
                    // destroyed headings, lists, and code fences until the
                    // later canonical-history reload replaced the live row.
                    markdown: truncate_multiline(markdown, 16_000),
                    reply_to: reply_to.clone(),
                    causation_id: causation_id.clone(),
                });
            }
            CliEvent::GroupMemberStatus {
                turn_id,
                group_id,
                agent_id,
                agent_name,
                state,
                detail,
            } => {
                self.flush_into(&mut rows);
                rows.push(StoryEvent::GroupMemberStatus {
                    turn_id: turn_id.clone(),
                    group_id: group_id.clone(),
                    agent_id: agent_id.clone(),
                    agent_name: agent_name.clone(),
                    state: state.clone(),
                    detail: truncate_multiline(detail, 500),
                });
            }
            CliEvent::SpecialistQueued {
                agent,
                subject,
                status,
            } => {
                self.flush_into(&mut rows);
                rows.push(StoryEvent::Handoff {
                    from: "orchestrator".to_string(),
                    to: agent.clone(),
                    subject: truncate(subject, 120),
                    background: status.contains("background handoff"),
                    handoff_id: String::new(),
                    requester: "orchestrator".to_string(),
                    receiver: agent.clone(),
                    status: status.clone(),
                    causation_id: None,
                });
            }
            CliEvent::BackgroundAgentSpawned {
                agent,
                subject,
                handoff_id,
                requester,
                receiver,
                status,
                causation_id,
            } => {
                self.flush_into(&mut rows);
                rows.push(StoryEvent::Handoff {
                    from: "orchestrator".to_string(),
                    to: agent.clone(),
                    subject: truncate(subject, 120),
                    background: true,
                    handoff_id: handoff_id.clone(),
                    requester: if requester.is_empty() {
                        "orchestrator".to_string()
                    } else {
                        requester.clone()
                    },
                    receiver: if receiver.is_empty() {
                        agent.clone()
                    } else {
                        receiver.clone()
                    },
                    status: if status.is_empty() {
                        "queued".to_string()
                    } else {
                        status.clone()
                    },
                    causation_id: causation_id.clone(),
                });
            }
            CliEvent::BackgroundAgentReturned {
                agent,
                subject,
                ok,
                body,
                handoff_id,
                requester,
                receiver,
                status,
                reply_to,
                causation_id,
                ..
            } => {
                self.flush_into(&mut rows);
                // The job is home — its live tally retires with it.
                self.tallies.remove(agent);
                rows.push(StoryEvent::Return {
                    agent: agent.clone(),
                    subject: subject.clone(),
                    body: body.clone(),
                    ok: *ok,
                    handoff_id: handoff_id.clone(),
                    requester: if requester.is_empty() {
                        "orchestrator".to_string()
                    } else {
                        requester.clone()
                    },
                    receiver: if receiver.is_empty() {
                        agent.clone()
                    } else {
                        receiver.clone()
                    },
                    status: if status.is_empty() {
                        if *ok { "done" } else { "blocked" }.to_string()
                    } else {
                        status.clone()
                    },
                    reply_to: reply_to.clone(),
                    causation_id: causation_id.clone(),
                });
            }
            CliEvent::SpecialistCompleted { agent, ok, .. } => {
                self.flush_into(&mut rows);
                self.tallies.remove(agent);
                // Completion only settles this agent's live stage. Its actual
                // return (including a bounded failure) is the single canonical
                // user-visible result, so do not duplicate it here.
                rows.push(StoryEvent::Settled {
                    agent: agent.clone(),
                    ok: *ok,
                });
            }
            CliEvent::WatcherCard {
                from,
                subject,
                body,
                ok,
            } => {
                self.flush_into(&mut rows);
                rows.push(StoryEvent::Card {
                    from: from.clone(),
                    subject: subject.clone(),
                    body: body.clone(),
                    ok: *ok,
                });
            }
            CliEvent::SteerQueued {
                from,
                to,
                subject,
                body,
            } => {
                self.flush_into(&mut rows);
                rows.push(StoryEvent::Steer {
                    from: from.clone(),
                    to: to.clone(),
                    subject: subject.clone(),
                    body: truncate(body, 4000),
                });
            }
            CliEvent::SteerDelivered { to, subject } => {
                rows.push(StoryEvent::SteerDelivered {
                    to: to.clone(),
                    subject: subject.clone(),
                });
            }
            CliEvent::WakeTurn {
                prompt,
                turn_id,
                origin,
            } => {
                self.flush_into(&mut rows);
                if !is_compaction_text(prompt) {
                    rows.push(StoryEvent::User {
                        text: truncate(prompt, 600),
                        turn_id: turn_id.clone(),
                        origin: origin.clone(),
                    });
                }
            }
            CliEvent::AskUser {
                id,
                agent,
                questions,
                approval,
            } => {
                self.flush_into(&mut rows);
                rows.push(StoryEvent::AskPending {
                    id: id.clone(),
                    agent: agent.clone(),
                    question: questions
                        .first()
                        .map(|q| q.question.clone())
                        .unwrap_or_default(),
                    options: questions
                        .first()
                        .map(|q| q.options.clone())
                        .unwrap_or_default(),
                    questions: questions.clone(),
                    approval: approval.clone(),
                });
            }
            CliEvent::GatewayNotice(text) => {
                if text.starts_with('⇄') || text.starts_with('⏹') {
                    // Chain hop/stop — a journal beat like any handoff.
                    self.flush_into(&mut rows);
                }
                if let Some(text) = visible_gateway_notice(text) {
                    rows.push(StoryEvent::Notice { text });
                }
            }
            CliEvent::TerminalFailure { message } => {
                self.flush_into(&mut rows);
                rows.push(StoryEvent::Failure {
                    agent: "system".into(), text: message.clone(),
                });
            }
            CliEvent::FinalOutput(markdown) => {
                self.flush_into(&mut rows);
                if !is_compaction_text(markdown) {
                    rows.push(StoryEvent::Answer {
                        markdown: markdown.clone(),
                    });
                }
            }
            // Cross-session answer signal — pass through directly so an open
            // different conversation can show one ephemeral toast under the
            // owner's editable display label. The desktop never journals it.
            CliEvent::CrossAnswer {
                session_id,
                owner_kind,
                owner_id,
                project_name,
                summary,
            } => {
                rows.push(StoryEvent::CrossAnswer {
                    session_id: session_id.clone(),
                    owner_kind: owner_kind.clone(),
                    owner_id: owner_id.clone(),
                    project_name: project_name.clone(),
                    summary: summary.clone(),
                });
            }
            CliEvent::Done => {
                self.flush_into(&mut rows);
            }
            // Internal barrier used by bounded one-shot settlement. It carries
            // no user-facing story content.
            CliEvent::BackgroundResultsAbsorbed { .. } => {}
            // Live plumbing (stream deltas, tool starts, moods, routing,
            // permissions, prompt snapshots) never reaches the story lane —
            // clients that want it subscribe to the raw lane.
            _ => {}
        }
        rows.retain(|row| !story_exposes_compaction(row));
        rows
    }

    /// Turn every agent's accumulated tally into one receipt row — called at
    /// narrative beats so receipts land exactly where the story pauses.
    fn flush_into(&mut self, rows: &mut Vec<StoryEvent>) {
        let tallies = std::mem::take(&mut self.tallies);
        for (agent, tally) in tallies {
            if !tally.is_empty() {
                rows.push(StoryEvent::Receipt {
                    agent,
                    text: tally.receipt(),
                });
            }
        }
    }
}

/// The narration brief: first sentence, capped, placeholder-free — the same
/// rule the TUI applies before drawing a `∴` row.
/// Provider mechanics masquerading as thought — never worth a reasoning row.
/// How much of a reasoning part reaches the windows and Mission Control.
/// Reasoning is a LIVE surface — the thinking box and the console's thought
/// row both REPLACE their content per part rather than accumulating — so a
/// generous cap costs one round's text, not a growing transcript. The old
/// 1200 clipped the middle out of ordinary extended-thinking blocks, which is
/// what "I want to see ALL reasoning" was about.
const REASONING_CAP: usize = 4000;

fn is_plumbing_reasoning(text: &str) -> bool {
    text.trim()
        .starts_with("Provider requested native tool call")
}

/// Gateway notices feed terminal diagnostics too, but the conversation is a
/// product surface—not a daemon log. Queue ids, trace filenames, checkpoint
/// handles, and wake mechanics belong in logs or dedicated controls. Keep
/// only notices that help the user act, and phrase queue failures without
/// exposing their storage key or internal error chain.
fn visible_gateway_notice(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();

    if lower.starts_with("queued prompt") || lower.starts_with("queued group turn") {
        return (lower.contains("failed")
            || lower.contains("could not")
            || lower.contains("stopped"))
        .then(|| {
            "A queued message could not run. Open the queue above the composer to review or remove it."
                .to_string()
        });
    }

    if lower.starts_with("queued message ready")
        || lower.starts_with("late popup answer queued")
        || lower.starts_with("provider-native context")
        || lower.starts_with("context auto-compacted")
        || lower.starts_with("context overflow recovered")
        || lower.starts_with("context overflow could not commit")
        || lower.starts_with("checkpoint ")
        || lower.starts_with("memory lookup is taking longer")
        || lower.starts_with("talk →")
        || lower == "after barrier"
        || lower.contains(" · trace ")
    {
        return None;
    }

    Some(trimmed.to_string())
}

/// Continuation folds are model-only. The marker stays in the session for
/// the next prompt; it is never a journal or canvas row.
fn is_compaction_text(text: &str) -> bool {
    text.contains("[AUTO-COMPACTED HISTORY")
}

fn story_exposes_compaction(row: &StoryEvent) -> bool {
    match row {
        StoryEvent::User { text, .. }
        | StoryEvent::Narration { text, .. }
        | StoryEvent::Commentary { text, .. }
        | StoryEvent::Receipt { text, .. }
        | StoryEvent::Brief { text, .. }
        | StoryEvent::Failure { text, .. }
        | StoryEvent::Notice { text } => is_compaction_text(text),
        StoryEvent::Answer { markdown } | StoryEvent::GroupMessage { markdown, .. } => {
            is_compaction_text(markdown)
        }
        StoryEvent::Return { body, .. }
        | StoryEvent::Card { body, .. }
        | StoryEvent::Steer { body, .. } => is_compaction_text(body),
        StoryEvent::CrossAnswer { summary, .. } => is_compaction_text(summary),
        StoryEvent::Context { items, .. } => items
            .iter()
            .any(|item| is_compaction_text(&item.preview) || is_compaction_text(&item.label)),
        StoryEvent::SubagentLifecycle { .. } => false,
        _ => false,
    }
}

fn narration_brief(text: &str) -> Option<String> {
    if text.starts_with("Provider requested native tool call") {
        return None;
    }
    let clean = text.replace('\n', " ").trim().to_string();
    let end = clean.find(". ").map(|i| i + 1).unwrap_or(clean.len());
    let brief = truncate(&clean[..end], 400);
    (!brief.is_empty()).then_some(brief)
}

/// Preserve the runtime's actual input summary. Canvas shows the exact tool
/// name and these arguments; guessing a single "interesting" word made calls
/// such as ask_user appear to contain unrelated prior work.
fn tool_target(_tool: &str, input_summary: &str) -> String {
    let raw = input_summary.trim();
    if raw.is_empty() {
        return String::new();
    }
    truncate(raw, 500)
}

fn truncate(text: &str, max: usize) -> String {
    let clean = text.replace('\n', " ");
    if clean.chars().count() <= max {
        clean
    } else {
        let cut: String = clean.chars().take(max).collect();
        format!("{cut}…")
    }
}

fn truncate_multiline(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max).collect();
        format!("{cut}\n…(truncated)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ask_user::AskUserQuestion;

    fn completed(agent: &str, tool: &str, n: u32) -> CliEvent {
        CliEvent::ToolCallCompleted {
            agent: agent.to_string(),
            tool_name: tool.to_string(),
            input_summary: format!("{tool}-{n}"),
            success: true,
            output_summary: "ok".to_string(),
            diff: None,
        }
    }

    #[test]
    fn ask_story_preserves_every_question_for_the_canvas_carousel() {
        let mut reducer = StoryReducer::new();
        let questions = vec![
            AskUserQuestion {
                question: "Which account should Nico use?".to_string(),
                header: Some("Account".to_string()),
                options: vec!["Work".to_string(), "Personal".to_string()],
                multi_select: false,
            },
            AskUserQuestion {
                question: "Who may reuse the login?".to_string(),
                header: Some("Scope".to_string()),
                options: vec!["Only Nico".to_string(), "Whole company".to_string()],
                multi_select: false,
            },
        ];

        let rows = reducer.push(&CliEvent::AskUser {
            id: "ask-12".to_string(),
            agent: "nico".to_string(),
            questions: questions.clone(),
            approval: None,
        });

        match rows.as_slice() {
            [StoryEvent::AskPending {
                question,
                options,
                questions: retained,
                ..
            }] => {
                assert_eq!(question, &questions[0].question);
                assert_eq!(options, &questions[0].options);
                assert_eq!(retained.len(), 2);
                assert_eq!(retained[1].header.as_deref(), Some("Scope"));
            }
            other => panic!("expected one ask row, got {other:?}"),
        }
    }

    #[test]
    fn receipts_flush_at_narrative_beats_not_before() {
        let mut reducer = StoryReducer::new();
        // Every completion journals its own Tool row (the expandable detail) —
        // but the compact Receipt waits for the narrative beat.
        for (n, tool) in [(1, "read"), (2, "read"), (3, "bash")] {
            let rows = reducer.push(&completed("coder", tool, n));
            assert_eq!(rows.len(), 1, "one Tool row per completion");
            assert!(matches!(&rows[0], StoryEvent::Tool { ok: true, .. }));
        }
        // The narrative beat lands the receipt FIRST, then the commentary row
        // (full text — no duplicate narration brief since 2026-07-13).
        // Reasoning is live status only: no transcript row, no receipt flush.
        let thinking = reducer.push(&CliEvent::AgentThinking {
            agent: "coder".to_string(),
            text: "**Planning the config edit**".to_string(),
        });
        assert!(matches!(thinking.as_slice(), [StoryEvent::Thinking { agent, .. }] if agent == "coder"));
        let rows = reducer.push(&CliEvent::AgentMessage {
            agent: "coder".to_string(),
            text: "Tests pass. Now the config file.".to_string(),
        });
        assert_eq!(rows.len(), 2);
        match &rows[0] {
            StoryEvent::Receipt { agent, text } => {
                assert_eq!(agent, "coder");
                assert!(text.contains("read 2 files") && text.contains("ran 1 command"));
            }
            other => panic!("expected receipt first, got {other:?}"),
        }
        match &rows[1] {
            StoryEvent::Commentary { text, .. } => {
                assert_eq!(text, "Tests pass. Now the config file.")
            }
            other => panic!("expected commentary, got {other:?}"),
        }
    }

    #[test]
    fn specialist_to_specialist_handoff_keeps_both_endpoints() {
        let mut reducer = StoryReducer::new();
        let rows = reducer.push(&CliEvent::AgentHandoff {
            handoff_id: "message_plan_review".to_string(),
            from: "Compass".to_string(),
            to: "Hawk".to_string(),
            subject: "review the completed plan".to_string(),
            background: true,
            requester: "planner".to_string(),
            receiver: "critic".to_string(),
            status: "queued".to_string(),
            causation_id: Some("message_parent".to_string()),
            body: None,
            reply_to: None,
        });
        assert!(matches!(
            rows.as_slice(),
            [StoryEvent::Handoff { from, to, subject, background: true, handoff_id, .. }]
                if from == "Compass" && to == "Hawk" && subject == "review the completed plan"
                    && handoff_id == "message_plan_review"
        ));
    }

    #[test]
    fn completed_peer_handoff_becomes_one_correlated_return() {
        let mut reducer = StoryReducer::new();
        let rows = reducer.push(&CliEvent::AgentHandoff {
            handoff_id: "message_figma".to_string(),
            from: "Theo".to_string(),
            to: "Nico".to_string(),
            subject: "Figma connection report".to_string(),
            background: false,
            requester: "researcher".to_string(),
            receiver: "scribe".to_string(),
            status: "done".to_string(),
            causation_id: Some("turn_figma".to_string()),
            body: Some("The report is ready.".to_string()),
            reply_to: Some("message_figma".to_string()),
        });
        assert!(matches!(
            rows.as_slice(),
            [StoryEvent::Return { agent, body, handoff_id, reply_to: Some(reply_to), .. }]
                if agent == "scribe" && body == "The report is ready."
                    && handoff_id == "message_figma" && reply_to == "message_figma"
        ));
    }

    /// Every specialist completion must settle the agent, success or failure.
    /// Regression for 2026-07-20 ("hawk and coder and iris are done, why are
    /// they shown as active, why do they all shimmer"): a successful
    /// completion emitted NOTHING, and the return card that would have cleared
    /// it only fires when a whole chain finishes — so a finished background
    /// specialist span forever.
    #[test]
    fn every_specialist_completion_settles_the_agent() {
        let mut reducer = StoryReducer::new();
        let rows = reducer.push(&CliEvent::SpecialistCompleted {
            agent: "coder".to_string(),
            ok: true,
            summary: "done".to_string(),
        });
        let settled: Vec<_> = rows
            .iter()
            .filter_map(|r| match r {
                StoryEvent::Settled { agent, ok } => Some((agent.as_str(), *ok)),
                _ => None,
            })
            .collect();
        assert_eq!(
            settled,
            vec![("coder", true)],
            "success must settle: {rows:?}"
        );
        // Success stays quiet otherwise — the return card tells that story.
        assert!(!rows.iter().any(|r| matches!(r, StoryEvent::Failure { .. })));

        // A failure settles too. The paired return owns the one visible error.
        let mut reducer = StoryReducer::new();
        let rows = reducer.push(&CliEvent::SpecialistCompleted {
            agent: "critic".to_string(),
            ok: false,
            summary: "accounts exhausted".to_string(),
        });
        assert!(
            rows.iter().any(
                |r| matches!(r, StoryEvent::Settled { agent, ok: false } if agent == "critic")
            ),
            "failure must settle: {rows:?}"
        );
        assert!(!rows.iter().any(|r| matches!(r, StoryEvent::Failure { .. })));
    }

    #[test]
    fn edit_diffs_keep_their_line_breaks() {
        let mut reducer = StoryReducer::new();
        let diff = "@@ notes.md\n- old line\n+ new line".to_string();
        let rows = reducer.push(&CliEvent::ToolCallCompleted {
            agent: "coder".to_string(),
            tool_name: "str_replace".to_string(),
            input_summary: "replace in notes.md".to_string(),
            success: true,
            output_summary: "ok".to_string(),
            diff: Some(diff.clone()),
        });
        let carried = rows.iter().find_map(|row| match row { StoryEvent::Tool { diff, .. } => diff.clone(), _ => None });
        assert_eq!(carried.as_deref(), Some(diff.as_str()), "Review counts +/- per line");
    }

    #[test]
    fn failed_tools_surface_once_without_flushing() {
        let mut reducer = StoryReducer::new();
        reducer.push(&completed("coder", "read", 1));
        let rows = reducer.push(&CliEvent::ToolCallCompleted {
            agent: "coder".to_string(),
            tool_name: "bash".to_string(),
            input_summary: "cargo test".to_string(),
            success: false,
            output_summary: "2 tests failed".to_string(),
            diff: None,
        });
        // Its own failed Tool row is the single visible error. The success
        // tally keeps accumulating and lands at the next narrative beat.
        assert_eq!(rows.len(), 1, "one compact failed tool row");
        assert!(matches!(&rows[0], StoryEvent::Tool { ok: false, .. }));
        // The pending read still lands at the next beat.
        let rows = reducer.push(&CliEvent::Done);
        assert!(matches!(&rows[0], StoryEvent::Receipt { text, .. } if text == "read 1 file"));
    }

    #[test]
    fn returns_flush_clear_the_tally_and_card_home() {
        let mut reducer = StoryReducer::new();
        reducer.push(&completed("browser", "browser_goto", 1));
        let rows = reducer.push(&CliEvent::BackgroundAgentReturned {
            agent: "browser".to_string(),
            subject: "checked the page".to_string(),
            ok: true,
            summary: "done".to_string(),
            body: "all good".to_string(),
            handoff_id: "return_page_check".to_string(),
            requester: "orchestrator".to_string(),
            receiver: "browser".to_string(),
            status: "done".to_string(),
            reply_to: Some("return_page_check".to_string()),
            causation_id: Some("message_page_check".to_string()),
        });
        assert!(matches!(&rows[0], StoryEvent::Receipt { text, .. } if text == "1 web/app call"));
        assert!(matches!(&rows[1], StoryEvent::Return { ok: true, .. }));
        // Tally retired with the job — nothing more to flush.
        assert!(reducer.push(&CliEvent::Done).is_empty());
    }

    #[test]
    fn watcher_cards_and_wakes_are_journal_beats() {
        let mut reducer = StoryReducer::new();
        reducer.push(&completed("coder", "write", 1));
        let rows = reducer.push(&CliEvent::WatcherCard {
            from: "Judge (enforcement)".to_string(),
            subject: "MID-TASK MESSAGE — watcher correction".to_string(),
            body: "stuck in a loop".to_string(),
            ok: true,
        });
        assert!(matches!(&rows[0], StoryEvent::Receipt { .. }));
        assert!(matches!(&rows[1], StoryEvent::Card { from, .. } if from.starts_with("Judge")));
        let rows = reducer.push(&CliEvent::WakeTurn {
            prompt: "GOAL HEARTBEAT (3 wakes left): ship it".to_string(),
            turn_id: None,
            origin: None,
        });
        assert!(
            matches!(&rows[0], StoryEvent::User { text, .. } if text.starts_with("GOAL HEARTBEAT"))
        );
    }

    #[test]
    fn routine_wake_keeps_its_typed_occurrence_identity() {
        let mut reducer = StoryReducer::new();
        let origin = crate::runtime::TurnOrigin::Routine {
            routine_id: "morning-check".to_string(),
            scheduled_for: "2026-08-26T11:00:00.000Z".to_string(),
            schedule: "daily 05:00".to_string(),
        };
        let rows = reducer.push(&CliEvent::WakeTurn {
            prompt: "Run the morning check".to_string(),
            turn_id: Some("routine:morning-check:1787742000000".to_string()),
            origin: Some(origin.clone()),
        });
        assert!(matches!(
            &rows[0],
            StoryEvent::User { turn_id: Some(turn_id), origin: Some(actual), .. }
                if turn_id == "routine:morning-check:1787742000000" && actual == &origin
        ));
    }

    #[test]
    fn late_answer_wake_keeps_the_visible_answer_and_typed_origin() {
        let mut reducer = StoryReducer::new();
        let origin = crate::runtime::TurnOrigin::AskAnswer {
            ask_id: "ask-example".to_string(),
            agent_id: Some("researcher".to_string()),
            display: "Research Grok groups.".to_string(),
        };
        let rows = reducer.push(&CliEvent::WakeTurn {
            prompt: "Research Grok groups.".to_string(),
            turn_id: Some("queued_example".to_string()),
            origin: Some(origin.clone()),
        });
        assert!(matches!(
            &rows[0],
            StoryEvent::User { text, turn_id: Some(turn_id), origin: Some(actual) }
                if text == "Research Grok groups." && turn_id == "queued_example" && actual == &origin
        ));
    }

    #[test]
    fn cross_answer_keeps_immutable_conversation_identity() {
        let mut reducer = StoryReducer::new();
        let rows = reducer.push(&CliEvent::CrossAnswer {
            session_id: "agent-school_coach".to_string(),
            owner_kind: Some("agent".to_string()),
            owner_id: Some("school_coach".to_string()),
            project_name: "Avery".to_string(),
            summary: "Draft finished".to_string(),
        });
        assert!(matches!(
            &rows[0],
            StoryEvent::CrossAnswer {
                owner_kind: Some(kind),
                owner_id: Some(id),
                project_name,
                ..
            } if kind == "agent" && id == "school_coach" && project_name == "Avery"
        ));
    }

    #[test]
    fn a_steer_is_a_journal_beat_and_settles_on_its_raw_subject() {
        // A steer is a handoff into someone already working: it must produce a
        // visible row when it is PARKED, not only when (or if) a watcher sent
        // it. The pre-2026-07-30 runtime drew a card only for `note.watcher`,
        // so a user's mid-task message rendered nothing anywhere.
        let mut reducer = StoryReducer::new();
        reducer.push(&completed("tester", "bash", 1));
        let rows = reducer.push(&CliEvent::SteerQueued {
            from: "user".to_string(),
            to: "tester".to_string(),
            subject: "steer from user".to_string(),
            body: "stop the clone check, just report what you have".to_string(),
        });
        assert!(matches!(&rows[0], StoryEvent::Receipt { .. }));
        assert!(matches!(&rows[1], StoryEvent::Steer { from, to, body, .. }
                if from == "user" && to == "tester" && body.starts_with("stop the clone")));
        // The settle carries the RAW subject the card was keyed with — not the
        // "MID-TASK MESSAGE — …" transcript heading — or a client with several
        // parked steers settles the wrong card.
        let rows = reducer.push(&CliEvent::SteerDelivered {
            to: "tester".to_string(),
            subject: "steer from user".to_string(),
        });
        assert!(
            matches!(&rows[0], StoryEvent::SteerDelivered { subject, .. }
                if subject == "steer from user")
        );
    }

    #[test]
    fn plumbing_events_never_reach_the_story() {
        let mut reducer = StoryReducer::new();
        for event in [
            CliEvent::Thinking,
            CliEvent::StreamDelta {
                kind: "text".to_string(),
                text: "chunk".to_string(),
            },
            CliEvent::Reasoning("Provider requested native tool call(s).".to_string()),
        ] {
            assert!(reducer.push(&event).is_empty(), "{event:?} leaked");
        }
        // ToolCallStarted is NOT plumbing anymore: it lights the live work dot
        // in the agent's window (StoryEvent::ToolStart) — and nothing else.
        let rows = reducer.push(&CliEvent::ToolCallStarted {
            agent: "coder".to_string(),
            tool_name: "read".to_string(),
            input_summary: "x".to_string(),
        });
        assert_eq!(rows.len(), 1);
        assert!(matches!(&rows[0], StoryEvent::ToolStart { agent, .. } if agent == "coder"));
    }

    #[test]
    fn internal_queue_ids_and_wake_plumbing_never_reach_the_story() {
        let mut reducer = StoryReducer::new();
        for text in [
            "queued prompt queued_c6d468483ccc45d694f986166d7921b3 is starting",
            "late popup answer queued — waking the agent with it",
            "4.2s · 1820 tokens · route codex:primary · trace turn-0d994.jsonl",
            "checkpoint checkpoint_72ad opened — phoenix rewind undoes this turn's edits",
            "Memory lookup is taking longer than expected. Continuing with the saved conversation; additional memories may not be included in this reply.",
        ] {
            assert!(
                reducer
                    .push(&CliEvent::GatewayNotice(text.to_string()))
                    .is_empty(),
                "internal notice leaked: {text}"
            );
        }

        let rows = reducer.push(&CliEvent::GatewayNotice(
            "queued prompt queued_c6d468483ccc45d694f986166d7921b3 failed: internal row missing"
                .to_string(),
        ));
        assert!(matches!(
            rows.as_slice(),
            [StoryEvent::Notice { text }] if text == "A queued message could not run. Open the queue above the composer to review or remove it."
        ));
    }

    #[test]
    fn compaction_continuation_never_becomes_a_story_row() {
        let mut reducer = StoryReducer::new();
        let dump = "[AUTO-COMPACTED HISTORY — 12 earlier messages were folded into this summary]\n\nKept the launch plan.";
        assert!(reducer
            .push(&CliEvent::AgentThinking {
                agent: "phoenix".to_string(),
                text: dump.to_string(),
            })
            .is_empty());
        assert!(reducer
            .push(&CliEvent::Reasoning(dump.to_string()))
            .is_empty());
        assert!(reducer
            .push(&CliEvent::FinalOutput(dump.to_string()))
            .is_empty());
        assert!(reducer
            .push(&CliEvent::PromptAssembled {
                agent: "coder".to_string(),
                user_prompt: format!("system\n{dump}\nuser: continue"),
                system_prompt: String::new(),
            })
            .is_empty());
    }

    #[test]
    fn context_compaction_lifecycle_preserves_agent_and_token_counts() {
        let mut reducer = StoryReducer::new();
        let rows = reducer.push(&CliEvent::ContextCompaction {
            agent: "school_coach".to_string(),
            status: "completed".to_string(),
            before_tokens: 251_904,
            after_tokens: 93_184,
            folded_messages: 42,
            limit: 262_144,
        });
        assert!(matches!(
            rows.as_slice(),
            [StoryEvent::ContextCompaction {
                agent,
                status,
                before_tokens: 251_904,
                after_tokens: 93_184,
                folded_messages: 42,
                limit: 262_144,
            }] if agent == "school_coach" && status == "completed"
        ));
        let json = serde_json::to_string(&rows[0]).expect("serialize compaction lifecycle");
        assert!(json.contains("\"kind\":\"context_compaction\""));
    }

    #[test]
    fn live_group_messages_preserve_markdown_newlines() {
        let mut reducer = StoryReducer::new();
        let markdown = "## Result\n\n- **First** item\n- `Second` item\n\n```text\nproof\n```";
        let rows = reducer.push(&CliEvent::GroupMessage {
            message_id: "group-message-proof".into(),
            group_id: "build-group".into(),
            round: 1,
            agent_id: "researcher".into(),
            agent_name: "Theo".into(),
            markdown: markdown.into(),
            reply_to: Some("turn-proof".into()),
            causation_id: None,
        });
        assert!(matches!(
            rows.as_slice(),
            [StoryEvent::GroupMessage { markdown: rendered, .. }] if rendered == markdown
        ));
    }

    #[test]
    fn group_member_lifecycle_reaches_the_story_stream() {
        let mut reducer = StoryReducer::new();
        let rows = reducer.push(&CliEvent::GroupMemberStatus {
            turn_id: "turn-group-status".into(),
            group_id: "build-group".into(),
            agent_id: "researcher".into(),
            agent_name: "Theo".into(),
            state: "waiting_user".into(),
            detail: "Waiting for your answer\nwithout holding a worker".into(),
        });
        assert!(matches!(
            rows.as_slice(),
            [StoryEvent::GroupMemberStatus {
                turn_id,
                agent_id,
                state,
                detail,
                ..
            }] if turn_id == "turn-group-status"
                && agent_id == "researcher"
                && state == "waiting_user"
                && detail.contains('\n')
        ));
    }

    #[test]
    fn story_events_serialize_with_kind_tags() {
        let row = StoryEvent::Receipt {
            agent: "coder".to_string(),
            text: "read 3 files".to_string(),
        };
        let json = serde_json::to_string(&row).unwrap();
        assert!(json.contains("\"kind\":\"receipt\""));
        let back: StoryEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, StoryEvent::Receipt { .. }));
    }

    #[test]
    fn legacy_handoff_and_return_story_rows_decode_with_empty_correlation() {
        let handoff: StoryEvent = serde_json::from_str(
            r#"{"kind":"handoff","from":"Phoenix","to":"Nico","subject":"Draft","background":true}"#,
        )
        .expect("legacy handoff story row");
        assert!(matches!(
            handoff,
            StoryEvent::Handoff {
                handoff_id,
                requester,
                receiver,
                status,
                causation_id: None,
                ..
            } if handoff_id.is_empty() && requester.is_empty() && receiver.is_empty() && status.is_empty()
        ));

        let returned: StoryEvent = serde_json::from_str(
            r#"{"kind":"return","agent":"Nico","subject":"Draft","body":"Done","ok":true}"#,
        )
        .expect("legacy return story row");
        assert!(matches!(
            returned,
            StoryEvent::Return {
                handoff_id,
                reply_to: None,
                causation_id: None,
                ..
            } if handoff_id.is_empty()
        ));
    }
}

#[cfg(test)]
mod owned_completion_tests {
    use super::*;
    #[test]
    fn owned_done_exposes_a_terminal_boundary_without_changing_legacy_rows() {
        use crate::runtime::postbox::{ExecutionScope, JournalEvent};
        let scope=ExecutionScope::new("turn".into(),"turn".into());
        let mut reducer=OwnedStoryReducer::default();
        let answer=reducer.push(&JournalEvent{scope:Some(scope.clone()),sequence:1,event:CliEvent::FinalOutput("Final".into())});
        assert!(matches!(&answer[..],[StoryEvent::Answer{markdown}] if markdown=="Final"));
        let done=reducer.push(&JournalEvent{scope:Some(scope),sequence:2,event:CliEvent::Done});
        assert!(matches!(&done[..],[StoryEvent::ExecutionEnded { error: None }]));
        assert_eq!(serde_json::to_value(&done[0]).unwrap()["kind"],"execution_ended");
        assert!(reducer.push(&JournalEvent{scope:None,sequence:3,event:CliEvent::Done}).is_empty());
    }

    #[test]
    fn owned_terminal_failure_preserves_the_preview_but_invalidates_delivery() {
        use crate::runtime::postbox::{ExecutionScope, JournalEvent};
        let scope = ExecutionScope::new("channel-turn".into(), "channel-turn".into());
        let mut reducer = OwnedStoryReducer::default();
        let preview = reducer.push(&JournalEvent {
            scope: Some(scope.clone()), sequence: 1,
            event: CliEvent::FinalOutput("Existing result; work may already have happened".into()),
        });
        assert!(matches!(&preview[..], [StoryEvent::Answer { .. }]));
        let rows = reducer.push(&JournalEvent {
            scope: Some(scope), sequence: 2,
            event: CliEvent::TerminalFailure { message: "Review the saved conversation".into() },
        });
        assert!(matches!(&rows[..], [
            StoryEvent::Failure { agent, .. }, StoryEvent::ExecutionEnded { error: Some(error) },
        ] if agent == "system" && error == "Review the saved conversation"));
        assert!(reducer.tasks.is_empty(), "a failed terminal cannot retain active reducer state");
        let value = serde_json::to_value(rows.last().unwrap()).unwrap();
        assert_eq!(value["kind"], "execution_ended");
        assert_eq!(value["error"], "Review the saved conversation");
        let legacy: StoryEvent = serde_json::from_value(serde_json::json!({"kind":"execution_ended"})).unwrap();
        assert!(matches!(legacy, StoryEvent::ExecutionEnded { error: None }));
        assert_eq!(serde_json::to_value(legacy).unwrap(), serde_json::json!({"kind":"execution_ended"}));
    }
}
