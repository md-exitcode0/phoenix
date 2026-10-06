use super::*;

#[test]
fn x11_keys_match_the_advertised_cross_backend_vocabulary() {
    for (input, expected) in [("enter","Return"),("esc","Escape"),("f3","F3"),
        ("CTRL+shift+f12","ctrl+shift+F12"),("pageup","Prior"),
        ("alt+tab","alt+Tab"),("KP_1","KP_1"),("a","a"),
        ("num7","KP_7"),("numpad1","KP_1"),("numpaddecimal","KP_Decimal")] {
        assert_eq!(x11_key_combo(input).unwrap(),expected);
    }
    assert!(x11_key_combo("ctrl++a").is_err());
    assert!(x11_key_combo("f36").is_err());
}

#[test]
#[ignore = "starts an isolated X server and xev to inspect actual delivered key events"]
fn live_x11_advertised_keys_reach_the_application() {
    use crate::tools::isolated_desktop::{self, DesktopScope};
    let home = tempfile::tempdir().unwrap();
    let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
    let scope = DesktopScope::agent("key-delivery", "probe", None).unwrap();
    let desktop = isolated_desktop::ensure(&scope).unwrap();
    let events = home.path().join("keys.log");
    let mut command = Command::new("stdbuf");
    command.args(["-oL", "xev", "-name", "Phoenix key delivery", "-event", "keyboard"])
        .stdout(std::fs::File::create(&events).unwrap()).stderr(Stdio::null());
    desktop.apply_to_command(&mut command);
    let mut child = command.spawn().unwrap();
    let result = isolated_desktop::with_scope(Some(scope.clone()), || -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let id = loop {
            if let Ok(ids) = run("xdotool", &["search", "--name", "^Phoenix key delivery$"]) {
                if let Some(id) = ids.lines().next() { break id.to_string(); }
            }
            anyhow::ensure!(Instant::now() < deadline, "xev did not map");
            std::thread::sleep(Duration::from_millis(20));
        };
        run("xdotool", &["windowfocus", "--sync", &id])?;
        for combo in ["enter", "esc", "f3", "num7", "numpaddecimal"] { key(KeyInput {combo:combo.into()})?; }
        type_text(TypeInput {text:"-1.8".into()})?;
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let captured = std::fs::read_to_string(&events)?;
            if ["Return", "Escape", "F3", "KP_7", "KP_Decimal", "minus", "period"].iter().all(|name| captured.contains(&format!(", {name})"))) {
                assert!(captured.lines().filter(|line| line.contains(", F3)")).all(|line| line.contains("state 0x0,")),
                    "plain F3 must not inject an implicit modifier: {captured}");
                break;
            }
            anyhow::ensure!(Instant::now() < deadline, "application did not receive all requested symbols: {captured}");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(key(KeyInput {combo:"PhoenixNotARealKeysym".into()}).is_err());
        Ok(())
    });
    let _ = child.kill(); let _ = child.wait();
    isolated_desktop::discard_scope(&scope);
    result.unwrap();
}

