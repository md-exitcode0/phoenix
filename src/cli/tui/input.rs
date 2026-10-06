//! Keyboard/paste handling, input editing, history, submit/queue, and
//! instant turn-stop.

use super::*;

impl Tui {
    // ── Input handling ────────────────────────────────────────────────

    pub(super) fn on_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        turn_tx: &mpsc::Sender<TurnRequest>,
    ) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        // Ctrl-C: clear the line; twice in a row quits.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            if self.input.is_empty() && self.quit_armed {
                self.should_quit = true;
            } else {
                self.input.clear();
                self.cursor = 0;
                self.quit_armed = true;
                self.feed.push(FeedItem::Notice(
                    "press Ctrl-C again to quit (a running turn finishes on the gateway)"
                        .to_string(),
                ));
            }
            return;
        }
        self.quit_armed = false;
        // An ask_user popup owns the keyboard while open — answering the
        // agent's question is the one thing happening.
        if self.ask_modal.is_some() {
            self.on_ask_key(key);
            return;
        }
        // The /resume picker owns the keyboard while open.
        if let Some(mut picker) = self.resume_picker.take() {
            match key.code {
                KeyCode::Up => {
                    picker.sel = picker.sel.checked_sub(1).unwrap_or(picker.items.len() - 1);
                    self.resume_picker = Some(picker);
                }
                KeyCode::Down => {
                    picker.sel = (picker.sel + 1) % picker.items.len();
                    self.resume_picker = Some(picker);
                }
                KeyCode::Enter => {
                    let (id, label) = picker.items[picker.sel].clone();
                    if id == self.state.session_id {
                        self.feed
                            .push(FeedItem::Notice("already on that session".to_string()));
                    } else {
                        // The label carries "name · age" — the name is enough.
                        let name = label.split(" · ").next().unwrap_or(&label).to_string();
                        self.switch_to_session(&id, &name);
                    }
                }
                KeyCode::Esc => {} // closed — picker stays taken
                _ => self.resume_picker = Some(picker),
            }
            return;
        }
        // While the slash menu is open, Up/Down hover, Tab fills, Enter picks.
        let menu = self.menu_matches();
        if !menu.is_empty() {
            self.menu_sel = self.menu_sel.min(menu.len() - 1);
            match key.code {
                KeyCode::Up => {
                    self.menu_sel = self.menu_sel.checked_sub(1).unwrap_or(menu.len() - 1);
                    return;
                }
                KeyCode::Down => {
                    self.menu_sel = (self.menu_sel + 1) % menu.len();
                    return;
                }
                KeyCode::Tab => {
                    let (name, _, _, takes_args) = menu[self.menu_sel];
                    self.input = if takes_args {
                        format!("{name} ")
                    } else {
                        name.to_string()
                    };
                    self.cursor = self.input.chars().count();
                    return;
                }
                KeyCode::Enter => {
                    // Enter picks the hovered command: no-arg commands run
                    // immediately; arg commands drop into the input bar.
                    let (name, _, _, takes_args) = menu[self.menu_sel];
                    if takes_args {
                        self.input = format!("{name} ");
                        self.cursor = self.input.chars().count();
                    } else {
                        self.input = name.to_string();
                        self.cursor = self.input.chars().count();
                        self.submit(turn_tx);
                    }
                    return;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Enter
                if key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) =>
            {
                self.insert_input_text("\n");
            }
            KeyCode::Enter => self.submit(turn_tx),
            KeyCode::Char(c) => {
                self.insert_input_char(c);
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.input.remove(byte_index(&self.input, self.cursor));
                }
            }
            KeyCode::Delete => {
                if self.cursor < self.input.chars().count() {
                    self.input.remove(byte_index(&self.input, self.cursor));
                }
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => {
                self.cursor = (self.cursor + 1).min(self.input.chars().count());
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.input.chars().count(),
            KeyCode::Up => self.history_step(-1),
            KeyCode::Down => self.history_step(1),
            KeyCode::PageUp => self.scroll_back = self.scroll_back.saturating_add(8),
            KeyCode::PageDown => self.scroll_back = self.scroll_back.saturating_sub(8),
            KeyCode::Esc => {
                if !self.input.is_empty() {
                    // First responsibility: clear what's being typed.
                    self.input.clear();
                    self.cursor = 0;
                    self.history_pos = None;
                } else if self.turn_running() {
                    // Empty input + running turn: Esc STOPS the turn (Claude
                    // Code semantics). Queued messages run next through the
                    // normal turn-end path.
                    self.stop_running_turn("Esc");
                }
                self.scroll_back = 0;
            }
            _ => {}
        }
    }

    /// Commands matching the current input prefix — the menu is open while
    /// the input is a bare `/name` prefix (no space yet). Commands with a
    /// fixed option set keep the menu open AFTER the space, listing their
    /// options as picks (hover + Enter applies) — nobody should have to
    /// memorize argument spellings.
    pub(super) fn menu_matches(&self) -> Vec<(&'static str, &'static str, &'static str, bool)> {
        if !self.input.starts_with('/') {
            return Vec::new();
        }
        // Option picker: `/display ` (+ optional partial arg) lists the modes.
        if let Some(rest) = self.input.strip_prefix("/display ") {
            const MODES: [(&str, &str, &str, bool); 2] = [
                (
                    "/display compact",
                    "/display compact",
                    "narrated journal — narration + aggregated work receipts",
                    false,
                ),
                (
                    "/display verbose",
                    "/display verbose",
                    "the same journal + red/green edit diffs as they land",
                    false,
                ),
            ];
            let partial = rest.trim();
            return MODES
                .iter()
                .copied()
                .filter(|(name, _, _, _)| {
                    partial.is_empty() || name.split(' ').nth(1).unwrap_or("").starts_with(partial)
                })
                .collect();
        }
        if self.input.contains(' ') {
            return Vec::new();
        }
        COMMANDS
            .iter()
            .copied()
            .filter(|(name, _, _, _)| name.starts_with(self.input.as_str()))
            .collect()
    }

    pub(super) fn history_step(&mut self, direction: isize) {
        if self.history.is_empty() {
            return;
        }
        let next = match (self.history_pos, direction) {
            (None, -1) => Some(self.history.len() - 1),
            (None, _) => None,
            (Some(0), -1) => Some(0),
            (Some(i), -1) => Some(i - 1),
            (Some(i), 1) if i + 1 < self.history.len() => Some(i + 1),
            (Some(_), 1) => None,
            (pos, _) => pos,
        };
        self.history_pos = next;
        self.input = next.map(|i| self.history[i].clone()).unwrap_or_default();
        self.cursor = self.input.chars().count();
    }

    pub(super) fn insert_input_char(&mut self, c: char) {
        self.input.insert(byte_index(&self.input, self.cursor), c);
        self.cursor += 1;
        self.menu_sel = 0;
        self.history_pos = None;
    }

    pub(super) fn insert_input_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let at = byte_index(&self.input, self.cursor);
        self.input.insert_str(at, text);
        self.cursor += text.chars().count();
        self.menu_sel = 0;
        self.history_pos = None;
    }

    /// Insert pasted text at the cursor verbatim — newlines included. Nothing
    /// is submitted: a paste only fills the buffer, and the embedded newlines
    /// survive until the user presses Enter (submit trims the surrounding
    /// whitespace but keeps the interior line breaks). `\r\n` is normalized to
    /// `\n` so a Windows-clipboard paste doesn't leave stray carriage returns.
    pub(super) fn on_paste(&mut self, text: &str) {
        let cleaned = text.replace("\r\n", "\n").replace('\r', "\n");
        if cleaned.is_empty() {
            return;
        }
        self.insert_input_text(&cleaned);
    }

    pub(super) fn submit(&mut self, turn_tx: &mpsc::Sender<TurnRequest>) {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.input.clear();
        self.cursor = 0;
        self.history_pos = None;
        self.scroll_back = 0;
        if text.starts_with('/') {
            self.history.push(text.clone());
            self.slash_command(&text);
            return;
        }
        self.history.push(text.clone());
        // The sent message is ALWAYS echoed into the feed instantly — before
        // any dispatch decision. A message that vanishes on Enter (the old
        // mid-turn drop) reads as data loss to the user, because it is.
        self.feed.push(FeedItem::User(text.clone()));
        if self.turn_running() {
            self.queued_inputs.push_back(text);
            self.feed.push(FeedItem::Notice(format!(
                "queued (#{}) — runs when the current turn finishes (/stop or Esc ends it now)",
                self.queued_inputs.len()
            )));
            return;
        }
        self.dispatch_turn(text, turn_tx);
    }

    /// Stop the running turn NOW. The gateway abort is fire-and-forget; the
    /// UI does not wait for it to round-trip — the spinner, ticker, and agent
    /// moods clear this instant, because "I pressed stop" must mean "it
    /// stopped showing work," not "it will stop once the daemon confirms."
    /// Trailing events from the dying turn are ignored (`cancelling`) until
    /// its TurnFinished arrives and runs any queued messages.
    pub(super) fn stop_running_turn(&mut self, how: &str) {
        let session = self.state.session_id.clone();
        tokio::spawn(async move {
            let _ = crate::cli::daemon::cancel_turn(session).await;
        });
        self.cancelling = true;
        self.turn_started = None;
        self.all_idle();
        self.clear_live_stream();
        self.stream_note.clear();
        self.feed.push(FeedItem::Notice(format!(
            "■ stopped ({how}){}",
            if self.queued_inputs.is_empty() {
                String::new()
            } else {
                " — queued messages run next".to_string()
            }
        )));
    }

    pub(super) fn dispatch_turn(&mut self, text: String, turn_tx: &mpsc::Sender<TurnRequest>) {
        // A fresh dispatch supersedes any lingering cancel state.
        self.cancelling = false;
        self.turn_started = Some(Instant::now());
        self.turn_activity = TurnActivityStats::default();
        self.seen_tool_inputs.clear();
        // Always name what is running — with queued messages in play, "which
        // message is this spinner for?" must never be a mystery.
        self.feed.push(FeedItem::Notice(format!(
            "▶ running: {}",
            truncate(&text, 90)
        )));
        self.set_mood("orchestrator", AgentMood::Thinking);
        if let Err(e) = turn_tx.try_send(TurnRequest {
            session_id: self.state.session_id.clone(),
            user_request: text.clone(),
            yolo: self.state.yolo,
        }) {
            // A dropped dispatch must not look like a running turn: requeue the
            // message and say what happened instead of spinning forever.
            self.turn_started = None;
            self.set_mood("orchestrator", AgentMood::Idle);
            self.queued_inputs.push_front(text);
            self.feed.push(FeedItem::Error(format!(
                "could not start the turn ({e}) — message kept in queue, check the gateway"
            )));
        }
    }

    /// Run the next user message queued while a turn was in flight.
    pub(super) fn run_next_queued(&mut self, turn_tx: &mpsc::Sender<TurnRequest>) {
        if self.turn_running() {
            return;
        }
        if let Some(text) = self.queued_inputs.pop_front() {
            self.dispatch_turn(text, turn_tx);
        }
    }
}

