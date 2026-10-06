//! Turn/gateway event ingestion: live stream deltas, tool rows, moods,
//! background spawn/return rendering, and turn-finished bookkeeping.

use super::*;

impl Tui {
    // ── Turn event rendering ──────────────────────────────────────────

    pub(super) fn on_turn_event(&mut self, event: CliEvent) {
        // Background-agent lifecycle events render unconditionally — they
        // arrive over the standing subscription, independent of any turn (and
        // must survive a cancelled turn: the detached job keeps working).
        match &event {
            CliEvent::BackgroundAgentSpawned { agent, subject, .. } => {
                let agent = agent.clone();
                let subject = subject.clone();
                self.turn_activity.background_handoffs += 1;
                self.background_jobs.push(BackgroundJobStatus {
                    agent: agent.clone(),
                    subject: subject.clone(),
                    status: "working in the background".to_string(),
                    done: false,
                    started: Instant::now(),
                });
                self.set_mood(&agent, AgentMood::Thinking);
                self.push_activity(
                    &agent,
                    "⧉",
                    format!("spawned in background: {}", truncate(&subject, 90)),
                    None,
                );
                return;
            }
            CliEvent::BackgroundAgentReturned {
                agent,
                subject,
                ok,
                summary,
                body,
                ..
            } => {
                let (agent, ok) = (agent.clone(), *ok);
                if let Some(job) = self
                    .background_jobs
                    .iter_mut()
                    .rev()
                    .find(|job| job.agent == agent && !job.done)
                {
                    job.done = true;
                    job.status = if ok {
                        "returned".to_string()
                    } else {
                        format!("FAILED — {summary}")
                    };
                }
                self.set_mood(&agent, AgentMood::Done);
                // The job is home — its live working row retires with it,
                // along with any streaming heartbeat it was showing.
                self.live_counts.remove(agent_key(&agent));
                self.stream_note.clear();
                self.flush_compact_tallies();
                self.feed.push(FeedItem::AgentReturn {
                    agent,
                    subject: subject.clone(),
                    body: body.clone(),
                    ok,
                });
                return;
            }
            CliEvent::WatcherCard {
                from,
                subject,
                body,
                ok,
            } => {
                // A watcher ruling or goal milestone is a journal beat, and it
                // renders as the same return card a specialist hands back —
                // the exact card resume reconstruction draws for these Talks,
                // never a dim plumbing line. Unconditional: the Judge sweep
                // rules on background lanes with no user turn running.
                self.flush_compact_tallies();
                self.feed.push(FeedItem::AgentReturn {
                    agent: from.clone(),
                    subject: subject.clone(),
                    body: body.clone(),
                    ok: *ok,
                });
                return;
            }
            CliEvent::SteerQueued {
                from,
                to,
                subject,
                body,
            } => {
                // A steer is work handed to someone already working, so it
                // draws the same inbound card a handoff does — not a dim
                // plumbing line. Unconditional, for the same reason
                // WatcherCard is: steers land on background lanes with no
                // user turn running.
                self.flush_compact_tallies();
                self.feed.push(FeedItem::AgentReturn {
                    agent: from.clone(),
                    subject: format!("→ {to} · {subject}"),
                    body: body.clone(),
                    ok: true,
                });
                return;
            }
            // The settle is a canvas affordance (it mutates a live card in
            // place); the TUI's feed is append-only, so a second row would
            // just be noise.
            CliEvent::SteerDelivered { .. } => return,
            CliEvent::WakeTurn { prompt, .. } => {
                // A daemon-initiated turn (goal heartbeat) opens with its
                // driving prompt as the bold ❯ row — the same row resume
                // replays for the transcript's User message (same 600-char
                // cap), so live and resumed feeds tell one story.
                self.flush_compact_tallies();
                self.feed.push(FeedItem::User(truncate(prompt, 600)));
                return;
            }
            _ => {}
        }
        // After a stop, the aborted turn may still emit a few in-flight
        // events before it dies. The user pressed stop — don't repaint
        // activity for a turn they killed.
        if self.cancelling {
            return;
        }
        match event {
            // Handled before the cancelling guard above.
            CliEvent::BackgroundAgentSpawned { .. }
            | CliEvent::BackgroundAgentReturned { .. }
            | CliEvent::VolumeWorkerLifecycle { .. }
            | CliEvent::WatcherCard { .. }
            | CliEvent::SteerQueued { .. }
            | CliEvent::SteerDelivered { .. }
            | CliEvent::WakeTurn { .. } => {}
            // Lifecycle barrier for bounded one-shot settlement. The TUI is
            // already persistent and must not render plumbing as a feed row.
            CliEvent::BackgroundResultsAbsorbed { .. } => {}
            // The canvas draws a live per-agent context gauge from this; the
            // TUI's own gauge is fed by the turn summary, so nothing to do.
            CliEvent::ContextUsage { .. } | CliEvent::ContextCompaction { .. } => {}
            CliEvent::StreamDelta { kind, text } => {
                if kind == "progress" {
                    // A named "waiting on" note lights that agent's working
                    // row — a background turn's opening provider call is
                    // otherwise invisible until its first tool runs.
                    if let Some((agent, _)) = text.split_once(" waiting on ") {
                        self.set_mood(agent, AgentMood::Thinking);
                    }
                    self.stream_note = text;
                } else {
                    self.live_stream.push_str(&text);
                    // Keep only the visible tail; the ticker is a window, not a log.
                    let len = self.live_stream.chars().count();
                    if len > 700 {
                        self.live_stream = self.live_stream.chars().skip(len - 700).collect();
                    }
                }
            }
            CliEvent::Thinking => {
                // A new provider call starts a fresh thought stream.
                self.live_stream.clear();
                self.stream_note.clear();
                // The mesh emits Thinking per provider call without naming the
                // agent; mark the most plausible actor (any agent already in a
                // tool keeps its state, otherwise the orchestrator thinks).
                if !self
                    .agents
                    .values()
                    .any(|c| matches!(c.mood, AgentMood::Tool(_) | AgentMood::Thinking))
                {
                    self.set_mood("orchestrator", AgentMood::Thinking);
                }
            }
            CliEvent::Reasoning(text) => {
                // The main turn's between-tool narration — anonymous "∴" row,
                // the journal's voice.
                if text.starts_with("Provider requested native tool call") {
                    return;
                }
                let cap = if self.state.show_reasoning { 400 } else { 140 };
                let brief = first_sentence(&text, cap);
                if !brief.is_empty() {
                    self.flush_compact_tallies();
                    self.push_activity("·", "∴", brief, None);
                }
            }
            CliEvent::AgentThinking { agent, text } | CliEvent::AgentMessage { agent, text } => {
                // A named agent's narration ("build running, fixing the reflog
                // parse next") is the story of ITS lane — attributed to the
                // agent so background Leo reads as Leo, not as the main
                // voice. One line in every display mode; /thinking widens it.
                if text.starts_with("Provider requested native tool call") {
                    return;
                }
                let cap = if self.state.show_reasoning { 400 } else { 140 };
                let brief = first_sentence(&text, cap);
                if !brief.is_empty() {
                    // The narration is the journal's beat — settle the "did
                    // N things" receipt first, then say what happens next.
                    self.flush_compact_tallies();
                    self.push_activity(&agent, "∴", brief, None);
                }
            }
            CliEvent::GroupMessage {
                agent_name,
                round,
                markdown,
                ..
            } => {
                self.flush_compact_tallies();
                self.push_activity(
                    &agent_name,
                    "◆",
                    format!("round {round}: {}", first_sentence(&markdown, 180)),
                    None,
                );
            }
            CliEvent::GroupMemberStatus {
                agent_name,
                state,
                detail,
                ..
            } => {
                self.flush_compact_tallies();
                self.push_activity(
                    &agent_name,
                    "◌",
                    format!("{state}: {}", truncate(&detail, 140)),
                    None,
                );
            }
            CliEvent::ToolCallStarted {
                agent,
                tool_name,
                input_summary,
            } => {
                // The "waiting on <model>" note belongs to the provider call;
                // once tools run it would be stale on the working row.
                self.stream_note.clear();
                let _ = input_summary;
                self.set_mood(&agent, AgentMood::Tool(tool_name.clone()));
                // No in-flight rows in either mode — the journal receipts
                // aggregate successes at the next narrative beat, and the
                // agent chip + working row already show what runs right now.
            }
            CliEvent::ToolCallCompleted {
                agent,
                tool_name,
                input_summary,
                success,
                output_summary,
                diff,
            } => {
                self.turn_activity.tool_calls += 1;
                match tool_category(&tool_name) {
                    Some("read") => self.turn_activity.reads += 1,
                    Some("edit") => self.turn_activity.edits += 1,
                    Some("command") => self.turn_activity.commands += 1,
                    Some("web") => self.turn_activity.web += 1,
                    _ => {}
                }
                let tool_key = format!("{tool_name}\n{input_summary}");
                if !self.seen_tool_inputs.insert(tool_key) {
                    self.turn_activity.duplicate_tool_calls += 1;
                }
                if !success {
                    self.turn_activity.failed_tool_calls += 1;
                } else {
                    // Per-agent live counter for that agent's own shimmer row
                    // — visible whether or not a user turn is running.
                    self.live_counts
                        .entry(agent_key(&agent))
                        .or_default()
                        .add(&tool_name);
                }
                self.set_mood(&agent, AgentMood::Thinking);
                if self.state.show_actions && self.subagent_row_visible(&agent) {
                    // Failures surface in every mode — silent errors are how
                    // trust dies. Successes journal, never log: the call joins
                    // the agent's tally and lands later as one "read 3 files ·
                    // ran 5 commands" receipt at the next narrative beat.
                    // Verbose adds the red/green edit diff the moment it lands.
                    if !success {
                        self.push_activity(
                            &agent,
                            "✘",
                            format!("{tool_name} → {}", truncate(&output_summary, 120)),
                            Some(false),
                        );
                    } else {
                        self.compact_tallies
                            .entry(agent.clone())
                            .or_default()
                            .add(&tool_name);
                        if self.state.display_mode == crate::cli::DisplayMode::Verbose {
                            if let Some(diff) = diff {
                                self.feed.push(FeedItem::Diff {
                                    agent: agent.clone(),
                                    diff,
                                });
                            }
                        }
                    }
                }
            }
            CliEvent::SpecialistDelegated { agent, subject } => {
                self.flush_compact_tallies();
                self.turn_activity.specialist_handoffs += 1;
                self.set_mood(&agent, AgentMood::Thinking);
                self.push_activity(
                    "orchestrator",
                    "⇒",
                    format!("delegated to {agent}: {}", truncate(&subject, 80)),
                    None,
                );
            }
            CliEvent::AgentHandoff {
                from,
                to,
                subject,
                background,
                ..
            } => {
                self.flush_compact_tallies();
                self.turn_activity.specialist_handoffs += 1;
                if background {
                    self.turn_activity.background_handoffs += 1;
                }
                self.set_mood(&to, AgentMood::Thinking);
                self.push_activity(
                    &from,
                    "⇄",
                    format!("{from} → {to}: {}", truncate(&subject, 80)),
                    None,
                );
            }
            CliEvent::SpecialistQueued {
                agent,
                subject,
                status,
            } => {
                if status.contains("background handoff") {
                    self.turn_activity.background_handoffs += 1;
                    self.background_jobs.push(BackgroundJobStatus {
                        agent: agent.clone(),
                        subject: subject.clone(),
                        status: status.clone(),
                        done: false,
                        started: Instant::now(),
                    });
                }
                self.set_mood(&agent, AgentMood::Idle);
                self.push_activity(
                    "orchestrator",
                    "⇢",
                    format!(
                        "{agent} queued: {} ({})",
                        truncate(&subject, 70),
                        truncate(&status, 90)
                    ),
                    None,
                );
            }
            CliEvent::SpecialistCompleted { agent, ok, summary } => {
                self.flush_compact_tallies();
                if let Some(job) = self
                    .background_jobs
                    .iter_mut()
                    .rev()
                    .find(|job| job.agent == agent && !job.done)
                {
                    job.done = true;
                    job.status = if ok {
                        "completed; result handed back through the mesh".to_string()
                    } else {
                        format!("FAILED — {summary}")
                    };
                }
                self.set_mood(&agent, AgentMood::Done);
                // Stage over — retire the live tally so a later baton to the
                // same agent starts a fresh count.
                self.live_counts.remove(agent_key(&agent));
                // No "completed, handing back" row on success: the agent-to-
                // agent return line already shows where the result went, and
                // the duplicate read as noise. Failures stay loud.
                if !ok {
                    self.push_activity(&agent, "✖", format!("failed: {summary}"), Some(false));
                }
            }
            CliEvent::SpecialistOutput { agent, summary } => {
                self.push_activity(&agent, "·", truncate(&summary, 200), None);
            }
            CliEvent::Routing { target, .. } => {
                self.last_route = target;
            }
            // The deterministic memory recall (Cognee) still runs in the
            // runtime, but the librarian is no longer a teammate — don't light
            // a chip or push a "Memory (lib)" activity row for it.
            CliEvent::LibrarianPass { .. } => {}
            CliEvent::PermissionCheck {
                tool_name, status, ..
            } => {
                if self.state.show_debug {
                    self.push_activity("·", "⚿", format!("{tool_name}: {status}"), None);
                }
            }
            CliEvent::PromptAssembled {
                agent,
                system_prompt,
                user_prompt,
            } => {
                self.state.last_prompt = Some(crate::cli::StoredPrompt {
                    agent,
                    system_prompt,
                    user_prompt,
                });
            }
            CliEvent::SessionResolved {
                session_id, status, ..
            } => {
                if self.state.show_debug {
                    self.push_activity("·", "≡", format!("session {session_id} ({status})"), None);
                }
            }
            CliEvent::AskUser {
                id,
                agent,
                questions,
                ..
            } => {
                let mut modal = AskModal {
                    id,
                    agent: agent.clone(),
                    answers: Vec::with_capacity(questions.len()),
                    questions,
                    current: 0,
                    sel: 0,
                    text: String::new(),
                    typing: false,
                    checked: Vec::new(),
                };
                modal.arm_current();
                self.flush_compact_tallies();
                self.push_activity(
                    &agent,
                    "◆",
                    "needs your input — answer in the popup".to_string(),
                    None,
                );
                if self.ask_modal.is_none() {
                    self.ask_modal = Some(modal);
                } else {
                    self.pending_asks.push_back(modal);
                }
            }
            CliEvent::GatewayNotice(text) => {
                if text.starts_with('⇄') || text.starts_with('⏹') {
                    // Chain hop/stop — a journal beat like any handoff.
                    self.flush_compact_tallies();
                }
                if let Some(detail) = text.strip_prefix("context auto-compacted: ") {
                    self.last_context_compaction = Some(detail.to_string());
                    self.refresh_ctx_estimate();
                }
                self.feed.push(FeedItem::Notice(text));
            }
            CliEvent::FinalOutput(markdown) => {
                self.flush_compact_tallies();
                self.feed.push(FeedItem::Blank);
                self.feed.push(FeedItem::Answer(markdown));
            }
            CliEvent::Done => {
                // End of an event stream. For a user-dispatched turn this is
                // followed by on_turn_finished (which repeats this cleanup),
                // but a background-wake turn has NO Turn socket — this Done,
                // forwarded over Subscribe, is its only end-of-turn signal.
                // Without it the wake's Thinking mood stayed lit forever
                // ("Phoenix thinking" while nothing runs).
                self.all_idle();
                self.clear_live_stream();
            }
            CliEvent::TerminalFailure { message } => {
                self.flush_compact_tallies();
                self.feed.push(FeedItem::Notice(message));
                self.all_idle();
                self.clear_live_stream();
            }
            CliEvent::TextChunk(_) => {}
            // Item 7: cross-session answer notification — the TUI ignores it
            // (only the canvas shows notification cards).
            CliEvent::CrossAnswer { .. } => {}
        }
    }

