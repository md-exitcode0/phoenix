//! Frame rendering: header, feed, agent bar, status bar, input box, and
//! the command menu.

use super::*;

/// Blend an RGB color toward another by `t` (0.0 = base, 1.0 = target).
fn mix(base: Color, target: (u8, u8, u8), t: f32) -> Color {
    let Color::Rgb(r, g, b) = base else {
        return base;
    };
    let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Color::Rgb(lerp(r, target.0), lerp(g, target.1), lerp(b, target.2))
}

/// An agent's hue once its work has settled into the transcript: pulled most
/// of the way to the grey ramp so the feed reads monochrome at a glance, but
/// each agent's rows keep a recognizable tint. Full saturation is reserved
/// for LIVE work (the shimmer rows).
fn settled(base: Color) -> Color {
    mix(base, (174, 172, 164), 0.45)
}

/// Split "Leo (coder) #2" into the persona ("Leo") and the rest
/// (" (coder) #2") so the role can render a step quieter than the name.
fn split_persona(name: &str) -> (&str, &str) {
    match name.find(" (") {
        Some(i) => name.split_at(i),
        None => (name, ""),
    }
}

/// Compact elapsed label next to live work: raw seconds under a minute
/// ("42s"), minutes+seconds under an hour ("1m 12s"), hours+minutes under a
/// day ("2h 5m" — seconds dropped), then days+hours ("1d 3h").
pub(super) fn human_elapsed(elapsed: std::time::Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else if secs < 86_400 {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d {}h", secs / 86_400, (secs % 86_400) / 3600)
    }
}

/// Phoenix shimmer: the text stays put while a brighter band sweeps
/// through the characters from the right, tick by tick. No spinner glyph —
/// the animation IS the text. The wave lives in `base`'s hue (each agent
/// shimmers in its own color): resting chars are the dimmed base, the band
/// lifts toward white.
pub(super) fn shimmer_spans(text: &str, tick: usize, base: Color) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len().max(1);
    // The band center travels right → left, with a short quiet gap between
    // sweeps so the motion reads as a wave, not a strobe.
    let period = n + 10;
    let center = (n as isize + 4) - (tick % period) as isize;
    let rest = mix(base, (0, 0, 0), 0.35);
    chars
        .into_iter()
        .enumerate()
        .map(|(i, ch)| {
            let color = match (i as isize - center).abs() {
                0 => mix(base, (255, 255, 255), 0.85),
                1 => mix(base, (255, 255, 255), 0.55),
                2 => mix(base, (255, 255, 255), 0.25),
                _ => rest,
            };
            Span::styled(ch.to_string(), Style::default().fg(color))
        })
        .collect()
}

/// Human verb for the tool an agent is inside right now — the working row
/// reads "Iris · editing…", "Elena · searching the web…", never a raw tool id.
pub(super) fn tool_verb(tool_name: &str) -> &'static str {
    match tool_name {
        "read" => "reading a file",
        "grep" => "grepping",
        "glob" | "list_directory" => "scanning files",
        "index_codebase" => "indexing the codebase",
        "codebase_search" | "symbol_search" | "file_symbols" | "callers" | "callees" | "impact"
        | "call_path" => "exploring the codebase",
        "bash" => "running a command",
        "write" | "str_replace" => "editing",
        "web_search" => "searching the web",
        "web_fetch" | "web_crawl" | "web_scrape" => "reading the web",
        "memory_recall" => "remembering",
        "talk" => "handing off",
        "todo" => "updating the plan",
        "ask_user" => "asking you",
        name if name.starts_with("browser_") => "browsing",
        name if name.starts_with("computer_") => "driving the desktop",
        name if name.starts_with("composio_") => "calling an app",
        _ => "working",
    }
}

/// A soft breathing pulse for the agent-bar dots (replaces the braille
/// spinner): the dot breathes in the agent's OWN color, rising and falling
/// with the tick — the same hue language as that agent's shimmer row.
pub(super) fn pulse_color(tick: usize, base: Color) -> Color {
    let depth = [0.55f32, 0.35, 0.15, 0.0, 0.15, 0.35][tick / 2 % 6];
    mix(base, (0, 0, 0), depth)
}

impl Tui {
    // ── Rendering ─────────────────────────────────────────────────────

