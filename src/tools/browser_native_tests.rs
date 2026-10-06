use super::*;
use crate::config::test_env::PhoenixHomeGuard;

// Run only through scripts/check-embedded-input-runtime.mjs. It supplies one
// disposable production Electron shell; this must never launch or attach to
// the user's ordinary browser. No provider or model is involved.
#[test]
#[ignore]
fn live_embedded_input_runtime_reaches_cold_reloaded_and_switched_targets() {
    let _live = live_lock();
    let home = crate::config::phoenix_home();
    assert!(home.to_string_lossy().starts_with("/tmp/phoenix-ui-acceptance-input-"));
    assert_eq!(std::env::var("PHOENIX_EMBEDDED_INPUT_FIXTURE").as_deref(), Ok("1"));
    assert!(std::env::var("PHOENIX_CHROMIUM_BRIDGE_URL").is_ok());
    assert!(std::env::var("PHOENIX_CHROMIUM_BRIDGE_TOKEN").is_ok());
    assert_eq!(std::env::var("PHOENIX_BROWSER_LOGIN_SOURCE").as_deref(), Ok("none"));
    struct Close;
    impl Drop for Close { fn drop(&mut self) { super::shutdown(); } }
    let _close = Close;
    let instance = "agent-input-regression";
    let run = |name: &str, input: Value| super::execute(name, input, Some(instance), None, None, None).unwrap();
    let html = r#"<!doctype html><title>Input fixture</title>
      <style>body{margin:24px}button,input{display:block;width:260px;height:48px;margin:20px}</style>
      <button id="choose">Choose telescope</button><label for="note">Fixture note</label><input id="note">
      <script>window.clicks=0;window.synthetic=0;window.keys=0;
      choose.onclick=e=>{if(e.isTrusted)window.clicks++;else window.synthetic++;};
      note.onkeydown=e=>{if(e.isTrusted)window.keys++;};</script>"#;
    let url = format!("data:text/html,{}", urlencoding::encode(html));
    super::execute("browser_navigate",serde_json::json!({"url":"data:text/html,<title>PRIVATE PEER WORKSPACE</title><p>Private peer information</p>"}),
        Some("agent-private-peer-regression"),None,None,None).unwrap();
    let witness = || {
        let slot = session_slot(instance);
        let guard = slot.lock().unwrap();
        let session = guard.as_ref().unwrap();
        assert_eq!(session.health.mode, "embedded", "fixture must use the real parked Electron view");
        let value = eval_string(&session.tab,
            "JSON.stringify({clicks:window.clicks,synthetic:window.synthetic,keys:window.keys,note:document.getElementById('note').value,width:innerWidth,height:innerHeight})").unwrap();
        (session.tab.get_target_id().to_string(), serde_json::from_str::<Value>(&value).unwrap())
    };
    let index_for = |state: &ToolOutput, label: &str| {
        state.content.lines().find(|line| line.contains(label))
            .and_then(|line| line.split(']').next())
            .and_then(|prefix| prefix.trim().trim_start_matches('[').parse::<i64>().ok())
            .unwrap_or_else(|| panic!("missing exact fixture control {label}: {}", state.content))
    };
    let mut receipts = Vec::new();
    for phase in ["cold", "reloaded"] {
        run("browser_navigate", serde_json::json!({"url":url}));
        // Reloading an identical page legitimately returns the production
        // unchanged-state digest. Ask for its full registry, not a screenshot.
        let state = run("browser_state", serde_json::json!({}));
        assert!(!state.content.contains("PRIVATE PEER WORKSPACE"));
        assert!(!state.content.contains("canvas-app/ui/index.html"));
        assert!(state.content.contains("Fixture note"), "HTML label must identify its actual input index");
        let (target, before) = witness();
        assert_eq!(before["clicks"], 0);
        run("browser_click", serde_json::json!({"index":index_for(&state,"Choose telescope")}));
        let (after_target, after) = witness();
        assert_eq!(after_target, target);
        assert_eq!(after["clicks"], 1, "one trusted click must reach the cold view");
        assert_eq!(after["synthetic"], 0, "synthetic fallback is not evidence of physical input");
        assert_eq!(after["width"], before["width"]);
        assert_eq!(after["height"], before["height"]);
        receipts.push(serde_json::json!({"phase":phase,"target":target,"observed":after}));
    }
    let original_target = witness().0;
    run("browser_navigate", serde_json::json!({"url":url,"new_tab":true}));
    let second = run("browser_state", serde_json::json!({}));
    let own_tabs: Vec<_> = second.content.lines().filter(|line| line.starts_with("  Tab ")).collect();
    assert_eq!(own_tabs.len(), 2, "only the two exact owned tabs belong in model context");
    assert!(!second.content.contains("PRIVATE PEER WORKSPACE"));
    assert!(!second.content.contains("canvas-app/ui/index.html"));
    let second_target = witness().0;
    assert_ne!(original_target, second_target);
    run("browser_click", serde_json::json!({"index":index_for(&second,"Choose telescope")}));
    assert_eq!(witness().1["clicks"], 1);
    run("browser_switch", serde_json::json!({"tab_id":original_target}));
    let first = run("browser_state", serde_json::json!({}));
    assert_eq!(witness().1["clicks"], 1, "inactive peer was not clicked");
    run("browser_click", serde_json::json!({"index":index_for(&first,"Choose telescope")}));
    let current = run("browser_state", serde_json::json!({}));
    run("browser_input", serde_json::json!({"index":index_for(&current,"Fixture note"),"text":"abc","clear":true}));
    let (target, first_after) = witness();
    assert_eq!(target, original_target);
    assert_eq!(first_after["clicks"], 2);
    assert_eq!(first_after["note"], "abc");
    assert!(first_after["keys"].as_u64().unwrap() >= 3);
    assert_eq!(first_after["synthetic"], 0);
    run("browser_switch", serde_json::json!({"tab_id":second_target}));
    let (_, second_after) = witness();
    assert_eq!(second_after["clicks"], 1);
    assert_eq!(second_after["note"], "");
    receipts.push(serde_json::json!({"phase":"switch-and-type","first":first_after,"second":second_after}));
    std::fs::write(home.join("runtime-input-result.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "passed":true,"mode":"embedded","receipts":receipts,"provider_calls":0,
        "html_label_preserved":true,"only_owned_tabs_listed":true,
        "scope":"Compiled model-tool entrypoints and the newly loaded real Electron readiness endpoint; DOM is read only as an independent outcome witness."})).unwrap()).unwrap();
}

#[test]
fn direct_browser_controls_reconnect_only_transport_and_target_discovery_failures() {
    assert!(direct_interaction_transport_error(
        "Unable to make method calls because underlying connection is closed"
    ));
    assert!(direct_interaction_transport_error(
        "the embedded browser tab was not discoverable over CDP"
    ));
    assert!(direct_interaction_transport_error(
        "target inspection failed: connection closed"
    ));
    assert!(!direct_interaction_transport_error(
        "no visible element matched Continue"
    ));
    assert!(!direct_interaction_transport_error(
        "navigation was rejected by the website"
    ));
}

#[test]
fn native_new_tab_recovers_electron_cdp_session_errors_without_masking_page_errors() {
    assert!(direct_new_tab_session_error(
        "Method call error -32602: No session with given id"
    ));
    assert!(direct_new_tab_session_error(
        "Method call error -32000: Not supported"
    ));
    assert!(direct_new_tab_session_error(
        "opening a new embedded browser tab failed: target closed"
    ));
    assert!(!direct_new_tab_session_error(
        "navigation was rejected by the website"
    ));
}

/// Tests drive the canonical instance; production's `execute` adds the
/// parallel-instance key and the journal session id. A local item shadows the
/// glob import, so every call site below stays 2-arg.
fn execute(tool_name: &str, input: Value) -> Result<ToolOutput> {
    super::execute(tool_name, input, None, None, None, None)
}

#[test]
fn login_wall_urls_are_classified_conservatively() {
    // The live Discord loop: /login with a redirect_to bounce.
    assert!(is_login_wall_url(
        "https://discord.com/login?redirect_to=%2Fchannels%2F%40me"
    ));
    assert!(is_login_wall_url("https://github.com/login"));
    assert!(is_login_wall_url("https://example.com/users/sign_in"));
    assert!(is_login_wall_url("https://example.com/auth/login/next"));
    // Bounce pattern without a login-ish path still counts.
    assert!(is_login_wall_url("https://site.com/gate?redirect_to=/home"));
    // Ordinary pages — including ones that merely CONTAIN the word — do not.
    assert!(!is_login_wall_url("https://discord.com/channels/@me"));
    assert!(!is_login_wall_url(
        "https://blog.example.com/why-logins-fail"
    ));
    assert!(!is_login_wall_url("chrome://newtab/"));
    assert!(!is_login_wall_url("about:blank"));
}

#[test]
fn missing_navigation_event_accepts_a_committed_destination() {
    let error = "The event waited for never came";
    assert!(navigation_wait_failure_is_usable(
        error,
        "about:blank",
        "https://x.com/home",
        "https://x.com/home",
        "complete",
    ));
    assert!(navigation_wait_failure_is_usable(
        error,
        "about:blank",
        "https://x.com/i/flow/login",
        "https://x.com/home",
        "interactive",
    ));
}

#[test]
fn missing_navigation_event_rejects_unproven_or_stale_pages() {
    let error = "The event waited for never came";
    assert!(!navigation_wait_failure_is_usable(
        error,
        "https://example.com/old",
        "https://example.com/old",
        "https://x.com/home",
        "complete",
    ));
    assert!(!navigation_wait_failure_is_usable(
        error,
        "about:blank",
        "https://x.com/home",
        "https://x.com/home",
        "loading",
    ));
    assert!(!navigation_wait_failure_is_usable(
        "net::ERR_NAME_NOT_RESOLVED",
        "about:blank",
        "https://x.com/home",
        "https://x.com/home",
        "complete",
    ));
}

#[test]
fn taught_browser_actions_classify_password_and_verification_fields_as_sensitive() {
    let mut password = BrowserSemanticTarget::default();
    password.input_type = "password".into();
    assert!(semantic_target_is_sensitive(&password));

    let mut otp = BrowserSemanticTarget::default();
    otp.input_type = "text".into();
    otp.autocomplete = "one-time-code".into();
    assert!(semantic_target_is_sensitive(&otp));

    let mut api_key = BrowserSemanticTarget::default();
    api_key.label = "API key".into();
    assert!(semantic_target_is_sensitive(&api_key));

    let mut ordinary = BrowserSemanticTarget::default();
    ordinary.label = "Search projects".into();
    assert!(!semantic_target_is_sensitive(&ordinary));
}