    pub(super) fn clear_live_stream(&mut self) {
        self.live_stream.clear();
        self.stream_note.clear();
    }

    /// Compact-mode journal beat: turn every agent's accumulated tool tally
    /// into one receipt row ("read 3 files · ran 5 commands"). Called before
    /// narration, handoffs, completions, and finals so the receipts land
    /// exactly where the story pauses.
    pub(super) fn flush_compact_tallies(&mut self) {
        if self.compact_tallies.is_empty() {
            return;
        }
        let tallies = std::mem::take(&mut self.compact_tallies);
        for (agent, tally) in tallies {
            if !tally.is_empty() {
                self.push_activity(&agent, "·", tally.receipt(), None);
            }
        }
    }

    pub(super) fn on_turn_finished(&mut self, outcome: RemoteOutcome) {
        self.flush_compact_tallies();
        self.clear_live_stream();
        let elapsed = self
            .turn_started
            .take()
            .map(|t| t.elapsed().as_secs_f32())
            .unwrap_or(0.0);
        self.turn_elapsed_last = elapsed;
        self.all_idle();
        // A turn the user stopped already showed "■ stopped"; its outcome is
        // the abort itself, so don't tack on a redundant error line.
        let was_cancelling = std::mem::take(&mut self.cancelling);
        match outcome {
            RemoteOutcome::Summary(summary) => self.apply_summary(summary, elapsed),
            RemoteOutcome::Error(_) if was_cancelling => {}
            RemoteOutcome::Error(message) => self.feed.push(FeedItem::Error(message)),
            RemoteOutcome::Lost => self.feed.push(FeedItem::Error(
                "the gateway dropped mid-turn — check the `phoenix` gateway terminal".to_string(),
            )),
        }
        self.refresh_ctx_estimate();
        self.feed.push(FeedItem::Blank);
    }