#[test]
#[ignore = "starts real Blender; PHOENIX_TEST_BLENDER must name the executable"]
fn live_blender_search_batch_adds_geometry_through_keys() {
    use crate::tools::isolated_desktop::{self, DesktopScope};
    let binary = std::env::var("PHOENIX_TEST_BLENDER").unwrap();
    let home = tempfile::tempdir().unwrap();
    let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
    let scope = DesktopScope::agent("blender-input", "probe", None).unwrap();
    let desktop = isolated_desktop::ensure(&scope).unwrap();
    let inventory = home.path().join("inventory.json");
    let mut command = Command::new(binary);
    command.args(["--factory-startup", "--disable-autoexec", "--window-geometry", "0", "0", "1440", "960", "--python"])
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/blender-input-fixture.py"))
        .arg("--").arg(&inventory)
        .env("BLENDER_USER_RESOURCES", home.path().join("blender"))
        .stdout(Stdio::null()).stderr(Stdio::null());
    desktop.apply_to_command(&mut command);
    let mut child = command.spawn().unwrap();
    let result = isolated_desktop::with_scope(Some(scope.clone()), || -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let id = loop {
            if let Ok(listing) = list_windows(ListWindowsInput{}) {
                if let Some(start) = listing.content.find('{') {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&listing.content[start..]) {
                        if let Some(id) = value["windows"].as_array().and_then(|windows|windows.iter()
                            .find(|window|window["title"].as_str().is_some_and(|title|title.contains("Blender"))))
                            .and_then(|window|window["id"].as_i64()) {break id;}
                    }
                }
            }
            anyhow::ensure!(Instant::now() < deadline, "Blender did not map");
            std::thread::sleep(Duration::from_millis(50));
        };
        focus_window(FocusWindowInput{id})?;
        let ready_deadline = Instant::now() + Duration::from_secs(20);
        while !inventory.is_file() {
            anyhow::ensure!(child.try_wait()?.is_none(), "Blender exited before the observer was ready");
            anyhow::ensure!(Instant::now() < ready_deadline, "Blender scene observer did not become ready");
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(Duration::from_millis(500));
        eprintln!("STARTUP {}",screenshot(ScreenshotInput{})?.content);
        click(ClickInput{x:Some(720),y:Some(697),button:"left".into(),double:false})?;
        std::thread::sleep(Duration::from_millis(300));
        key(KeyInput{combo:"esc".into()})?;
        if std::env::var_os("PHOENIX_TEST_BLENDER_HOVER").is_some() {
            // Place the private seat pointer over the Outliner before the
            // agent targets the viewport. No user display is touched.
            gdbus_call("SetRestorePointer",&["false".into()])?;
            click(ClickInput{x:Some(1350),y:Some(180),button:"left".into(),double:false})?;
            gdbus_call("SetRestorePointer",&["true".into()])?;
            std::thread::sleep(Duration::from_millis(100));
            let raw=gdbus_call("Status",&[])?;
            let state:serde_json::Value=serde_json::from_str(&unwrap_gvariant_string(&raw))?;
            anyhow::ensure!(state["user_pointer"]["x"]==1350 && state["user_pointer"]["y"]==180,
                "private pointer fixture failed: {state}");
        }
        click(ClickInput{x:Some(590),y:Some(450),button:"left".into(),double:false})?;
        let before = screenshot(ScreenshotInput{})?;
        eprintln!("BEFORE {}", before.content);
        eprintln!("BACKEND {}",status(StatusInput{})?.content);
        let input = serde_json::from_value(serde_json::json!({"actions":[
            {"type":"key","combo":"f3"},
            {"type":"type","text":"Add UV Sphere"},
            {"type":"key","combo":"enter"}
        ]}))?;
        if std::env::var_os("PHOENIX_TEST_BLENDER_WINDOW_KEYS").is_some() {
            window_act(WindowActInput{id,actions:serde_json::json!([
                {"type":"key","combo":"f3"},{"type":"type","text":"Add UV Sphere"},
                {"type":"key","combo":"enter"}
            ]),capture:false})?;
        } else {act(input)?;}
        key(KeyInput{combo:"num1".into()})?;
        std::thread::sleep(Duration::from_millis(500));
        let after = screenshot(ScreenshotInput{})?;
        eprintln!("AFTER {}", after.content);
        let state: serde_json::Value = serde_json::from_slice(&std::fs::read(&inventory)?)?;
        eprintln!("INVENTORY {state}");
        if std::env::var_os("PHOENIX_TEST_BLENDER_HOVER").is_some() {
            let state:serde_json::Value=serde_json::from_str(&unwrap_gvariant_string(&gdbus_call("Status",&[])?))?;
            anyhow::ensure!(state["user_pointer"]["x"]==1350 && state["user_pointer"]["y"]==180,
                "keyboard routing failed to restore the private pointer: {state}");
        }
        anyhow::ensure!(state["objects"].as_array().is_some_and(|objects| objects.iter().any(|o| o["name"] == "Sphere" && o["type"] == "MESH")),
            "UI search batch did not create the requested sphere: {state}");
        anyhow::ensure!(state["views"].as_array().is_some_and(|views|views.iter().any(|view|
            view["perspective"]=="ORTHO" && view["rotation"].as_array().is_some_and(|rotation|
                rotation.len()==4 && (rotation[0].as_f64().unwrap_or(0.)-std::f64::consts::FRAC_1_SQRT_2).abs()<0.001
                && (rotation[1].as_f64().unwrap_or(0.)-std::f64::consts::FRAC_1_SQRT_2).abs()<0.001))),
            "keypad1 did not select Blender's front view: {state}");
        if std::env::var_os("PHOENIX_TEST_BLENDER_OVERLAYS").is_some() {
            eprintln!("OVERLAY_SHORTCUTS {}",state["overlay_shortcuts"]);
            anyhow::ensure!(state["views"][0]["overlays"]==true,"fixture must start with overlays visible");
            window_act(WindowActInput{id,actions:serde_json::json!([
                {"type":"key","combo":"shift+alt+z"}
            ]),capture:true})?;
            let deadline=Instant::now()+Duration::from_secs(2);
            loop {
                let observed:serde_json::Value=serde_json::from_slice(&std::fs::read(&inventory)?)?;
                if observed["views"][0]["overlays"]==false {eprintln!("OVERLAYS_DISABLED {observed}");break;}
                anyhow::ensure!(Instant::now()<deadline,"shortcut did not disable overlays: {observed}");
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        Ok(())
    });
    let _ = child.kill(); let _ = child.wait();
    isolated_desktop::discard_scope(&scope);
    result.unwrap();
}

#[cfg(unix)]
#[test]
fn open_launches_a_downloaded_executable_as_an_application() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let executable = dir.path().join("portable application");
    let receipt = dir.path().join("launched");
    std::fs::write(&executable, format!("#!/bin/sh\nprintf launched > '{}'\n", receipt.display())).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let result = open(OpenInput { target: executable.to_string_lossy().into_owned() }).unwrap();
    assert_eq!(result.summary, "application launched");
    assert_eq!(std::fs::read_to_string(receipt).unwrap(), "launched");
    assert!(!result.content.contains("user's screen"));
}

#[cfg(unix)]
#[test]
fn executable_detection_keeps_documents_and_directories_as_documents() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let document = dir.path().join("notes.txt");
    std::fs::write(&document, "plain document").unwrap();
    std::fs::set_permissions(&document, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(!is_executable_file(&document));
    assert!(!is_executable_file(dir.path()));
}

#[test]
fn app_discovery_respects_private_xdg_data_roots() {
    use std::ffi::OsStr;
    let dirs = desktop_entry_dirs_for(
        Some(OsStr::new("/private/data")),
        Some(OsStr::new("/example/home")),
        Some(OsStr::new("/opt/apps:relative:/extra")),
    );
    assert_eq!(dirs[0], std::path::Path::new("/private/data/applications"));
    assert!(dirs.contains(&std::path::PathBuf::from("/opt/apps/applications")));
    assert!(dirs.contains(&std::path::PathBuf::from("/extra/applications")));
    assert!(!dirs.contains(&std::path::PathBuf::from("relative/applications")));
    assert!(!dirs.contains(&std::path::PathBuf::from("/example/home/.local/share/applications")));
    let fallback = desktop_entry_dirs_for(Some(OsStr::new("relative")), Some(OsStr::new("/home/test")), None);
    assert_eq!(fallback[0], std::path::Path::new("/home/test/.local/share/applications"));
}