// Real CDP verification for the desktop's teach-agent path. It proves that a
// user click focuses the semantic target, real key events type into it, and
// the returned receipt classifies the password without echoing its value.
#[test]
#[ignore]
fn live_taught_browser_interaction_clicks_types_and_redacts() {
    let _live = live_lock();
    let home = tempfile::tempdir().unwrap();
    let _home = PhoenixHomeGuard::set_private(home.path());
    std::env::set_var("PHOENIX_BROWSER_HEADLESS", "1");
    std::env::set_var("PHOENIX_BROWSER_PROFILE", "phoenix");
    let instance = "agent-teach-live";
    let mut frames = super::frames_subscribe();
    let navigate = BrowserUserAction::Navigate {
        url: "data:text/html,<style>input{position:absolute;left:10px;top:10px;width:240px;height:44px}@keyframes glow{from{background:%23fff}to{background:%23f80}}body{animation:glow .35s infinite alternate}</style><label for=p>Account password</label><input id=p name=password type=password>".into(),
        new_tab: true,
    };
    user_interact(instance, &navigate).expect("teaching navigation failed");
    let resized = user_interact(
        instance,
        &BrowserUserAction::Resize {
            width: 1024,
            height: 720,
        },
    )
    .expect("teaching viewport resize failed");
    assert_eq!(resized.action, "resize");
    let viewport = super::execute(
        "browser_evaluate",
        serde_json::json!({"code": "JSON.stringify({width: window.innerWidth, height: window.innerHeight})"}),
        Some(instance),
        None,
        None,
        None,
    )
    .expect("teaching viewport witness failed");
    assert!(
        viewport.content.contains("\\\"width\\\":1024")
            && viewport.content.contains("\\\"height\\\":720"),
        "browser page did not adopt the requested viewport: {}",
        viewport.content
    );
    let click = BrowserUserAction::Click { x: 40.0, y: 30.0 };
    let click_started = std::time::Instant::now();
    let clicked = user_interact(instance, &click).expect("teaching click failed");
    assert!(
        click_started.elapsed() < std::time::Duration::from_millis(750),
        "local teaching click took {:?}",
        click_started.elapsed()
    );
    eprintln!("live browser click latency: {:?}", click_started.elapsed());
    assert_eq!(
        clicked
            .target
            .as_ref()
            .map(|target| target.element_id.as_str()),
        Some("p")
    );
    let typing = BrowserUserAction::Type {
        text: "secret-that-must-not-be-returned".into(),
        clear: true,
        sensitive: false,
        parameter_name: None,
        target_hint: clicked.target.clone(),
    };
    let type_started = std::time::Instant::now();
    let typed = user_interact(instance, &typing).expect("teaching type failed");
    assert!(
        type_started.elapsed() < std::time::Duration::from_millis(750),
        "local teaching input took {:?}",
        type_started.elapsed()
    );
    eprintln!("live browser input latency: {:?}", type_started.elapsed());
    assert!(typed.sensitive);
    let encoded = serde_json::to_string(&typed).unwrap();
    assert!(!encoded.contains("secret-that-must-not-be-returned"));
    // Old canvases and already-taught routines persisted uppercase names.
    // They must remain valid at the CDP boundary (the reported regression was
    // three `Key not found: BACKSPACE` toasts).
    user_interact(
        instance,
        &BrowserUserAction::SendKeys {
            keys: "BACKSPACE".into(),
        },
    )
    .expect("legacy uppercase Backspace failed");
    let witness = super::execute(
        "browser_evaluate",
        serde_json::json!({"code": "document.querySelector('#p').value.length"}),
        Some(instance),
        None,
        None,
        None,
    )
    .expect("password field witness failed");
    assert!(witness.content.contains("31"), "{}", witness.content);
    let frame_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut frame_count = 0usize;
    let mut max_width = 0u32;
    while std::time::Instant::now() < frame_deadline {
        match frames.try_recv() {
            Ok(frame) if frame.instance == instance => {
                frame_count += 1;
                max_width = max_width.max(frame.w);
            }
            Ok(_) | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                std::thread::sleep(std::time::Duration::from_millis(4));
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
        }
    }
    shutdown();
    assert!(
        frame_count >= 20,
        "animated teaching page delivered only {frame_count} frames in 2s"
    );
    assert!(
        max_width >= 950,
        "resized teaching frames stayed blurry at only {max_width}px wide"
    );
    eprintln!("live browser screencast: {frame_count} frames/2s, max width {max_width}px");
}

#[test]
fn login_wall_streak_counts_per_host_and_clears_on_success() {
    let home = tempfile::tempdir().unwrap();
    let _home = PhoenixHomeGuard::set_private(home.path());
    let instance = format!("wall-test-{}", uuid::Uuid::new_v4());
    let wall = "https://discord.com/login?redirect_to=%2Fchannels%2F%40me";
    assert_eq!(login_wall_streak(&instance, None, wall), Some(1));
    assert_eq!(login_wall_streak(&instance, None, wall), Some(2));
    // Another host's wall keeps its own streak.
    assert_eq!(
        login_wall_streak(&instance, None, "https://github.com/login"),
        Some(1)
    );
    assert_eq!(login_wall_streak(&instance, None, wall), Some(3));
    // A non-wall page on the host = the login landed; the streak resets.
    assert_eq!(
        login_wall_streak(&instance, None, "https://discord.com/channels/@me"),
        None
    );
    assert_eq!(login_wall_streak(&instance, None, wall), Some(1));
    // Passive looks never count: only location-establishing actions do.
    assert!(action_counts_wall("navigate"));
    assert!(action_counts_wall("click"));
    assert!(!action_counts_wall("state"));
    assert!(!action_counts_wall("screenshot"));
    assert!(!action_counts_wall("console"));
}

#[test]
fn login_wall_streak_survives_gateway_memory_loss() {
    let home = tempfile::tempdir().unwrap();
    let _home = PhoenixHomeGuard::set_private(home.path());
    let instance = format!("wall-restart-{}", uuid::Uuid::new_v4());
    let session_id = format!("wall-session-{}", uuid::Uuid::new_v4());
    let wall = "https://github.com/login";
    assert_eq!(
        login_wall_streak(&instance, Some(&session_id), wall),
        Some(1)
    );
    LOGIN_WALLS
        .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clear();
    assert_eq!(
        login_wall_streak(&instance, Some(&session_id), wall),
        Some(2)
    );
}

#[test]
fn environment_banners_name_the_instability_and_the_user_impact() {
    // First relaunch: loud, names the destroyed user step.
    let first = relaunch_note(1);
    assert!(first.contains("restart #1"));
    assert!(first.contains("did NOT complete"));
    assert!(!first.contains("UNSTABLE"));
    // Repeat relaunches: the environment is declared unstable — the agent is
    // told to change strategy, not re-run the plan.
    let second = relaunch_note(2);
    assert!(second.contains("UNSTABLE"));
    assert!(second.contains("ask_user"));
    // The login-loop notice escalates and carries the crash context.
    let notice = login_wall_notice(3, "discord.com", 2);
    assert!(notice.contains("sighting #3 on discord.com"));
    assert!(notice.contains("restarted 2×"));
    assert!(notice.contains("credential_list"));
    assert!(notice.contains("browser_input_credential"));
    assert!(notice.contains("do not ask the user for a password Phoenix already has"));
    assert!(notice.contains("pre-submit field/remount failure is not a rejected password"));
    assert!(notice.contains("preserve the unfinished"));
    assert!(notice.contains("reuse any pending login request"));
    assert!(notice.contains("Do not open a generic ask_user login"));
    assert!(notice.contains("or repeat a dismissed request"));
    assert!(notice.contains("A missing desktop bridge is a runtime failure"));
    let calm = login_wall_notice(4, "github.com", 0);
    assert!(!calm.contains("restarted"));
}

#[test]
fn host_prefix_cookie_is_host_only_secure_root() {
    use crate::tools::browser_cookies::PortedCookie;
    // __Host- with a domain + non-root path + not-secure must be corrected,
    // or Chrome silently rejects it (this was dropping github's auth cookie).
    let p = to_cookie_param(PortedCookie {
        domain: ".github.com".into(),
        name: "__Host-user_session_same_site".into(),
        value: "v".into(),
        path: "/login".into(),
        secure: false,
        http_only: true,
        same_site: None,
        expires: None,
    });
    assert_eq!(p.domain, None, "__Host- must be host-only");
    assert_eq!(p.path.as_deref(), Some("/"));
    assert_eq!(p.secure, Some(true));
    // __Secure- forces secure but keeps its domain.
    let s = to_cookie_param(PortedCookie {
        domain: ".x.com".into(),
        name: "__Secure-tok".into(),
        value: "v".into(),
        path: "/".into(),
        secure: false,
        http_only: false,
        same_site: None,
        expires: None,
    });
    assert_eq!(s.secure, Some(true));
    assert_eq!(s.domain.as_deref(), Some(".x.com"));
}

#[test]
fn configured_login_source_is_an_authoritative_cookie_overlay() {
    // A managed Chrome session may contain an expired cookie with the same
    // name/domain/path as the fresh Zen cookie. Filtering source cookies
    // against Chrome silently preserved the expired value and logged Surf out.
    // The configured login source must therefore be sent as one complete
    // SetCookies overlay; Chrome-only cookies remain because nothing deletes
    // them.
    let src = include_str!("browser_native/session.rs");
    let port = src
        .split_once("fn import_cookie_set")
        .and_then(|(_, rest)| rest.split_once("\n    /// Cleanly flush"))
        .map(|(body, _)| body)
        .expect("shared cookie injection path present");
    assert!(port.contains("Network::SetCookies"));
    assert!(port.contains("Network::SetCookies { cookies: params }"));
    assert!(!port.contains("cookies_absent_from"));
    assert!(!port.contains("Network::DeleteCookies"));
}

#[test]
fn indexed_browser_state_redacts_password_and_one_time_code_values() {
    let source = super::actions::INDEXER_JS;
    assert!(source.contains("secretField"));
    assert!(source.contains("data-phx-vault-secret"));
    assert!(source.contains("[secret set]"));
    assert!(source.contains("one-time-code"));
    assert!(source.contains("secretField?'':"));
}

#[test]
fn credential_fill_recovers_one_semantically_identical_remounted_field() {
    let source = include_str!("browser_native/actions.rs");
    assert!(source.contains("window.__phxSecretFingerprint"));
    assert!(source.contains("REFIND_SECRET_FIELD_JS"));
    assert!(source.contains("candidates.length!==1"));
    assert!(source.contains("credential retry into remounted"));
    assert!(source.contains("if recovered == \"READY\""));
    assert!(source.contains("SECRET_FIELD_TARGET_STATUS_JS"));
    assert!(source.contains("REQUIRED_STABLE_READS: u8 = 3"));
    assert!(source.contains("CLEAR_SECRET_TRACKING_JS"));
}

