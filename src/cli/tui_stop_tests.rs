use super::*;

fn test_app() -> Tui {
    // A brand-new session id never resolves to a file, so replay/ctx are
    // no-ops — the App is a pure in-memory state machine for these tests.
    Tui::new(
        AppState::new(false, true, "main-stop-test".to_string()),
        false,
    )
}

#[tokio::test]
async fn esc_stops_instantly_and_swallows_the_dying_turns_events() {
    let mut app = test_app();
    app.turn_started = Some(Instant::now());
    assert!(app.turn_running());

    app.stop_running_turn("Esc");
    // The whole point: the spinner is GONE the instant stop is pressed,
    // without waiting for the gateway's abort to round-trip back.
    assert!(
        !app.turn_running(),
        "stop must clear the working state immediately"
    );
    assert!(app.cancelling);

    // The aborted turn may still emit a couple of in-flight events; none
    // of them may repaint activity or revive the spinner.
    let feed_len = app.feed.len();
    app.on_turn_event(CliEvent::Thinking);
    app.on_turn_event(CliEvent::Reasoning("late".to_string()));
    assert_eq!(
        app.feed.len(),
        feed_len,
        "events from a stopped turn are ignored"
    );
    assert!(!app.turn_running());

    // When the abort finally lands, cancel state clears and no redundant
    // "turn stopped by user" error is tacked on (we already showed it).
    app.on_turn_finished(RemoteOutcome::Error("turn stopped by user".to_string()));
    assert!(!app.cancelling);
    assert!(
        !app.feed
            .iter()
            .any(|i| matches!(i, FeedItem::Error(m) if m.contains("stopped by user"))),
        "the stop outcome must not double-report as an error"
    );
}