#[test]
fn failed_move_does_not_mark_cursor_placed() {
    let scope = crate::tools::isolated_desktop::DesktopScope::agent(
        "computer-use-tests",
        "failed-move",
        None,
    )
    .unwrap();
    crate::tools::isolated_desktop::with_scope(Some(scope), || {
        let backend_error =
            move_then_mark(|| anyhow::bail!("injected backend move failure")).unwrap_err();
        assert_eq!(backend_error.to_string(), "injected backend move failure");

        let click_error = require_cursor_placed("computer_click").unwrap_err();
        assert!(
            click_error
                .to_string()
                .contains("agent cursor has not been placed yet"),
            "{click_error:#}"
        );
        let scroll_error = require_cursor_placed("computer_scroll").unwrap_err();
        assert!(
            scroll_error
                .to_string()
                .contains("agent cursor has not been placed yet"),
            "{scroll_error:#}"
        );
    });
}

#[test]
fn successful_move_marks_cursor_placed() {
    let scope = crate::tools::isolated_desktop::DesktopScope::agent(
        "computer-use-tests",
        "successful-move",
        None,
    )
    .unwrap();
    crate::tools::isolated_desktop::with_scope(Some(scope), || {
        move_then_mark(|| Ok(())).unwrap();
        require_cursor_placed("computer_click").unwrap();
    });
}

#[test]
fn click_rejects_unknown_button_before_backend_detection() {
    let err = click(ClickInput {
        x: None,
        y: None,
        button: "side".to_string(),
        double: false,
    })
    .unwrap_err();
    assert_eq!(err.to_string(), "button must be left, right, or middle");
}

#[test]
fn click_rejects_invalid_coordinates_before_backend_detection() {
    for (x, y, expected) in [
        (
            Some(40),
            None,
            "computer_click coordinates must provide both x and y, or neither",
        ),
        (
            None,
            Some(80),
            "computer_click coordinates must provide both x and y, or neither",
        ),
        (
            Some(-1),
            Some(80),
            "computer_click coordinates must be non-negative screen pixels (got -1, 80)",
        ),
    ] {
        let err = click(ClickInput {
            x,
            y,
            button: "left".to_string(),
            double: false,
        })
        .unwrap_err();
        assert_eq!(err.to_string(), expected);
    }
}

#[test]
fn move_rejects_negative_coordinates_before_backend_detection() {
    let err = move_cursor(MoveInput {
        x: 12,
        y: -4,
        duration_ms: 0,
    })
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "computer_move coordinates must be non-negative screen pixels (got 12, -4)"
    );
}

#[test]
fn drag_rejects_negative_coordinates_before_backend_detection() {
    let err = drag(DragInput {
        from_x: 12,
        from_y: 24,
        to_x: -1,
        to_y: 48,
        duration_ms: 0,
    })
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "computer_drag end coordinates must be non-negative screen pixels (got -1, 48)"
    );
}

#[test]
fn open_rejects_unknown_target_with_guidance() {
    let err = open(OpenInput {
        target: "/definitely/not/a/real/phoenix-target".to_string(),
    })
    .unwrap_err();
    assert!(err.to_string().contains("no installed application matches"));
}

#[test]
fn open_never_launches_a_url_or_browser_application() {
    for target in [
        "https://example.com",
        "file:///tmp/example.html",
        "firefox",
        "Google Chrome",
        "zen-browser",
    ] {
        let error = open(OpenInput {
            target: target.to_string(),
        })
        .unwrap_err();
        assert!(
            error.to_string().contains("inside Phoenix")
                || error.to_string().contains("browser windows are disabled"),
            "unexpected error for {target}: {error:#}"
        );
    }
}

#[test]
fn browser_apps_are_hard_routed_to_the_managed_browser() {
    for app in [
        "Zen",
        "Firefox",
        "Google Chrome",
        "Chromium",
        "Brave Browser",
        "Microsoft Edge",
        "msedge",
        "LibreWolf",
        "CloakBrowser",
        "Opera GX",
    ] {
        let error = reject_browser_app_target(app).unwrap_err();
        assert!(
            error.to_string().contains("managed browser"),
            "{app}: {error:#}"
        );
        let read_error = app_read(AppReadInput {
            app: app.to_string(),
            max_chars: None,
        })
        .unwrap_err();
        assert!(
            read_error.to_string().contains("managed browser"),
            "{app}: {read_error:#}"
        );
    }
    for native_app in [
        "LibreOffice Writer",
        "GIMP",
        "Knowledge",
        "Ledger Live",
        "Zenity",
        "Bravery Editor",
    ] {
        assert!(
            reject_browser_app_target(native_app).is_ok(),
            "{native_app} is not a browser"
        );
    }
}

#[test]
fn every_targeted_window_and_app_entrypoint_applies_the_browser_guard() {
    let source = include_str!("computer_use.rs");
    for signature in [
        "pub fn focus_window(input: FocusWindowInput)",
        "pub fn capture_window(input: CaptureWindowInput)",
        "pub fn window_act(input: WindowActInput)",
        "pub fn lower_window(input: LowerWindowInput)",
    ] {
        let body = source
            .split(signature)
            .nth(1)
            .unwrap_or_else(|| panic!("missing {signature}"))
            .split("\npub fn ")
            .next()
            .unwrap();
        assert!(
            body.contains("reject_browser_window_target(input.id)?"),
            "{signature} lost its browser guard"
        );
    }
    for signature in [
        "fn click_element(input: ClickElementInput)",
        "fn type_into(input: TypeIntoInput)",
        "pub fn app_inspect(input: AppInspectInput)",
        "pub fn app_locate(input: AppLocateInput)",
        "pub fn app_read(input: AppReadInput)",
    ] {
        let body = source
            .split(signature)
            .nth(1)
            .unwrap_or_else(|| panic!("missing {signature}"))
            .split("\npub fn ")
            .next()
            .unwrap();
        assert!(
            body.contains("reject_browser_app_target"),
            "{signature} lost its browser guard"
        );
    }
}