#[test]
fn credential_origin_is_live_https_only_and_rechecked_on_retry() {
    assert_eq!(
        credential_host_for_url("https://VVS-MOODLE.PEMBINAHILLS.CA/login/index.php").unwrap(),
        "vvs-moodle.pembinahills.ca"
    );
    assert!(credential_host_for_url("about:blank").is_err());
    assert!(credential_host_for_url("file:///tmp/login.html").is_err());
    assert!(credential_host_for_url("javascript:alert(1)").is_err());

    let source = include_str!("browser_native/mod.rs");
    let attempt = source
        .split_once("fn attempt_action(")
        .map(|(_, body)| body)
        .expect("attempt_action exists");
    assert!(attempt.contains("validate_credential_origin(session, credential)?"));
    assert!(source.contains("the secret was not replayed"));
    assert!(source.contains("Some(Err(retry_err)) if !is_transport_dead(&retry_err)"));
}

#[test]
fn quiet_browser_transport_never_self_destructs_between_agent_actions() {
    let source = include_str!("../../vendor/headless_chrome/src/browser/transport/mod.rs");
    let timeout = source
        .split_once("RecvTimeoutError::Timeout =>")
        .map(|(_, body)| body)
        .expect("transport timeout branch exists");
    assert!(timeout.contains("keeping the websocket alive"));
    assert!(timeout.contains("continue;"));
}

// Real regression proof for the failure Avery hit: leave an isolated Chromium
// completely quiet for several transport idle windows, then issue a fresh CDP
// command. Before the transport patch this deterministically failed because
// the incoming loop closed its own websocket on the first quiet timeout.
#[test]
#[ignore]
fn live_isolated_chromium_survives_multiple_transport_idle_windows() {
    let Some(binary) = which_chrome() else {
        eprintln!("skipping live idle transport test: Chromium is unavailable");
        return;
    };
    let profile = tempfile::Builder::new()
        .prefix("phx-idle-transport-")
        .tempdir()
        .expect("isolated Chromium profile");
    let mut builder = LaunchOptionsBuilder::default();
    builder
        .path(Some(binary))
        .headless(true)
        .sandbox(false)
        .idle_browser_timeout(Duration::from_millis(250))
        .user_data_dir(Some(profile.path().to_path_buf()));
    let browser = Browser::new(builder.build().expect("valid launch options"))
        .expect("launch isolated Chromium");
    let tab = first_browser_tab(&browser).expect("initial Chromium tab");
    tab.navigate_to("data:text/html,<title>Phoenix idle witness</title>")
        .expect("navigate idle witness");
    tab.wait_until_navigated().expect("idle witness loaded");

    std::thread::sleep(Duration::from_millis(850));
    let title = eval_string(&tab, "document.title")
        .expect("CDP must remain usable after more than three idle windows");
    assert_eq!(title, "Phoenix idle witness");
}

// A real Chromium page replaces its password input immediately after focus,
// matching the Moodle hydration failure. The protected fill must re-find the
// unique replacement, keep it populated, and continue redacting its value.
#[test]
#[ignore]
fn live_credential_fill_survives_a_framework_remount() {
    let _live = live_lock();
    let home = tempfile::tempdir().expect("isolated Phoenix home");
    let _home_guard = PhoenixHomeGuard::set_private(home.path());
    std::env::set_var("PHOENIX_BROWSER_HEADLESS", "1");
    std::env::set_var("PHOENIX_BROWSER_PROFILE", "phoenix");
    let mut session = Session::open("agent-credential-job-remount-live")
        .expect("launch isolated credential browser");
    let html = "data:text/html,<input id=p name=password type=password placeholder=Password><script>p.addEventListener('focus',()=>setTimeout(()=>p.replaceWith(p.cloneNode()),75),{once:true})</script>";
    session
        .tab
        .navigate_to(html)
        .expect("navigate remount fixture");
    session
        .tab
        .wait_until_navigated()
        .expect("remount fixture loaded");
    let before = index_state(&session).expect("index password field");
    assert!(before.contains("[0]<input"), "{before}");

    actions::input_secret(&mut session, 0, "protected-test-secret")
        .expect("protected fill must recover the remounted password input");
    let length = eval_string(&session.tab, "document.querySelector('#p').value.length")
        .expect("read non-secret length witness");
    assert_eq!(length, "21");
    let after = index_state(&session).expect("redacted state after secure fill");
    assert!(after.contains("[secret set]"), "{after}");
    assert!(
        !after.contains("protected-test-secret"),
        "secret leaked into state"
    );
}

#[test]
fn idle_suspend_restore_request_is_explicit_and_one_shot() {
    let instance = "agent-idle-restore-test";
    super::clear_restore_requested(instance);
    assert!(!super::restore_requested(instance));
    super::mark_restore_requested(instance);
    assert!(super::restore_requested(instance));
    super::clear_restore_requested(instance);
    assert!(!super::restore_requested(instance));
}

#[test]
fn login_port_injects_cookies_in_one_batch_not_per_cookie_delete() {
    // Regression guard for the silent-logout bug: headless_chrome's
    // `Tab::set_cookies` does one `Network.deleteCookies` round-trip per
    // cookie before `setCookies`. With a real Firefox/Zen store (~1200
    // cookies) that blew past port_logins' 8s budget after ~600 deletes, so
    // `setCookies` never ran and every login was lost. port_logins MUST use
    // the single-batch `Network::SetCookies` call instead. Pin both facts in
    // the source so a future refactor can't quietly regress to the slow path.
    let src = include_str!("browser_native/session.rs");
    let port = src
        .split_once("fn import_cookie_set")
        .and_then(|(_, rest)| rest.split_once("\n    /// Cleanly flush"))
        .map(|(body, _)| body)
        .expect("shared cookie injection path present");
    assert!(
        port.contains("Network::SetCookies"),
        "port_logins must inject via the single-batch Network::SetCookies call"
    );
    assert!(
        !port.contains(".set_cookies("),
        "port_logins must NOT call headless_chrome's per-cookie set_cookies \
             (the per-cookie delete loop that caused the silent-logout wedge)"
    );
}

#[test]
fn tool_names_resolve() {
    assert!(is_browser_tool("browser_click"));
    assert!(is_browser_tool("browser_state"));
    assert!(is_browser_tool("browser_status"));
    assert!(!is_browser_tool("browser_quit"));
    assert!(!is_browser_tool("browser_task"));
    assert!(!is_browser_tool("read"));
}

#[test]
fn local_status_line_reports_config_without_launching_browser() {
    let _guard = BROWSER_ENV_LOCK.lock().unwrap();
    let prev_profile = std::env::var_os("PHOENIX_BROWSER_PROFILE");
    let prev_attach = std::env::var_os("PHOENIX_BROWSER_ATTACH");
    let prev_headless = std::env::var_os("PHOENIX_BROWSER_HEADLESS");
    let prev_login = std::env::var_os("PHOENIX_BROWSER_LOGIN_SOURCE");
    let prev_binary = std::env::var_os("PHOENIX_BROWSER_BINARY");
    std::env::set_var("PHOENIX_BROWSER_PROFILE", "zen");
    std::env::set_var("PHOENIX_BROWSER_ATTACH", "9223");
    std::env::set_var("PHOENIX_BROWSER_HEADLESS", "1");
    std::env::set_var("PHOENIX_BROWSER_LOGIN_SOURCE", "zen");
    std::env::set_var("PHOENIX_BROWSER_BINARY", "/tmp/fake-chrome");

    let line = local_status_line();

    restore_env_var("PHOENIX_BROWSER_PROFILE", prev_profile);
    restore_env_var("PHOENIX_BROWSER_ATTACH", prev_attach);
    restore_env_var("PHOENIX_BROWSER_HEADLESS", prev_headless);
    restore_env_var("PHOENIX_BROWSER_LOGIN_SOURCE", prev_login);
    restore_env_var("PHOENIX_BROWSER_BINARY", prev_binary);
    assert!(line.contains("browser · source zen"), "{line}");
    assert!(line.contains("explicit 9223"), "{line}");
    assert!(line.contains("binary /tmp/fake-chrome"), "{line}");
    assert!(line.contains("login source zen"), "{line}");
    assert!(line.contains("headless on"), "{line}");
}

#[test]
fn managed_browser_defaults_use_hardware_gl_without_resource_throttles() {
    let source = include_str!("browser_native/session.rs");
    let launch = source
        .split_once("let mut args: Vec<String>")
        .and_then(|(_, rest)| rest.split_once("args.extend(prefs.extra_args"))
        .map(|(body, _)| body)
        .expect("managed Chrome launch args");
    assert!(launch.contains("--use-gl=angle"));
    assert!(launch.contains("--use-angle=gl"));
    assert!(!launch.contains("--renderer-process-limit"));
    assert!(!launch.contains("--disk-cache-size"));
    assert!(!launch.contains("--media-cache-size"));
    assert!(!launch.contains("swiftshader"));
}

#[test]
fn native_surface_launch_is_x11_app_mode_with_a_unique_class() {
    let token = "0123456789abcdef0123456789abcdef";
    let args = native_surface_launch_args(Some(token));
    #[cfg(target_os = "linux")]
    {
        assert!(args.iter().any(|arg| arg == "--ozone-platform=x11"));
        assert!(args.iter().any(|arg| arg == "--app=about:blank"));
        assert!(args
            .iter()
            .any(|arg| arg == &format!("--class=phoenix-browser-{token}")));
        assert!(args
            .iter()
            .any(|arg| arg == "--window-position=-32000,-32000"));
    }
    assert!(native_surface_launch_args(None).is_empty());
}

#[test]
fn managed_browser_is_never_a_separate_visible_window() {
    assert!(managed_launch_headless(None));
    assert!(managed_launch_headless(Some(
        "0123456789abcdef0123456789abcdef"
    )));
    assert!(!BROWSER_ACTIONS.contains(&"visibility"));
    assert!(!BROWSER_ACTIONS.contains(&"login_handoff"));
    let runtime_source = include_str!("browser_native/mod.rs");
    let session_source = include_str!("browser_native/session.rs");
    let actions_source = include_str!("browser_native/actions.rs");
    let legacy_cleanup_source = include_str!("browser_native/handoff.rs");
    assert!(runtime_source.contains("current_embedded_target"));
    assert!(runtime_source.contains("session.health.mode == \"embedded\""));
    assert!(session_source.contains("embedded_browser_target_id"));
    assert!(session_source.contains("current_browser_environment()?"));
    assert!(session_source.contains("will not launch as a separate app"));
    assert!(session_source.contains("if session.health.mode != \"embedded\""));
    assert!(session_source.contains("if self.health.mode != \"embedded\""));
    assert!(actions_source.contains("fn open_embedded_tab"));
    assert!(actions_source.contains("if new_tab && is_embedded_session(session)"));
    assert!(actions_source.contains("switch_embedded_tab(session"));
    assert!(actions_source.contains("close_embedded_tab(session"));
    assert!(session_source.contains("super::detach_native_surface(instance)"));
    assert!(!runtime_source.contains("set_visibility_override(&instance, visible)"));
    assert!(!legacy_cleanup_source.contains("pub(super) fn open("));
}