    pub(super) fn render(&mut self, frame: &mut ratatui::Frame) {
        let input_height = input_box_height(&self.input, frame.area().height, frame.area().width);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),            // header
                Constraint::Min(4),               // feed
                Constraint::Length(1),            // agent bar
                Constraint::Length(1),            // status bar
                Constraint::Length(input_height), // input
            ])
            .split(frame.area());

        self.render_header(frame, rows[0]);
        self.render_feed(frame, rows[1]);
        self.render_agent_bar(frame, rows[2]);
        self.render_status_bar(frame, rows[3]);
        self.render_input(frame, rows[4]);
        self.render_command_menu(frame, rows[1], rows[4]);
        self.render_ask_modal(frame, rows[1]);
        self.render_resume_picker(frame, rows[1]);
    }

    /// The /resume session picker, floating centered over the feed.
    pub(super) fn render_resume_picker(&self, frame: &mut ratatui::Frame, feed_area: Rect) {
        let Some(picker) = &self.resume_picker else {
            return;
        };
        let width = feed_area.width.saturating_sub(6).clamp(40, 84);
        let inner = width.saturating_sub(6) as usize;
        let mut lines: Vec<Line> = Vec::new();
        for (i, (_, label)) in picker.items.iter().enumerate() {
            let hovered = i == picker.sel;
            let marker = if hovered { " ▸ " } else { "   " };
            let style = if hovered {
                Style::default().fg(TEXT)
            } else {
                Style::default().fg(MID)
            };
            let shown: String = label.chars().take(inner).collect();
            let mut line = Line::from(vec![
                Span::styled(marker.to_string(), Style::default().fg(ACCENT)),
                Span::styled(shown, style),
            ]);
            if hovered {
                line = line.style(Style::default().bg(SURFACE));
            }
            lines.push(line);
        }
        let height = (lines.len() as u16 + 2).min(feed_area.height);
        let area = Rect {
            x: feed_area.x + (feed_area.width.saturating_sub(width)) / 2,
            y: feed_area.y + (feed_area.height.saturating_sub(height)) / 2,
            width,
            height,
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(SUBTLE))
            .title(Span::styled(
                " resume a session ",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ))
            .title_bottom(Span::styled(
                " ↑↓ · Enter switches · Esc closes ",
                Style::default().fg(DIM),
            ));
        frame.render_widget(Clear, area);
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

    /// An agent's `ask_user` question, floating centered over the feed.
    /// One question at a time; options hover like the command menu, the last
    /// row is always free-text. The border carries the asking agent's hue —
    /// this is a live "the turn is waiting on you" moment.
    pub(super) fn render_ask_modal(&self, frame: &mut ratatui::Frame, feed_area: Rect) {
        let Some(modal) = &self.ask_modal else {
            return;
        };
        let q = &modal.questions[modal.current];
        let hue = support::agent_color(&modal.agent);
        let width = feed_area.width.saturating_sub(6).clamp(40, 76);
        let inner = width.saturating_sub(4) as usize;

        let mut lines: Vec<Line> = Vec::new();
        // The question itself, wrapped — the loudest text in the popup.
        for piece in wrap_plain(&q.question, inner) {
            lines.push(Line::from(Span::styled(
                format!(" {piece}"),
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            )));
        }
        lines.push(Line::default());
        let type_row = q.options.len();
        for (i, opt) in q.options.iter().enumerate() {
            let hovered = !modal.typing && modal.sel == i;
            let tick = if q.multi_select {
                if modal.checked.get(i).copied().unwrap_or(false) {
                    "▣ "
                } else {
                    "▢ "
                }
            } else {
                ""
            };
            let marker = if hovered { " ▸ " } else { "   " };
            let style = if hovered {
                Style::default().fg(TEXT)
            } else {
                Style::default().fg(MID)
            };
            let mut line = Line::from(vec![
                Span::styled(marker.to_string(), Style::default().fg(hue)),
                Span::styled(format!("{}. ", i + 1), Style::default().fg(DIM)),
                Span::styled(format!("{tick}{opt}"), style),
            ]);
            if hovered {
                line = line.style(Style::default().bg(SURFACE));
            }
            lines.push(line);
        }
        // The free-text row: a quiet prompt that becomes an input when
        // picked. Typed text WRAPS at the popup border (hard char-wrap, like
        // the main input box) so a long answer stays visible instead of
        // running out of the frame.
        let text_hovered = modal.typing || modal.sel == type_row;
        let marker = if text_hovered { " ▸ " } else { "   " };
        if modal.typing {
            let wrap_cols = inner.saturating_sub(5).max(10);
            let chars: Vec<char> = modal.text.chars().collect();
            let chunk_count = chars.len() / wrap_cols + 1;
            for chunk in 0..chunk_count {
                let start = chunk * wrap_cols;
                let end = ((chunk + 1) * wrap_cols).min(chars.len());
                let piece: String = chars[start..end].iter().collect();
                let mut spans = vec![Span::styled(
                    if chunk == 0 {
                        marker.to_string()
                    } else {
                        "   ".to_string()
                    },
                    Style::default().fg(hue),
                )];
                spans.push(Span::styled(
                    if chunk == 0 { "❯ " } else { "  " }.to_string(),
                    Style::default().fg(hue),
                ));
                spans.push(Span::styled(piece, Style::default().fg(TEXT)));
                if chunk == chunk_count - 1 {
                    spans.push(Span::styled("▏", Style::default().fg(hue)));
                }
                lines.push(Line::from(spans).style(Style::default().bg(SURFACE)));
            }
        } else {
            let mut spans = vec![Span::styled(marker.to_string(), Style::default().fg(hue))];
            spans.push(Span::styled(
                "type your own answer…".to_string(),
                Style::default().fg(if text_hovered { MID } else { DIM }),
            ));
            let mut text_line = Line::from(spans);
            if text_hovered {
                text_line = text_line.style(Style::default().bg(SURFACE));
            }
            lines.push(text_line);
        }

        let height = (lines.len() as u16 + 2).min(feed_area.height);
        let area = Rect {
            x: feed_area.x + (feed_area.width.saturating_sub(width)) / 2,
            y: feed_area.y + (feed_area.height.saturating_sub(height)) / 2,
            width,
            height,
        };
        let progress = if modal.questions.len() > 1 {
            format!(" {}/{}", modal.current + 1, modal.questions.len())
        } else {
            String::new()
        };
        let header = q.header.as_deref().unwrap_or("question");
        let hint = if modal.typing {
            "Enter answers · Esc back"
        } else if q.multi_select {
            "↑↓ · Space ticks · Enter answers · Esc dismisses"
        } else {
            "↑↓ · Enter answers · Esc dismisses"
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(hue))
            .title(Span::styled(
                format!(" {} asks — {header}{progress} ", modal.agent),
                Style::default().fg(hue).add_modifier(Modifier::BOLD),
            ))
            .title_bottom(Span::styled(format!(" {hint} "), Style::default().fg(DIM)));
        frame.render_widget(Clear, area);
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

    /// Slash-command popup floating just above the input box.
    pub(super) fn render_command_menu(
        &self,
        frame: &mut ratatui::Frame,
        feed_area: Rect,
        input_area: Rect,
    ) {
        let matches = self.menu_matches();
        if matches.is_empty() {
            return;
        }
        let selected = self.menu_sel.min(matches.len() - 1);
        let height = (matches.len() as u16 + 2).min(feed_area.height);
        let width = input_area.width.min(74).max(30);
        let area = Rect {
            x: input_area.x,
            y: input_area.y.saturating_sub(height),
            width,
            height,
        };
        let mut lines: Vec<Line> = Vec::new();
        for (i, (name, usage, description, _)) in matches.iter().enumerate() {
            let hovered = i == selected;
            // The hovered row reads as a solid selection bar, not just an
            // arrow: subtle surface background, accent name, brighter blurb.
            let (marker, name_style, desc_style) = if hovered {
                (
                    "▸ ",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                    Style::default().fg(MID),
                )
            } else {
                ("  ", Style::default().fg(MID), Style::default().fg(DIM))
            };
            let mut spans = vec![
                Span::styled(marker.to_string(), Style::default().fg(ACCENT)),
                Span::styled(format!("{name:<10}"), name_style),
                Span::styled(format!(" {description}"), desc_style),
            ];
            if hovered && usage != name {
                spans.push(Span::styled(
                    format!("  ({usage})"),
                    Style::default().fg(DIM),
                ));
            }
            let mut line = Line::from(spans);
            if hovered {
                line = line.style(Style::default().bg(SURFACE));
            }
            lines.push(line);
        }
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(SUBTLE))
            .title(Span::styled(
                " commands — ↑↓ · Enter · Tab ",
                Style::default().fg(DIM),
            ));
        frame.render_widget(Clear, area);
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

    pub(super) fn render_header(&self, frame: &mut ratatui::Frame, area: Rect) {
        // Static flame gradient across the wordmark — identity without motion.
        // The only things that ever animate are live work (shimmer, pulses).
        let mut spans: Vec<Span> = vec![
            Span::raw(" "),
            Span::styled("▲ ", Style::default().fg(FLAME[0])),
        ];
        for (i, ch) in "PHOENIX".chars().enumerate() {
            spans.push(Span::styled(
                ch.to_string(),
                Style::default()
                    .fg(FLAME[(i * FLAME.len()) / 7])
                    .add_modifier(Modifier::BOLD),
            ));
        }
        // The session id is an address, not a headline — first chunk only,
        // in hairline grey (the full id lives in /status).
        let id = &self.state.session_id;
        let short_id: String = if id.chars().count() > 14 {
            format!("{}…", id.chars().take(13).collect::<String>())
        } else {
            id.clone()
        };
        spans.push(Span::styled(
            format!("   {short_id}"),
            Style::default().fg(SUBTLE),
        ));
        let gateway = if self.gateway_ok {
            Line::from(vec![
                Span::styled("● ", Style::default().fg(mix(GREEN, (0, 0, 0), 0.25))),
                Span::styled("gateway ", Style::default().fg(DIM)),
            ])
        } else {
            Line::from(vec![
                Span::styled("○ ", Style::default().fg(RED)),
                Span::styled("gateway down ", Style::default().fg(RED)),
            ])
        };
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        frame.render_widget(Paragraph::new(gateway.right_aligned()), area);
    }

    pub(super) fn render_feed(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        let width = area.width.saturating_sub(3).max(10) as usize;
        let mut lines: Vec<Line> = Vec::new();
        for item in &self.feed {
            match item {
                FeedItem::Blank => lines.push(Line::raw("")),
                FeedItem::User(text) => {
                    lines.push(Line::raw(""));
                    lines.push(Line::from(vec![
                        Span::styled(
                            " ❯ ",
                            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            text.clone(),
                            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                        ),
                    ]));
                }
                FeedItem::Activity {
                    agent,
                    symbol,
                    text,
                    ok,
                } => {
                    // Settled rows read monochrome: grey text, quiet glyphs,
                    // the agent's name carrying only a whisper of its hue
                    // (full color is reserved for live shimmer rows).
                    // Narration reads at mid brightness with no glyph at all;
                    // failures are the one thing allowed to shout.
                    let name = short_agent(agent);
                    let mut spans: Vec<Span> = Vec::new();
                    let text_style = match (*ok, *symbol) {
                        (Some(false), _) => {
                            spans.push(Span::styled("  ✘ ", Style::default().fg(RED)));
                            Style::default().fg(RED)
                        }
                        (Some(true), _) => {
                            spans.push(Span::styled(
                                "  ✔ ",
                                Style::default().fg(mix(GREEN, (0, 0, 0), 0.25)),
                            ));
                            Style::default().fg(DIM)
                        }
                        (None, "∴") => {
                            spans.push(Span::raw("  "));
                            Style::default().fg(MID)
                        }
                        (None, "⇒" | "⇄" | "⧉" | "⇢") => {
                            spans.push(Span::styled(
                                format!("  {symbol} "),
                                Style::default().fg(settled(agent_color(agent))),
                            ));
                            Style::default().fg(MID)
                        }
                        _ => {
                            spans.push(Span::styled("  · ", Style::default().fg(SUBTLE)));
                            Style::default().fg(DIM)
                        }
                    };
                    if !name.is_empty() {
                        let (persona, role) = split_persona(&name);
                        spans.push(Span::styled(
                            persona.to_string(),
                            Style::default().fg(settled(agent_color(agent))),
                        ));
                        if !role.is_empty() {
                            spans.push(Span::styled(role.to_string(), Style::default().fg(SUBTLE)));
                        }
                        spans.push(Span::raw("  "));
                    }
                    spans.push(Span::styled(text.clone(), text_style));
                    lines.push(Line::from(spans));
                }
                FeedItem::Answer(markdown) => {
                    lines.extend(markdown_lines(markdown));
                }
                FeedItem::Diff { agent, diff } => {
                    // Modern diff: tinted backgrounds under +/- lines (editor
                    // style), the path header in the agent's settled hue.
                    for raw in diff.lines() {
                        let style = if raw.starts_with("@@") {
                            Style::default().fg(settled(agent_color(agent)))
                        } else if raw.starts_with('+') {
                            Style::default()
                                .fg(Color::Rgb(150, 220, 165))
                                .bg(Color::Rgb(26, 38, 28))
                        } else if raw.starts_with('-') {
                            Style::default()
                                .fg(Color::Rgb(240, 130, 120))
                                .bg(Color::Rgb(42, 26, 24))
                        } else {
                            Style::default().fg(DIM)
                        };
                        lines.push(Line::from(vec![
                            Span::styled("    ▏ ".to_string(), Style::default().fg(SUBTLE)),
                            Span::styled(truncate(raw, width.saturating_sub(8)), style),
                        ]));
                    }
                }
                FeedItem::AgentReturn {
                    agent,
                    subject,
                    body,
                    ok,
                } => {
                    // A returned result is a card: a colored left rule in the
                    // agent's hue, a bold header, and the body in quiet grey —
                    // scannable without shouting a wall of saturated text.
                    let color = agent_color(agent);
                    let bar = Span::styled("  ▌ ".to_string(), Style::default().fg(color));
                    let name = short_agent(agent);
                    let (persona, role) = split_persona(&name);
                    lines.push(Line::raw(""));
                    let mut header = vec![
                        bar.clone(),
                        Span::styled(
                            persona.to_string(),
                            Style::default().fg(color).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(role.to_string(), Style::default().fg(DIM)),
                    ];
                    if *ok {
                        header.push(Span::styled(
                            format!(" returned · {}", truncate(subject, 70)),
                            Style::default().fg(MID),
                        ));
                    } else {
                        header.push(Span::styled(
                            format!(" FAILED · {}", truncate(subject, 70)),
                            Style::default().fg(RED).add_modifier(Modifier::BOLD),
                        ));
                    }
                    lines.push(Line::from(header));
                    let body_width = width.saturating_sub(6).max(30);
                    for raw_line in body.lines().take(40) {
                        lines.push(Line::from(vec![
                            bar.clone(),
                            Span::styled(truncate(raw_line, body_width), Style::default().fg(DIM)),
                        ]));
                    }
                    let hidden = body.lines().count().saturating_sub(40);
                    if hidden > 0 {
                        lines.push(Line::from(vec![
                            bar.clone(),
                            Span::styled(
                                format!(
                                    "… +{hidden} more lines (the orchestrator has the full result)"
                                ),
                                Style::default().fg(SUBTLE).add_modifier(Modifier::ITALIC),
                            ),
                        ]));
                    }
                    lines.push(Line::raw(""));
                }
                FeedItem::Notice(text) => lines.push(Line::from(Span::styled(
                    format!("  {text}"),
                    Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
                ))),
                FeedItem::Error(text) => lines.push(Line::from(Span::styled(
                    format!("  ✘ {text}"),
                    Style::default().fg(RED),
                ))),
            }
        }
        // Live "thinking" shimmer row while a turn runs — plus the stream
        // ticker: the tail of what the model is thinking/writing RIGHT NOW.
        if let Some(started) = self.turn_started {
            // Transport plumbing ("waiting on <model>", stream byte counts)
            // is debug-only; the shimmer and the receipts are the story.
            let note = if self.stream_note.is_empty() || !self.state.show_debug {
                String::new()
            } else {
                format!(" · {}", self.stream_note.trim())
            };
            // No mode shows per-tool rows, so the working row carries the
            // live aggregate — the "called 12 tools, read 6 files" counter.
            let aggregate = if self.turn_activity.tool_calls > 0 {
                let stats = self.turn_activity;
                let mut parts = vec![format!("{} tools", stats.tool_calls)];
                if stats.reads > 0 {
                    parts.push(format!("{} reads", stats.reads));
                }
                if stats.edits > 0 {
                    parts.push(format!("{} edits", stats.edits));
                }
                if stats.commands > 0 {
                    parts.push(format!("{} cmds", stats.commands));
                }
                if stats.web > 0 {
                    parts.push(format!("{} web", stats.web));
                }
                if stats.failed_tool_calls > 0 {
                    parts.push(format!("{} failed", stats.failed_tool_calls));
                }
                format!(" · {}", parts.join(" · "))
            } else {
                String::new()
            };
            // The shimmering word is the animation; everything else on the
            // row stays dim and steady.
            let mut spans = vec![Span::raw("  ")];
            spans.extend(shimmer_spans("thinking…", self.tick, ACCENT));
            spans.push(Span::styled(
                format!(" {}", human_elapsed(started.elapsed())),
                Style::default().fg(DIM),
            ));
            if !aggregate.is_empty() {
                spans.push(Span::styled(aggregate, Style::default().fg(DIM)));
            }
            if !note.is_empty() {
                spans.push(Span::styled(note, Style::default().fg(DIM)));
            }
            lines.push(Line::from(spans));
        }
        // One live shimmer row per agent actively working RIGHT NOW —
        // background jobs, chain members, idle wakes — so the team stays
        // visible after the orchestrator's own turn ends. Each row carries
        // the agent's name, what it is doing, its job clock, and its own
        // running tally ("Canvas · thinking… 42s · read 3 files · 2 web/app
        // calls"). Moods light these rows from the event stream and retire
        // on completion/return.
        let mut wave_offset = 1usize;
        for (key, _) in AGENT_ORDER {
            if key == "orchestrator" && self.turn_started.is_some() {
                continue; // the turn row above already covers the orchestrator
            }
            let chip = &self.agents[key];
            let verb = match &chip.mood {
                AgentMood::Thinking => "thinking",
                AgentMood::Tool(name) => tool_verb(name),
                AgentMood::Idle | AgentMood::Done => continue,
            };
            let mut spans = vec![Span::raw("  ")];
            spans.extend(shimmer_spans(
                &format!("{} · {verb}…", chip.label),
                self.tick + wave_offset * 5,
                agent_color(key),
            ));
            if let Some(job) = self
                .background_jobs
                .iter()
                .rev()
                .find(|job| !job.done && agent_key(&job.agent) == key)
            {
                spans.push(Span::styled(
                    format!(" {}", human_elapsed(job.started.elapsed())),
                    Style::default().fg(DIM),
                ));
            }
            if let Some(tally) = self.live_counts.get(key) {
                if !tally.is_empty() {
                    spans.push(Span::styled(
                        format!(" · {}", tally.receipt()),
                        Style::default().fg(DIM),
                    ));
                }
            }
            lines.push(Line::from(spans));
            wave_offset += 1;
        }
        // Proof of life for long background generations (the "it hung" case):
        // the provider's streaming heartbeat renders dim under the working
        // rows while no user turn is running. Bytes and time only — no model
        // ids.
        if self.turn_started.is_none()
            && self.stream_note.trim_start().starts_with("model streaming")
            && self
                .agents
                .values()
                .any(|c| matches!(c.mood, AgentMood::Thinking | AgentMood::Tool(_)))
        {
            lines.push(Line::from(Span::styled(
                format!("  ⋯ {}", self.stream_note.trim()),
                Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
            )));
        }
        if self.turn_started.is_some() {
            if !self.live_stream.is_empty() {
                let tail: String = self.live_stream.replace('\n', " ");
                let window = 3 * width.saturating_sub(6).max(20);
                let chars = tail.chars().count();
                let visible: String = if chars > window {
                    tail.chars().skip(chars - window).collect()
                } else {
                    tail
                };
                lines.push(Line::from(Span::styled(
                    format!("  ⋯ {}", visible.trim_start()),
                    Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
                )));
            }
        }

        // No box, no animated border — the conversation is the page, framed
        // by whitespace. Liveness belongs to the shimmer rows and the agent
        // bar; scroll position lives in the status bar.
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        // EXACT wrapped height from ratatui's own wrap logic — a char-count
        // estimate undercounts word-boundary wrapping, which made max_scroll
        // too small and left the newest rows unreachable below the viewport.
        let total_rows = paragraph.line_count(area.width);
        let visible = area.height as usize;
        let max_scroll = total_rows.saturating_sub(visible) as u16;
        self.scroll_back = self.scroll_back.min(max_scroll);
        let offset = max_scroll.saturating_sub(self.scroll_back);
        frame.render_widget(paragraph.scroll((offset, 0)), area);
    }

    pub(super) fn render_agent_bar(&self, frame: &mut ratatui::Frame, area: Rect) {
        // Quiet roster: idle names sink to hairline grey (no dots, no
        // separators); whoever is alive carries a breathing dot, its color,
        // and a human verb. The bar answers "who's doing what" at a glance
        // without ever being loud.
        let mut spans: Vec<Span> = vec![Span::raw("  ")];
        for (key, _) in AGENT_ORDER.iter() {
            let chip = &self.agents[key];
            match &chip.mood {
                AgentMood::Idle => {
                    spans.push(Span::styled(
                        format!("{}   ", chip.label),
                        Style::default().fg(SUBTLE),
                    ));
                }
                AgentMood::Done => {
                    spans.push(Span::styled(
                        "✔ ",
                        Style::default().fg(mix(GREEN, (0, 0, 0), 0.25)),
                    ));
                    spans.push(Span::styled(
                        format!("{}   ", chip.label),
                        Style::default().fg(settled(agent_color(chip.label))),
                    ));
                }
                mood => {
                    // Alive: the dot breathes in the agent's own hue — the
                    // same color language as its shimmer row in the feed.
                    let verb = match mood {
                        AgentMood::Tool(name) => tool_verb(name),
                        _ => "thinking",
                    };
                    spans.push(Span::styled(
                        "● ",
                        Style::default().fg(pulse_color(self.tick, agent_color(chip.label))),
                    ));
                    spans.push(Span::styled(
                        chip.label.to_string(),
                        Style::default()
                            .fg(agent_color(chip.label))
                            .add_modifier(Modifier::BOLD),
                    ));
                    spans.push(Span::styled(
                        format!(" {verb}   "),
                        Style::default().fg(DIM),
                    ));
                }
            }
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    pub(super) fn context_ratio(&self) -> f64 {
        let used = self.ctx_tokens_estimate.min(self.context_window);
        used as f64 / self.context_window.max(1) as f64
    }

    pub(super) fn render_status_bar(&self, frame: &mut ratatui::Frame, area: Rect) {
        let ratio = self.context_ratio();
        let cells = 12usize;
        let filled = (ratio * cells as f64).round() as usize;
        let gauge_color = if ratio > 0.85 {
            RED
        } else if ratio > 0.6 {
            ACCENT
        } else {
            GREEN
        };
        // The gauge is the headline; everything after it is one dim line of
        // facts. Thin bar glyphs, no heavy blocks.
        let mut spans = vec![
            Span::styled("  ctx ", Style::default().fg(DIM)),
            Span::styled("━".repeat(filled), Style::default().fg(gauge_color)),
            Span::styled("━".repeat(cells - filled), Style::default().fg(SUBTLE)),
            Span::styled(
                format!(" {:.0}%", ratio * 100.0),
                Style::default()
                    .fg(gauge_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    " ~{}/{}",
                    format_count(self.ctx_tokens_estimate),
                    format_count(self.context_window)
                ),
                Style::default().fg(SUBTLE),
            ),
        ];
        if self.scroll_back > 0 {
            spans.push(Span::styled(
                format!("  ·  ↑{} scrolled", self.scroll_back),
                Style::default().fg(ACCENT),
            ));
        }
        // Session-cumulative processed tokens and tool-output compression are
        // debug-only stats — they scared users (a 4.1M "tokens" reading is NOT
        // context usage). Surface them under /view debug, never on the default bar.
        if self.state.show_debug {
            spans.push(Span::styled(
                format!("  ·  session {} tok", format_count(self.session_tokens)),
                Style::default().fg(DIM),
            ));
            if self.compression_saved_tokens > 0 {
                let ratio = if self.compression_raw_tokens > 0 {
                    format!(
                        " ({:.0}% of {})",
                        100.0 * self.compression_saved_tokens as f64
                            / self.compression_raw_tokens as f64,
                        format_count(self.compression_raw_tokens)
                    )
                } else {
                    String::new()
                };
                spans.push(Span::styled(
                    format!(
                        "  ·  tools compressed ~{} tok{ratio}",
                        format_count(self.compression_saved_tokens)
                    ),
                    Style::default().fg(mix(GREEN, (0, 0, 0), 0.25)),
                ));
            }
        }
        spans.push(Span::styled(
            format!("  ·  route {}", self.last_route),
            Style::default().fg(DIM),
        ));
        if !self.queued_inputs.is_empty() {
            spans.push(Span::styled(
                format!("  ·  queued {}", self.queued_inputs.len()),
                Style::default().fg(ACCENT),
            ));
        }
        if let Some(started) = self.turn_started {
            spans.push(Span::styled(
                format!("  ·  turn {}", human_elapsed(started.elapsed())),
                Style::default().fg(ACCENT),
            ));
        } else if self.turn_elapsed_last > 0.0 {
            // Sub-minute turns keep the decimal (that precision reads well);
            // longer ones humanize like the live clocks.
            let last = if self.turn_elapsed_last < 60.0 {
                format!("{:.1}s", self.turn_elapsed_last)
            } else {
                human_elapsed(std::time::Duration::from_secs_f32(self.turn_elapsed_last))
            };
            spans.push(Span::styled(
                format!("  ·  last {last}"),
                Style::default().fg(DIM),
            ));
        }
        if self.state.yolo {
            spans.push(Span::styled("  ·  ", Style::default().fg(SUBTLE)));
            spans.push(Span::styled(
                "YOLO",
                Style::default().fg(RED).add_modifier(Modifier::BOLD),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    pub(super) fn render_input(&self, frame: &mut ratatui::Frame, area: Rect) {
        // The frame stays a quiet hairline in every state — a full accent
        // rectangle is the loudest thing a TUI can draw, and the accent
        // prompt glyph + cursor already say "type here". While a turn runs
        // the hint rides in the border title, dim.
        let hint = if self.turn_running() {
            " working — Enter queues · Esc stops "
        } else {
            ""
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(SUBTLE))
            .title(Span::styled(hint, Style::default().fg(DIM)));
        let view = input_view(&self.input, self.cursor, area.width, area.height);
        let lines = view
            .lines
            .iter()
            .enumerate()
            .map(|(idx, text)| {
                let prompt = if view.first_visible_line + idx == 0 {
                    "❯ "
                } else {
                    "  "
                };
                Line::from(vec![
                    Span::styled(
                        prompt.to_string(),
                        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(text.clone(), Style::default().fg(TEXT)),
                ])
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(lines).block(block), area);
        frame.set_cursor_position(Position::new(
            area.x + 3 + view.cursor_col as u16,
            area.y + 1 + view.cursor_row as u16,
        ));
    }
}

#[cfg(test)]
mod elapsed_tests {
    use super::human_elapsed;
    use std::time::Duration;

    #[test]
    fn elapsed_label_scales_seconds_minutes_hours_days() {
        assert_eq!(human_elapsed(Duration::from_secs(0)), "0s");
        assert_eq!(human_elapsed(Duration::from_secs(42)), "42s");
        assert_eq!(human_elapsed(Duration::from_secs(72)), "1m 12s");
        assert_eq!(human_elapsed(Duration::from_secs(59 * 60 + 59)), "59m 59s");
        // Hours drop the seconds entirely.
        assert_eq!(
            human_elapsed(Duration::from_secs(2 * 3600 + 5 * 60 + 33)),
            "2h 5m"
        );
        assert_eq!(
            human_elapsed(Duration::from_secs(86_400 + 3 * 3600)),
            "1d 3h"
        );
    }
}

/// Greedy word-wrap for plain text (no styling seams inside words).
fn wrap_plain(text: &str, width: usize) -> Vec<String> {
    let w = width.max(20);
    let mut out = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current = word.to_string();
        } else if current.chars().count() + 1 + word.chars().count() <= w {
            current.push(' ');
            current.push_str(word);
        } else {
            out.push(current);
            current = word.to_string();
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}