#[test]
fn browser_surface_boundary_is_identical_in_every_runtime_prompt() {
    let computer = include_str!("../../prompts/computer_use_system.md");
    let orchestrator = include_str!("../../prompts/orchestrator_system.md");
    let shared = include_str!("../runtime/shared_contract.rs");
    for (name, prompt) in [
        ("computer", computer),
        ("orchestrator", orchestrator),
        ("shared", shared),
    ] {
        assert!(prompt.contains("managed browser"), "{name} prompt");
        assert!(prompt.contains("Zen"), "{name} prompt");
        assert!(prompt.contains("computer_*"), "{name} prompt");
    }
    assert!(!computer.contains("click Sign in in Zen"));
    assert!(!computer.contains("Only drive a browser by pixels"));
}

#[test]
fn finds_apps_by_fuzzy_name_in_desktop_entries() {
    let dir = std::env::temp_dir().join(format!("phx-desktop-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("com.expressvpn.ExpressVPN.desktop"),
        "[Desktop Entry]\nType=Application\nName=ExpressVPN\nExec=expressvpn-gui\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("hidden-helper.desktop"),
        "[Desktop Entry]\nType=Application\nName=Express Helper\nNoDisplay=true\nExec=helper\n",
    )
    .unwrap();
    let dirs = vec![dir.clone()];

    // Spaced, cased, partial — all resolve to the visible entry.
    for query in ["expressvpn", "Express VPN", "ExpressVPN", "express"] {
        let app =
            find_desktop_app(query, &dirs).unwrap_or_else(|| panic!("`{query}` should resolve"));
        assert_eq!(app.display_name, "ExpressVPN");
        assert_eq!(app.id, "com.expressvpn.ExpressVPN");
    }
    assert!(find_desktop_app("not-installed-app", &dirs).is_none());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn finds_app_by_keywords_when_name_differs() {
    // The exact e529a013 failure: "system monitor" must resolve the
    // Resources app via its Keywords, even though its Name is "Resources".
    let dir = std::env::temp_dir().join(format!("phx-kw-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
            dir.join("net.nokyan.Resources.desktop"),
            "[Desktop Entry]\nType=Application\nName=Resources\nKeywords=System;Monitor;Task;Manager;CPU;RAM;\nExec=resources\n",
        )
        .unwrap();
    let dirs = vec![dir.clone()];

    let app = find_desktop_app("system monitor", &dirs)
        .expect("`system monitor` should resolve via Keywords");
    assert_eq!(app.id, "net.nokyan.Resources");
    assert!(find_desktop_app("task manager", &dirs).is_some());
    assert!(find_desktop_app("spotify", &dirs).is_none());

    // A miss on a related query still points the agent at the real app.
    let suggestions = suggest_apps("gnome-system-monitor", &dirs, 6);
    assert!(
        suggestions.iter().any(|n| n == "Resources"),
        "expected Resources in suggestions, got {suggestions:?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn launch_app_does_not_block_on_a_long_running_child() {
    // The e529a013 hang: run()/output() blocked until the launched GUI app
    // exited. launch_app must return promptly even for a child that keeps
    // running (it waits only for the launcher's own exit, ~1.5s cap).
    let start = std::time::Instant::now();
    launch_app("sleep", &["30"]).expect("sleep launches");
    assert!(
        start.elapsed() < std::time::Duration::from_secs(4),
        "launch_app blocked on the child for {:?}",
        start.elapsed()
    );
}

#[test]
#[ignore = "live: drives a real window through the GNOME bridge (needs the reloaded extension)"]
fn live_layered_window_round_trips() {
    // Layered mode end-to-end: list windows, capture one WITHOUT focusing it,
    // push it below the stack, and run a no-op-ish window batch (a wait) —
    // proving capture/act work on a window the user may have covered.
    let listed = list_windows(ListWindowsInput {}).expect("window list");
    let json_start = listed.content.find('{').expect("json in list output");
    let parsed: serde_json::Value =
        serde_json::from_str(listed.content[json_start..].trim()).expect("windows json");
    let id = parsed["windows"][0]["id"].as_i64().expect("a window id");

    let shot = capture_window(CaptureWindowInput { id }).expect("window capture");
    assert!(
        shot.content.contains("Screenshot saved: "),
        "{}",
        shot.content
    );
    assert!(shot.content.contains("window-relative"), "{}", shot.content);

    let lowered = lower_window(LowerWindowInput { id }).expect("lower");
    assert!(lowered.content.contains("below"), "{}", lowered.content);

    let acted = window_act(WindowActInput {
        id,
        actions: serde_json::json!([{"type": "wait", "ms": 50}]),
        capture: true,
    })
    .expect("window batch");
    assert!(
        acted.content.contains("window action(s)"),
        "{}",
        acted.content
    );
}

#[test]
#[ignore = "live: reads the real a11y tree via the bundled AT-SPI helper"]
fn live_atspi_targets_and_inspect_through_rust() {
    let targets = app_targets(AppTargetsInput {}).expect("a11y apps list");
    eprintln!(
        "targets: {}",
        &targets.content[..targets.content.len().min(400)]
    );
    assert!(
        targets.content.contains("\"ok\": true") || targets.content.contains("\"ok\":true"),
        "helper should return ok JSON"
    );
}

#[test]
fn launch_app_reports_immediate_launcher_failure() {
    // A launcher that exits non-zero right away surfaces as an error.
    assert!(launch_app("false", &[]).is_err());
    assert!(launch_app("true", &[]).is_ok());
}

#[test]
fn scoped_desktop_exec_is_direct_and_never_needs_host_dbus_activation() {
    let parsed = parse_desktop_exec("sample-app --title 'Private desktop' %U %%")
        .expect("parse a normal desktop Exec line");
    assert_eq!(
        parsed,
        vec![
            "sample-app".to_string(),
            "--title".to_string(),
            "Private desktop".to_string(),
            "%".to_string(),
        ]
    );
    let source = include_str!("computer_use.rs");
    let body = source
        .split("fn launch_desktop_app(app: &DesktopApp)")
        .nth(1)
        .expect("scoped desktop launcher")
        .split("fn launch_desktop_exec_in_scope")
        .next()
        .expect("scoped launcher body");
    assert!(body.contains("current_scope().is_some()"));
    assert!(body.contains("launch_desktop_exec_in_scope"));
}

#[test]
fn png_dimensions_reads_ihdr() {
    let mut bytes = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    bytes.extend_from_slice(&[0, 0, 0, 13]); // IHDR chunk length
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&1920u32.to_be_bytes());
    bytes.extend_from_slice(&2160u32.to_be_bytes());
    bytes.extend_from_slice(&[8, 2, 0, 0, 0]); // bit depth, color type, etc.
    let p = std::env::temp_dir().join(format!("phx-png-{}.png", std::process::id()));
    std::fs::write(&p, &bytes).unwrap();
    assert_eq!(png_dimensions(&p), Some((1920, 2160)));
    std::fs::remove_file(&p).ok();
    assert_eq!(
        png_dimensions(std::path::Path::new("/nonexistent.png")),
        None
    );
}

#[cfg(unix)]
#[test]
fn first_scoped_screenshot_creates_both_private_directory_levels() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("fresh-screenshot-root");
    let scope = root.join("desktop-first-agent");
    assert!(!root.exists());
    prepare_shot_directory(&scope).unwrap();
    for path in [&root, &scope] {
        assert_eq!(std::fs::metadata(path).unwrap().permissions().mode() & 0o777, 0o700);
    }
    image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 180, 0, 255]))
        .save(scope.join("actual.png")).unwrap();
    prepare_shot_directory(&scope).unwrap();
    assert!(scope.join("actual.png").is_file());
}