#[test]
fn native_surface_profile_ids_reject_window_identity_injection() {
    for invalid in ["../escape", "agent one", "a/b", "agent;touch", "é"] {
        assert!(
            validate_browser_surface_instance(invalid).is_err(),
            "{invalid}"
        );
    }
    assert!(validate_browser_surface_instance("").is_ok());
    assert!(validate_browser_surface_instance("agent-nico_2").is_ok());
}

#[test]
fn combos_parse() {
    let (mods, key) = parse_combo("Control+a").unwrap();
    assert!(matches!(mods.as_slice(), [ModifierKey::Ctrl]));
    assert_eq!(key, "a");
    assert!(parse_combo("Enter").is_none());
}

#[test]
fn urls_normalize() {
    assert_eq!(normalize_url("example.com".into()), "https://example.com");
    assert_eq!(normalize_url("https://x.io".into()), "https://x.io");
    assert_eq!(
        normalize_url("file:///tmp/x.html".into()),
        "file:///tmp/x.html"
    );
}

#[test]
fn locked_profile_essentials_copy() {
    let src = tempfile::tempdir().expect("source profile root");
    let home = tempfile::tempdir().expect("isolated Phoenix home");
    std::fs::create_dir_all(src.path().join("Default")).unwrap();
    std::fs::write(src.path().join("Local State"), "{}").unwrap();
    std::fs::write(src.path().join("Default/Cookies"), "cookies").unwrap();
    std::fs::write(src.path().join("Default/Login Data"), "logins").unwrap();
    let _home_guard = PhoenixHomeGuard::set_private(home.path());
    let dst = copy_profile_essentials(src.path()).expect("copy failed");
    assert!(dst.join("Default/Cookies").is_file());
    assert!(dst.join("Default/Login Data").is_file());
    assert!(dst.join("Local State").is_file());
}

#[test]
fn profile_copy_never_retains_credentials_missing_from_the_new_donor() {
    let first = tempfile::tempdir().expect("first profile root");
    let second = tempfile::tempdir().expect("second profile root");
    let home = tempfile::tempdir().expect("isolated Phoenix home");
    std::fs::create_dir_all(first.path().join("Default")).unwrap();
    std::fs::create_dir_all(second.path().join("Default")).unwrap();
    std::fs::write(first.path().join("Default/Cookies"), "old-secret-cookie").unwrap();
    let _home_guard = PhoenixHomeGuard::set_private(home.path());
    let dst = copy_profile_essentials(first.path()).expect("first copy failed");
    assert!(dst.join("Default/Cookies").is_file());
    copy_profile_essentials(second.path()).expect("second copy failed");
    assert!(
        !dst.join("Default/Cookies").exists(),
        "credentials from an earlier donor must not bleed into a new profile"
    );
}

// Phoenix retains the existing canonical profile, while every other coworker
// gets a stable private directory and cannot escape through a crafted id.
#[test]
fn coworker_profiles_are_durable_private_and_path_safe() {
    let home = tempfile::tempdir().expect("isolated Phoenix home");
    let _home_guard = PhoenixHomeGuard::set_private(home.path());
    let canonical = home.path().join("browser/profile");
    assert_eq!(
        profile_dir_for_instance("agent-phoenix", "phoenix").unwrap(),
        canonical,
        "existing Phoenix logins stay active after migration"
    );
    assert_eq!(
        profile_dir_for_instance("agent-finance", "phoenix").unwrap(),
        home.path().join("browser/profiles/agent-finance")
    );
    assert!(profile_dir_for_instance("../../escape", "phoenix").is_err());
}

#[test]
fn absent_parent_cookie_jar_keeps_a_volume_worker_browser_empty_and_separate() {
    let home = tempfile::tempdir().expect("isolated Phoenix home");
    let _home_guard = PhoenixHomeGuard::set_private(home.path());
    let parent = profile_dir_for_instance("agent-coder", "phoenix").unwrap();
    let worker = profile_dir_for_instance("volume-worker-volume-test-1", "phoenix").unwrap();
    assert_ne!(parent, worker, "worker must never share the parent profile");
    assert!(
        crate::tools::chromium_cookies::read_managed_profile_cookies(&parent)
            .unwrap()
            .is_empty(),
        "a parent that never opened its browser is an empty portable snapshot"
    );
    assert!(
        !worker.exists(),
        "reading a missing parent jar must not pre-create the worker profile"
    );
}

/// Live tests drive the canonical instance's process-global browser slot (and real env
/// vars), so two live tests on parallel test threads corrupt each other —
/// one test's `shutdown()` kills the other's page. Observed 2026-07-01 as
/// 11/15 "failures" in a default-parallel `--ignored` run that all pass
/// serially. Every live test takes this lock first, making
/// `cargo test -- --ignored` safe at any thread count.
fn live_lock() -> std::sync::MutexGuard<'static, ()> {
    static LIVE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LIVE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[test]
fn js_string_escapes() {
    assert_eq!(js_string("a\"b"), "\"a\\\"b\"");
}

#[test]
fn shutdown_slot_acquisition_is_bounded_when_an_action_holds_the_lock() {
    let slot = std::sync::Mutex::new(());
    let held = slot.lock().expect("hold browser slot");
    let budget = std::time::Duration::from_millis(25);
    let started = std::time::Instant::now();
    assert!(
        try_lock_for(&slot, budget).is_none(),
        "shutdown must skip a contended instance instead of waiting forever"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "the bounded acquisition must return promptly"
    );
    drop(held);
    assert!(
        try_lock_for(&slot, budget).is_some(),
        "an uncontended instance must remain flushable"
    );
}