#[tokio::test]
async fn compact_mode_journals_tools_into_receipts_at_narrative_beats() {
    let mut app = test_app();
    app.state.display_mode = crate::cli::DisplayMode::Compact;
    let completed = |tool: &str, n: u32| CliEvent::ToolCallCompleted {
        agent: "coder".to_string(),
        tool_name: tool.to_string(),
        input_summary: format!("{tool}-input-{n}"),
        success: true,
        output_summary: "ok".to_string(),
        diff: None,
    };
    // Three reads and a command accumulate silently…
    app.on_turn_event(completed("read", 1));
    app.on_turn_event(completed("read", 2));
    app.on_turn_event(completed("read", 3));
    app.on_turn_event(completed("bash", 4));
    assert!(
        !app.feed
            .iter()
            .any(|i| matches!(i, FeedItem::Activity { text, .. } if text.contains("read 3 files"))),
        "no receipt before a narrative beat"
    );
    // …then the agent narrates — the receipt lands FIRST, then the narration.
    app.on_turn_event(CliEvent::Reasoning(
        "Now that that's done, I'll check the config file.".to_string(),
    ));
    let rows: Vec<String> = app
        .feed
        .iter()
        .filter_map(|i| match i {
            FeedItem::Activity { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    let receipt = rows
        .iter()
        .position(|t| t.contains("read 3 files") && t.contains("ran 1 command"))
        .expect("aggregated receipt row expected");
    let narration = rows
        .iter()
        .position(|t| t.contains("check the config file"))
        .expect("narration row expected");
    assert!(receipt < narration, "receipt lands before the narration");

    // The provider's no-prose placeholder is never shown as narration.
    let feed_len = app.feed.len();
    app.on_turn_event(CliEvent::Reasoning(
        "Provider requested native tool call(s).".to_string(),
    ));
    assert_eq!(app.feed.len(), feed_len, "placeholder narration suppressed");
}

#[tokio::test]
async fn watcher_cards_render_as_return_cards_even_while_cancelling() {
    // A Judge/Oracle ruling or goal milestone must draw the SAME return card
    // a specialist report-back does — the clean look resume reconstruction
    // gives these — never a dim Notice line. And it must survive the
    // cancelling guard: the sweep rules on background lanes regardless of
    // what the user's own turn is doing.
    let mut app = test_app();
    app.turn_started = Some(Instant::now());
    app.stop_running_turn("Esc");
    assert!(app.cancelling);

    app.on_turn_event(CliEvent::WatcherCard {
        from: "Judge (enforcement)".to_string(),
        subject: "halt enforced — Surf (browser)'s turn stopped".to_string(),
        body: "An off-course ruling was ignored for 3 rounds.".to_string(),
        ok: true,
    });
    app.on_turn_event(CliEvent::WatcherCard {
        from: "Oracle".to_string(),
        subject: "goal recognized — g-001".to_string(),
        body: "\"all tests green\"\nThis is a GOAL run.".to_string(),
        ok: true,
    });

    let cards: Vec<(&str, &str)> = app
        .feed
        .iter()
        .filter_map(|i| match i {
            FeedItem::AgentReturn { agent, subject, .. } => {
                Some((agent.as_str(), subject.as_str()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        cards,
        vec![
            (
                "Judge (enforcement)",
                "halt enforced — Surf (browser)'s turn stopped"
            ),
            ("Oracle", "goal recognized — g-001"),
        ],
        "watcher cards must land as AgentReturn rows, cancelling or not"
    );
    assert!(
        !app.feed
            .iter()
            .any(|i| matches!(i, FeedItem::Notice(t) if t.contains("halt") || t.contains("goal"))),
        "no grey plumbing duplicate of a watcher card"
    );

    // A goal wake opens with its driving prompt as the bold ❯ user row —
    // the exact row resume replays for the transcript's User message.
    app.on_turn_event(CliEvent::WakeTurn {
        prompt: format!("GOAL HEARTBEAT (11 wakes left): {}", "x".repeat(700)),
        turn_id: None,
        origin: None,
    });
    match app.feed.last() {
        Some(FeedItem::User(text)) => {
            assert!(text.starts_with("GOAL HEARTBEAT (11 wakes left):"));
            assert_eq!(
                text.chars().count(),
                601,
                "wake prompt caps at 600 chars + ellipsis, like replay"
            );
        }
        _ => panic!("wake prompt must render as the ❯ user row"),
    }
}

#[tokio::test]
async fn bare_display_opens_the_mode_picker_and_args_still_apply() {
    let mut app = test_app();
    // Bare /display refills the input so the option menu opens — it must NOT
    // silently cycle the mode anymore.
    let before = app.state.display_mode;
    app.slash_command("/display");
    assert_eq!(app.input, "/display ");
    assert_eq!(
        app.state.display_mode, before,
        "bare /display must not cycle"
    );
    // The menu lists exactly the two modes as immediate picks.
    let menu = app.menu_matches();
    let names: Vec<&str> = menu.iter().map(|(n, _, _, _)| *n).collect();
    assert_eq!(names, vec!["/display compact", "/display verbose"]);
    assert!(
        menu.iter().all(|(_, _, _, takes_args)| !takes_args),
        "options run immediately on Enter"
    );
    // Typing a partial arg narrows the picker.
    app.input = "/display v".to_string();
    let menu = app.menu_matches();
    assert_eq!(menu.len(), 1);
    assert_eq!(menu[0].0, "/display verbose");
    // Explicit args still apply directly.
    app.slash_command("/display verbose");
    assert_eq!(app.state.display_mode, crate::cli::DisplayMode::Verbose);
}

#[tokio::test]
async fn a_background_wake_turns_done_event_clears_the_thinking_mood() {
    // A background-wake turn has no Turn socket, so no on_turn_finished ever
    // fires — its forwarded Done event is the only end-of-turn signal. The
    // bug: "Phoenix thinking" stayed lit forever after a wake processed a
    // background return while the session sat idle.
    let mut app = test_app();
    app.on_turn_event(CliEvent::Thinking);
    assert!(
        app.agents
            .values()
            .any(|c| matches!(c.mood, AgentMood::Thinking)),
        "wake turn lights the orchestrator chip"
    );

    app.on_turn_event(CliEvent::Done);
    assert!(
        app.agents
            .values()
            .all(|c| matches!(c.mood, AgentMood::Idle)),
        "Done must clear every mood — no phantom 'thinking' after the wake"
    );
    assert!(app.live_stream.is_empty(), "ticker cleared with the moods");
}

#[tokio::test]
async fn progress_note_shows_then_clears_when_tools_start() {
    let mut app = test_app();
    app.turn_started = Some(Instant::now());

    // A phase note (librarian preload / provider wait) lands on the working row.
    app.on_turn_event(CliEvent::StreamDelta {
        kind: "progress".to_string(),
        text: "librarian recalling memory…".to_string(),
    });
    assert_eq!(app.stream_note, "librarian recalling memory…");

    // Once a tool starts, the wait-note is stale — the tool mood takes over.
    app.on_turn_event(CliEvent::ToolCallStarted {
        agent: "lib".to_string(),
        tool_name: "memory_read".to_string(),
        input_summary: "memory/WARM/x.md".to_string(),
    });
    assert!(
        app.stream_note.is_empty(),
        "a running tool supersedes the provider-wait note"
    );

    // A fresh provider call clears any prior note before its own arrives.
    app.on_turn_event(CliEvent::StreamDelta {
        kind: "progress".to_string(),
        text: "Orchestrator waiting on mock-model…".to_string(),
    });
    app.on_turn_event(CliEvent::Thinking);
    assert!(
        app.stream_note.is_empty(),
        "Thinking starts a fresh thought stream with no stale note"
    );
}

#[tokio::test]
async fn a_fresh_dispatch_clears_lingering_cancel_state() {
    let mut app = test_app();
    app.cancelling = true;
    app.turn_activity.tool_calls = 9;
    let (tx, _rx) = mpsc::channel::<TurnRequest>(4);
    app.dispatch_turn("next task".to_string(), &tx);
    assert!(
        !app.cancelling,
        "dispatching a new turn supersedes cancel state"
    );
    assert!(app.turn_running());
    assert_eq!(
        app.turn_activity,
        TurnActivityStats::default(),
        "fresh turns must not inherit prior activity counters"
    );
    assert!(
        app.seen_tool_inputs.is_empty(),
        "fresh turns must not inherit duplicate-tool keys"
    );
}

#[test]
fn runtime_binary_status_line_names_version_path_and_hash() {
    let line = runtime_binary_status_line();
    assert!(line.contains("binary · phoenix"), "{line}");
    assert!(line.contains(env!("CARGO_PKG_VERSION")), "{line}");
    assert!(line.contains("sha256"), "{line}");
}

#[test]
fn status_command_separates_context_from_processed_tokens() {
    let mut app = test_app();
    app.ctx_tokens_estimate = 20_000;
    app.context_window = 200_000;
    app.session_tokens = 4_100_000;
    app.compression_saved_tokens = 72_000;

    app.slash_command("/status");

    let notices = app
        .feed
        .iter()
        .filter_map(|item| match item {
            FeedItem::Notice(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        notices
            .iter()
            .any(|line| line.contains("context ~20.0k/200.0k (10%)")),
        "{notices:#?}"
    );
    assert!(
        notices
            .iter()
            .any(|line| line.contains("processed this session · 4.1M model tokens")),
        "{notices:#?}"
    );
    assert!(
        notices
            .iter()
            .any(|line| line.contains("tool output compressed ~72.0k tokens out of context")),
        "{notices:#?}"
    );
    assert!(
        notices
            .iter()
            .all(|line| !line.contains("4.1M session tokens")),
        "{notices:#?}"
    );
    assert!(
        notices.iter().any(|line| line.starts_with("browser · ")),
        "{notices:#?}"
    );
    assert!(
        notices.iter().any(|line| line.starts_with("computer · ")),
        "{notices:#?}"
    );
}

#[test]
fn status_command_lists_background_specialist_jobs() {
    let mut app = test_app();
    app.on_turn_event(CliEvent::SpecialistQueued {
            agent: "coder".to_string(),
            subject: "write background note".to_string(),
            status: "background handoff; sender does not block, and completion will hand back through the mesh".to_string(),
        });
    app.on_turn_event(CliEvent::SpecialistQueued {
            agent: "researcher".to_string(),
            subject: "collect sources".to_string(),
            status: "background handoff; sender does not block, and completion will hand back through the mesh".to_string(),
        });
    app.on_turn_event(CliEvent::SpecialistCompleted {
        agent: "researcher".to_string(),
        ok: true,
        summary: "found 5 sources".to_string(),
    });

    app.slash_command("/status");

    let notices = app
        .feed
        .iter()
        .filter_map(|item| match item {
            FeedItem::Notice(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        notices
            .iter()
            .any(|line| { line.contains("background specialists · 1 running · 2 total seen") }),
        "{notices:#?}"
    );
    assert!(
        notices
            .iter()
            .any(|line| line.contains("running · Leo (coder) · write background note")),
        "{notices:#?}"
    );
    assert!(
        notices
            .iter()
            .any(|line| line.contains("done · Theo (researcher) · collect sources")),
        "{notices:#?}"
    );
}

#[test]
fn status_command_lists_queued_user_messages() {
    let mut app = test_app();
    app.queued_inputs
        .push_back("second thing after current turn".to_string());
    app.queued_inputs
        .push_back("third thing with enough detail to identify it".to_string());

    app.slash_command("/status");

    let notices = app
        .feed
        .iter()
        .filter_map(|item| match item {
            FeedItem::Notice(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        notices
            .iter()
            .any(|line| line.contains("queued user messages · 2 waiting")),
        "{notices:#?}"
    );
    assert!(
        notices
            .iter()
            .any(|line| line.contains("queued #1 · second thing after current turn")),
        "{notices:#?}"
    );
    assert!(
        notices.iter().any(|line| {
            line.contains("queued #2 · third thing with enough detail to identify it")
        }),
        "{notices:#?}"
    );
}

#[test]
fn status_command_keeps_last_context_compaction_notice() {
    let mut app = test_app();
    app.on_turn_event(CliEvent::GatewayNotice(
        "context auto-compacted: 42 messages folded, ~180k -> ~38k tokens (receipt fallback)"
            .to_string(),
    ));

    app.slash_command("/status");

    let notices = app
        .feed
        .iter()
        .filter_map(|item| match item {
            FeedItem::Notice(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        notices.iter().any(|line| line.contains(
            "last context compaction · 42 messages folded, ~180k -> ~38k tokens (receipt fallback)"
        )),
        "{notices:#?}"
    );
}

#[test]
fn compact_command_reports_session_archive_state() {
    let session_id = format!("main-compact-test-{}", uuid::Uuid::new_v4());
    let mut app = Tui::new(AppState::new(false, true, session_id), false);

    app.slash_command("/compact");

    let notices = app
        .feed
        .iter()
        .filter_map(|item| match item {
            FeedItem::Notice(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        notices.iter().any(|line| line == &"Session Compaction"),
        "{notices:#?}"
    );
    assert!(
        notices
            .iter()
            .any(|line| line.contains("context archive · none yet")),
        "{notices:#?}"
    );
    assert!(
        notices
            .iter()
            .any(|line| line.contains("durable session auto-compaction runs")),
        "{notices:#?}"
    );
}

#[test]
fn direct_visibility_toggles_match_view_command() {
    let mut app = test_app();
    assert!(app.state.show_actions);
    assert!(!app.state.show_reasoning);
    assert!(!app.state.show_debug);

    app.slash_command("/actions");
    app.slash_command("/thinking");
    app.slash_command("/debug");

    assert!(!app.state.show_actions);
    assert!(app.state.show_reasoning);
    assert!(app.state.show_debug);

    let notices = app
        .feed
        .iter()
        .filter_map(|item| match item {
            FeedItem::Notice(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        notices
            .iter()
            .any(|line| line.contains("tool activity rows: off")),
        "{notices:#?}"
    );
    assert!(
        notices
            .iter()
            .any(|line| line.contains("thinking rows: on")),
        "{notices:#?}"
    );
    assert!(
        notices.iter().any(|line| line.contains("debug rows: on")),
        "{notices:#?}"
    );

    app.slash_command("/view actions");
    assert!(app.state.show_actions);
}

#[test]
fn direct_visibility_toggles_are_menu_discoverable() {
    let mut app = test_app();
    app.input = "/a".to_string();
    let menu = app.menu_matches();
    assert!(menu.iter().any(|(name, _, _, _)| *name == "/actions"));

    app.input = "/thi".to_string();
    let menu = app.menu_matches();
    assert!(menu.iter().any(|(name, _, _, _)| *name == "/thinking"));

    app.input = "/d".to_string();
    let menu = app.menu_matches();
    assert!(menu.iter().any(|(name, _, _, _)| *name == "/debug"));
}

#[test]
fn defaults_command_shows_usage_without_save_arg() {
    let mut app = test_app();

    app.slash_command("/defaults");

    let notices = app
        .feed
        .iter()
        .filter_map(|item| match item {
            FeedItem::Notice(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        notices
            .iter()
            .any(|line| line.contains("usage: /defaults save")),
        "{notices:#?}"
    );
}

#[test]
fn defaults_command_is_menu_discoverable() {
    let mut app = test_app();
    app.input = "/def".to_string();

    let menu = app.menu_matches();

    assert!(
        menu.iter()
            .any(|(name, usage, _, takes_args)| *name == "/defaults"
                && *usage == "/defaults save"
                && *takes_args),
        "{menu:#?}"
    );
}

#[test]
fn alt_enter_composes_multiline_input_and_plain_enter_submits() {
    let mut app = test_app();
    let (tx, mut rx) = mpsc::channel::<TurnRequest>(2);

    for c in "first".chars() {
        app.on_key(
            crossterm::event::KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            &tx,
        );
    }
    app.on_key(
        crossterm::event::KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT),
        &tx,
    );
    for c in "second".chars() {
        app.on_key(
            crossterm::event::KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            &tx,
        );
    }

    assert_eq!(app.input, "first\nsecond");
    assert_eq!(app.cursor, "first\nsecond".chars().count());

    app.on_key(
        crossterm::event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &tx,
    );

    let request = rx.try_recv().expect("plain Enter should submit the turn");
    assert_eq!(request.user_request, "first\nsecond");
    assert!(app
        .feed
        .iter()
        .any(|item| { matches!(item, FeedItem::User(text) if text == "first\nsecond") }));
}

#[test]
fn shift_enter_also_composes_multiline_input() {
    let mut app = test_app();
    let (tx, _rx) = mpsc::channel::<TurnRequest>(1);
    app.input = "top".to_string();
    app.cursor = app.input.chars().count();

    app.on_key(
        crossterm::event::KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
        &tx,
    );

    assert_eq!(app.input, "top\n");
    assert_eq!(app.cursor, "top\n".chars().count());
}

#[tokio::test]
async fn resume_picker_navigates_and_switches_sessions() {
    let home = tempfile::tempdir().unwrap();
    let _home_guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
    let (tx, _rx) = mpsc::channel::<TurnRequest>(4);
    let mut app = test_app();
    app.resume_picker = Some(ResumePicker {
        items: vec![
            (
                "main-stop-test".to_string(),
                "current one · 1m ago (current)".to_string(),
            ),
            (
                "main-other-abc".to_string(),
                "reddit research · 2h ago".to_string(),
            ),
        ],
        sel: 0,
    });

    // Down hovers the second row; typing does NOT leak into the input bar.
    app.on_key(
        crossterm::event::KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        &tx,
    );
    assert!(app.input.is_empty());
    assert_eq!(app.resume_picker.as_ref().unwrap().sel, 1);

    // Enter switches to the hovered session and closes the picker.
    app.on_key(
        crossterm::event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &tx,
    );
    assert!(app.resume_picker.is_none());
    assert_eq!(app.state.session_id, "main-other-abc");
    assert!(app
        .feed
        .iter()
        .any(|item| matches!(item, FeedItem::Notice(n) if n.contains("reddit research"))));

    // Esc closes without switching.
    app.resume_picker = Some(ResumePicker {
        items: vec![("main-x".to_string(), "x · now".to_string())],
        sel: 0,
    });
    app.on_key(
        crossterm::event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &tx,
    );
    assert!(app.resume_picker.is_none());
    assert_eq!(app.state.session_id, "main-other-abc");
}

#[test]
fn input_box_height_grows_for_multiline_prompts_but_stays_bounded() {
    assert_eq!(input_box_height("", 40, 80), 3);
    assert_eq!(input_box_height("one\ntwo\nthree", 40, 80), 5);
    assert_eq!(input_box_height("1\n2\n3\n4\n5\n6\n7\n8", 40, 80), 7);
    assert_eq!(input_box_height("1\n2\n3\n4\n5", 9, 80), 3);
    // A long single line WRAPS, so the box grows for it too: 40 chars at
    // 12-wide (8 content cols) = 6 wrapped rows → capped rendering height.
    assert_eq!(input_box_height(&"x".repeat(40), 40, 12), 7);
}

#[test]
fn input_view_keeps_cursor_line_visible_for_tall_multiline_prompt() {
    let input = "one\ntwo\nthree\nfour\nfive";
    let cursor = input.chars().count();

    let view = input_view(input, cursor, 80, 5);

    assert_eq!(view.first_visible_line, 2);
    assert_eq!(view.lines, vec!["three", "four", "five"]);
    assert_eq!(view.cursor_row, 2);
    assert_eq!(view.cursor_col, 4);
}

#[test]
fn input_view_wraps_long_lines_at_the_border_instead_of_scrolling() {
    // 12-wide box → 8 content cols. The 26-char line wraps into 4 rows;
    // the viewport (2 content rows) scrolls to keep the cursor visible.
    let input = "short\nabcdefghijklmnopqrstuvwxyz";
    let cursor = input.chars().count();

    let view = input_view(input, cursor, 12, 4);

    assert_eq!(view.lines, vec!["qrstuvwx", "yz"]);
    assert_eq!(view.cursor_row, 1);
    assert_eq!(view.cursor_col, 2);

    // Cursor mid-line lands on the right wrapped row.
    let view = input_view("abcdefghijklmnop", 10, 12, 5);
    assert_eq!(view.lines, vec!["abcdefgh", "ijklmnop", ""]);
    assert_eq!(view.cursor_row, 1);
    assert_eq!(view.cursor_col, 2);
}

#[test]
fn session_message_counts_reports_tool_results() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("session.json");
    let mut session = crate::session::Session::new_main("model", "prompt");
    session.messages.push(crate::session::Message::User {
        content: "hello".to_string(),
    });
    session.messages.push(crate::session::Message::ToolResult {
        tool_name: "read".to_string(),
        input: "a.rs".to_string(),
        success: true,
        output: "contents".to_string(),
    });
    session.messages.push(crate::session::Message::Assistant {
        content: "done".to_string(),
    });
    std::fs::write(&path, serde_json::to_string(&session).unwrap()).unwrap();

    assert_eq!(session_message_counts(&path), Some((3, 1)));
}

#[test]
fn context_archive_status_line_reports_archive_file() {
    let temp = tempfile::tempdir().unwrap();
    let session_id = "main-archive-test";
    let archive = temp.path().join(format!("{session_id}.archive.jsonl"));
    std::fs::write(
        &archive,
        "{\"role\":\"user\",\"content\":\"one\"}\n\n{\"role\":\"assistant\",\"content\":\"two\"}\n",
    )
    .unwrap();

    let line = context_archive_status_line_for_path(&archive).unwrap();
    assert!(
        line.contains("context archive · 2 archived folded messages"),
        "{line}"
    );
    assert!(
        line.contains(&format!("{session_id}.archive.jsonl")),
        "{line}"
    );
}

#[test]
fn turn_footer_summarizes_tools_failures_and_background_handoffs() {
    let mut app = test_app();
    // Route + trace are /debug-only plumbing; this test keys the footer off
    // the trace path, so run it in debug view.
    app.state.show_debug = true;
    app.on_turn_event(CliEvent::ToolCallCompleted {
        agent: "coder".to_string(),
        tool_name: "read".to_string(),
        input_summary: "a.rs".to_string(),
        success: true,
        output_summary: "ok".to_string(),
        diff: None,
    });
    app.on_turn_event(CliEvent::ToolCallCompleted {
        agent: "coder".to_string(),
        tool_name: "bash".to_string(),
        input_summary: "cargo test".to_string(),
        success: false,
        output_summary: "failed".to_string(),
        diff: None,
    });
    app.on_turn_event(CliEvent::ToolCallCompleted {
        agent: "coder".to_string(),
        tool_name: "read".to_string(),
        input_summary: "a.rs".to_string(),
        success: true,
        output_summary: "ok again".to_string(),
        diff: None,
    });
    app.on_turn_event(CliEvent::SpecialistDelegated {
        agent: "coder".to_string(),
        subject: "background note".to_string(),
    });
    app.on_turn_event(CliEvent::SpecialistQueued {
            agent: "coder".to_string(),
            subject: "background note".to_string(),
            status: "background handoff; sender does not block, and completion will hand back through the mesh".to_string(),
        });

    app.apply_summary(
        TurnSummary {
            completion: crate::cli::daemon::TurnCompletion::Completed,
            final_markdown: "done".to_string(),
            main_session_id: "main-stop-test".to_string(),
            run_id: "run-1".to_string(),
            trace_path: "trace.json".to_string(),
            route: "direct".to_string(),
            total_tokens: 123,
            orchestrator_tokens: None,
            coder_tokens: None,
            compression_saved_tokens: 0,
            compression_raw_tokens: 0,
            context_window: None,
            background_work_pending: false,
        },
        2.5,
    );

    let footer = app
        .feed
        .iter()
        .rev()
        .find_map(|item| match item {
            FeedItem::Notice(text) if text.contains("trace") => Some(text.as_str()),
            _ => None,
        })
        .expect("footer notice");
    assert!(
        footer.contains("3 tools (1 failed, 1 duplicate)"),
        "{footer}"
    );
    assert!(footer.contains("1 handoffs (1 background)"), "{footer}");
    // Zero compression = zero extra ink on the receipt.
    assert!(!footer.contains("compressed"), "{footer}");
}

// Per-turn compression visibility (2026-07-07 goal): a turn that shaved tool
// output shows the amount ON the existing receipt row — one segment, no
// dedicated row, and nothing at all when the turn compressed nothing.
#[test]
fn turn_receipt_shows_compression_only_when_nonzero() {
    let mut app = test_app();
    app.apply_summary(
        TurnSummary {
            completion: crate::cli::daemon::TurnCompletion::Completed,
            final_markdown: "done".to_string(),
            main_session_id: "main-compress-test".to_string(),
            run_id: "run-2".to_string(),
            trace_path: "trace.json".to_string(),
            route: "direct".to_string(),
            total_tokens: 5_000,
            orchestrator_tokens: None,
            coder_tokens: None,
            compression_saved_tokens: 9_400,
            compression_raw_tokens: 0,
            context_window: None,
            background_work_pending: false,
        },
        1.2,
    );
    let footer = app
        .feed
        .iter()
        .rev()
        .find_map(|item| match item {
            FeedItem::Notice(text) if text.contains("tokens") => Some(text.as_str()),
            _ => None,
        })
        .expect("footer notice");
    assert!(footer.contains("~9.4k tok compressed away"), "{footer}");
}

#[test]
fn subagents_toggle_hides_working_steps_but_never_spawn_or_return() {
    let mut app = test_app();
    // Per-call rows are gone in every mode — the toggle now gates the failure
    // rows (and journal receipts) a sub-agent produces.
    assert!(app.state.show_subagent_tools, "visible by default");

    let failed = |agent: &str| CliEvent::ToolCallCompleted {
        agent: agent.to_string(),
        tool_name: "web_search".to_string(),
        input_summary: "market scan".to_string(),
        success: false,
        output_summary: "connection refused".to_string(),
        diff: None,
    };
    // Toggle OFF: sub-agent tool rows are filtered…
    app.slash_command("/subagents");
    assert!(!app.state.show_subagent_tools);
    let feed_len = app.feed.len();
    app.on_turn_event(failed("researcher"));
    assert_eq!(app.feed.len(), feed_len, "sub-agent tool rows hidden");
    // …but orchestrator rows still show…
    app.on_turn_event(failed("orchestrator"));
    assert!(app.feed.len() > feed_len, "orchestrator rows always show");
    // …and spawn/return events always render, toggle or not.
    let feed_len = app.feed.len();
    app.on_turn_event(CliEvent::BackgroundAgentSpawned {
        agent: "researcher".to_string(),
        subject: "market scan".to_string(),
        handoff_id: String::new(),
        requester: String::new(),
        receiver: String::new(),
        status: String::new(),
        causation_id: None,
    });
    app.on_turn_event(CliEvent::BackgroundAgentReturned {
        agent: "researcher".to_string(),
        subject: "market scan".to_string(),
        ok: true,
        summary: "3 competitors".to_string(),
        body: "## Findings\nthree competitors found".to_string(),
        handoff_id: String::new(),
        requester: String::new(),
        receiver: String::new(),
        status: String::new(),
        reply_to: None,
        causation_id: None,
    });
    assert!(
        app.feed.len() >= feed_len + 2,
        "spawn + return always visible"
    );
    assert!(
        app.feed.iter().any(|item| matches!(
            item,
            FeedItem::AgentReturn { agent, ok, body, .. }
                if agent == "researcher" && *ok && body.contains("three competitors")
        )),
        "return renders as the agent-colored result block"
    );
    // The /status job list tracks the background job through its lifecycle.
    assert!(app.background_jobs.iter().any(|job| job.done));

    // Toggle back ON restores the rows.
    app.slash_command("/subagents");
    assert!(app.state.show_subagent_tools);
}

#[tokio::test]
async fn background_return_renders_even_while_cancelling() {
    // A background agent is DETACHED — the user stopping the foreground turn
    // must not swallow its return.
    let mut app = test_app();
    app.turn_started = Some(Instant::now());
    app.stop_running_turn("Esc");
    assert!(app.cancelling);
    app.on_turn_event(CliEvent::BackgroundAgentReturned {
        agent: "coder".to_string(),
        subject: "big build".to_string(),
        ok: true,
        summary: "built".to_string(),
        body: "build finished green".to_string(),
        handoff_id: String::new(),
        requester: String::new(),
        receiver: String::new(),
        status: String::new(),
        reply_to: None,
        causation_id: None,
    });
    assert!(
        app.feed.iter().any(|item| matches!(
            item,
            FeedItem::AgentReturn { agent, .. } if agent == "coder"
        )),
        "background returns survive a cancelled foreground turn"
    );
}

#[test]
fn agent_key_resolves_every_label_shape_to_the_role() {
    // Regression: mesh events carry full display names ("Leo (coder)") since
    // 2026-07-02d; the key must extract the role or every specialist row falls
    // into the orchestrator bucket — the exact planner-mislabeling bug again.
    for (raw, expected) in [
        ("coder", "coder"),
        ("Coder", "coder"),
        ("coder#2", "coder"),
        ("Leo (coder)", "coder"),
        ("Leo (coder) #2", "coder"),
        ("Compass (planner)", "planner"),
        ("Hawk (critic)", "critic"),
        ("Probe (tester)", "tester"),
        ("Phoenix (orchestrator)", "orchestrator"),
        ("Pixel (computer_use)", "computer_use"),
    ] {
        assert_eq!(agent_key(raw), expected, "label {raw:?}");
    }
}

#[test]
fn short_agent_preserves_instance_numbers_and_display_names() {
    assert_eq!(short_agent("coder"), "Leo (coder)");
    assert_eq!(short_agent("coder#2"), "Leo (coder) #2");
    assert_eq!(short_agent("Leo (coder)"), "Leo (coder)");
    assert_eq!(short_agent("·"), "");
}

#[test]
fn display_modes_gate_tool_rows_and_render_diffs() {
    use crate::cli::DisplayMode;
    let completed = |success: bool, diff: Option<String>| CliEvent::ToolCallCompleted {
        agent: "coder".to_string(),
        tool_name: if diff.is_some() {
            "str_replace"
        } else {
            "read"
        }
        .to_string(),
        input_summary: "src/a.rs".to_string(),
        success,
        output_summary: if success {
            "ok"
        } else {
            "error[E0308]: mismatch"
        }
        .to_string(),
        diff,
    };
    let activity_rows = |app: &Tui| {
        app.feed
            .iter()
            .filter(|item| matches!(item, FeedItem::Activity { .. }))
            .count()
    };
    let diff_rows = |app: &Tui| {
        app.feed
            .iter()
            .filter(|item| matches!(item, FeedItem::Diff { .. }))
            .count()
    };

    // Compact: successes leave no rows (counters only); failures always show.
    let mut app = test_app();
    app.state.display_mode = DisplayMode::Compact;
    app.on_turn_event(completed(true, None));
    assert_eq!(activity_rows(&app), 0, "compact hides success rows");
    assert_eq!(app.turn_activity.tool_calls, 1, "counters still tick");
    assert_eq!(app.turn_activity.reads, 1);
    app.on_turn_event(completed(false, None));
    assert_eq!(activity_rows(&app), 1, "failures always surface");

    // Verbose: the same journal (no per-call success rows) PLUS the
    // red/green diff block the moment an edit lands.
    let mut app = test_app();
    app.state.display_mode = DisplayMode::Verbose;
    app.on_turn_event(completed(
        true,
        Some("@@ src/a.rs\n- old\n+ new".to_string()),
    ));
    assert_eq!(activity_rows(&app), 0, "verbose journals successes too");
    assert_eq!(diff_rows(&app), 1, "verbose renders the diff block");
    assert_eq!(app.turn_activity.edits, 1, "str_replace counted as edit");

    // Compact: the diff stays out — the journal receipt is the whole story.
    let mut app = test_app();
    app.state.display_mode = DisplayMode::Compact;
    app.on_turn_event(completed(
        true,
        Some("@@ src/a.rs\n- old\n+ new".to_string()),
    ));
    assert_eq!(diff_rows(&app), 0, "compact hides diffs");
}

#[test]
fn background_agents_keep_a_live_tally_with_no_turn_running() {
    // The user must see Iris/Ada working AFTER the orchestrator's turn
    // ends: a background agent's tool events (arriving over Subscribe with
    // turn_started = None) feed its own live per-agent counter, which clears
    // when the job returns.
    let mut app = test_app();
    assert!(app.turn_started.is_none(), "no user turn is running");
    app.on_turn_event(CliEvent::BackgroundAgentSpawned {
        agent: "presentation".to_string(),
        subject: "build the deck".to_string(),
        handoff_id: String::new(),
        requester: String::new(),
        receiver: String::new(),
        status: String::new(),
        causation_id: None,
    });
    let completed = |tool: &str| CliEvent::ToolCallCompleted {
        agent: "presentation".to_string(),
        tool_name: tool.to_string(),
        input_summary: format!("{tool}-input"),
        success: true,
        output_summary: "ok".to_string(),
        diff: None,
    };
    app.on_turn_event(completed("read"));
    app.on_turn_event(completed("web_fetch"));
    let tally = app
        .live_counts
        .get("presentation")
        .expect("background work must accumulate a live tally");
    assert_eq!(tally.reads, 1);
    assert_eq!(tally.web, 1);
    assert!(
        app.background_jobs.iter().any(|job| !job.done),
        "the running job drives the live shimmer row"
    );
    // Mood is lit → the agent has a working row even with no turn.
    assert!(matches!(
        app.agents["presentation"].mood,
        AgentMood::Thinking | AgentMood::Tool(_)
    ));

    app.on_turn_event(CliEvent::BackgroundAgentReturned {
        agent: "presentation".to_string(),
        subject: "build the deck".to_string(),
        ok: true,
        summary: "deck done".to_string(),
        body: "## Deck\ndone".to_string(),
        handoff_id: String::new(),
        requester: String::new(),
        receiver: String::new(),
        status: String::new(),
        reply_to: None,
        causation_id: None,
    });
    assert!(
        app.live_counts.get("presentation").is_none(),
        "the live tally retires with the job"
    );
}

#[test]
fn narration_one_liner_rides_in_every_display_mode() {
    let mut app = test_app();
    app.state.display_mode = crate::cli::DisplayMode::Compact;
    app.state.show_reasoning = false;
    app.on_turn_event(CliEvent::AgentThinking {
        agent: "coder".to_string(),
        text: "Build is running; I'll check the config loader while I wait.".to_string(),
    });
    assert!(
        app.feed.iter().any(|item| matches!(
            item,
            FeedItem::Activity { text, .. } if text.contains("Build is running")
        )),
        "the between-tool narration shows even in compact with /thinking off"
    );
}

#[tokio::test]
async fn clear_keeps_its_semantics_with_the_fresh_start_index_kick() {
    // /clear fires a detached memory-index request at the gateway; with no
    // gateway (tests) the request fails silently and the command's visible
    // behavior is unchanged: transcript wiped, session untouched.
    let mut app = test_app();
    app.feed.push(FeedItem::Notice("old row".to_string()));
    app.slash_command("/clear");
    assert!(
        app.feed
            .iter()
            .all(|item| !matches!(item, FeedItem::Notice(text) if text.contains("old row"))),
        "/clear wipes the transcript"
    );
    assert!(
        app.feed.iter().any(
            |item| matches!(item, FeedItem::Notice(text) if text.contains("session main-stop-test unchanged"))
        ),
        "/clear keeps the session"
    );
    assert_eq!(app.state.session_id, "main-stop-test");
}