#[cfg(unix)]
#[test]
fn screenshot_directory_rejects_a_symlinked_ancestor() {
    let tmp = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let root = tmp.path().join("screenshot-root");
    std::os::unix::fs::symlink(other.path(), &root).unwrap();
    assert!(prepare_shot_directory(&root.join("agent")).is_err());
    assert!(!other.path().join("agent").exists());
}

#[cfg(unix)]
#[test]
fn screenshot_paths_are_unique_private_and_validation_repairs_file_mode() {
    use std::os::unix::fs::PermissionsExt;

    let (first, _) = shot_path("desktop").unwrap();
    let (second, _) = shot_path("desktop").unwrap();
    assert_ne!(first, second);
    assert_eq!(
        std::fs::metadata(first.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );

    image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 0, 255]))
        .save(&first)
        .unwrap();
    std::fs::set_permissions(&first, std::fs::Permissions::from_mode(0o644)).unwrap();
    verify_shot_written(&first, &first.display().to_string()).unwrap();
    assert_eq!(
        std::fs::metadata(&first).unwrap().permissions().mode() & 0o777,
        0o600
    );
    std::fs::remove_file(first).unwrap();
}

#[test]
fn xdotool_shell_geometry_parses_without_assuming_line_order() {
    assert_eq!(
        parse_xdotool_geometry("WINDOW=4194308\nHEIGHT=937\nX=-12\nSCREEN=0\nWIDTH=1440\nY=126"),
        Some((-12, 126, 1440, 937))
    );
    assert_eq!(parse_xdotool_geometry("X=0\nY=0\nWIDTH=100"), None);
    assert_eq!(
        parse_xdotool_geometry("X=not-a-number\nY=0\nWIDTH=100\nHEIGHT=100"),
        None
    );
}

#[test]
#[ignore = "live: enumerates the current X11/Xwayland window table with xdotool"]
fn live_x11_window_listing_returns_structured_windows() {
    let listed = list_x11_windows().expect("xdotool window list");
    let json_start = listed.content.find('{').expect("json in window list");
    let parsed: serde_json::Value =
        serde_json::from_str(&listed.content[json_start..]).expect("valid window JSON");
    assert_eq!(parsed["ok"].as_bool(), Some(true));
    let windows = parsed["windows"].as_array().expect("windows array");
    assert!(!windows.is_empty(), "at least one visible X11 window");
    assert!(windows.iter().all(|window| {
        window["id"].is_i64()
            && window["title"].is_string()
            && window["width"].as_i64().is_some_and(|width| width > 0)
            && window["height"].as_i64().is_some_and(|height| height > 0)
    }));
}

#[cfg(unix)]
#[test]
fn screenshot_validation_rejects_symlinks_and_non_png_content() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside");
    std::fs::write(&outside, b"outside").unwrap();
    let link = dir.path().join("shot.png");
    symlink(&outside, &link).unwrap();
    assert!(verify_shot_written(&link, "shot.png").is_err());
    assert_eq!(std::fs::read(&outside).unwrap(), b"outside");

    let invalid = dir.path().join("invalid.png");
    std::fs::write(&invalid, vec![b'x'; 32]).unwrap();
    assert!(verify_shot_written(&invalid, "invalid.png").is_err());

    let truncated = dir.path().join("truncated.png");
    let mut header = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    header.extend_from_slice(&640u32.to_be_bytes());
    header.extend_from_slice(&480u32.to_be_bytes());
    std::fs::write(&truncated, header).unwrap();
    assert!(verify_shot_written(&truncated, "truncated.png").is_err());
    assert!(!truncated.exists());
}

#[test]
fn pending_screenshot_guard_removes_failures_and_preserves_success() {
    let dir = tempfile::tempdir().unwrap();
    let failed = dir.path().join("failed.png");
    std::fs::write(&failed, b"partial").unwrap();
    {
        let _pending = PendingShot::new(failed.clone());
    }
    assert!(!failed.exists());

    let complete = dir.path().join("complete.png");
    std::fs::write(&complete, b"complete").unwrap();
    {
        let mut pending = PendingShot::new(complete.clone());
        pending.keep();
    }
    assert_eq!(std::fs::read(&complete).unwrap(), b"complete");
}