#[test]
fn shutdown_slot_locks_share_one_global_deadline() {
    let first = std::sync::Mutex::new(());
    let second = std::sync::Mutex::new(());
    let _first_held = first.lock().expect("hold first slot");
    let _second_held = second.lock().expect("hold second slot");
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(30);

    assert!(try_lock_until(&first, deadline).is_none());
    let after_first = std::time::Instant::now();
    assert!(try_lock_until(&second, deadline).is_none());
    assert!(
        after_first.elapsed() < std::time::Duration::from_millis(250),
        "an exhausted global deadline must not restart for the next slot"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn shutdown_pid_sweep_matches_only_same_root_phoenix_profiles() {
    let temp = tempfile::tempdir().expect("tempdir");
    let proc_root = temp.path().join("proc");
    let browser_root = temp.path().join("phoenix/browser");
    std::fs::create_dir_all(&browser_root).expect("browser root");
    for (pid, profile) in [
        (100_u32, browser_root.join("profile")),
        (101_u32, temp.path().join("real-browser/Default")),
        (102_u32, temp.path().join("phoenix/browser-evil/profile")),
    ] {
        let process = proc_root.join(pid.to_string());
        std::fs::create_dir_all(&process).expect("fake proc pid");
        std::fs::write(
            process.join("cmdline"),
            format!(
                "chromium\0--user-data-dir={}\0--no-first-run\0",
                profile.display()
            ),
        )
        .expect("fake cmdline");
    }
    let pids =
        phoenix_profile_holder_pids_at(&proc_root, &browser_root, unsafe { libc::geteuid() });
    assert_eq!(
        pids,
        vec![100],
        "the real browser and prefix-lookalike directories must never enter the shutdown kill set"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn scoped_pid_sweep_matches_one_exact_profile() {
    let temp = tempfile::tempdir().expect("tempdir");
    let proc_root = temp.path().join("proc");
    let leaf_one = temp.path().join("phoenix/browser/profile-leaf-1");
    for (pid, profile) in [
        (100_u32, leaf_one.clone()),
        (101_u32, temp.path().join("phoenix/browser/profile-leaf-10")),
        (102_u32, temp.path().join("phoenix/browser/profile-leaf-2")),
    ] {
        let process = proc_root.join(pid.to_string());
        std::fs::create_dir_all(&process).expect("fake proc pid");
        std::fs::write(
            process.join("cmdline"),
            format!("chromium\0--user-data-dir={}\0", profile.display()),
        )
        .expect("fake cmdline");
    }
    let pids = exact_profile_holder_pids_at(&proc_root, &leaf_one, unsafe { libc::geteuid() });
    assert_eq!(
        pids,
        vec![100],
        "targeted cleanup must not use prefix matching"
    );
}

#[test]
fn disposable_profile_shape_never_matches_a_durable_coworker() {
    assert!(is_disposable_browser_profile("volume-worker-volume-a-1"));
    assert!(is_disposable_browser_profile("agent-coder-job-4f9a2c"));
    assert!(!is_disposable_browser_profile("agent-coder"));
    assert!(!is_disposable_browser_profile("agent-phoenix"));
    assert!(!is_disposable_browser_profile(
        "agent-coder-job-../../escape"
    ));
}

#[test]
fn disposable_named_agent_jobs_can_seed_from_their_canonical_browser() {
    assert!(validate_volume_cookie_seed_pair("agent-coder-job-4f9a2c", "agent-coder").is_ok());
    assert!(
        validate_volume_cookie_seed_pair("volume-worker-volume-batch-1", "agent-school_coach")
            .is_ok()
    );
    assert!(validate_volume_cookie_seed_pair("agent-coder", "agent-coder").is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn ephemeral_profile_force_cleanup_beats_a_held_browser_slot_lock() {
    let Some(python) = ["/usr/bin/python3", "/bin/python3"]
        .into_iter()
        .find(|path| std::path::Path::new(path).is_file())
    else {
        eprintln!("skipping held-slot cleanup test: python3 unavailable");
        return;
    };
    let home = tempfile::tempdir().expect("isolated Phoenix home");
    let _home_guard = PhoenixHomeGuard::set_private(home.path());
    let instance = "agent-coder-job-force-cleanup";
    let profile = profile_dir_for_instance(instance, "phoenix").unwrap();
    std::fs::create_dir_all(&profile).unwrap();

    // This is not a Chrome process, but its exact managed-profile argument
    // exercises the PIDFD identity fence without a heavyweight live browser.
    // Python accepts the trailing argument and remains alive until the force
    // sweep signals it.
    let mut holder = std::process::Command::new(python)
        .args([
            "-c",
            "import time; time.sleep(30)",
            &format!("--user-data-dir={}", profile.display()),
        ])
        .spawn()
        .expect("spawn exact-profile holder");

    let slot = std::sync::Arc::new(std::sync::Mutex::<Option<Session>>::new(None));
    let registry = SESSIONS.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    registry
        .lock()
        .unwrap()
        .insert(instance.to_string(), std::sync::Arc::clone(&slot));
    let held = slot.lock().expect("hold the browser session slot");

    let outcome = discard_ephemeral_browser_profile(instance);
    drop(held);
    if outcome.is_err() {
        let _ = holder.kill();
        let _ = holder.wait();
    }
    outcome.expect("force cleanup must not depend on acquiring the held slot");
    assert!(
        holder.try_wait().unwrap().is_some(),
        "exact-profile holder survived disposable-profile cleanup"
    );
    assert!(
        !profile.exists(),
        "profile was not removed after confirmed exit"
    );
    assert!(
        !SESSIONS
            .get()
            .unwrap()
            .lock()
            .unwrap()
            .contains_key(instance),
        "held slot registry entry was retained after force cleanup"
    );
}

#[test]
fn transport_dead_detects_underlying_connection_closed() {
    assert!(is_transport_dead(&anyhow::anyhow!(
        "Unable to make method calls because underlying connection is closed"
    )));
}

// Self-heal after a wedged navigate (the 2026-06-13 Moon Dev X failure:
// a hung navigate bricked the whole browser lane until `phoenix restart`).
// A navigate to a black-hole address must time out, drop the session, and
// the NEXT navigate must succeed on a fresh session — no restart.
//   PHOENIX_BROWSER_HEADLESS=1 cargo test live_browser_self_heals -- --ignored --nocapture
#[test]
#[ignore]
fn live_browser_self_heals_after_a_wedged_navigate() {
    let _live = live_lock();
    std::env::set_var("PHOENIX_BROWSER_HEADLESS", "1");
    std::env::set_var("PHOENIX_BROWSER_PROFILE", "phoenix");
    // 10.255.255.1 is a routable-but-black-holed address: the TCP SYN gets
    // no answer, so the page never loads and navigate cannot complete.
    let t0 = std::time::Instant::now();
    let wedged = execute(
        "browser_navigate",
        serde_json::json!({"url": "http://10.255.255.1/"}),
    );
    let wedge_elapsed = t0.elapsed();
    eprintln!(
        "wedged navigate returned in {:?}: {:?}",
        wedge_elapsed,
        wedged.as_ref().map(|o| &o.summary)
    );
    assert!(
        wedged.is_err(),
        "a black-hole navigate must error, not hang forever"
    );
    assert!(
        wedge_elapsed < std::time::Duration::from_secs(50),
        "the 30s navigation fence plus recovery must remain bounded; took {wedge_elapsed:?}"
    );

    // The lane must be alive again on the very next call — no restart.
    let healed = execute(
        "browser_navigate",
        serde_json::json!({"url": "example.com"}),
    )
    .expect("session must self-heal and the next navigate must succeed");
    assert!(
        healed.content.contains("BROWSER STATE"),
        "healed nav must return live state"
    );
    shutdown();
}

// A recoverable action error must NOT tear down the browser (the old
// "randomly closes" instability). After a bad click the SAME page must
// still be live — same session, not a relaunched blank one.
//   PHOENIX_BROWSER_HEADLESS=1 cargo test live_browser_survives_a_bad_action -- --ignored --nocapture
#[test]
#[ignore]
fn live_browser_survives_a_bad_action() {
    let _live = live_lock();
    std::env::set_var("PHOENIX_BROWSER_HEADLESS", "1");
    std::env::set_var("PHOENIX_BROWSER_PROFILE", "phoenix");
    execute(
        "browser_navigate",
        serde_json::json!({"url": "example.com"}),
    )
    .expect("initial navigate");
    // Click an index that cannot exist — an action-level error.
    let bad = execute("browser_click", serde_json::json!({"index": 9999}));
    assert!(bad.is_err(), "bad click should error");
    // The browser must still be alive on the SAME page — not relaunched.
    let state = execute("browser_state", serde_json::json!({})).expect("state after bad click");
    assert!(
        state.content.to_lowercase().contains("example"),
        "session was torn down by a recoverable error (instability bug): {}",
        state.content
    );
    shutdown();
}

// Live login-port proof — launches a real Chromium through the SAME path the
// agent uses (execute → Session::open → port_logins), then checks the ported
// session is actually logged in. Requires a local Firefox/Zen profile with a
// live reddit login and `login_source` set in ~/.phoenix/config.toml. Run:
//   PHOENIX_BROWSER_HEADLESS=1 cargo test live_browser_login_port -- --ignored --nocapture
#[test]
#[ignore]
fn live_browser_login_port_logs_in() {
    let _live = live_lock();
    std::env::set_var("PHOENIX_BROWSER_HEADLESS", "1");
    // /notifications is a login-walled page: logged out → "Welcome to Reddit"
    // login page; logged in → the "Inbox" view. This is exactly the page the
    // failing live run hit.
    let out = execute(
        "browser_navigate",
        serde_json::json!({"url": "https://www.reddit.com/notifications"}),
    )
    .expect("navigate failed");
    shutdown();
    let body = out.content;
    assert!(
        !body.contains("Welcome to Reddit") && !body.to_lowercase().contains("/login"),
        "ported session is LOGGED OUT (redirected to login) — cookie port failed:\n{body}"
    );
    assert!(
        body.contains("Inbox") || body.contains("notifications"),
        "expected the logged-in notifications view:\n{body}"
    );
}

// The reconnect path (transport-dead recovery): after a launch, the port-scan
// must find the live chrome and reconnect_to_live_chrome must drive the SAME
// chrome (its existing tab/page), instead of killing+relaunching. This is what
// stops "browser died and went back to twitter".
//   PHOENIX_BROWSER_HEADLESS=1 cargo test live_browser_reconnects_to_same_chrome -- --ignored --nocapture
#[test]
#[ignore]
fn live_browser_reconnects_to_same_chrome() {
    let _live = live_lock();
    std::env::set_var("PHOENIX_BROWSER_HEADLESS", "1");
    std::env::set_var("PHOENIX_BROWSER_PROFILE", "phoenix");
    execute(
        "browser_navigate",
        serde_json::json!({"url": "example.com"}),
    )
    .expect("nav");
    let profile = resolve_user_data_dir("phoenix");
    let port = chrome_debug_port(&profile);
    assert!(
        port.is_some(),
        "port-scan must find the live chrome on the profile"
    );
    let prev_target = session_slot("")
        .lock()
        .unwrap()
        .as_ref()
        .map(|s| s.tab.get_target_id().to_string());
    let reconn = reconnect_to_live_chrome(&profile, prev_target.as_deref(), true, None, None)
        .expect("reconnect to the live chrome must succeed");
    // real_url() is what browser_state reports — must show the live page even
    // though the freshly-connected get_url() cache still reads about:blank.
    let url = real_url(&reconn.tab);
    assert!(
        url.contains("example"),
        "reconnect must report the live page (not the stale cache), got: {url}"
    );
    // connect-mode Browser — forget it so it doesn't race the global session's
    // close; then shutdown() (owns the process) tears chrome down.
    std::mem::forget(reconn);
    shutdown();
}

// The mid-login teardown of 2026-07-06: the browser sat EVENT-QUIET while the
// user typed at a login wall (>30s = the stock crate's idle window), and the
// stock crate closed its own websocket — either the transport loop idling out,
// or the browser-events loop dying and the post-login redirect's
// targetInfoChanged breaking the transport against the dead listener. Either
// way the next action hit transport-dead → relaunch → the login evaporated.
// With the vendored patches + keepalive + IDLE_SOCKET_BUDGET, the SAME chrome
// (same pid, no recovery banner) must survive a long quiet stretch and a
// post-idle navigation.
//   PHOENIX_BROWSER_HEADLESS=1 cargo test live_browser_survives_an_event_quiet_handoff -- --ignored --nocapture
#[test]
#[ignore]
fn live_browser_survives_an_event_quiet_handoff() {
    let _live = live_lock();
    std::env::set_var("PHOENIX_BROWSER_HEADLESS", "1");
    std::env::set_var("PHOENIX_BROWSER_PROFILE", "phoenix");
    execute(
        "browser_navigate",
        serde_json::json!({"url": "example.com"}),
    )
    .expect("initial navigate");
    let pid_before = session_slot("")
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|s| s._browser.get_process_id());
    assert!(
        pid_before.is_some(),
        "launched session must own a chrome pid"
    );
    // The handoff: 45 seconds with zero CDP actions and zero page events —
    // past the stock 30s idle window that used to kill the socket.
    eprintln!("idling 45s (the login-handoff window)…");
    std::thread::sleep(std::time::Duration::from_secs(45));
    // The "post-login redirect": the first navigation after the quiet stretch,
    // which fires the browser-level event the old transport crashed against.
    let after = execute(
        "browser_navigate",
        serde_json::json!({"url": "example.org"}),
    )
    .expect("post-idle navigate must succeed");
    let pid_after = session_slot("")
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|s| s._browser.get_process_id());
    assert_eq!(
        pid_before, pid_after,
        "the SAME chrome must survive an event-quiet handoff — a pid change means it was torn down and relaunched"
    );
    assert!(
        !after.content.contains("BROWSER ENVIRONMENT CHANGED"),
        "the handoff must be survived cleanly, not recovered from: {}",
        after.content
    );
    shutdown();
}

// The in-canvas browser view: a navigate on the REAL tool path must push
// screencast frames into the broadcast channel the daemon's SubscribeBrowser
// taps — base64 JPEG, canonical instance, the page's URL once loaded.
//   PHOENIX_BROWSER_HEADLESS=1 cargo test live_browser_screencast_streams_frames -- --ignored --nocapture
#[test]
#[ignore]
fn live_browser_screencast_streams_frames() {
    use tokio::sync::broadcast::error::TryRecvError;
    let _live = live_lock();
    std::env::set_var("PHOENIX_BROWSER_HEADLESS", "1");
    std::env::set_var("PHOENIX_BROWSER_PROFILE", "phoenix");
    // Subscribe BEFORE browsing — frames are dropped when nobody listens.
    let mut rx = super::frames_subscribe();
    execute(
        "browser_navigate",
        serde_json::json!({"url": "example.com"}),
    )
    .expect("navigate");
    // The first frame can still carry the cached about:blank URL — wait for
    // one that shows the loaded page.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut on_page = None;
    while std::time::Instant::now() < deadline && on_page.is_none() {
        match rx.try_recv() {
            Ok(frame) => {
                assert_eq!(
                    frame.instance, "",
                    "canonical Surf frames carry the empty scope"
                );
                assert!(!frame.data.is_empty(), "frame must carry base64 JPEG data");
                if frame.url.contains("example") {
                    on_page = Some(frame);
                }
            }
            Err(TryRecvError::Empty) => std::thread::sleep(std::time::Duration::from_millis(150)),
            Err(TryRecvError::Lagged(_)) => continue,
            Err(TryRecvError::Closed) => break,
        }
    }
    let frame = on_page.expect("a screencast frame of the loaded page must arrive");
    assert!(
        frame.w > 0 && frame.h > 0,
        "frame must carry its dimensions"
    );
    shutdown();
}

// A fresh launch must show exactly ONE tab — the "browser shows two
// about:blank tabs / randomly reopens with two" symptom was launch opening a
// redundant second tab on top of chrome's own initial tab.
//   PHOENIX_BROWSER_HEADLESS=1 cargo test live_browser_launches_with_a_single_tab -- --ignored --nocapture
#[test]
#[ignore]
fn live_browser_launches_with_a_single_tab() {
    let _live = live_lock();
    std::env::set_var("PHOENIX_BROWSER_HEADLESS", "1");
    std::env::set_var("PHOENIX_BROWSER_PROFILE", "phoenix");
    let state = execute("browser_state", serde_json::json!({})).expect("state");
    shutdown();
    // `open_tabs_text` lists tabs only when there are 2+. A fresh launch with
    // the single-tab fix has exactly one, so the multi-tab list is absent.
    // Before the fix (chrome-initial + an extra new_tab) this said "Open tabs".
    assert!(
        !state.content.contains("Open tabs"),
        "a fresh launch must have ONE tab (no multi-tab list):\n{}",
        state.content
    );
}

// A `beforeunload` handler pops a "leave site?" dialog that blocks navigation
// forever with nobody to answer it — a top cause of the wedge→teardown loop.
// The ported dialog auto-handler (browser-use popups_watchdog) must accept it
// so the next navigate completes FAST, not after the 30s wedge budget.
//   PHOENIX_BROWSER_HEADLESS=1 cargo test live_browser_auto_handles_beforeunload -- --ignored --nocapture
#[test]
#[ignore]
fn live_browser_auto_handles_beforeunload_dialog() {
    let _live = live_lock();
    std::env::set_var("PHOENIX_BROWSER_HEADLESS", "1");
    std::env::set_var("PHOENIX_BROWSER_PROFILE", "phoenix");
    // Arm a beforeunload prompt on the current page.
    execute(
            "browser_navigate",
            serde_json::json!({"url": "data:text/html,<script>window.onbeforeunload=function(e){e.preventDefault();return 'stay?'}</script><h1>armed</h1>"}),
        )
        .expect("arming navigate failed");
    // Navigating away fires beforeunload. With the handler it accepts instantly;
    // without it, this wedges ~30s. Assert it returns well under the wedge cap.
    let t0 = std::time::Instant::now();
    let away = execute(
        "browser_navigate",
        serde_json::json!({"url": "example.com"}),
    );
    let elapsed = t0.elapsed();
    shutdown();
    assert!(away.is_ok(), "navigate away should succeed: {away:?}");
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "navigate took {elapsed:?} — beforeunload dialog was NOT auto-handled (it wedged)"
    );
}

// A click on an SPA-style control (mutates the DOM, never navigates) must
// return FAST. The old post-click `wait_until_navigated()` blocked the entire
// 20s default timeout here because no navigation ever fires — the live
// "tried to click, it did not work [froze ~20s], then it crashed" the user
// reported. settle_page (bounded networkIdle) must return in ~1s.
//   PHOENIX_BROWSER_HEADLESS=1 cargo test live_browser_click_does_not_freeze_on_spa -- --ignored --nocapture
#[test]
#[ignore]
fn live_browser_click_does_not_freeze_on_spa() {
    let _live = live_lock();
    std::env::set_var("PHOENIX_BROWSER_HEADLESS", "1");
    std::env::set_var("PHOENIX_BROWSER_PROFILE", "phoenix");
    execute(
            "browser_navigate",
            serde_json::json!({"url": "data:text/html,<button onclick=\"this.textContent='clicked'\">go</button>"}),
        )
        .expect("navigate failed");
    // Time only the click. Old behavior wedged ~20s; new caps at settle_page's
    // 6s and early-exits in ~1s on a page that never navigates.
    let t0 = std::time::Instant::now();
    let clicked = execute("browser_click", serde_json::json!({"index": 0}));
    let elapsed = t0.elapsed();
    shutdown();
    assert!(clicked.is_ok(), "click should succeed: {clicked:?}");
    assert!(
            elapsed < std::time::Duration::from_secs(12),
            "click took {elapsed:?} — it blocked on a navigation that never came (the 20s post-click freeze is back)"
        );
}

// A tab opened with new_tab:true must receive the FULL watchdog coverage
// (dialog/crash/keepalive) via adopt_tab — not run blind. Proof: arm a
// beforeunload in the NEW tab, then navigate it away; with the dialog handler
// attached it completes instantly, without it the navigate wedges ~30s. This
// guards the "new/switched tabs run blind, crash undetected" regression.
//   PHOENIX_BROWSER_HEADLESS=1 cargo test live_browser_new_tab_gets_watchdogs -- --ignored --nocapture
#[test]
#[ignore]
fn live_browser_new_tab_gets_watchdogs() {
    let _live = live_lock();
    std::env::set_var("PHOENIX_BROWSER_HEADLESS", "1");
    std::env::set_var("PHOENIX_BROWSER_PROFILE", "phoenix");
    execute(
            "browser_navigate",
            serde_json::json!({"url": "data:text/html,<script>window.onbeforeunload=function(e){e.preventDefault();return 'stay?'}</script><h1>armed</h1>", "new_tab": true}),
        )
        .expect("open armed page in a new tab");
    // Navigating the (now-active) new tab away fires beforeunload. If adopt_tab
    // wired the dialog handler onto it, this returns fast; otherwise it wedges.
    let t0 = std::time::Instant::now();
    let away = execute(
        "browser_navigate",
        serde_json::json!({"url": "example.com"}),
    );
    let elapsed = t0.elapsed();
    shutdown();
    assert!(away.is_ok(), "navigate away should succeed: {away:?}");
    assert!(
            elapsed < std::time::Duration::from_secs(20),
            "navigate took {elapsed:?} — the NEW tab never got the dialog watchdog (adopt_tab regression)"
        );
}

// Scroll must find and move the element that ACTUALLY scrolls. On SPA feeds
// the window doesn't scroll — an inner overflow container does — and the old
// `window.scrollBy` silently no-op'd there ("scroll fails"). This page's only
// scroll is an inner container; assert smart-scroll moved it.
//   PHOENIX_BROWSER_HEADLESS=1 cargo test live_browser_scroll_finds_inner_container -- --ignored --nocapture
#[test]
#[ignore]
fn live_browser_scroll_finds_inner_container() {
    let _live = live_lock();
    std::env::set_var("PHOENIX_BROWSER_HEADLESS", "1");
    std::env::set_var("PHOENIX_BROWSER_PROFILE", "phoenix");
    // Body can't scroll (overflow:hidden); only #feed (overflow:auto) scrolls.
    let url = "data:text/html,<body style='margin:0;height:100vh;overflow:hidden'>\
                   <div id='feed' style='height:100vh;overflow:auto'>\
                   <div style='height:6000px'>tall</div></div></body>";
    execute("browser_navigate", serde_json::json!({ "url": url })).expect("nav");
    let scrolled = execute(
        "browser_scroll",
        serde_json::json!({"down": true, "pages": 2}),
    )
    .expect("scroll");
    assert!(
        scrolled.content.contains("Scrolled down"),
        "smart scroll should report success on an inner-container page, got: {}",
        scrolled.content
    );
    let top = execute(
        "browser_evaluate",
        serde_json::json!({"code": "document.getElementById('feed').scrollTop"}),
    )
    .expect("eval scrollTop");
    shutdown();
    let first = top.content.lines().next().unwrap_or("").trim().to_string();
    let value: f64 = first.parse().unwrap_or(0.0);
    assert!(
        value > 0.0,
        "inner #feed must have scrolled (scrollTop > 0), got: {first:?}\n{}",
        top.content
    );
}

// Custom React-style dropdowns are buttons/comboboxes plus a portal listbox,
// not native <select> nodes. Phoenix must open, enumerate, and choose those
// without falling into the historical "not a <select>" retry loop.
//   PHOENIX_BROWSER_HEADLESS=1 cargo test live_browser_custom_dropdown_opens_lists_and_selects -- --ignored --nocapture
#[test]
#[ignore]
fn live_browser_custom_dropdown_opens_lists_and_selects() {
    let _live = live_lock();
    let home = tempfile::tempdir().unwrap();
    let _home = PhoenixHomeGuard::set_private(home.path());
    std::env::set_var("PHOENIX_BROWSER_HEADLESS", "1");
    std::env::set_var("PHOENIX_BROWSER_PROFILE", "phoenix");
    let url = "data:text/html,<button id='pick' role='combobox' aria-controls='menu' aria-expanded='false'>Choose</button>\
               <div id='menu' role='listbox' style='display:none'><button role='option'>Alpha</button><button role='option'>Beta</button></div>\
               <output id='chosen'></output><script>const p=document.getElementById('pick'),m=document.getElementById('menu');\
               p.onclick=()=>{m.style.display='block';p.setAttribute('aria-expanded','true')};\
               m.onclick=e=>{if(e.target.getAttribute('role')==='option'){document.getElementById('chosen').textContent=e.target.textContent;m.style.display='none';p.setAttribute('aria-expanded','false')}};</script>";
    execute("browser_navigate", serde_json::json!({ "url": url })).expect("nav");
    let options = execute("browser_dropdown_options", serde_json::json!({"index": 0}))
        .expect("custom options");
    assert!(
        options.content.contains("Alpha") && options.content.contains("Beta"),
        "custom options should be visible: {}",
        options.content
    );
    let selected = execute(
        "browser_select_dropdown",
        serde_json::json!({"index": 0, "text": "Beta"}),
    )
    .expect("custom selection");
    assert!(
        selected
            .content
            .contains("Selected 'Beta' in custom dropdown"),
        "custom selection receipt: {}",
        selected.content
    );
    let witness = execute(
        "browser_evaluate",
        serde_json::json!({"code": "document.getElementById('chosen').textContent"}),
    )
    .expect("selection witness");
    shutdown();
    assert!(witness.content.contains("Beta"), "{}", witness.content);
}

// Writing must work on BOTH field kinds: a plain <textarea> AND a
// contenteditable div (the X/Twitter, Reddit, Slack compose box). The old
// `e.value = …` path wrote nothing into contenteditable (no .value) — the
// "couldn't draft a post" bug. Assert the text actually lands in each.
//   PHOENIX_BROWSER_HEADLESS=1 cargo test live_browser_input_writes_into_contenteditable_and_textarea -- --ignored --nocapture
#[test]
#[ignore]
fn live_browser_input_writes_into_contenteditable_and_textarea() {
    let _live = live_lock();
    std::env::set_var("PHOENIX_BROWSER_HEADLESS", "1");
    std::env::set_var("PHOENIX_BROWSER_PROFILE", "phoenix");
    let url = "data:text/html,<body>\
                   <div id='ce' contenteditable='true' style='border:1px solid'></div>\
                   <textarea id='ta'></textarea></body>";
    execute("browser_navigate", serde_json::json!({ "url": url })).expect("nav");
    // Index 0 = the contenteditable div, index 1 = the textarea (DOM order).
    let ce = execute(
        "browser_input",
        serde_json::json!({"index": 0, "text": "hello from phoenix"}),
    )
    .expect("input into contenteditable");
    assert!(
        ce.content.contains("Typed into"),
        "contenteditable write should verify, got: {}",
        ce.content
    );
    let ta = execute(
        "browser_input",
        serde_json::json!({"index": 1, "text": "draft post body"}),
    )
    .expect("input into textarea");
    assert!(
        ta.content.contains("Typed into"),
        "textarea write: {}",
        ta.content
    );
    let check = execute(
            "browser_evaluate",
            serde_json::json!({"code": "document.getElementById('ce').textContent+'|'+document.getElementById('ta').value"}),
        )
        .expect("eval readback");
    shutdown();
    assert!(
        check.content.contains("hello from phoenix") && check.content.contains("draft post body"),
        "both fields must hold the typed text, got: {}",
        check.content
    );
}

// The real X/Twitter failure: text appeared in the DOM but the editor's own
// `input` handler never fired, so its model stayed empty and the Post button
// stayed DISABLED. This models that exactly — a contenteditable whose `input`
// listener enables a disabled button — and asserts that typing fires the real
// event (only CDP Input.insertText does; a DOM write would leave it disabled).
//   PHOENIX_BROWSER_HEADLESS=1 cargo test live_browser_input_fires_editor_events -- --ignored --nocapture
#[test]
#[ignore]
fn live_browser_input_fires_editor_events_and_enables_submit() {
    let _live = live_lock();
    std::env::set_var("PHOENIX_BROWSER_HEADLESS", "1");
    std::env::set_var("PHOENIX_BROWSER_PROFILE", "phoenix");
    let url = "data:text/html,<div id='ed' contenteditable='true' style='border:1px solid'></div>\
                   <button id='post' disabled>Post</button>\
                   <script>document.getElementById('ed').addEventListener('input',function(){\
                   document.getElementById('post').disabled=this.textContent.trim().length===0;});</script>";
    execute("browser_navigate", serde_json::json!({ "url": url })).expect("nav");
    execute(
        "browser_input",
        serde_json::json!({"index": 0, "text": "Loops are the current unlock for LLMs."}),
    )
    .expect("type into draft-like editor");
    let disabled = execute(
        "browser_evaluate",
        serde_json::json!({"code": "document.getElementById('post').disabled"}),
    )
    .expect("eval button state");
    shutdown();
    assert!(
        disabled.content.contains("false"),
        "the editor's input handler must have fired and enabled Post — got disabled={}",
        disabled.content
    );
}

// clear=true (the default) must REPLACE existing text, not append. The
// select-all-then-type path means the first keystroke overwrites the old value.
//   PHOENIX_BROWSER_HEADLESS=1 cargo test live_browser_input_clear_replaces -- --ignored --nocapture
#[test]
#[ignore]
fn live_browser_input_clear_replaces_existing_text() {
    let _live = live_lock();
    std::env::set_var("PHOENIX_BROWSER_HEADLESS", "1");
    std::env::set_var("PHOENIX_BROWSER_PROFILE", "phoenix");
    execute(
        "browser_navigate",
        serde_json::json!({"url": "data:text/html,<textarea id='ta'>OLD</textarea>"}),
    )
    .expect("nav");
    execute(
        "browser_input",
        serde_json::json!({"index": 0, "text": "NEWVALUE"}),
    )
    .expect("type with clear");
    // Precise boolean eval — the page state dump echoes the data: URL (which
    // contains "OLD"), so assert on the field value itself, not the dump.
    let val = execute(
            "browser_evaluate",
            serde_json::json!({"code": "[document.getElementById('ta').value==='NEWVALUE', document.getElementById('ta').value.includes('OLD')]"}),
        )
        .expect("eval");
    shutdown();
    assert!(
        val.content.contains("[true,false]"),
        "clear must replace OLD with NEWVALUE exactly, got: {}",
        val.content
    );
}

// Live smoke test — launches a real headless Chromium. Run explicitly:
//   PHOENIX_BROWSER_HEADLESS=1 cargo test live_browser -- --ignored --nocapture
#[test]
#[ignore]
fn live_browser_navigates_and_indexes() {
    let _live = live_lock();
    std::env::set_var("PHOENIX_BROWSER_HEADLESS", "1");
    let out = execute(
        "browser_navigate",
        serde_json::json!({"url": "example.com"}),
    )
    .expect("navigate failed");
    assert!(out.content.contains("BROWSER STATE"));
    assert!(out.content.contains("example.com") || out.content.contains("Example"));
    // The link on example.com must be indexed as an interactive element.
    let state = execute("browser_state", serde_json::json!({})).expect("state failed");
    assert!(
        state.content.contains("[0]"),
        "no indexed element: {}",
        state.content
    );
    // Multi-tab: a second tab must show up in the state with a usable id.
    let out = execute(
        "browser_navigate",
        serde_json::json!({"url": "example.org", "new_tab": true}),
    )
    .expect("new_tab navigate failed");
    assert!(
        out.content.contains("Open tabs"),
        "no tabs list in state: {}",
        out.content
    );
    let tab_id = out
        .content
        .lines()
        .find_map(|l| l.trim().strip_prefix("Tab ")?.split(':').next())
        .expect("no tab id in state")
        .to_string();
    let switched =
        execute("browser_switch", serde_json::json!({"tab_id": tab_id})).expect("switch failed");
    assert!(switched.content.contains("Switched"));
    shutdown();
}

#[test]
#[ignore]
fn live_browser_status_reports_health() {
    let _live = live_lock();
    std::env::set_var("PHOENIX_BROWSER_HEADLESS", "1");
    let out = execute("browser_status", serde_json::json!({})).expect("status failed");
    assert!(
        out.content.contains("Browser mode:"),
        "missing mode: {}",
        out.content
    );
    assert!(
        out.content.contains("Current URL:"),
        "missing url: {}",
        out.content
    );
    assert!(
        out.content.contains("Open tabs:"),
        "missing tab count: {}",
        out.content
    );
    shutdown();
}

#[test]
#[ignore]
fn live_browser_status_reports_attach_port_fallback() {
    let _live = live_lock();
    let mut prefs = browser_prefs();
    prefs.source = "chrome".to_string();
    prefs.attach.clear();
    prefs.attach_port = 9;
    prefs.headless = true;
    prefs.login_source = "none".to_string();

    let session = Session::open_inner(&prefs, "").expect("fallback launch failed");
    let status = browser_diagnostics(&session);
    assert!(
        status.contains("Configured Chrome DevTools port 9 was not reachable"),
        "missing fallback note: {status}"
    );
    assert!(status.contains("Browser mode: launch"), "{status}");
    drop(session);
}

// Serializer depth: shadow DOM + same-origin iframe + registry click.
//   PHOENIX_BROWSER_HEADLESS=1 cargo test live_serializer -- --ignored
#[test]
#[ignore]
fn live_serializer_sees_shadow_and_iframes() {
    let _live = live_lock();
    std::env::set_var("PHOENIX_BROWSER_HEADLESS", "1");
    std::env::set_var("PHOENIX_BROWSER_PROFILE", "phoenix");
    let html = r##"<html><body>
            <div id="host"></div>
            <iframe srcdoc="<button id='inner'>Iframe button</button>"></iframe>
            <button onclick="document.title='CLICKED'">Top button</button>
            <script>
              var r = document.getElementById('host').attachShadow({mode:'open'});
              r.innerHTML = '<button>Shadow button</button>';
            </script>
        </body></html>"##;
    let path = std::env::temp_dir().join("phx-dom-test.html");
    std::fs::write(&path, html).unwrap();
    let out = execute(
        "browser_navigate",
        serde_json::json!({"url": format!("file://{}", path.display())}),
    )
    .expect("navigate failed");
    assert!(out.content.contains("Shadow button"), "{}", out.content);
    assert!(out.content.contains("[shadow]"), "{}", out.content);
    assert!(out.content.contains("Iframe button"), "{}", out.content);
    assert!(out.content.contains("[iframe]"), "{}", out.content);
    assert!(out.content.contains("Page:"), "{}", out.content);
    // Registry-based click must reach the top button and run its handler.
    let idx = out
        .content
        .lines()
        .find(|l| l.contains("Top button"))
        .and_then(|l| l.split(']').next())
        .and_then(|l| l.trim_start_matches('[').parse::<i64>().ok())
        .expect("no index for Top button");
    let clicked =
        execute("browser_click", serde_json::json!({"index": idx})).expect("click failed");
    assert!(
        clicked.content.contains("Title: CLICKED"),
        "{}",
        clicked.content
    );
    shutdown();
    std::fs::remove_file(&path).ok();
}

// The browser configuration variables are process-global. Keep this focused
// lock for the non-live status test; PHOENIX_HOME uses the crate-wide guard.
static BROWSER_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn restore_env_var(key: &str, prev: Option<std::ffi::OsString>) {
    match prev {
        Some(prev) => std::env::set_var(key, prev),
        None => std::env::remove_var(key),
    }
}

#[test]
fn resolve_user_data_dir_routes_login_sources_to_phoenix_profile() {
    // Without the fix, `source = "zen"` (a Firefox-family login source)
    // was treated as a literal relative path — Chrome was launched with
    // `--user-data-dir=zen`, the CWD-relative entry changed on every
    // fresh start, the profile appeared to "move" and the CDP session
    // never held. Recognize all known login sources as the stable
    // Phoenix-owned profile and let `port_logins` inject the cookies.
    let home = tempfile::tempdir().expect("tempdir");
    let _home_guard = PhoenixHomeGuard::set_private(home.path());

    let expected = home.path().join("browser/profile");

    for src in [
        "zen",
        "Zen",
        "ZEN",
        "firefox",
        "librewolf",
        "floorp",
        "waterfox",
        // Chromium-family are ported INTO Phoenix's profile too, so they resolve
        // to the sandbox profile, not the user's own Chrome data dir.
        "brave",
        "chromium",
        "edge",
    ] {
        assert_eq!(
            resolve_user_data_dir(src),
            expected,
            "source {:?} should resolve to the Phoenix profile",
            src
        );
    }

    // Empty / "phoenix" / "PHOENIX" have always meant the sandbox
    // profile — make sure the new branch didn't regress them.
    assert_eq!(resolve_user_data_dir(""), expected);
    assert_eq!(resolve_user_data_dir("phoenix"), expected);
    assert_eq!(resolve_user_data_dir("PHOENIX"), expected);

    // Anything else is a literal path (tildes still get expanded).
    assert_eq!(
        resolve_user_data_dir("/explicit/path/to/profile"),
        std::path::PathBuf::from("/explicit/path/to/profile")
    );
    // A non-browser bare word is still treated as a literal profile path.
    assert_eq!(
        resolve_user_data_dir("safari"),
        std::path::PathBuf::from("safari")
    );
}

#[test]
fn midpoint_from_quad_centers_a_content_quad() {
    // Stable-id click (2026-07-02): the CDP content quad is a flat list of 4
    // corner points [x1,y1, x2,y2, x3,y3, x4,y4]. The centroid must land in the
    // middle of the element's content box — this is what click_by_backend_id
    // aims at. A 100x100 box at (100,100): corners in any order -> (150,150).
    let quad = vec![100.0, 100.0, 200.0, 100.0, 200.0, 200.0, 100.0, 200.0];
    let mid = midpoint_from_quad(&quad).expect("a full quad has a midpoint");
    assert_eq!(mid.x, 150.0);
    assert_eq!(mid.y, 150.0);

    // Corners in a different order still center correctly (min/max, not average).
    let quad2 = vec![200.0, 200.0, 100.0, 100.0, 200.0, 100.0, 100.0, 200.0];
    let mid2 = midpoint_from_quad(&quad2).expect("order-independent midpoint");
    assert_eq!(mid2.x, 150.0);
    assert_eq!(mid2.y, 150.0);

    // A degenerate quad (fewer than 8 values) has no midpoint -> the caller bails
    // and falls back to the JS path (no crash).
    assert!(midpoint_from_quad(&[0.0, 0.0, 1.0, 1.0]).is_none());
}

// Multi-act batch (2026-07-03, donor: browser-use multi_act): one tool call
// runs input → click against the same state, state attaches ONCE, and the
// second call proves the cross-round state-dedupe collapses an unchanged page
// to a one-line receipt.
//   PHOENIX_BROWSER_HEADLESS=1 cargo test live_browser_act_batches -- --ignored --nocapture
#[test]
#[ignore]
fn live_browser_act_batches_input_and_click_in_one_call() {
    let _live = live_lock();
    std::env::set_var("PHOENIX_BROWSER_HEADLESS", "1");
    std::env::set_var("PHOENIX_BROWSER_PROFILE", "phoenix");
    // The typed input registers structurally (the indexer serializes the
    // live `value` PROPERTY — typing sets the property, not the attribute),
    // so the after-batch state genuinely differs and attaches in full; the
    // fuzzy signature still dedupes title/text-only ticks by design.
    execute(
        "browser_navigate",
        serde_json::json!({"url": "data:text/html,<input id=q><button onclick=\"document.title=document.getElementById('q').value\">go</button>"}),
    )
    .expect("navigate failed");
    let acted = execute(
        "browser_act",
        serde_json::json!({"actions": [
            {"action": "input", "index": 0, "text": "batched"},
            {"action": "click", "index": 1}
        ]}),
    )
    .expect("browser_act failed");
    assert!(acted.content.contains("1. input"), "{}", acted.content);
    assert!(acted.content.contains("2. click"), "{}", acted.content);
    // State attached exactly once.
    assert_eq!(
        acted
            .content
            .matches("=== BROWSER STATE (current page) ===")
            .count(),
        1,
        "{}",
        acted.content
    );
    assert!(
        acted.content.contains("Title: batched"),
        "{}",
        acted.content
    );

    // A read-only action on the unchanged page collapses to the dedupe line.
    let second =
        execute("browser_find_text", serde_json::json!({"text": "go"})).expect("find_text failed");
    shutdown();
    assert!(
        second.content.contains("BROWSER STATE unchanged"),
        "{}",
        second.content
    );
}

// Stable-id click (2026-07-02): a click by [index] must drive a REAL CDP mouse
// event (event.isTrusted=true) via the captured backendNodeId, not the JS
// `e.click()` fallback (untrusted, isTrusted=false). The button only counts
// trusted clicks, so a "c1" text change proves the stable-id path fired a real
// click; if it had fallen back to the JS synthetic click the text would read
// "synthetic" and the counter would stay 0.
//   PHOENIX_BROWSER_HEADLESS=1 cargo test live_browser_click_uses_stable_backend_node_id -- --ignored --nocapture
#[test]
#[ignore]
fn live_browser_click_uses_stable_backend_node_id() {
    let _live = live_lock();
    std::env::set_var("PHOENIX_BROWSER_HEADLESS", "1");
    std::env::set_var("PHOENIX_BROWSER_PROFILE", "phoenix");
    execute(
        "browser_navigate",
        serde_json::json!({"url": "data:text/html,<button onclick=\"if(event.isTrusted){window.__c=(window.__c||0)+1; this.textContent='c'+window.__c} else { this.textContent='synthetic' }\">go</button>"}),
    )
    .expect("navigate failed");
    let clicked = execute("browser_click", serde_json::json!({"index": 0}));
    // Read the DOM directly: the state echo may legitimately collapse to the
    // "unchanged" dedupe line (button TEXT is not in the structural
    // signature), so the button's own text is the only honest witness.
    let witness = execute(
        "browser_evaluate",
        serde_json::json!({"code": "document.body.innerText"}),
    );
    let text = witness.as_ref().map(|o| o.content.as_str()).unwrap_or("");
    let text = text.to_string();
    shutdown();
    assert!(clicked.is_ok(), "click should succeed: {clicked:?}");
    assert!(
        text.contains("c1"),
        "click must be a REAL trusted mouse event (event.isTrusted) driven by the stable backendNodeId, firing the counter -> c1; button text: {text}"
    );
}

// SPA remount recovery: replacing a button with an equivalent new DOM node
// after browser_state detaches both the JS reference and backendNodeId. Phoenix
// may relocate it only when its exact semantic fingerprint is unique, then must
// still deliver a trusted mouse click.
//   PHOENIX_BROWSER_HEADLESS=1 cargo test live_browser_click_recovers_unique_spa_remount -- --ignored --nocapture
#[test]
#[ignore]
fn live_browser_click_recovers_unique_spa_remount() {
    let _live = live_lock();
    std::env::set_var("PHOENIX_BROWSER_HEADLESS", "1");
    std::env::set_var("PHOENIX_BROWSER_PROFILE", "phoenix");
    execute(
        "browser_navigate",
        serde_json::json!({"url": "data:text/html,<button id='target' aria-label='Continue' onclick=\"document.body.dataset.clicked=event.isTrusted?'trusted':'synthetic'\">Continue</button><script>var timer=setInterval(function(){var old=document.querySelector('button[data-phx-idx]');if(old){clearInterval(timer);old.replaceWith(old.cloneNode(true))}},10)</script>"}),
    )
    .expect("navigate failed");
    std::thread::sleep(std::time::Duration::from_millis(300));
    let clicked = execute("browser_click", serde_json::json!({"index": 0}));
    let witness = execute(
        "browser_evaluate",
        serde_json::json!({"code": "document.body.dataset.clicked || 'missing'"}),
    );
    let text = witness.as_ref().map(|o| o.content.as_str()).unwrap_or("");
    let text = text.to_string();
    shutdown();
    assert!(
        clicked.is_ok(),
        "remounted click should recover: {clicked:?}"
    );
    assert!(
        text.contains("trusted"),
        "recovered click must be a real trusted mouse event: {text}"
    );
}

#[test]
#[ignore]
fn live_browser_click_refuses_ambiguous_spa_remount() {
    let _live = live_lock();
    std::env::set_var("PHOENIX_BROWSER_HEADLESS", "1");
    std::env::set_var("PHOENIX_BROWSER_PROFILE", "phoenix");
    execute(
        "browser_navigate",
        serde_json::json!({"url": "data:text/html,<button aria-label='Continue' onclick=\"document.body.dataset.clicked='yes'\">Continue</button><script>var timer=setInterval(function(){var old=document.querySelector('button[data-phx-idx]');if(old){clearInterval(timer);var one=old.cloneNode(true),two=old.cloneNode(true);one.removeAttribute('data-phx-idx');two.removeAttribute('data-phx-idx');old.replaceWith(one,two)}},10)</script>"}),
    )
    .expect("navigate failed");
    std::thread::sleep(std::time::Duration::from_millis(300));
    let clicked = execute("browser_click", serde_json::json!({"index": 0}));
    let witness = execute(
        "browser_evaluate",
        serde_json::json!({"code": "document.body.dataset.clicked || 'untouched'"}),
    )
    .expect("witness failed");
    shutdown();
    assert!(
        clicked.is_err(),
        "ambiguous remount must fail closed: {clicked:?}; witness={}",
        witness.content
    );
    assert!(witness.content.contains("untouched"));
}

// Fuzzy state signature (2026-07-03): the "state unchanged" receipt must key on
// STRUCTURE (URL + indexed element identity), not byte identity — a ticking
// timestamp in element text re-shipped ~12KB of identical structure after
// every action on live sites.
#[test]
fn state_signature_ignores_element_text_noise() {
    let before = "URL: https://x.com/home\nTitle: (2) Home / X\nPage: 300 links\n[0]<a href=\"/home\">Home</a>\n[1]<button aria-label=\"Post\">Post · 2m ago</button>";
    let after = "URL: https://x.com/home\nTitle: (3) Home / X\nPage: 301 links\n[0]<a href=\"/home\">Home</a>\n[1]<button aria-label=\"Post\">Post · 3m ago</button>";
    assert_eq!(
        state_signature(before),
        state_signature(after),
        "text-only ticks (timestamps, title counters, page stats) must not defeat the unchanged receipt"
    );
}

#[test]
fn state_signature_sees_structural_change() {
    let base = "URL: https://x.com/home\n[0]<a href=\"/home\">Home</a>";
    let new_element =
        "URL: https://x.com/home\n[0]<a href=\"/home\">Home</a>\n[1]<button>Reply</button>";
    let new_url = "URL: https://x.com/notifications\n[0]<a href=\"/home\">Home</a>";
    let new_attr = "URL: https://x.com/home\n[0]<a href=\"/explore\">Home</a>";
    assert_ne!(state_signature(base), state_signature(new_element));
    assert_ne!(state_signature(base), state_signature(new_url));
    assert_ne!(state_signature(base), state_signature(new_attr));
}

#[test]
fn state_signature_counts_scroll_boundaries() {
    let mid = "URL: https://x.com/home\n[0]<a href=\"/home\">Home</a>";
    let bottom = "URL: https://x.com/home\n[0]<a href=\"/home\">Home</a>\n[End of page]";
    assert_ne!(
        state_signature(mid),
        state_signature(bottom),
        "reaching the end of the page is a real change"
    );
}
