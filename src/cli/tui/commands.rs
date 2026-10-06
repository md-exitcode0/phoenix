//! Slash commands: /help, /view, /resume, /defaults, /cron, /status rows,
//! and the compact report.

use super::*;

impl Tui {
    pub(super) fn slash_command(&mut self, raw: &str) {
        let command = raw.split_whitespace().next().unwrap_or("");
        match command {
            "/help" => {
                for line in [
                    "/stop — stop the running turn; queued messages run next (also: Esc)",
                    "/clear — clear the screen (session unchanged)",
                    "/new — start a fresh session · /resume — pick a past session from a menu",
                    "/status — session, models, gateway, context · /model — configured models",
                    "/provider <id> [api-key] [model] — switch LLM lane (ollama | ollama-cloud | openai-codex)",
                    "/memory — memory store summary by tier",
                    "/burn — where tokens and minutes go (per agent, model, day)",
                    "/mcp — connected MCP servers (manage with `phoenix configure`)",
                    "/compact — session compaction/archive state",
                    "/checkpoints — rewindable edit checkpoints · /rewind — undo last edit turn",
                    "/cron add <when> :: <prompt> · /cron list · /cron rm <id> — scheduled wake-ups",
                    "    when = every 10m | daily 09:30 | in 45m | at 2026-06-12T08:00",
                    "/yolo · /safe — tool confinement off / on",
                    "/view actions|thinking|debug — toggle feed row kinds",
                    "/defaults save — persist yolo/actions/thinking/debug toggles",
                    "/quit — exit (a running turn finishes on the gateway)",
                    "type / for the command menu — ↑↓ hover, Enter pick, Tab fill",
                    "Alt-Enter or Shift-Enter inserts a newline; Enter sends",
                    "Esc — stop the running turn (or clear typed input) · PageUp/PageDown scroll · Ctrl-C twice quits",
                ] {
                    self.feed.push(FeedItem::Notice(line.to_string()));
                }
            }
            // Quit-digest note: the run loop fires the session digest on exit
            // (covers /quit AND Ctrl-C-twice), so this arm only sets the flag.
            "/quit" | "/exit" | "/q" => self.should_quit = true,
            "/stop" => {
                if self.turn_running() {
                    self.stop_running_turn("/stop");
                } else {
                    self.feed
                        .push(FeedItem::Notice("no turn is running".to_string()));
                }
            }
            "/new" => {
                // Switching away ends the old session just like /quit does —
                // digest it now so the fresh session can recall it immediately,
                // and snapshot its screen so /resume restores it verbatim.
                self.persist_feed_now();
                let ending = self.state.session_id.clone();
                tokio::spawn(async move {
                    let _ = crate::cli::daemon::request_session_digest(ending).await;
                });
                let new_session_id = format!("main-{}", uuid::Uuid::new_v4());
                let last_session = crate::config::phoenix_home().join("last_session");
                if let Err(error) = crate::config::private_io::atomic_write_private(
                    &last_session,
                    new_session_id.as_bytes(),
                ) {
                    self.feed.push(FeedItem::Error(format!(
                        "could not create a resumable session: {error:#}"
                    )));
                    return;
                }
                self.state.session_id = new_session_id;
                self.session_tokens = 0;
                self.refresh_ctx_estimate();
                // A new session is a clean slate: clear the old transcript so
                // the switch is unmistakable, then greet like a fresh start.
                // (Appending one dim notice under a full replayed feed read as
                // "nothing happened".)
                self.feed.clear();
                self.feed_saved_len = 0;
                self.main_msgs_seen = 0;
                self.scroll_back = 0;
                self.splash();
                self.feed.push(FeedItem::Notice(format!(
                    "new session: {}",
                    self.state.session_id
                )));
            }
            "/provider" => {
                let parts: Vec<&str> = raw.split_whitespace().skip(1).collect();
                if parts.is_empty() {
                    for line in [
                        "usage: /provider <id> [api-key] [model]",
                        "  /provider ollama [model] — local Ollama, no key",
                        "  /provider ollama-cloud <api-key> [model] — Ollama Cloud (key saved to auth-profiles)",
                        "  /provider openai-codex — back to the Codex OAuth lane",
                    ] {
                        self.feed.push(FeedItem::Notice(line.to_string()));
                    }
                } else {
                    let id = parts[0];
                    let (key, model) = if id == "ollama-cloud" {
                        (parts.get(1).copied(), parts.get(2).copied())
                    } else {
                        (None, parts.get(1).copied())
                    };
                    match crate::cli::commands::switch_provider_config(id, model, key) {
                        Ok(receipt) => self.feed.push(FeedItem::Notice(receipt)),
                        Err(error) => self.feed.push(FeedItem::Notice(error.to_string())),
                    }
                }
            }
            "/cron" => self.cron_command(raw),
            "/checkpoints" => {
                let store =
                    crate::tools::checkpoint::CheckpointStore::new(&crate::config::phoenix_home());
                match store.scopes_result() {
                    Ok(scopes) if scopes.is_empty() => {
                        self.feed.push(FeedItem::Notice(
                            "no checkpoints — nothing the agent edited is rewindable yet"
                                .to_string(),
                        ));
                    }
                    Ok(scopes) => {
                        for scope in scopes.iter().take(10) {
                            self.feed.push(FeedItem::Notice(format!(
                                "checkpoint {} · session {} · {} file(s)",
                                scope.scope, scope.session, scope.files
                            )));
                        }
                        self.feed.push(FeedItem::Notice(
                            "/rewind restores the newest and consumes it".to_string(),
                        ));
                    }
                    Err(error) => self.feed.push(FeedItem::Error(format!(
                        "checkpoints unreadable: {error:#}"
                    ))),
                }
            }
            "/rewind" => {
                let store =
                    crate::tools::checkpoint::CheckpointStore::new(&crate::config::phoenix_home());
                let workspace = std::env::current_dir().unwrap_or_default();
                match store.rewind_latest(&workspace) {
                    Ok(None) => self
                        .feed
                        .push(FeedItem::Notice("no checkpoints to rewind".to_string())),
                    Ok(Some(report)) => {
                        for path in &report.restored {
                            self.feed.push(FeedItem::Notice(format!("restored {path}")));
                        }
                        for path in &report.deleted {
                            self.feed.push(FeedItem::Notice(format!(
                                "deleted {path} (was created by the agent)"
                            )));
                        }
                        self.feed.push(FeedItem::Notice(format!(
                            "rewound checkpoint {} ({} restored, {} deleted)",
                            report.scope,
                            report.restored.len(),
                            report.deleted.len()
                        )));
                    }
                    Err(error) => self
                        .feed
                        .push(FeedItem::Error(format!("rewind failed: {error:#}"))),
                }
            }
            "/yolo" => {
                self.state.yolo = true;
                self.feed.push(FeedItem::Notice(
                    "yolo ON — tools run unconfined".to_string(),
                ));
            }
            "/safe" => {
                self.state.yolo = false;
                self.feed
                    .push(FeedItem::Notice("workspace confinement ON".to_string()));
            }
            "/mcp" => {
                let servers = crate::config::PhoenixConfig::load()
                    .map(|c| c.profile.mcp_servers)
                    .unwrap_or_default();
                if servers.is_empty() {
                    self.feed.push(FeedItem::Notice(
                        "no MCP servers connected — add one with `phoenix configure` → MCP servers"
                            .to_string(),
                    ));
                } else {
                    for server in &servers {
                        let transport = if server.is_remote() {
                            format!("remote · {}", server.url.as_deref().unwrap_or(""))
                        } else {
                            format!("local · {}", server.command)
                        };
                        let mut line = format!("● {} — {transport}", server.name);
                        if let Some(route) = server.route.as_deref() {
                            line.push_str(&format!(" · for: {route}"));
                        }
                        if !server.enabled {
                            line.push_str(" · parked");
                        }
                        self.feed.push(FeedItem::Notice(line));
                    }
                    self.feed.push(FeedItem::Notice(
                        "agents reach these via mcp_servers/mcp_call · manage with `phoenix configure` → MCP servers"
                            .to_string(),
                    ));
                }
            }
            "/actions" => self.toggle_view("actions"),
            "/thinking" => self.toggle_view("thinking"),
            "/debug" => self.toggle_view("debug"),
            "/subagents" => self.toggle_view("subagents"),
            "/display" => {
                use crate::cli::DisplayMode;
                let arg = raw.split_whitespace().nth(1).unwrap_or("");
                if arg.is_empty() {
                    // Bare `/display` opens the option picker instead of
                    // silently cycling: refill the input so the menu lists
                    // the three modes (arrows to hover, Enter applies).
                    // Nobody should have to memorize the mode names.
                    self.input = "/display ".to_string();
                    self.cursor = self.input.chars().count();
                    self.menu_sel = 0;
                    return;
                }
                match DisplayMode::parse(arg) {
                    Some(mode) => {
                        self.state.display_mode = mode;
                        let blurb = match mode {
                            DisplayMode::Compact => "narration + aggregated work receipts",
                            DisplayMode::Verbose => "the same journal + red/green edit diffs",
                        };
                        self.feed.push(FeedItem::Notice(format!(
                            "display mode: {} — {blurb} (persist with /defaults save)",
                            mode.label()
                        )));
                    }
                    None => self.feed.push(FeedItem::Notice(
                        "usage: /display <compact|verbose> — or bare /display for the picker"
                            .to_string(),
                    )),
                }
            }
            "/defaults" => self.defaults_command(raw),
            "/view" => {
                let target = raw.split_whitespace().nth(1).unwrap_or("");
                if matches!(target, "actions" | "thinking" | "debug") {
                    self.toggle_view(target);
                } else {
                    self.feed.push(FeedItem::Notice(
                        "usage: /view actions|thinking|debug".to_string(),
                    ));
                }
            }
            "/clear" => {
                self.feed.clear();
                self.scroll_back = 0;
                // A cleared screen is a fresh start: index the pending memory
                // backlog now (detached on the gateway) so everything already
                // saved is searchable from here on. The session itself is
                // untouched, so there is no digest to run.
                tokio::spawn(async {
                    let _ = crate::cli::daemon::request_memory_index().await;
                });
                self.feed.push(FeedItem::Notice(format!(
                    "screen cleared — session {} unchanged",
                    self.state.session_id
                )));
            }
            "/status" | "/session" => {
                self.feed.push(FeedItem::Notice(format!(
                    "session {} · permissions {} ({}) · workspace {} · gateway {}",
                    self.state.session_id,
                    self.state.permission_label(),
                    self.state.permission_detail(),
                    std::env::current_dir()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|_| "?".to_string()),
                    if self.gateway_ok { "up" } else { "DOWN" },
                )));
                self.feed.push(FeedItem::Notice(format!(
                    "context ~{}/{} ({:.0}%)",
                    format_count(self.ctx_tokens_estimate),
                    format_count(self.context_window),
                    self.context_ratio() * 100.0,
                )));
                self.feed.push(FeedItem::Notice(format!(
                    "processed this session · {} model tokens{}",
                    format_count(self.session_tokens),
                    if self.compression_saved_tokens > 0 {
                        if self.compression_raw_tokens > 0 {
                            format!(
                                " · tool output ~{} of {} tokens compressed out of context ({:.0}%)",
                                format_count(self.compression_saved_tokens),
                                format_count(self.compression_raw_tokens),
                                100.0 * self.compression_saved_tokens as f64
                                    / self.compression_raw_tokens as f64
                            )
                        } else {
                            format!(
                                " · tool output compressed ~{} tokens out of context",
                                format_count(self.compression_saved_tokens)
                            )
                        }
                    } else {
                        String::new()
                    }
                )));
                if let Ok(config) = crate::config::PhoenixConfig::load() {
                    let llm = &config.profile.llm;
                    self.feed.push(FeedItem::Notice(format!(
                        "models · orchestrator {} · specialist {} (provider {})",
                        llm.orchestrator(),
                        llm.specialist(),
                        llm.provider,
                    )));
                }
                self.feed
                    .push(FeedItem::Notice(runtime_binary_status_line()));
                self.feed
                    .push(FeedItem::Notice(crate::tools::composio::local_status_line()));
                self.feed.push(FeedItem::Notice(
                    crate::tools::browser_native::local_status_line(),
                ));
                self.feed
                    .push(FeedItem::Notice(crate::tools::computer_local_status_line()));
                if let Some(run) = &self.state.last_run {
                    self.feed.push(FeedItem::Notice(format!(
                        "last run {} · route {} · trace {}",
                        run.run_id,
                        run.route,
                        run.trace_path.display()
                    )));
                }
                if let Some(compaction) = &self.last_context_compaction {
                    self.feed.push(FeedItem::Notice(format!(
                        "last context compaction · {compaction}"
                    )));
                }
                if let Some(line) = context_archive_status_line(&self.state.session_id) {
                    self.feed.push(FeedItem::Notice(line));
                }
                self.push_queued_status_rows();
                self.push_background_status_rows();
            }
            "/model" => match crate::config::PhoenixConfig::load() {
                Ok(config) => {
                    let llm = &config.profile.llm;
                    self.feed.push(FeedItem::Notice(format!(
                        "orchestrator {} · specialist {} · provider {}",
                        llm.orchestrator(),
                        llm.specialist(),
                        llm.provider,
                    )));
                    self.feed.push(FeedItem::Notice(
                            "change models with `phoenix onboard` (or edit ~/.phoenix/config.toml), then restart the gateway".to_string(),
                        ));
                }
                Err(error) => self
                    .feed
                    .push(FeedItem::Error(format!("config unreadable: {error:#}"))),
            },
            "/burn" => {
                // Token/latency burn-down from the runtime's own telemetry
                // (donor idea: codeburn) — per agent, model, and day.
                match crate::cli::burn::load_rows_result(&crate::config::phoenix_home()) {
                    Ok(rows) => {
                        for line in crate::cli::burn::render(&rows) {
                            self.feed.push(FeedItem::Notice(line));
                        }
                    }
                    Err(error) => self.feed.push(FeedItem::Error(format!(
                        "burn telemetry unreadable: {error:#}"
                    ))),
                }
            }
            "/memory" => {
                // Cognee knowledge-graph store (the file tiers are archived).
                let root = crate::config::phoenix_cognee_root();
                let size: u64 = walkdir_size(&root);
                self.feed.push(FeedItem::Notice(if size == 0 {
                    format!("memory graph empty ({})", root.display())
                } else {
                    format!(
                        "memory graph ~{} KB on disk ({}) — `phoenix memory <query>` recalls, `phoenix memory --graph` renders the HTML viewer",
                        size / 1024,
                        root.display()
                    )
                }));
            }
            "/compact" => self.push_compact_report(),
            "/resume" => self.resume_command(raw),
            other => self.feed.push(FeedItem::Notice(format!(
                "unknown command {other} — /help lists what the app supports"
            ))),
        }
    }

    pub(super) fn defaults_command(&mut self, raw: &str) {
        match raw.split_whitespace().nth(1) {
            Some("save") => match crate::cli::prefs::save(self.state.current_defaults()) {
                Ok(path) => self.feed.push(FeedItem::Notice(format!(
                    "saved current toggles (yolo/actions/thinking/debug) as defaults · {}",
                    path.display()
                ))),
                Err(error) => self.feed.push(FeedItem::Error(format!(
                    "failed to save defaults: {error:#}"
                ))),
            },
            _ => self.feed.push(FeedItem::Notice(
                "usage: /defaults save — persists current yolo/actions/thinking/debug toggles"
                    .to_string(),
            )),
        }
    }

    pub(super) fn push_background_status_rows(&mut self) {
        if self.background_jobs.is_empty() {
            return;
        }
        let running = self.background_jobs.iter().filter(|job| !job.done).count();
        self.feed.push(FeedItem::Notice(format!(
            "background specialists · {} running · {} total seen",
            running,
            self.background_jobs.len()
        )));
        let mut rows = self
            .background_jobs
            .iter()
            .rev()
            .take(5)
            .map(|job| {
                format!(
                    "{} · {} · {} ({})",
                    if job.done { "done" } else { "running" },
                    crate::runtime::delegation::agent_display_name(&job.agent),
                    truncate(&job.subject, 70),
                    truncate(&job.status, 90)
                )
            })
            .collect::<Vec<_>>();
        rows.reverse();
        for row in rows {
            self.feed.push(FeedItem::Notice(row));
        }
    }

    pub(super) fn push_queued_status_rows(&mut self) {
        if self.queued_inputs.is_empty() {
            return;
        }
        self.feed.push(FeedItem::Notice(format!(
            "queued user messages · {} waiting",
            self.queued_inputs.len()
        )));
        for (index, text) in self.queued_inputs.iter().take(5).enumerate() {
            self.feed.push(FeedItem::Notice(format!(
                "queued #{} · {}",
                index + 1,
                truncate(text, 90)
            )));
        }
        if self.queued_inputs.len() > 5 {
            self.feed.push(FeedItem::Notice(format!(
                "queued · {} more not shown",
                self.queued_inputs.len() - 5
            )));
        }
    }

    pub(super) fn push_compact_report(&mut self) {
        self.refresh_ctx_estimate();
        self.feed
            .push(FeedItem::Notice("Session Compaction".to_string()));
        self.feed.push(FeedItem::Notice(format!(
            "session {} · context ~{}/{} ({:.0}%)",
            self.state.session_id,
            format_count(self.ctx_tokens_estimate),
            format_count(self.context_window),
            self.context_ratio() * 100.0
        )));

        let transcript = session_file(&self.state.session_id);
        let (messages, tool_results) = session_message_counts(&transcript).unwrap_or((0, 0));
        let transcript_size = std::fs::metadata(&transcript)
            .map(|meta| format_bytes(meta.len()))
            .unwrap_or_else(|_| "missing".to_string());
        self.feed.push(FeedItem::Notice(format!(
            "transcript · {} messages · {} tool results · size {} · {}",
            format_count(messages as u64),
            format_count(tool_results as u64),
            transcript_size,
            transcript.display()
        )));

        if let Some(line) = context_archive_status_line(&self.state.session_id) {
            self.feed.push(FeedItem::Notice(line));
        } else {
            self.feed.push(FeedItem::Notice(
                "context archive · none yet; no durable history has been folded for this session"
                    .to_string(),
            ));
        }

        if let Some(compaction) = &self.last_context_compaction {
            self.feed.push(FeedItem::Notice(format!(
                "last context compaction · {compaction}"
            )));
        } else {
            self.feed.push(FeedItem::Notice(
                "last context compaction · none observed in this TUI run".to_string(),
            ));
        }
        self.feed.push(FeedItem::Notice(
            "durable session auto-compaction runs before oversized model rounds; tool-output compression is separate and shown in /status"
                .to_string(),
        ));
    }

    /// `/resume` — list recent sessions; `/resume <id-prefix>` switches to one.
    pub(super) fn resume_command(&mut self, raw: &str) {
        let sessions_dir = crate::config::phoenix_home().join("sessions");
        let mut sessions: Vec<(String, std::time::SystemTime)> = std::fs::read_dir(&sessions_dir)
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|entry| {
                        let name = entry.file_name().to_string_lossy().to_string();
                        // Main sessions only — specialist transcripts ride along.
                        let id = name.strip_suffix(".json")?;
                        if id.contains("__") {
                            return None;
                        }
                        let mtime = entry.metadata().ok()?.modified().ok()?;
                        Some((id.to_string(), mtime))
                    })
                    .collect()
            })
            .unwrap_or_default();
        sessions.sort_by(|a, b| b.1.cmp(&a.1));

        let arg = raw.split_whitespace().nth(1).unwrap_or("");
        if arg.is_empty() {
            // Bare /resume opens the PICKER: a selectable list (↑↓ + Enter
            // switches, Esc closes) — nobody should have to retype an id.
            if sessions.is_empty() {
                self.feed
                    .push(FeedItem::Notice("no saved sessions yet".to_string()));
                return;
            }
            let now = std::time::SystemTime::now();
            let items: Vec<(String, String)> = sessions
                .iter()
                .take(15)
                .map(|(id, mtime)| {
                    let age_mins = now
                        .duration_since(*mtime)
                        .map(|d| d.as_secs() / 60)
                        .unwrap_or(0);
                    let age = if age_mins < 60 {
                        format!("{age_mins}m ago")
                    } else if age_mins < 60 * 24 {
                        format!("{}h ago", age_mins / 60)
                    } else {
                        format!("{}d ago", age_mins / (60 * 24))
                    };
                    let marker = if *id == self.state.session_id {
                        " (current)"
                    } else {
                        ""
                    };
                    let name = session_title(&sessions_dir.join(format!("{id}.json")))
                        .unwrap_or_else(|| id.clone());
                    (id.clone(), format!("{name} · {age}{marker}"))
                })
                .collect();
            self.resume_picker = Some(super::ResumePicker { items, sel: 0 });
            return;
        }

        // Match the query against the human name OR the id (case-insensitive),
        // so the user resumes by what they see ("reddit", "browser") or a raw id.
        let needle = arg.to_lowercase();
        let matches: Vec<(String, String)> = sessions
            .iter()
            .filter_map(|(id, _)| {
                let name = session_title(&sessions_dir.join(format!("{id}.json")))
                    .unwrap_or_else(|| id.clone());
                if id.to_lowercase().contains(&needle) || name.to_lowercase().contains(&needle) {
                    Some((id.clone(), name))
                } else {
                    None
                }
            })
            .collect();
        match matches.as_slice() {
            [] => self.feed.push(FeedItem::Notice(format!(
                "no session matches `{arg}` — bare /resume lists them"
            ))),
            [(id, name)] => self.switch_to_session(&id.clone(), &name.clone()),
            many => {
                self.feed.push(FeedItem::Notice(format!(
                    "`{arg}` matches {} sessions — be more specific:",
                    many.len()
                )));
                for (_, name) in many.iter().take(5) {
                    self.feed.push(FeedItem::Notice(format!("  {name}")));
                }
            }
        }
    }

    /// Switch the TUI onto another saved session (the /resume core).
    pub(super) fn switch_to_session(&mut self, id: &str, name: &str) {
        // Snapshot the outgoing session's screen before leaving it,
        // so switching back later restores it verbatim.
        self.persist_feed_now();
        if let Err(error) = crate::session::SessionStore::validate_session_id(id) {
            self.feed.push(FeedItem::Error(format!(
                "cannot resume invalid session id `{id}`: {error}"
            )));
            return;
        }
        let last_session = crate::config::phoenix_home().join("last_session");
        if let Err(error) =
            crate::config::private_io::atomic_write_private(&last_session, id.as_bytes())
        {
            self.feed.push(FeedItem::Error(format!(
                "could not persist the resumed session: {error:#}"
            )));
            return;
        }
        self.state.session_id = id.to_string();
        self.session_tokens = 0;
        self.refresh_ctx_estimate();
        self.feed.clear();
        self.feed_saved_len = 0;
        self.main_msgs_seen = 0;
        self.scroll_back = 0;
        self.splash();
        // Bring back that session's screen exactly as it was left
        // (snapshot first, transcript reconstruction as fallback).
        self.replay_session();
        self.feed.push(FeedItem::Notice(format!(
            "resumed “{name}” — its context loads with your next message"
        )));
    }

    /// `/cron` — list by default; `add <when> :: <prompt>`; `rm <id>`.
    pub(super) fn cron_command(&mut self, raw: &str) {
        let rest = raw.strip_prefix("/cron").unwrap_or("").trim();
        let (verb, args) = match rest.split_once(' ') {
            Some((v, a)) => (v, a.trim()),
            None => (rest, ""),
        };
        match verb {
            "" | "list" => {
                let entries = match crate::cron::load() {
                    Ok(entries) => entries,
                    Err(error) => {
                        self.feed.push(FeedItem::Error(format!(
                            "could not load crons without risking data loss: {error:#}"
                        )));
                        return;
                    }
                };
                if entries.is_empty() {
                    self.feed.push(FeedItem::Notice(
                        "no crons — /cron add every 30m :: check my inbox".to_string(),
                    ));
                    return;
                }
                for e in entries {
                    self.feed.push(FeedItem::Notice(format!(
                        "{} · {} · next {} · [{}] {}{}",
                        e.id,
                        e.describe_schedule(),
                        e.next_run
                            .with_timezone(&chrono::Local)
                            .format("%m-%d %H:%M"),
                        e.session_id,
                        truncate(&e.prompt, 60),
                        if e.enabled { "" } else { " (disabled)" },
                    )));
                }
            }
            "add" => {
                let Some((when, prompt)) = args.split_once("::") else {
                    self.feed.push(FeedItem::Error(
                        "usage: /cron add <when> :: <prompt>   (when = every 10m | daily 09:30 | in 45m | at 2026-06-12T08:00)"
                            .to_string(),
                    ));
                    return;
                };
                match crate::cron::add(&self.state.session_id, when.trim(), prompt.trim()) {
                    Ok(entry) => self.feed.push(FeedItem::Notice(format!(
                        "cron {} created — {} · next {} · wakes this session with: {}",
                        entry.id,
                        entry.describe_schedule(),
                        entry
                            .next_run
                            .with_timezone(&chrono::Local)
                            .format("%m-%d %H:%M"),
                        truncate(&entry.prompt, 80),
                    ))),
                    Err(error) => self.feed.push(FeedItem::Error(format!("{error:#}"))),
                }
            }
            "rm" | "remove" | "delete" => match crate::cron::remove(args) {
                Ok(entry) => self.feed.push(FeedItem::Notice(format!(
                    "cron {} deleted ({})",
                    entry.id,
                    entry.describe_schedule()
                ))),
                Err(error) => self.feed.push(FeedItem::Error(format!("{error:#}"))),
            },
            other => self.feed.push(FeedItem::Error(format!(
                "unknown /cron verb `{other}` — use add, list, or rm"
            ))),
        }
    }
}