#[test]
fn window_method_error_explains_relogin() {
    let mapped = window_method_error(anyhow!(
            "gdbus failed: Error: GDBus.Error:org.freedesktop.DBus.Error.UnknownMethod: No such method 'ListWindows'"
        ));
    assert!(mapped.to_string().contains("log out and back in"));
}

#[test]
fn status_never_hard_errors_when_bridge_is_missing() {
    // The bridge is typically unreachable in test environments; status
    // must still succeed (launcher-only on Wayland, x11 where available)
    // instead of blocking the whole desktop surface.
    let out = status(StatusInput {}).unwrap();
    assert!(out.content.contains("backend:"));
}

#[test]
fn local_status_line_reports_computer_surface_without_hard_error() {
    let line = local_status_line();
    assert!(line.contains("computer · desktop backend status"), "{line}");
}

#[test]
fn wait_caps_duration() {
    let out = wait(WaitInput { ms: 1 }).unwrap();
    assert!(out.content.contains("1 ms"));
}

#[test]
fn scroll_rejects_zero_vector() {
    let err = scroll(ScrollInput { dx: 0, dy: 0 }).unwrap_err();
    assert!(err.to_string().contains("non-zero"));
}

#[test]
fn gvariant_str_quotes_so_numbers_and_specials_stay_strings() {
    // A bare number passed to a string-typed gdbus method parses as an int
    // and errors — quoting keeps it a string. This is the computer-use
    // "couldn't type 2024 / [draft]" bug.
    assert_eq!(gvariant_str("42"), "\"42\"");
    assert_eq!(gvariant_str("hello world"), "\"hello world\"");
    assert_eq!(gvariant_str("[draft]"), "\"[draft]\"");
    // Embedded quotes, backslashes and newlines get C-style escapes.
    assert_eq!(gvariant_str("say \"hi\""), "\"say \\\"hi\\\"\"");
    assert_eq!(gvariant_str("a\\b"), "\"a\\\\b\"");
    assert_eq!(gvariant_str("line1\nline2"), "\"line1\\nline2\"");
}

#[test]
fn action_batch_deserializes_the_full_vocabulary() {
    let json = serde_json::json!({"actions": [
        {"type": "move", "x": 10, "y": 20},
        {"type": "click", "x": 10, "y": 20, "button": "left"},
        {"type": "double_click", "x": 5, "y": 5},
        {"type": "type", "text": "hello"},
        {"type": "key", "combo": "ctrl+a"},
        {"type": "scroll", "dx": 0, "dy": -300},
        {"type": "drag", "from_x": 1, "from_y": 2, "to_x": 3, "to_y": 4},
        {"type": "wait", "ms": 50}
    ]});
    let parsed: ActInput = serde_json::from_value(json).unwrap();
    assert!(parsed.screenshot, "screenshot defaults to true");
    let labels: Vec<_> = parsed.actions.iter().map(Action::label).collect();
    assert_eq!(
        labels,
        [
            "move",
            "click",
            "double_click",
            "type",
            "key",
            "scroll",
            "drag",
            "wait"
        ]
    );
}

#[test]
fn unwrap_gvariant_string_extracts_inner_json() {
    assert_eq!(
        unwrap_gvariant_string("('{\"ok\":true,\"windows\":[]}',)"),
        "{\"ok\":true,\"windows\":[]}"
    );
    assert_eq!(unwrap_gvariant_string("  ('hi',)  "), "hi");
    // Passthrough when not GVariant-wrapped.
    assert_eq!(unwrap_gvariant_string("plain text"), "plain text");
}

#[test]
fn focus_window_input_parses_id() {
    let parsed: FocusWindowInput = serde_json::from_value(serde_json::json!({"id": 42})).unwrap();
    assert_eq!(parsed.id, 42);
}

#[test]
fn act_rejects_empty_batch() {
    let err = act(ActInput {
        actions: vec![],
        screenshot: false,
    })
    .unwrap_err();
    assert!(err.to_string().contains("non-empty"));
}

#[test]
#[ignore = "live: moves the separate agent cursor through the real GNOME bridge"]
fn live_computer_act_runs_a_batch_through_the_bridge() {
    // Non-destructive: moves only the SEPARATE agent cursor (no clicks, no
    // typing, no screenshot of the user's screen). Validates the batched
    // executor end-to-end against the live dev.phoenix.Cursor bridge.
    let out = act(ActInput {
        actions: vec![
            Action::Move(MoveInput {
                x: 300,
                y: 300,
                duration_ms: 0,
            }),
            Action::Wait(WaitInput { ms: 50 }),
            Action::Move(MoveInput {
                x: 900,
                y: 500,
                duration_ms: 0,
            }),
        ],
        screenshot: false,
    })
    .expect("batch should run against the live bridge");
    assert!(
        out.content.contains("Ran 3 action(s)"),
        "got: {}",
        out.content
    );
    assert!(out.content.contains("3. move"));
}