    pub(super) fn apply_summary(&mut self, summary: TurnSummary, elapsed: f32) {
        self.session_tokens += summary.total_tokens as u64;
        self.compression_saved_tokens += summary.compression_saved_tokens;
        self.compression_raw_tokens += summary.compression_raw_tokens;
        self.last_route = summary.route.clone();
        let activity = self.turn_activity_summary();
        self.state.last_run = Some(crate::cli::StoredRun {
            run_id: summary.run_id.clone(),
            trace_path: summary.trace_path.clone().into(),
            reply_summary: crate::cli::summarize_title(&summary.final_markdown),
            token_total: summary.total_tokens,
            route: summary.route.clone(),
        });
        // The footer is a receipt, not a debug dump: time, tokens, and what
        // the turn did. Transport plumbing (route, trace path) is /debug-only.
        let plumbing = if self.state.show_debug {
            format!(
                " · route {} · trace {}",
                summary.route,
                short_path(&summary.trace_path)
            )
        } else {
            String::new()
        };
        // Compression visibility (2026-07-07 goal): what THIS turn shaved off
        // tool output before it reached model context. One segment on the
        // existing receipt — never its own row, absent when nothing compressed.
        let compressed = if summary.compression_saved_tokens >= 100 {
            // With the denominator when known: "~3.1k of 9.8k tok (32%)"
            // proves the layer worked; a bare saved-count proves nothing.
            if summary.compression_raw_tokens > 0 {
                format!(
                    " · tool output ~{} of {} tok compressed away ({:.0}%)",
                    format_count(summary.compression_saved_tokens),
                    format_count(summary.compression_raw_tokens),
                    100.0 * summary.compression_saved_tokens as f64
                        / summary.compression_raw_tokens as f64
                )
            } else {
                format!(
                    " · ~{} tok compressed away",
                    format_count(summary.compression_saved_tokens)
                )
            }
        } else {
            String::new()
        };
        self.feed.push(FeedItem::Notice(format!(
            "{elapsed:.1}s · {} tokens{}{}{}",
            format_count(summary.total_tokens as u64),
            activity,
            compressed,
            plumbing,
        )));
    }