impl Tui {
    // ── ask_user popup keys ───────────────────────────────────────────
    //
    // The modal owns the keyboard: arrows hover, Space toggles (multi),
    // Enter answers, Esc dismisses the whole ask (the agent proceeds on
    // its own judgment — a popup must never hold a turn hostage).

    pub(super) fn on_ask_key(&mut self, key: crossterm::event::KeyEvent) {
        let Some(mut modal) = self.ask_modal.take() else {
            return;
        };
        let q = modal.questions[modal.current].clone();
        let type_row = q.options.len(); // the "type your own" row index
        let mut answered: Option<String> = None;
        let mut dismissed = false;

        if modal.typing {
            match key.code {
                KeyCode::Enter => {
                    let text = modal.text.trim().to_string();
                    if !text.is_empty() {
                        answered = Some(text);
                    }
                }
                KeyCode::Esc => {
                    if q.options.is_empty() {
                        dismissed = true;
                    } else {
                        modal.typing = false;
                        modal.text.clear();
                    }
                }
                KeyCode::Backspace => {
                    modal.text.pop();
                }
                KeyCode::Char(c) => modal.text.push(c),
                _ => {}
            }
        } else {
            match key.code {
                KeyCode::Up => {
                    modal.sel = modal.sel.checked_sub(1).unwrap_or(type_row);
                }
                KeyCode::Down => {
                    modal.sel = if modal.sel >= type_row {
                        0
                    } else {
                        modal.sel + 1
                    };
                }
                KeyCode::Char(' ') if q.multi_select && modal.sel < type_row => {
                    modal.checked[modal.sel] = !modal.checked[modal.sel];
                }
                // 1-9 jump straight to an option (single-select picks it).
                KeyCode::Char(c @ '1'..='9') => {
                    let idx = c as usize - '1' as usize;
                    if idx < type_row {
                        if q.multi_select {
                            modal.sel = idx;
                            modal.checked[idx] = !modal.checked[idx];
                        } else {
                            answered = Some(q.options[idx].clone());
                        }
                    }
                }
                KeyCode::Enter => {
                    if modal.sel == type_row {
                        modal.typing = true;
                    } else if q.multi_select {
                        let picked: Vec<String> = q
                            .options
                            .iter()
                            .zip(&modal.checked)
                            .filter(|(_, on)| **on)
                            .map(|(opt, _)| opt.clone())
                            .collect();
                        if !picked.is_empty() {
                            answered = Some(picked.join(", "));
                        } else {
                            // Enter on a multi with nothing ticked picks the
                            // hovered option — the obvious intent.
                            answered = Some(q.options[modal.sel].clone());
                        }
                    } else {
                        answered = Some(q.options[modal.sel].clone());
                    }
                }
                KeyCode::Esc => dismissed = true,
                _ => {}
            }
        }

        if dismissed {
            self.resolve_ask(
                modal,
                "The user dismissed the question without answering. Proceed with your \
                 best judgment and state the assumption you made in your final answer."
                    .to_string(),
                true,
            );
            return;
        }

        if let Some(text) = answered {
            let header = q.header.as_deref().unwrap_or("Question");
            modal
                .answers
                .push(format!("\n  [{header}] Q: {}\n  A: {text}", q.question));
            if modal.current + 1 < modal.questions.len() {
                modal.current += 1;
                modal.arm_current();
            } else {
                let body = format!(
                    "Collected {} answer(s) from user:{}",
                    modal.answers.len(),
                    modal.answers.concat()
                );
                self.resolve_ask(modal, body, false);
                return;
            }
        }
        self.ask_modal = Some(modal);
    }

    /// Send the answer back to the awaiting turn and surface the next queued
    /// ask, if any.
    fn resolve_ask(&mut self, modal: AskModal, answer: String, dismissed: bool) {
        let note = if dismissed {
            format!(
                "question dismissed — {} continues on its own judgment",
                modal.agent
            )
        } else {
            format!("answered — {} continues", modal.agent)
        };
        self.push_activity(&modal.agent.clone(), "◆", note, Some(!dismissed));
        let id = modal.id;
        tokio::spawn(async move {
            // Local (in-process) turns resolve directly; gateway turns go
            // over the socket.
            if !crate::runtime::asks::answer(&id, answer.clone()) {
                let _ = crate::cli::daemon::answer_ask(id, answer).await;
            }
        });
        self.ask_modal = self.pending_asks.pop_front();
    }
}