#[test]
fn window_act_rejects_empty_and_non_array_actions() {
    let err = window_act(WindowActInput {
        id: 1,
        actions: serde_json::json!([]),
        capture: false,
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("non-empty"), "got: {err}");

    let err = window_act(WindowActInput {
        id: 1,
        actions: serde_json::json!({"type": "click"}),
        capture: false,
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("JSON array"), "got: {err}");
}

#[test]
fn window_act_rejects_unknown_action_types_before_touching_the_bridge() {
    let err = window_act(WindowActInput {
        id: 1,
        actions: serde_json::json!([{"type": "unsupported_action", "x": 1, "y": 2}]),
        capture: false,
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("supported"), "got: {err}");
}

#[test]
fn window_act_rejects_oversized_batch() {
    let actions: Vec<serde_json::Value> = (0..26)
        .map(|_| serde_json::json!({"type": "wait", "ms": 1}))
        .collect();
    let err = window_act(WindowActInput {
        id: 1,
        actions: serde_json::Value::Array(actions),
        capture: false,
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("cap is 25"), "got: {err}");
}

#[test]
fn act_rejects_oversized_batch() {
    // The cap fires before any action runs, so this never touches the bridge.
    let actions = (0..ACT_BATCH_CAP + 1)
        .map(|_| Action::Wait(WaitInput { ms: 0 }))
        .collect();
    let err = act(ActInput {
        actions,
        screenshot: false,
    })
    .unwrap_err();
    assert!(err.to_string().contains("too long"));
}

#[test]
fn setup_help_names_the_extension_and_fallback() {
    assert!(SETUP_HELP.contains("phoenix-cursor@phoenix.dev"));
    assert!(SETUP_HELP.contains("xdotool"));
}

#[test]
fn atspi_helper_uses_action_api_for_action_names() {
    assert!(ATSPI_HELPER.contains("def action_name"));
    assert!(ATSPI_HELPER.contains("get_action_name(index)"));
    assert!(!ATSPI_HELPER.contains("hasattr(action_iface, \"get_name\")"));
}

#[test]
fn atspi_helper_exposes_screen_bounds_for_mouse_targeting() {
    // The a11y-to-mouse bridge: the helper must read the Component interface's
    // SCREEN extents and expose a click center (cx,cy), plus a `locate` command
    // that resolves an element by text. Pin these so the "accessibility sees,
    // mouse acts" path can't silently regress to action-only.
    assert!(ATSPI_HELPER.contains("def screen_bounds"));
    assert!(ATSPI_HELPER.contains("Atspi.CoordType.SCREEN"));
    assert!(ATSPI_HELPER.contains("\"cx\""));
    assert!(ATSPI_HELPER.contains("def cmd_locate"));
}

// Live end-to-end of the REAL Rust path: app_locate → atspi_helper_path()
// (materializes the helper under ~/.phoenix/atspi) → shells python3 → parses
// the JSON → builds the "click it at (cx,cy)" guidance. Read-only (no click).
// Needs a running AT-SPI bus + a named app with a labelled element. Run:
//   cargo test --lib live_app_locate_resolves_click_point -- --ignored --nocapture
#[test]
#[ignore]
fn live_app_locate_resolves_click_point() {
    // Discover a real app from the live bus, then inspect it for any element
    // that has both a name and bounds, then locate THAT name — so the test is
    // not pinned to one machine's app set.
    let apps = app_targets(AppTargetsInput {}).expect("app_targets");
    eprintln!("apps: {}", apps.content);
    let inspect = app_inspect(AppInspectInput {
        app: "Zen".to_string(),
        max: Some(60),
    });
    let inspect = match inspect {
        Ok(o) => o,
        Err(e) => {
            eprintln!("no Zen app on this bus ({e}); skipping locate assertion");
            return;
        }
    };
    // Pull the first named element out of the inspect JSON.
    let v: serde_json::Value = serde_json::from_str(
        inspect
            .content
            .splitn(2, '\n')
            .nth(1)
            .unwrap_or(&inspect.content),
    )
    .expect("inspect JSON");
    let name = v["elements"]
        .as_array()
        .and_then(|els| {
            els.iter()
                .find(|e| e.get("bounds").is_some() && !e["name"].as_str().unwrap_or("").is_empty())
        })
        .and_then(|e| e["name"].as_str())
        .expect("some named+bounded element")
        .to_string();
    eprintln!("locating: {name}");
    let out = app_locate(AppLocateInput {
        app: "Zen".to_string(),
        query: Some(name.clone()),
        queries: None,
    })
    .expect("app_locate through the real Rust path");
    eprintln!("locate result: {}", out.content);
    assert!(
        out.content.contains("Click it with `computer_click`"),
        "guidance must tell the model to click the resolved point: {}",
        out.content
    );
}

#[test]
fn app_locate_requires_app_and_query() {
    let err = app_locate(AppLocateInput {
        app: "  ".to_string(),
        query: Some("Sign in".to_string()),
        queries: None,
    })
    .unwrap_err();
    assert!(err.to_string().contains("app"), "{err}");
}

#[test]
fn computer_use_prompt_prefers_private_desktop_grounding_and_mouse() {
    // Isolated Xephyr desktops do not share the host AT-SPI bus. The prompt
    // must teach screenshot/OCR grounding plus real X11 input and must not
    // resurrect host accessibility action tools.
    let p = include_str!("../../prompts/computer_use_system.md");
    assert!(p.contains("computer_locate"));
    assert!(p.contains("screenshots/OCR observe and mouse/keyboard actions change the interface"));
    assert!(!p.contains("computer_app_act"));
    assert!(!p.contains("computer_app_type"));
}

#[test]
fn computer_use_prompt_avoids_duplicate_observation_without_blind_dialog_actions() {
    // The old fixed call count encouraged clicking newly opened dialogs from
    // window titles alone. Keep economy and visual grounding together.
    let p = include_str!("../../prompts/computer_use_system.md");
    assert!(p.contains("smallest observation that grounds the next action"));
    assert!(p.contains("Avoid duplicate screenshots of unchanged state"));
    assert!(p.contains("does not show dialog contents or button coordinates"));
    assert!(p.contains("inspect that dialog before clicking"));
    assert!(p.contains("health probe for a failure, not a required opening move"));
}

#[test]
fn computer_use_prompt_teaches_the_private_desktop_mode() {
    // Each agent now owns a Xephyr/Xvfb display. The prompt must keep it from
    // reaching back into the user's host desktop/DBus session or retrying
    // host-only GNOME bridge tools.
    let p = include_str!("../../prompts/computer_use_system.md");
    assert!(p.contains("Private Agent Desktop"));
    assert!(p.contains("GNOME workspace"));
    assert!(p.contains("X11 workspace"));
    assert!(p.contains("direct scoped `.desktop` Exec launches"));
    assert!(p.contains("host session-bus activation"));
    assert!(p.contains("accessibility tools remain unavailable in isolated scopes"));
    assert!(!p.contains("runtime slips your input"));
    assert!(!p.contains("workflow runs layered on real windows"));
    assert!(!p.contains("desktop is a shared live surface"));
}

// --- one-turn perception rewrite (2026-07-08) -------------------------------

#[test]
fn app_locate_requires_app_and_at_least_one_query() {
    let err = app_locate(AppLocateInput {
        app: "Zen".to_string(),
        query: None,
        queries: None,
    })
    .unwrap_err();
    assert!(err.to_string().contains("queries"), "{err:#}");

    // Whitespace-only entries don't count as queries.
    let err = app_locate(AppLocateInput {
        app: "Zen".to_string(),
        query: Some("   ".to_string()),
        queries: Some(vec![String::new()]),
    })
    .unwrap_err();
    assert!(err.to_string().contains("queries"), "{err:#}");

    let err = app_locate(AppLocateInput {
        app: String::new(),
        query: Some("Add Game".to_string()),
        queries: None,
    })
    .unwrap_err();
    assert!(err.to_string().contains("app"), "{err:#}");
}

#[test]
fn app_read_requires_app() {
    let err = app_read(AppReadInput {
        app: "  ".to_string(),
        max_chars: None,
    })
    .unwrap_err();
    assert!(err.to_string().contains("computer_app_read"), "{err:#}");
}

#[test]
fn act_parses_element_grounded_steps() {
    // The whole point of click_element/type_into: a locate → click → type flow
    // as ONE batch step, located by visible text at execution time.
    let action: Action = serde_json::from_value(serde_json::json!({
        "type": "click_element", "app": "Zen", "query": "Add Game"
    }))
    .expect("click_element parses");
    assert_eq!(action.label(), "click_element");

    let action: Action = serde_json::from_value(serde_json::json!({
        "type": "type_into", "app": "Zen", "query": "Name", "text": "Azul"
    }))
    .expect("type_into parses");
    assert_eq!(action.label(), "type_into");

    // Old vocabulary still parses unchanged.
    let action: Action = serde_json::from_value(serde_json::json!({
        "type": "click", "x": 10, "y": 20
    }))
    .expect("plain click parses");
    assert_eq!(action.label(), "click");
}

#[test]
fn missed_locate_formats_closest_visible_texts() {
    // A miss must ground the NEXT step: the 2026-07-08 Pixel run guessed
    // "no games" / "No games found" / "no results" blind — three model
    // round-trips a single closest-texts line would have saved.
    let miss = serde_json::json!({
        "ok": false,
        "query": "no games",
        "closest": [
            {"text": "No games match your search", "role": "text", "cx": 640, "cy": 512},
            {"text": "0 games", "role": "text", "cx": 900, "cy": 300}
        ]
    });
    let formatted = format_closest(&miss);
    assert!(formatted.contains("Closest visible texts"), "{formatted}");
    assert!(
        formatted.contains("No games match your search"),
        "{formatted}"
    );
    assert!(formatted.contains("at (640, 512)"), "{formatted}");
    assert!(formatted.contains("at (900, 300)"), "{formatted}");

    // No closest data → no noise appended.
    assert!(format_closest(&serde_json::json!({"ok": false})).is_empty());
    assert!(format_closest(&serde_json::json!({"ok": false, "closest": []})).is_empty());
}

#[test]
fn atspi_helper_carries_the_perception_rewrite() {
    // The bundled helper is stamped to ~/.phoenix/atspi on content change; pin
    // the rewrite's load-bearing pieces so an edit can't silently drop them:
    // text-content matching (a bare `<span>5 games</span>` has an EMPTY
    // accessible name), the read command, multi-query locate, closest-on-miss.
    let helper = include_str!("../../desktop/atspi/phoenix_atspi.py");
    assert!(helper.contains("def text_content"));
    assert!(helper.contains("def cmd_read"));
    assert!(helper.contains("locate-many"));
    assert!(helper.contains("def closest_texts"));
    assert!(helper.contains("get_text_iface"));
    assert!(helper.contains("def serve"));
    assert!(helper.contains("def cmd_activate"));
    assert!(helper.contains("def cmd_settext_query"));
    assert!(helper.contains("action_iface.do_action"));
}

#[test]
fn computer_use_prompt_teaches_one_call_checklists_and_web_handback() {
    let p = include_str!("../../prompts/computer_use_system.md");
    assert!(p.contains("Checklists are ONE call, never one call per item"));
    assert!(p.contains("computer_read_text"));
    assert!(p.contains("local HTML files"));
    assert!(p.contains("click_element"));
    assert!(p.contains("type_into"));
}
#[test]
fn browser_window_guard_uses_application_identity_before_document_title() {
    for (app, title) in [
        ("blender", "File Browser"),
        ("org.blender.Blender", "Firefox.blend - Blender"),
        ("org.gnome.TextEditor", "browser.rs"),
        ("org.gnome.Nautilus", "Chrome"),
    ] {
        assert!(!is_browser_window_identity(app, title), "{app}: {title}");
    }
    for (app, title) in [
        ("firefox", "Banana reference"),
        ("google-chrome", "Blender"),
        ("org.mozilla.firefox", "File Browser"),
        ("", "Mozilla Firefox"),
        ("  ", "Chromium"),
    ] {
        assert!(is_browser_window_identity(app, title), "{app}: {title}");
    }
}