    pub(super) fn turn_activity_summary(&self) -> String {
        let stats = self.turn_activity;
        let mut parts = Vec::new();
        if stats.tool_calls > 0 {
            let duplicate = if stats.duplicate_tool_calls > 0 {
                format!(", {} duplicate", stats.duplicate_tool_calls)
            } else {
                String::new()
            };
            if stats.failed_tool_calls > 0 {
                parts.push(format!(
                    "{} tools ({} failed{})",
                    stats.tool_calls, stats.failed_tool_calls, duplicate
                ));
            } else {
                parts.push(if duplicate.is_empty() {
                    format!("{} tools", stats.tool_calls)
                } else {
                    format!(
                        "{} tools ({})",
                        stats.tool_calls,
                        duplicate.trim_start_matches(", ")
                    )
                });
            }
        }
        // Category breakdown — the compact-mode story of what those tools were.
        let mut categories = Vec::new();
        if stats.reads > 0 {
            categories.push(format!("{} reads", stats.reads));
        }
        if stats.edits > 0 {
            categories.push(format!("{} edits", stats.edits));
        }
        if stats.commands > 0 {
            categories.push(format!("{} cmds", stats.commands));
        }
        if stats.web > 0 {
            categories.push(format!("{} web", stats.web));
        }
        if !categories.is_empty() {
            parts.push(categories.join(" · "));
        }
        if stats.specialist_handoffs > 0 {
            let mut handoff = format!("{} handoffs", stats.specialist_handoffs);
            if stats.background_handoffs > 0 {
                handoff.push_str(&format!(" ({} background)", stats.background_handoffs));
            }
            parts.push(handoff);
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!(" · {}", parts.join(" · "))
        }
    }
}
