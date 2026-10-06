//! The live Chrome session: launch/attach/reconnect, cookie porting,
//! watchdogs (console/dialog/crash/keepalive), tab adoption and recovery.

use super::*;

pub(super) const CONSOLE_RING: usize = 200;

/// Wire Runtime console/exception events from `tab` into a shared ring.
pub(super) fn attach_console_listener(
    tab: &Arc<Tab>,
) -> Arc<std::sync::Mutex<std::collections::VecDeque<String>>> {
    use headless_chrome::protocol::cdp::types::Event;
    let ring: Arc<std::sync::Mutex<std::collections::VecDeque<String>>> =
        Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
    let sink = Arc::clone(&ring);
    let _ = tab.enable_runtime();
    let push = move |line: String| {
        let mut ring = sink.lock().unwrap_or_else(|p| p.into_inner());
        if ring.len() >= CONSOLE_RING {
            ring.pop_front();
        }
        ring.push_back(line);
    };
    let listener = Arc::new(move |event: &Event| match event {
        Event::RuntimeConsoleAPICalled(e) => {
            let args = e
                .params
                .args
                .iter()
                .map(|arg| {
                    arg.value
                        .as_ref()
                        .map(|v| v.to_string())
                        .or_else(|| arg.description.clone())
                        .unwrap_or_else(|| "(object)".to_string())
                })
                .collect::<Vec<_>>()
                .join(" ");
            push(format!("console.{:?}: {}", e.params.Type, args).to_lowercase());
        }
        Event::RuntimeExceptionThrown(e) => {
            let detail = &e.params.exception_details;
            let what = detail
                .exception
                .as_ref()
                .and_then(|ex| ex.description.clone())
                .unwrap_or_else(|| detail.text.clone());
            push(format!("EXCEPTION: {what}"));
        }
        _ => {}
    });
    if let Err(error) = tab.add_event_listener(listener) {
        tracing::warn!("browser console listener not attached: {error:#}");
    }
    ring
}

/// Auto-handle JavaScript dialogs so they never wedge the session — ported from
/// browser-use's `watchdogs/popups_watchdog.py`. A page with a `beforeunload`
/// handler pops an "are you sure you want to leave?" dialog on navigation; with
/// nobody to answer it, our navigate hangs forever → 30s wedge → the browser used
/// to get torn down ("randomly closes and reopens"). CDP pauses JS until the
/// dialog is handled, so we answer it the instant it opens. Policy matches
/// browser-use: alert/confirm/beforeunload → accept (OK / allow navigation),
/// prompt → dismiss (we have no input to give).
pub(super) fn attach_dialog_handler(tab: &Arc<Tab>) {
    use headless_chrome::protocol::cdp::types::Event;
    use headless_chrome::protocol::cdp::Page::DialogType;
    let tab_for_listener = Arc::clone(tab);
    let listener = Arc::new(move |event: &Event| {
        if let Event::PageJavascriptDialogOpening(opening) = event {
            let accept = !matches!(opening.params.Type, DialogType::Prompt);
            let dialog = tab_for_listener.get_dialog();
            let outcome = if accept {
                dialog.accept(None)
            } else {
                dialog.dismiss()
            };
            if let Err(error) = outcome {
                tracing::warn!("browser: auto-handling JS dialog failed: {error:#}");
            }
        }
    });
    if let Err(error) = tab.add_event_listener(listener) {
        tracing::warn!("browser dialog handler not attached: {error:#}");
    }
}

/// Watch for renderer crashes (browser-use `watchdogs/crash_watchdog.py`). CDP
/// fires `Inspector.targetCrashed` when the page's renderer process dies; we flag
/// it so `execute()` recovers the tab on the next call instead of the agent
/// hitting a confusing dead-tab error. Returns the shared flag the Session holds.
pub(super) fn attach_crash_detector(tab: &Arc<Tab>) -> Arc<std::sync::atomic::AtomicBool> {
    use headless_chrome::protocol::cdp::types::Event;
    use std::sync::atomic::Ordering;
    let crashed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = Arc::clone(&crashed);
    let listener = Arc::new(move |event: &Event| {
        if matches!(event, Event::InspectorTargetCrashed(_)) {
            tracing::warn!("browser: renderer target crashed — flagging for tab recovery");
            flag.store(true, Ordering::SeqCst);
        }
    });
    if let Err(error) = tab.add_event_listener(listener) {
        tracing::warn!("browser crash detector not attached: {error:#}");
    }
    crashed
}

/// Keep the CDP websocket warm so it never goes idle-stale between turns. A
/// persisted browser session that sat idle (user stops a turn, resumes minutes
/// later) had its socket die, so the resumed turn's first CDP call hit
/// transport-dead → drop+respawn (which KILLS the live chrome). A cheap periodic
/// round-trip prevents that. The thread holds only a Weak<Tab>, so it stops the
/// moment the session is dropped; transient errors are ignored (a genuinely dead
/// transport is handled by the next real action, not here).
/// How long the CDP socket may go without incoming traffic before the crate
/// gives up on it, and the bound on any one method-call response wait. The
/// crate default is 30s — with the 20s keepalive that left a 10s margin, so
/// one slow ping (busy page, paused renderer) closed the socket under a live
/// chrome. 120s tolerates five missed pings while still bounding a zombie
/// call. NOTE: `Browser::connect_with_timeout`'s Duration parameter IS this
/// idle timeout, not a connection timeout — passing 5s there made every
/// attached/reconnected session's socket die after 5 quiet seconds.
pub(super) const IDLE_SOCKET_BUDGET: Duration = Duration::from_secs(120);

pub(super) fn spawn_keepalive(tab: &Arc<Tab>) {
    const KEEPALIVE: Duration = Duration::from_secs(20);
    let weak = Arc::downgrade(tab);
    std::thread::spawn(move || {
        // One log line at the failure EDGE, never per ping: a keepalive that
        // starts failing is the earliest visible sign of socket/renderer
        // trouble, and the timestamp is the forensic anchor the 2026-07-06
        // teardown hunt never had. After several consecutive failures the
        // thread exits: a dead transport never heals, and the Weak<Tab> can
        // outlive the session (reconnect `mem::forget`s the dead one, whose
        // leaked Browser still holds the tabs list) — without the exit these
        // threads would ping dead sockets forever.
        let mut consecutive_failures = 0u32;
        loop {
            std::thread::sleep(KEEPALIVE);
            match weak.upgrade() {
                None => break,
                Some(tab) => match eval_string(&tab, "0") {
                    Ok(_) => {
                        if consecutive_failures > 0 {
                            super::blog("keepalive recovered — socket healthy again");
                            consecutive_failures = 0;
                        }
                    }
                    Err(error) => {
                        consecutive_failures += 1;
                        if consecutive_failures == 1 {
                            super::blog(&format!(
                                "keepalive ping started failing ({error:#}) — socket or renderer trouble"
                            ));
                        }
                        if consecutive_failures >= 3 {
                            break; // dead transport — stop pinging a corpse
                        }
                    }
                },
            }
        }
    });
}

/// Effective browser preferences: `[profile.browser]` in config (set once),
/// with `PHOENIX_BROWSER_*` env vars as one-off overrides.
pub(super) struct BrowserPrefs {
    /// "chrome" | "phoenix"/empty (sandboxed profile) | explicit path.
    pub(super) source: String,
    /// Explicit attach spec from env (ws:// URL or port); empty = none.
    pub(super) attach: String,
    /// Debug port probed when source = chrome (config attach_port, default 9222).
    pub(super) attach_port: u16,
    pub(super) headless: bool,
    /// Explicit browser executable (config `binary` / PHOENIX_BROWSER_BINARY).
    /// Any Chromium-compatible build: CloakBrowser, Brave, chromium.
    pub(super) binary: String,
    /// Extra launch flags from config `extra_args`.
    pub(super) extra_args: Vec<String>,
    /// Port logins from this source browser (Zen/Firefox-family) into the
    /// session at start. Empty/"none" = off. Config `login_source` /
    /// PHOENIX_BROWSER_LOGIN_SOURCE.
    pub(super) login_source: String,
    /// Cleanly close Phoenix-owned Chromium after this idle duration. The
    /// profile and restorable Chrome session remain durable.
    pub(super) suspend_after: Option<Duration>,
}

pub(super) fn browser_prefs() -> BrowserPrefs {
    let cfg = crate::config::PhoenixConfig::load()
        .ok()
        .and_then(|c| c.profile.browser);
    let settings_scope = crate::settings::SettingsScope::Global;
    let source = std::env::var("PHOENIX_BROWSER_PROFILE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            crate::settings::explicit_string("browser.profile_source", &settings_scope)
                .filter(|value| !value.trim().is_empty())
        })
        .or_else(|| cfg.as_ref().and_then(|b| b.source.clone()))
        .unwrap_or_default();
    let attach = std::env::var("PHOENIX_BROWSER_ATTACH").unwrap_or_default();
    let attach_port = crate::settings::explicit_u64("browser.attach_port", &settings_scope)
        .and_then(|value| u16::try_from(value).ok())
        .or_else(|| cfg.as_ref().and_then(|b| b.attach_port))
        .unwrap_or(9222);
    let headless = std::env::var("PHOENIX_BROWSER_HEADLESS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or_else(|_| {
            crate::settings::explicit_bool("browser.headless", &settings_scope)
                .or_else(|| cfg.as_ref().and_then(|b| b.headless))
                .unwrap_or(true)
        });
    let binary = std::env::var("PHOENIX_BROWSER_BINARY")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            crate::settings::explicit_string("browser.binary", &settings_scope)
                .filter(|value| !value.trim().is_empty())
        })
        .or_else(|| cfg.as_ref().and_then(|b| b.binary.clone()))
        .unwrap_or_default();
    let extra_args = cfg
        .as_ref()
        .and_then(|b| b.extra_args.clone())
        .unwrap_or_default();
    let login_source = match std::env::var("PHOENIX_BROWSER_LOGIN_SOURCE")
        .ok()
        .map(|source| source.trim().to_ascii_lowercase())
        .filter(|source| !source.is_empty())
    {
        Some(source) if source == "none" => String::new(),
        Some(source) if source == "auto" => {
            crate::tools::browser_cookies::detect_active_source().unwrap_or_default()
        }
        Some(source) => source,
        None => crate::tools::login_request::resolve_cookie_source().unwrap_or_default(),
    };
    let suspend_seconds = std::env::var("PHOENIX_BROWSER_SUSPEND_AFTER_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds == 0 || (60..=86_400).contains(seconds))
        .or_else(|| {
            crate::settings::effective_u64(
                "browser.suspend_after_seconds",
                &crate::settings::SettingsScope::Global,
            )
        })
        .or_else(|| {
            cfg.as_ref()
                .and_then(|browser| browser.suspend_after_seconds)
        })
        .unwrap_or(0);
    let suspend_after = (suspend_seconds > 0).then(|| Duration::from_secs(suspend_seconds));
    BrowserPrefs {
        source,
        attach,
        attach_port,
        headless,
        binary,
        extra_args,
        login_source,
        suspend_after,
    }
}

/// Linux flags that create a uniquely identifiable, decoration-light X11 app
/// window for the desktop to embed. The token is generated internally and is
/// constrained to UUID hex before it reaches this helper.
pub(super) fn native_surface_launch_args(token: Option<&str>) -> Vec<String> {
    #[cfg(target_os = "linux")]
    {
        let Some(token) = token else {
            return Vec::new();
        };
        debug_assert!(
            !token.is_empty()
                && token.len() <= 64
                && token.chars().all(|ch| ch.is_ascii_hexdigit()),
            "native surface token must be bounded UUID hex"
        );
        vec![
            "--ozone-platform=x11".to_string(),
            "--app=about:blank".to_string(),
            format!("--class=phoenix-browser-{token}"),
            // Chrome creates the X11 window before the desktop can discover
            // its PID/class and reparent it. Start outside every practical
            // monitor so that handshake never flashes a separate browser on
            // the user's desktop; the host moves it into the pane immediately.
            "--window-position=-32000,-32000".to_string(),
            "--disable-session-crashed-bubble".to_string(),
        ]
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = token;
        Vec::new()
    }
}

/// A Phoenix-managed Chromium process is normally headless unless the desktop
/// owns a native in-app surface lease. A scoped agent desktop is the second
/// deliberate exception: its Chromium is headed *inside that agent's private
/// Xephyr/Xvfb display*, never on the user's host desktop.
pub(super) fn managed_launch_headless(_surface_token: Option<&str>) -> bool {
    // The host desktop renders persistent coworker browsers through Electron's
    // WebContentsView. Any managed-Chrome fallback must remain headless; a
    // stale surface lease must never create a second operating-system window.
    true
}

pub fn local_status_line() -> String {
    let prefs = browser_prefs();
    let source = if prefs.source.trim().is_empty() {
        "phoenix".to_string()
    } else {
        prefs.source.clone()
    };
    let profile_dir = resolve_user_data_dir(&source);
    let binary = if prefs.binary.trim().is_empty() {
        which_chrome()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "auto-detect failed until launch".to_string())
    } else {
        shellexpand(&prefs.binary)
    };
    let attach = if !prefs.attach.trim().is_empty() {
        format!("explicit {}", prefs.attach.trim())
    } else if source.eq_ignore_ascii_case("chrome") {
        format!("probe port {}", prefs.attach_port)
    } else {
        "launch managed browser".to_string()
    };
    let login = if prefs.login_source.trim().is_empty() {
        "none".to_string()
    } else {
        prefs.login_source.clone()
    };
    format!(
        "browser · source {source} · {attach} · profile {} · binary {} · login source {login} · headless {} · suspend {} · extra args {}",
        profile_dir.display(),
        binary,
        if prefs.headless { "on" } else { "off" },
        prefs
            .suspend_after
            .map(|duration| format!("after {}s idle", duration.as_secs()))
            .unwrap_or_else(|| "off".to_string()),
        prefs.extra_args.len()
    )
}

/// Convert a ported source-browser cookie into a CDP `CookieParam`. `url` is set
/// explicitly so the cookie lands on its real domain rather than the current
/// (often blank) tab; unknown CDP fields default.
pub(super) fn to_cookie_param(
    cookie: crate::tools::browser_cookies::PortedCookie,
) -> Network::CookieParam {
    use headless_chrome::protocol::cdp::Network::CookieSameSite;
    let host = cookie.domain.trim_start_matches('.');
    // Honor cookie-name PREFIXES (RFC6265bis) or Chrome SILENTLY rejects them:
    //   __Host-   → host-only (NO domain attribute), path "/", Secure
    //   __Secure- → Secure
    // Firefox stores these for real auth cookies (e.g. github
    // __Host-user_session_same_site); copying them with a domain set made every
    // __Host- login cookie fail to inject (Network.setCookies still reports ok).
    let is_host_prefix = cookie.name.starts_with("__Host-");
    let is_secure_prefix = is_host_prefix || cookie.name.starts_with("__Secure-");
    let path = if is_host_prefix {
        "/".to_string()
    } else {
        cookie.path.clone()
    };
    let domain = if is_host_prefix {
        None
    } else {
        Some(cookie.domain.clone())
    };
    Network::CookieParam {
        name: cookie.name,
        value: cookie.value,
        url: Some(format!("https://{host}{path}")),
        domain,
        path: Some(path),
        secure: Some(cookie.secure || is_secure_prefix),
        http_only: Some(cookie.http_only),
        same_site: cookie.same_site.and_then(|s| match s.as_str() {
            "Strict" => Some(CookieSameSite::Strict),
            "Lax" => Some(CookieSameSite::Lax),
            "None" => Some(CookieSameSite::None),
            _ => None,
        }),
        expires: cookie.expires,
        priority: None,
        same_party: None,
        source_scheme: None,
        source_port: None,
        partition_key: None,
    }
}

/// Resolve the user-data-dir Chrome should be launched with for the
/// configured `source` value. Recognized values:
/// - `"chrome"`           → the user's own Chrome profile (with the
///                          `copy_profile_essentials` snapshot fallback)
/// - `""` / `"phoenix"`   → Phoenix-managed sandboxed profile
/// - `"zen"`, `"firefox"`, → Firefox-family login sources whose cookies are
///   `"librewolf"`, …       ported over CDP after launch. These are NOT
///                          filesystem paths — treating them as such would
///                          launch Chrome with `--user-data-dir=zen`, a
///                          cwd-relative entry that wanders on every fresh
///                          start and produces a brand-new profile each run.
/// - anything else        → literal path (may be `~/`-prefixed)
pub(crate) fn resolve_user_data_dir(source: &str) -> std::path::PathBuf {
    let source = source.trim();
    if source.eq_ignore_ascii_case("chrome") {
        home_join(".config/google-chrome")
    } else if source.is_empty()
        || source.eq_ignore_ascii_case("phoenix")
        || is_supported_source(source)
    {
        crate::config::phoenix_home().join("browser/profile")
    } else {
        std::path::PathBuf::from(shellexpand(source))
    }
}

/// Resolve a durable coworker browser identity to its Phoenix-owned profile.
/// Phoenix keeps the original canonical profile so every existing login stays
/// active after migration. Other coworkers start private and empty; cookies
/// are copied only through an explicit scoped import/grant.
pub(crate) fn profile_dir_for_instance(instance: &str, source: &str) -> Result<std::path::PathBuf> {
    if instance.is_empty() || instance == "agent-phoenix" {
        return Ok(resolve_user_data_dir(source));
    }
    anyhow::ensure!(
        !instance.is_empty()
            && instance.len() <= 128
            && instance
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')),
        "invalid browser profile id"
    );
    Ok(crate::config::phoenix_home()
        .join("browser/profiles")
        .join(instance))
}

/// Stable, user-revealable destination for native website downloads. Keeping
/// downloads outside Chrome's profile prevents cache cleanup/profile repair
/// from deleting the user's files. The profile id remains part of the path so
/// coworkers cannot accidentally overwrite one another's artifacts.
pub(crate) fn download_dir_for_instance(instance: &str) -> Result<std::path::PathBuf> {
    let instance = if instance.is_empty() {
        "agent-phoenix"
    } else {
        instance
    };
    anyhow::ensure!(
        instance.len() <= 128
            && instance
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')),
        "invalid browser profile id"
    );
    Ok(crate::config::phoenix_home()
        .join("downloads")
        .join(instance))
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct EmbeddedBrowserTarget {
    target_id: String,
    debugger_port: u16,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct EmbeddedBrowserSurfaceTab {
    pub id: String,
    pub title: String,
    pub url: String,
    pub active: bool,
    #[serde(default)]
    pub favicon: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct EmbeddedBrowserSurfaceState {
    pub target_id: Option<String>,
    #[serde(default)]
    pub tabs: Vec<EmbeddedBrowserSurfaceTab>,
}

/// Drive the Electron-owned WebContentsView tab strip through the same private
/// authenticated loopback channel used to create the first embedded page.
/// The gateway then adopts the returned target over CDP, so the visible tab
/// and the agent's working tab can never diverge.
pub(super) fn embedded_browser_surface_action(
    instance: &str,
    action: &str,
    target_id: Option<&str>,
    url: Option<&str>,
) -> Result<Option<EmbeddedBrowserSurfaceState>> {
    let Some(base) = crate::config::paths::chromium_bridge_url()?
    else {
        return Ok(None);
    };
    let endpoint_path = match action {
        "status" => "browser/status",
        "new_tab" => "browser/new-tab",
        "switch_tab" => "browser/switch-tab",
        "close_tab" => "browser/close-tab",
        _ => anyhow::bail!("unsupported embedded browser surface action"),
    };
    let token = crate::config::paths::read_chromium_bridge_token()?
        .or_else(|| std::env::var("PHOENIX_CHROMIUM_BRIDGE_TOKEN").ok())
        .context("PHOENIX_CHROMIUM_BRIDGE_TOKEN is missing")?;
    anyhow::ensure!(
        token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Chromium bridge token is invalid"
    );
    let base = reqwest::Url::parse(base.trim()).context("invalid Chromium bridge URL")?;
    anyhow::ensure!(
        base.scheme() == "http" && base.host_str() == Some("127.0.0.1"),
        "Chromium bridge must use loopback HTTP"
    );
    let mut endpoint = base
        .join(endpoint_path)
        .context("invalid Chromium browser-tab endpoint")?;
    endpoint.query_pairs_mut().append_pair("token", &token);
    let profile = if instance.is_empty() {
        "agent-phoenix"
    } else {
        instance
    };
    let response = reqwest::blocking::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .build()
        .context("could not initialize Chromium bridge client")?
        .post(endpoint)
        .json(&serde_json::json!({
            "instance": profile,
            "targetId": target_id,
            "url": url,
        }))
        .send()
        .map_err(|_| anyhow::anyhow!("Phoenix Chromium shell is not reachable"))?;
    anyhow::ensure!(
        response.status().is_success(),
        "Phoenix Chromium shell rejected browser tab action (HTTP {})",
        response.status().as_u16()
    );
    let body = response
        .bytes()
        .map_err(|_| anyhow::anyhow!("could not read the Chromium browser tab state"))?;
    let state = serde_json::from_slice(&body)
        .context("Phoenix Chromium shell returned invalid browser tab state")?;
    Ok(Some(state))
}

/// Capture the exact embedded target without revealing its pane or opening a
/// different browser. The shell owns the short-lived compositor frame pump;
/// this HTTP deadline includes both capture and cleanup instead of inheriting
/// the CDP connection's much longer idle timeout.
pub(super) fn embedded_browser_capture(instance: &str, target_id: &str, full_page: bool) -> Result<Vec<u8>> {
    let base = crate::config::paths::chromium_bridge_url()?
        .context("Phoenix Chromium bridge is unavailable for this browser capture")?;
    let token = crate::config::paths::read_chromium_bridge_token()?
        .or_else(|| std::env::var("PHOENIX_CHROMIUM_BRIDGE_TOKEN").ok())
        .context("PHOENIX_CHROMIUM_BRIDGE_TOKEN is missing")?;
    request_embedded_capture(&base, &token, instance, target_id, full_page)
}

/// The design controller captures the full document at an exact CSS viewport
/// width, including pages whose overflow would otherwise widen the bitmap.
pub(super) fn embedded_design_capture(instance: &str, target_id: &str, width: u32, height: u32) -> Result<Vec<u8>> {
    let base = crate::config::paths::chromium_bridge_url()?
        .context("Phoenix Chromium bridge is unavailable for design preview")?;
    let token = crate::config::paths::read_chromium_bridge_token()?
        .or_else(|| std::env::var("PHOENIX_CHROMIUM_BRIDGE_TOKEN").ok())
        .context("Phoenix Chromium bridge credential is missing")?;
    request_embedded_capture_clip(&base, &token, instance, target_id, true, Some((width, height)))
}

fn request_embedded_input_ready(base: &str, token: &str, instance: &str, target_id: &str) -> Result<()> {
    use std::io::Read;
    anyhow::ensure!(token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Chromium bridge token is invalid");
    anyhow::ensure!(!target_id.is_empty() && target_id.len() <= 128,
        "Browser input requires its exact target ID");
    let base = reqwest::Url::parse(base.trim()).context("invalid Chromium bridge URL")?;
    anyhow::ensure!(base.scheme() == "http" && base.host_str() == Some("127.0.0.1"),
        "Chromium bridge must use loopback HTTP");
    let mut endpoint = base.join("browser/prepare-input").context("invalid Chromium input endpoint")?;
    endpoint.query_pairs_mut().append_pair("token", token);
    let instance = if instance.is_empty() { "agent-phoenix" } else { instance };
    let response = reqwest::blocking::Client::builder()
        .no_proxy().connect_timeout(Duration::from_secs(2)).timeout(Duration::from_secs(8))
        .build().context("could not initialize Chromium input client")?
        .post(endpoint).json(&serde_json::json!({"instance":instance,"targetId":target_id}))
        // Never expose the credential-bearing URL or classify this pre-input
        // refusal as a failed gesture that should be automatically replayed.
        .send().map_err(|_| anyhow::anyhow!("Phoenix browser input preparation failed; no input sent"))?;
    if !response.status().is_success() {
        // The bridge says why (tab closed, tab changed during preparation);
        // a bare status left the agent guessing and hacking field values.
        let status = response.status().as_u16();
        let reason = response.text().ok()
            .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
            .and_then(|value| value["error"].as_str().map(|text| text.chars().take(200).collect::<String>()))
            .unwrap_or_default();
        anyhow::bail!("Phoenix browser input preparation rejected (HTTP {status}{}{reason}); no input sent",
            if reason.is_empty() { "" } else { ": " });
    }
    let mut bytes = Vec::new();
    response.take(4097).read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("Phoenix browser input preparation was not confirmed; no input sent"))?;
    anyhow::ensure!(bytes.len() <= 4096, "Chromium input readiness response is too large; no input sent");
    let ready: serde_json::Value = serde_json::from_slice(&bytes)
        .context("invalid Chromium input readiness response; no input sent")?;
    anyhow::ensure!(ready["ready"] == true && ready["instance"] == instance && ready["targetId"] == target_id,
        "Chromium input readiness belongs to a different or unready target; no input sent");
    Ok(())
}

fn request_embedded_capture(base: &str, token: &str, instance: &str, target_id: &str, full_page: bool) -> Result<Vec<u8>> {
    request_embedded_capture_clip(base, token, instance, target_id, full_page, None)
}

fn request_embedded_capture_clip(base: &str, token: &str, instance: &str, target_id: &str, full_page: bool, clip: Option<(u32, u32)>) -> Result<Vec<u8>> {
    use std::io::Read;
    const MAX_BYTES: u64 = 24 * 1024 * 1024;
    anyhow::ensure!(token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Chromium bridge token is invalid");
    anyhow::ensure!(!target_id.is_empty() && target_id.len() <= 128, "Browser capture requires its exact target ID");
    let base = reqwest::Url::parse(base.trim()).context("invalid Chromium bridge URL")?;
    anyhow::ensure!(base.scheme() == "http" && base.host_str() == Some("127.0.0.1"),
        "Chromium bridge must use loopback HTTP");
    let mut endpoint = base.join("browser/capture").context("invalid Chromium capture endpoint")?;
    endpoint.query_pairs_mut().append_pair("token", token);
    let instance = if instance.is_empty() { "agent-phoenix" } else { instance };
    let mut body = serde_json::json!({"instance":instance,"targetId":target_id,"fullPage":full_page});
    if let Some((width, height)) = clip {
        anyhow::ensure!(full_page && (320..=3840).contains(&width) && (240..=12000).contains(&height)
            && u64::from(width) * u64::from(height) <= 32 * 1024 * 1024,
            "Design preview capture dimensions are outside the supported bounds");
        body["clip"] = serde_json::json!({"x":0,"y":0,"width":width,"height":height,"scale":1});
    }
    let response = reqwest::blocking::Client::builder()
        .no_proxy().connect_timeout(Duration::from_secs(2)).timeout(Duration::from_secs(10))
        .build().context("could not initialize Chromium capture client")?
        .post(endpoint).json(&body)
        .send().map_err(|_| anyhow::anyhow!("Phoenix Chromium screenshot request failed or exceeded its deadline"))?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let mut body = Vec::new();
        response.take(4096).read_to_end(&mut body)
            .map_err(|_| anyhow::anyhow!("could not read the Chromium capture failure"))?;
        let detail = serde_json::from_slice::<serde_json::Value>(&body).ok()
            .and_then(|value| value.get("error")?.as_str().map(|text| text.chars().take(400).collect::<String>()));
        anyhow::bail!("Phoenix Chromium capture rejected (HTTP {status}): {}", detail.as_deref().unwrap_or("no image returned"));
    }
    let header = |name: &str| response.headers().get(name).and_then(|value| value.to_str().ok());
    anyhow::ensure!(header("x-phoenix-instance") == Some(instance) && header("x-phoenix-target-id") == Some(target_id),
        "Chromium screenshot belongs to a different browser target");
    anyhow::ensure!(header("content-type") == Some("image/png"), "Chromium screenshot response is not PNG");
    anyhow::ensure!(response.content_length().is_none_or(|length| length <= MAX_BYTES), "Chromium screenshot is too large");
    let mut bytes = Vec::new();
    response.take(MAX_BYTES + 1).read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("Chromium screenshot body failed or exceeded its deadline"))?;
    anyhow::ensure!(bytes.len() as u64 <= MAX_BYTES && bytes.len() >= 33
        && bytes.starts_with(b"\x89PNG\r\n\x1a\n") && &bytes[12..16] == b"IHDR",
        "Chromium returned an empty or invalid PNG screenshot");
    let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap()) as u64;
    let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap()) as u64;
    anyhow::ensure!(width > 0 && height > 0 && width <= 16384 && height <= 16384
        && width * height <= 32 * 1024 * 1024, "Chromium screenshot dimensions are outside the supported bounds");
    if let Some((expected_width, expected_height)) = clip {
        anyhow::ensure!(width == u64::from(expected_width) && height == u64::from(expected_height),
            "Design preview returned a bitmap with the wrong dimensions");
    }
    Ok(bytes)
}

#[cfg(test)]
mod embedded_capture_tests {
    use super::request_embedded_capture;
    use std::io::{BufRead, Read, Write};

    #[test]
    fn embedded_input_readiness_requires_exact_target_and_bounded_ack_before_input() {
        for case in 0..7 {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}/", listener.local_addr().unwrap());
            let mut ack = serde_json::json!({"ready":true,"instance":"agent-coder","targetId":"exact-target"});
            match case {
                1 => ack["instance"] = "agent-other".into(),
                2 => ack["targetId"] = "other-target".into(),
                3 => ack["ready"] = false.into(),
                _ => {}
            }
            let body = match case {
                4 => "x".repeat(4097),
                5 => "{".into(),
                _ => ack.to_string(),
            };
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert!(line.starts_with("POST /browser/prepare-input?token="));
                let mut length = 0usize;
                loop {
                    line.clear(); reader.read_line(&mut line).unwrap();
                    if line == "\r\n" { break; }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                }
                assert!(length > 0 && length < 1024);
                let mut payload = vec![0; length]; reader.read_exact(&mut payload).unwrap();
                assert_eq!(serde_json::from_slice::<serde_json::Value>(&payload).unwrap(),
                    serde_json::json!({"instance":"agent-coder","targetId":"exact-target"}));
                let status = if case == 6 { 403 } else { 200 };
                let headers = format!("HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                stream.write_all(headers.as_bytes()).unwrap();
                let _ = stream.write_all(body.as_bytes());
            });
            let token = "a".repeat(64);
            let ready = super::request_embedded_input_ready(&base, &token, "agent-coder", "exact-target");
            server.join().unwrap();
            let mut gestures = 0;
            let outcome = ready.map(|()| { gestures += 1; });
            assert_eq!(gestures, usize::from(case == 0), "case {case} must reject before gesture");
            if let Err(error) = outcome {
                assert!(!error.to_string().contains(&token));
                assert!(!super::super::is_transport_dead(&error), "readiness refusal must not replay a batch");
            }
        }
        assert!(super::request_embedded_input_ready("http://127.0.0.1:1", "invalid", "agent-coder", "exact-target").is_err());
    }

    #[test]
    fn embedded_capture_binds_exact_target_and_refuses_invalid_output() {
        let mut cursor = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 2).write_to(&mut cursor, image::ImageFormat::Png).unwrap();
        let png = cursor.into_inner();
        for case in 0..7 {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}/", listener.local_addr().unwrap());
            let mut body = png.clone();
            let mut status = 200;
            let target = if case == 1 { "other-target" } else { "expected-target" };
            let instance = if case == 2 { "other-coworker" } else { "agent-coder" };
            match case {
                3 => body.clear(),
                4 => body.truncate(12),
                5 => body[16..20].copy_from_slice(&20000u32.to_be_bytes()),
                6 => { status = 400; body = br#"{"error":"Browser capture target closed before completion"}"#.to_vec(); }
                _ => {}
            }
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert!(line.starts_with("POST /browser/capture?token="));
                let mut length = 0;
                loop {
                    line.clear(); reader.read_line(&mut line).unwrap();
                    if line == "\r\n" { break; }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                }
                assert!(length > 0 && length < 1024);
                let mut payload = vec![0; length]; reader.read_exact(&mut payload).unwrap();
                let request: serde_json::Value = serde_json::from_slice(&payload).unwrap();
                assert_eq!(request, serde_json::json!({"instance":"agent-coder","targetId":"expected-target","fullPage":true}));
                let headers = format!("HTTP/1.1 {status} OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nx-phoenix-instance: {instance}\r\nx-phoenix-target-id: {target}\r\nConnection: close\r\n\r\n", body.len());
                stream.write_all(headers.as_bytes()).unwrap();
                // A caller rejecting owner headers may close before the body.
                let _ = stream.write_all(&body);
            });
            let result = request_embedded_capture(&base, &"a".repeat(64), "agent-coder", "expected-target", true);
            server.join().unwrap();
            if case == 0 {
                assert_eq!(result.unwrap(), png);
            } else {
                let error = result.unwrap_err().to_string();
                assert!(match case {
                    1 | 2 => error.contains("different browser target"),
                    3 | 4 => error.contains("invalid PNG"),
                    5 => error.contains("dimensions"),
                    6 => error.contains("target closed"),
                    _ => false,
                }, "case {case}: {error}");
                assert!(!error.contains(&"a".repeat(64)), "bridge credentials must not enter tool errors");
            }
        }
    }
}

/// Instance suffix for Iris's disposable design-review capture browser.
pub(crate) const DESIGN_REVIEW_INSTANCE_SUFFIX: &str = "-job-design-review";

fn embedded_browser_target(instance: &str) -> Result<Option<EmbeddedBrowserTarget>> {
    let Some(base) = crate::config::paths::chromium_bridge_url()?
    else {
        return Ok(None);
    };
    // The desktop can restart without restarting the detached gateway. Read
    // the durable private credential on every bridge request so the gateway
    // never keeps using a stale inherited token and receiving HTTP 403.
    let token = crate::config::paths::read_chromium_bridge_token()?
        .or_else(|| std::env::var("PHOENIX_CHROMIUM_BRIDGE_TOKEN").ok())
        .context("PHOENIX_CHROMIUM_BRIDGE_TOKEN is missing")?;
    anyhow::ensure!(
        token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Chromium bridge token is invalid"
    );
    let base = reqwest::Url::parse(base.trim()).context("invalid Chromium bridge URL")?;
    anyhow::ensure!(
        base.scheme() == "http" && base.host_str() == Some("127.0.0.1"),
        "Chromium bridge must use loopback HTTP"
    );
    let mut endpoint = base
        .join("browser/open")
        .context("invalid Chromium browser-open endpoint")?;
    endpoint.query_pairs_mut().append_pair("token", &token);
    let profile = if instance.is_empty() {
        "agent-phoenix"
    } else {
        instance
    };
    // Parallel workers own independent tab strips while inheriting the
    // spawning coworker's Electron session partition. This shares cookies,
    // local/session storage, service workers, and site authentication without
    // sharing a mutable tab or copying secrets into worker context.
    let profile_owner = crate::tools::isolated_desktop::current_parent_browser_instance()
        .unwrap_or_else(|| profile.to_string());
    let response = reqwest::blocking::Client::builder()
        // This is an authenticated loopback control plane. System HTTP proxy
        // settings must never intercept it or turn browser opening into a
        // proxy timeout.
        .no_proxy()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .build()
        .context("could not initialize Chromium bridge client")?
        .post(endpoint)
        .json(&serde_json::json!({
            "instance": profile,
            "profileOwner": profile_owner,
        }))
        .send()
        // Reqwest errors include the full request URL. Discard that source so
        // the bridge credential in its query string can never reach UI/logs.
        .map_err(|_| anyhow::anyhow!("Phoenix Chromium shell is not reachable"))?;
    anyhow::ensure!(
        response.status().is_success(),
        "Phoenix Chromium shell rejected browser creation (HTTP {})",
        response.status().as_u16()
    );
    let body = response
        .bytes()
        .map_err(|_| anyhow::anyhow!("could not read the Chromium browser target"))?;
    let target: EmbeddedBrowserTarget = serde_json::from_slice(&body)
        .context("Phoenix Chromium shell returned an invalid browser target")?;
    anyhow::ensure!(
        !target.target_id.is_empty() && target.target_id.len() <= 128,
        "Phoenix Chromium shell returned an invalid target id"
    );
    Ok(Some(target))
}

pub(super) fn embedded_browser_target_id(instance: &str) -> Result<Option<String>> {
    embedded_browser_target(instance).map(|target| target.map(|target| target.target_id))
}

/// Destroy a disposable worker's WebContentsViews while retaining the shared
/// parent session partition. Closing these independent tabs cannot log the
/// parent out; it only releases the child's renderer and surface bookkeeping.
pub(super) fn discard_embedded_browser_surface(instance: &str) -> Result<()> {
    let Some(base) = crate::config::paths::chromium_bridge_url()?
    else {
        return Ok(());
    };
    let token = crate::config::paths::read_chromium_bridge_token()?
        .or_else(|| std::env::var("PHOENIX_CHROMIUM_BRIDGE_TOKEN").ok())
        .context("PHOENIX_CHROMIUM_BRIDGE_TOKEN is missing")?;
    anyhow::ensure!(
        token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Chromium bridge token is invalid"
    );
    let base = reqwest::Url::parse(base.trim()).context("invalid Chromium bridge URL")?;
    anyhow::ensure!(
        base.scheme() == "http" && base.host_str() == Some("127.0.0.1"),
        "Chromium bridge must use loopback HTTP"
    );
    let mut endpoint = base
        .join("browser/discard")
        .context("invalid Chromium browser-discard endpoint")?;
    endpoint.query_pairs_mut().append_pair("token", &token);
    // Worker cleanup runs from `VolumeWorkerCleanup::drop`, which can execute
    // directly on a Tokio worker. A reqwest blocking Client owns an internal
    // runtime and panics if that runtime is dropped from an async context.
    // Keep the bounded HTTP cleanup synchronous to the caller, but construct
    // and drop the client on a plain OS thread so cleanup can never take down
    // the parent turn after a worker has already produced its result.
    let instance = instance.to_string();
    std::thread::spawn(move || -> Result<()> {
        let response = reqwest::blocking::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(5))
            .build()
            .context("could not initialize Chromium bridge client")?
            .post(endpoint)
            .json(&serde_json::json!({ "instance": instance }))
            .send()
            .map_err(|_| anyhow::anyhow!("Phoenix Chromium shell is not reachable"))?;
        anyhow::ensure!(
            response.status().is_success(),
            "Phoenix Chromium shell rejected worker browser cleanup (HTTP {})",
            response.status().as_u16()
        );
        Ok(())
    })
    .join()
    .map_err(|_| anyhow::anyhow!("Chromium worker browser cleanup thread panicked"))?
}

impl Session {
    /// Prepare the exact current embedded page before its first gesture. The
    /// shell pumps a tiny frame only for a hidden view; ordinary observations,
    /// idle time and standalone Chromium do not pay this cost.
    pub(super) fn prepare_input(&self) -> Result<()> {
        if self.health.mode != "embedded" { return Ok(()); }
        let base = crate::config::paths::chromium_bridge_url()?
            .context("Phoenix browser input preparation is unavailable; no input sent")?;
        let token = crate::config::paths::read_chromium_bridge_token()?
            .or_else(|| std::env::var("PHOENIX_CHROMIUM_BRIDGE_TOKEN").ok())
            .context("PHOENIX_CHROMIUM_BRIDGE_TOKEN is missing")?;
        request_embedded_input_ready(&base, &token, &self.instance, self.tab.get_target_id())
    }

    /// Attach to an already-running browser if one is reachable, else launch.
    ///
    /// With `source = "chrome"`, the configured debug port is probed first so a
    /// live Chrome (started with `--remote-debugging-port`) is used directly;
    /// otherwise Chrome's profile is launched — copied first if Chrome holds
    /// the lock. An explicit PHOENIX_BROWSER_ATTACH target that is unreachable
    /// is a hard error (never silently fall back to a different browser).
    ///
    /// `instance` is "" for Phoenix's original profile; every coworker passes
    /// its durable profile id. Disposable workers attach their own target/tab
    /// strip to the parent's authenticated Electron partition when available;
    /// the isolated managed-Chromium path remains the bridge-down fallback.
    pub(super) fn open(instance: &str) -> Result<Self> {
        // Builds before the embedded browser could leave a separate login
        // handoff alive. That surface no longer exists in the product, so close
        // the exact legacy owner before opening the in-app/headless session.
        // `finish` verifies the recorded pid/profile before touching anything.
        if super::handoff::active(instance).is_some() {
            super::handoff::finish(instance)
                .context("could not retire a legacy external login window")?;
        }
        let prefs = browser_prefs();
        let session = Self::open_inner(&prefs, instance)?;
        // Always-fresh login porting: read the chosen source browser's current
        // cookies and inject them now, so the session is as logged-in as that
        // browser is right now. Best-effort — a porting failure never blocks the
        // browser from opening.
        // Legacy login_source is authority for Phoenix's preserved profile,
        // not a blanket cookie grant to every coworker.
        if instance.is_empty() || instance == "agent-phoenix" {
            session.port_logins(&prefs.login_source);
        }
        let credential_owner = crate::tools::isolated_desktop::current_parent_browser_instance()
            .unwrap_or_else(|| instance.to_string());
        session.refresh_cookie_grants(&credential_owner);
        // Keep the CDP websocket warm. A persisted session that sits idle between
        // turns (the user stops, then resumes minutes later) had its websocket go
        // stale, so the next action hit `transport-dead` → drop+respawn, which KILLS
        // the still-alive chrome and reopens it ("browser died, went back to
        // twitter"). A cheap periodic CDP call keeps the socket alive so a resumed
        // turn reuses the same browser. Tied to the tab via a Weak — it exits when
        // the session is dropped.
        if session.health.mode != "embedded" {
            spawn_keepalive(&session.tab);
        }
        teaching_observer::arm(&session.tab, instance);
        // Legacy managed Chrome still needs the bounded frame stream when no
        // native surface is mounted. Chromium-shell targets render directly
        // inside Phoenix and must never spend CPU encoding duplicate JPEGs.
        if session.health.mode != "embedded" {
            screencast::arm(&session.tab, instance);
        }
        Ok(session)
    }

    pub(super) fn open_inner(prefs: &BrowserPrefs, instance: &str) -> Result<Self> {
        let isolated_desktop = crate::tools::isolated_desktop::current_browser_environment()?;
        // A bridge-owned Electron WebContentsView is a host-app surface. It
        // must never be selected for an agent that owns a separate X desktop,
        // even if the bridge is available in the parent process.
        // Iris's design review needs no logins and must paint while nobody is
        // looking. An embedded view parked off-screen never gets a compositor
        // frame on Wayland, so this one instance always runs headless.
        if isolated_desktop.is_none() && !instance.ends_with(DESIGN_REVIEW_INSTANCE_SUFFIX) {
            if let Some(target) = embedded_browser_target(instance)? {
                return Self::attach_embedded(target, instance);
            }
        }
        if isolated_desktop.is_none()
            && !instance.is_empty()
            && !instance.starts_with("volume-worker-")
            && !instance.contains("-job-")
        {
            anyhow::bail!(
                "Phoenix's in-app browser is unavailable. Keep the Phoenix desktop open; this coworker's browser is shared with its right sidebar and will not launch as a separate app."
            );
        }
        // A lease can outlive the desktop process while the gateway keeps
        // running. If the authenticated Electron target was unavailable above,
        // the lease is stale: retire it and continue with the headless frame
        // fallback. Never turn that stale bit into a separate host window.
        if isolated_desktop.is_none() && super::native_surface_attached(instance) {
            super::detach_native_surface(instance);
        }
        // A parallel instance can never share an attach target — one running
        // chrome is one CDP surface. Scoped instances always launch their own.
        if isolated_desktop.is_some() || !instance.is_empty() {
            return Self::launch(prefs, Vec::new(), instance);
        }
        if !prefs.attach.trim().is_empty() {
            let ws = resolve_ws_url(prefs.attach.trim()).with_context(|| {
                format!(
                    "explicit browser attach target {:?} is not reachable",
                    prefs.attach.trim()
                )
            })?;
            return Self::attach(ws);
        }
        let mut notes = Vec::new();
        if prefs.source.eq_ignore_ascii_case("chrome") {
            match resolve_ws_url(&prefs.attach_port.to_string()) {
                Ok(ws) => match Self::attach(ws.clone()) {
                    Ok(session) => return Ok(session),
                    Err(error) => notes.push(format!(
                        "Configured Chrome DevTools port {} resolved to {ws}, but attach failed: {error:#}. Falling back to launching a managed Chromium.",
                        prefs.attach_port
                    )),
                },
                Err(error) => {
                    notes.push(format!(
                        "Configured Chrome DevTools port {} was not reachable: {error:#}. Falling back to launching a managed Chromium.",
                        prefs.attach_port
                    ));
                }
            }
        }
        Self::launch(prefs, notes, instance)
    }

    /// Best-effort compatibility import for Phoenix's pre-redesign canonical
    /// profile. New coworker sharing always goes through site-scoped grants.
    pub(super) fn port_logins(&self, source: &str) {
        let source = source.trim();
        if source.is_empty() || source.eq_ignore_ascii_case("none") {
            return;
        }
        match self.import_cookie_set(source, None) {
            Ok((refreshed, skipped)) => tracing::info!(
                "login port ({source}): refreshed {refreshed} source cookies in Phoenix's preserved profile ({skipped} device-bound skipped)"
            ),
            Err(error) => tracing::warn!("login port ({source}): {error:#}"),
        }
    }

    /// Import cookies for one exact site tree into this coworker's live
    /// profile. Device-bound Google/Microsoft sessions are never transplanted;
    /// those must be established natively through login handoff.
    pub(super) fn import_site_cookies(&self, source: &str, site: &str) -> Result<(usize, usize)> {
        let site = crate::tools::browser_cookie_grants::normalize_site(site)?;
        let outcome = self.import_cookie_set(source, Some(&site))?;
        anyhow::ensure!(
            outcome.0 > 0,
            "no portable cookies for {site} were available in {source}; use login handoff for device-bound or localStorage-backed sessions"
        );
        Ok(outcome)
    }

    /// Apply every durable grant visible to this coworker on browser launch.
    /// A bad or locked source browser never prevents Chrome from opening; each
    /// grant is independently retried on a later launch.
    fn refresh_cookie_grants(&self, instance: &str) {
        let Ok(agent_id) = crate::tools::browser_cookie_grants::agent_id_for_profile(instance)
        else {
            return;
        };
        let grants = match crate::tools::browser_cookie_grants::applicable_to(&agent_id) {
            Ok(grants) => grants,
            Err(error) => {
                tracing::warn!("cookie grants for {agent_id}: {error:#}");
                return;
            }
        };
        for grant in grants {
            let site = (!grant.all_portable).then_some(grant.site.as_str());
            match self.import_cookie_set(&grant.source, site) {
                Ok((refreshed, skipped)) => tracing::info!(
                    "cookie grant {}: refreshed {refreshed} {} cookies for {agent_id} ({skipped} device-bound skipped)",
                    grant.grant_id,
                    grant.site
                ),
                Err(error) => tracing::warn!(
                    "cookie grant {} for {agent_id} could not refresh: {error:#}",
                    grant.grant_id
                ),
            }
        }
    }

    fn import_cookie_set(&self, source: &str, site: Option<&str>) -> Result<(usize, usize)> {
        let source = source.trim();
        anyhow::ensure!(
            crate::tools::browser_cookies::is_supported_source(source),
            "unsupported cookie source `{source}`"
        );
        // Chromium-family (Chrome/Brave/Edge/…) decrypt their OS-Crypt store;
        // Firefox-family read the plaintext cookies.sqlite. Same PortedCookie
        // shape and the same device-bound filtering downstream.
        let cookies = crate::tools::browser_cookies::read_source_cookies(source)?;
        let cookies = cookies.into_iter().filter(|cookie| {
            site.is_none_or(|site| {
                crate::tools::browser_cookie_grants::domain_matches_site(&cookie.domain, site)
            })
        });
        // Never transplant device/token-bound sessions (Google/Microsoft): the
        // server invalidates them on a foreign browser and logs the user out of
        // their REAL browser too. Drop them before injection — strictly safer.
        let cookies = cookies.collect::<Vec<_>>();
        let total = cookies.len();
        let cookies: Vec<_> = cookies
            .into_iter()
            .filter(|c| !crate::tools::browser_cookies::is_session_bound_domain(&c.domain))
            .collect();
        let skipped = total - cookies.len();
        let params: Vec<Network::CookieParam> = cookies.into_iter().map(to_cookie_param).collect();
        let tab = Arc::clone(&self.tab);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // `login_source = "zen"` is an explicit authority choice. Overlay
            // every cookie Zen currently has, while leaving managed-only
            // cookies untouched. Filtering conflicts out here preserved stale,
            // server-invalid managed auth forever: a refreshed Zen GitHub
            // `user_session` could never replace the identically named cookie.
            //
            // This remains one batched CDP call. `Tab::set_cookies` is avoided
            // because its per-cookie delete pre-pass timed out on large stores.
            let refreshed = params.len();
            let outcome = if params.is_empty() {
                Ok(())
            } else {
                tab.call_method(Network::SetCookies { cookies: params })
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            };
            let _ = tx.send(outcome.map(|()| refreshed));
        });
        let refreshed = match rx.recv_timeout(Duration::from_secs(8)) {
            Ok(Ok(refreshed)) => refreshed,
            Ok(Err(error)) => anyhow::bail!("cookie injection failed: {error}"),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                anyhow::bail!("cookie injection timed out after 8s")
            }
            Err(error) => anyhow::bail!("cookie injection worker failed: {error}"),
        };
        Ok((refreshed, skipped))
    }

    /// Cleanly flush this managed browser's storage to its profile before the
    /// Session is dropped/relaunched. A launched Session OWNS its chrome, so
    /// SIGTERM makes Chrome shut down gracefully — flushing cookies AND
    /// localStorage (Discord's session token lives there) — instead of the
    /// SIGKILL that `TemporaryProcess::drop` would do, which loses the login
    /// the user just completed. No-op for attach/reconnect: those drive a
    /// browser Phoenix does not own and must never terminate.
    pub(super) fn flush_and_close(&self) {
        self.flush_and_close_until(std::time::Instant::now() + Duration::from_secs(5));
    }

    /// Share the daemon's one browser-drain deadline. This keeps a slot that
    /// was busy for part of shutdown from starting a brand-new five-second
    /// grace period after every other instance has already finished.
    pub(super) fn flush_and_close_until(&self, deadline: std::time::Instant) {
        if matches!(self.health.mode, "attach" | "embedded")
            || (self.health.mode == "reconnect" && self.owned_pid.is_none())
        {
            return;
        }
        // Stop the high-rate JPEG producer before waiting for Chrome's profile
        // flush. This makes turn completion immediately quiet even if Chrome
        // needs a moment to exit.
        let _ = self.tab.stop_screencast();
        if let Some(pid) = self.owned_pid.or_else(|| self._browser.get_process_id()) {
            if sigterm_and_wait_until(pid, deadline) {
                super::blog(&format!(
                    "clean-closed chrome pid {pid} — cookies/localStorage flushed to the profile"
                ));
            } else {
                super::blog(&format!(
                    "chrome pid {pid} did NOT exit before the graceful-close deadline after SIGTERM — storage flush not guaranteed"
                ));
            }
        }
    }

    pub(super) fn attach(ws_url: String) -> Result<Self> {
        let browser = Browser::connect_with_timeout(ws_url.clone(), IDLE_SOCKET_BUDGET)
            .with_context(|| format!("failed to attach to live browser at {ws_url}"))?;
        // A fresh tab keeps us out of whatever the user is currently viewing.
        let tab = match browser.new_tab() {
            Ok(t) => t,
            Err(_) => existing_page_tab(&browser)
                .context("attached, but could not open or find a usable tab")?,
        };
        tab.set_default_timeout(Duration::from_secs(20));
        let console = attach_console_listener(&tab);
        attach_dialog_handler(&tab);
        let crashed = attach_crash_detector(&tab);
        // Attach sessions idle exactly like launched ones (the user's own
        // browser sits quiet between turns) — keep their socket warm too.
        spawn_keepalive(&tab);
        super::blog(&format!("attached to the user's live browser at {ws_url}"));
        Ok(Self {
            _browser: browser,
            tab,
            owned_pid: None,
            surface_token: None,
            crashed,
            instance: String::new(), // attach mode serves Phoenix's original profile
            headless: false,         // the user's own running browser is visible
            health: BrowserHealth {
                mode: "attach",
                attach: Some(ws_url),
                user_data_dir: None,
                binary: None,
                login_source: String::new(),
                notes: Vec::new(),
            },
            console,
            backend_ids: std::sync::Mutex::new(std::collections::HashMap::new()),
            last_state_hash: std::sync::atomic::AtomicU64::new(0),
        })
    }

    fn attach_embedded(target: EmbeddedBrowserTarget, instance: &str) -> Result<Self> {
        let ws_url = resolve_ws_url(&target.debugger_port.to_string())
            .context("Chromium shell did not expose its DevTools endpoint")?;
        let browser = Browser::connect_with_timeout(ws_url.clone(), IDLE_SOCKET_BUDGET)
            .with_context(|| format!("failed to attach to Phoenix Chromium at {ws_url}"))?;
        // `getOrCreateDevToolsTargetId` is synchronous in Electron, but the
        // browser-level Target domain can publish the new WebContentsView a
        // few frames later. Refresh the exact target briefly rather than
        // falling back to a screenshot browser because of that harmless race.
        let discover_deadline = Instant::now() + Duration::from_secs(3);
        let tab = loop {
            browser.register_missing_tabs();
            if let Some(tab) = browser.get_tabs().lock().ok().and_then(|tabs| {
                tabs.iter()
                    .find(|tab| tab.get_target_id().as_str() == target.target_id.as_str())
                    .cloned()
            }) {
                break tab;
            }
            if Instant::now() >= discover_deadline {
                anyhow::bail!(
                    "Chromium shell target {} for '{}' was not discoverable over CDP",
                    target.target_id,
                    if instance.is_empty() {
                        "agent-phoenix"
                    } else {
                        instance
                    }
                );
            }
            std::thread::sleep(Duration::from_millis(30));
        };
        tab.set_default_timeout(Duration::from_secs(20));
        let console = attach_console_listener(&tab);
        attach_dialog_handler(&tab);
        let crashed = attach_crash_detector(&tab);
        super::blog(&format!(
            "attached instance '{}' to in-app Chromium target {}",
            if instance.is_empty() {
                "agent-phoenix"
            } else {
                instance
            },
            target.target_id
        ));
        Ok(Self {
            _browser: browser,
            tab,
            owned_pid: None,
            surface_token: super::native_surface_token(instance),
            crashed,
            instance: instance.to_string(),
            headless: false,
            health: BrowserHealth {
                mode: "embedded",
                attach: Some(ws_url),
                user_data_dir: None,
                binary: None,
                login_source: String::new(),
                notes: vec!["native Chromium WebContentsView inside Phoenix".to_string()],
            },
            console,
            backend_ids: std::sync::Mutex::new(std::collections::HashMap::new()),
            last_state_hash: std::sync::atomic::AtomicU64::new(0),
        })
    }

    pub(super) fn launch(prefs: &BrowserPrefs, notes: Vec<String>, instance: &str) -> Result<Self> {
        let isolated_desktop = crate::tools::isolated_desktop::current_browser_environment()?;
        // A scoped agent desktop is a real isolation boundary. It never uses a
        // host-owned native-surface token, and Chromium is visible only inside
        // that scope's Xephyr/Xvfb display. Ordinary non-scoped work retains
        // the existing headless/native-pane behavior.
        let surface_token = isolated_desktop
            .is_none()
            .then(|| super::native_surface_token(instance))
            .flatten();
        if surface_token.is_some() {
            anyhow::ensure!(
                super::browser_surface_supported(),
                "native Chromium surface requires Linux X11 or XWayland (DISPLAY is not set)"
            );
        }
        let headless =
            isolated_desktop.is_none() && managed_launch_headless(surface_token.as_deref());
        let source = prefs.source.trim();
        let mut user_data_dir = profile_dir_for_instance(instance, source)?;

        // Donor parity: a running Chrome holds its profile lock. Instead of
        // failing, snapshot the essential auth data (cookies, logins,
        // preferences) into a Phoenix-owned copy and launch from that.
        // (SingletonLock is a symlink to a nonexistent target — check the
        // link itself, not what it points at.)
        let phoenix_browser_root = crate::config::phoenix_home().join("browser");
        let canonical_instance = instance.is_empty() || instance == "agent-phoenix";
        let managed_profile = profile_dir_for_instance(instance, "phoenix")?;
        let native_profile_allowed = user_data_dir == managed_profile
            || (canonical_instance
                && user_data_dir == phoenix_browser_root.join("chrome-profile-copy"));
        // Match the desktop's authenticated PID/profile contract exactly.
        // A mere lexical descendant of `.phoenix/browser` is not sufficient:
        // custom paths (including `..` components) must be snapshotted into
        // the one canonical copy the host is willing to embed.
        let native_external_profile = surface_token.is_some() && !native_profile_allowed;
        if native_external_profile
            || (source.eq_ignore_ascii_case("chrome")
                && std::fs::symlink_metadata(user_data_dir.join("SingletonLock")).is_ok())
        {
            // A native child window must belong to a Phoenix-owned profile:
            // the desktop verifies that exact `--user-data-dir` before it
            // reparents the XID. Snapshot external Chrome/custom sources even
            // when they are currently unlocked; cookies and saved logins come
            // with the copy, but Phoenix never embeds or later signals a
            // process that owns an arbitrary user path.
            user_data_dir = copy_profile_essentials(&user_data_dir)?;
        }
        std::fs::create_dir_all(&user_data_dir).ok();

        // Explicit binary wins (CloakBrowser/Brave/any Chromium build);
        // otherwise autodetect Chrome. A configured-but-missing binary is a
        // hard error — never silently fall back to a different browser.
        let chrome = if prefs.binary.trim().is_empty() {
            which_chrome()
        } else {
            let path = std::path::PathBuf::from(shellexpand(prefs.binary.trim()));
            if !path.is_file() {
                bail!(
                    "configured browser binary not found: {} (config [profile.browser] binary / PHOENIX_BROWSER_BINARY)",
                    path.display()
                );
            }
            Some(path)
        };
        // A newly opened browser should already be sharp in a typical desktop
        // conversation pane; the Canvas immediately follows with an exact
        // resize once its viewport is laid out.
        let window = (1600u32, 1000u32);
        let mut builder = LaunchOptionsBuilder::default();
        builder
            .headless(headless)
            .sandbox(false)
            // WebGL/3D must WORK in the managed chrome (2026-07-19: a site's
            // 3D product configurator rendered as a dead "disabled" canvas).
            // enable_gpu(true) drops the crate's --disable-gpu; the
            // ANGLE/SwiftShader args below give a reliable software GL path
            // headless on any box (newer Chrome refuses swiftshader without
            // the explicit opt-in flag).
            .enable_gpu(true)
            .window_size(Some(window))
            .idle_browser_timeout(IDLE_SOCKET_BUDGET)
            .user_data_dir(Some(user_data_dir.clone()));
        if let Some(desktop) = isolated_desktop.as_ref() {
            builder.process_envs(Some(desktop.browser_process_env()));
        }
        {
            let mut args: Vec<String> = vec![
                // The old unconditional SwiftShader path rendered every page
                // on CPU. A browser swarm could therefore pin several cores
                // all night. Chrome 146's verified hardware path is ANGLE over
                // EGL/OpenGL (Intel Mesa on this workstation).
                "--use-gl=angle".to_string(),
                "--use-angle=gl".to_string(),
            ];
            if super::restore_requested(instance) {
                args.push("--restore-last-session".to_string());
            }
            if isolated_desktop.is_some() {
                // Chromium otherwise notices the inherited Wayland session
                // before DISPLAY and opens on the user's compositor. These
                // flags pin it to the per-agent X server supplied above.
                args.push("--ozone-platform=x11".to_string());
                args.push("--disable-features=UseOzonePlatform".to_string());
            }
            if !headless {
                args.extend(native_surface_launch_args(surface_token.as_deref()));
            }
            args.extend(prefs.extra_args.clone());
            // LaunchOptions takes OsStr refs; leak the small one-time vec so
            // the borrows live for the browser's lifetime.
            let leaked: &'static [String] = Box::leak(args.into_boxed_slice());
            builder.args(leaked.iter().map(std::ffi::OsStr::new).collect());
        }
        if let Some(path) = &chrome {
            builder.path(Some(path.clone()));
        }
        let options = builder.build().map_err(|e| anyhow::anyhow!(e))?;
        // A previous Phoenix run — especially one ended by `phoenix restart`, where
        // the old gateway died without dropping its Browser — can leave a Chrome
        // still bound to this exact profile dir. It holds the profile's singleton
        // lock, so a fresh launch on the same dir hands off to the orphan and then
        // hangs forever waiting for a DevTools port that never opens (the live
        // "about:blank, stuck at 163s" wedge). Kill OUR OWN leftovers — gated to
        // Phoenix-owned profile dirs so the user's real Chrome is never touched —
        // and clear the stale singleton files before launching.
        if user_data_dir.starts_with(crate::config::phoenix_home()) {
            kill_stale_profile_holders(&user_data_dir);
            clear_singletons(&user_data_dir);
        }
        let browser = bounded_browser_launch(options).with_context(|| {
            format!(
                "failed to launch Chromium (profile dir {})",
                user_data_dir.display()
            )
        })?;
        let download_dir = download_dir_for_instance(instance)?;
        std::fs::create_dir_all(&download_dir).with_context(|| {
            format!(
                "failed to create browser download directory {}",
                download_dir.display()
            )
        })?;
        use headless_chrome::protocol::cdp::Browser as CdpBrowser;
        browser
            .call_method(CdpBrowser::SetDownloadBehavior {
                behavior: CdpBrowser::SetDownloadBehaviorBehaviorOption::Allow,
                browser_context_id: None,
                download_path: Some(download_dir.to_string_lossy().into_owned()),
                events_enabled: Some(true),
            })
            .context("failed to enable native website downloads")?;
        // Reuse the tab Chrome already opened on launch instead of opening a
        // second one. `new_tab()` here was the source of the "browser shows TWO
        // about:blank tabs" the user kept seeing — every (re)launch left chrome's
        // own initial tab PLUS our extra one. One tab per launch is cleaner and
        // less disorienting; fall back to new_tab only if the initial never came.
        let tab = super::engine::first_browser_tab(&browser)
            .context("failed to obtain the first browser tab")?;
        super::clear_restore_requested(instance);
        let owned_pid = browser.get_process_id();
        super::blog(&format!(
            "launched chrome pid {} ({}) on {}",
            owned_pid
                .map(|p| p.to_string())
                .unwrap_or_else(|| "?".into()),
            if headless { "headless" } else { "VISIBLE" },
            user_data_dir.display()
        ));
        tab.set_default_timeout(Duration::from_secs(20));
        let console = attach_console_listener(&tab);
        attach_dialog_handler(&tab);
        let crashed = attach_crash_detector(&tab);
        Ok(Self {
            _browser: browser,
            tab,
            owned_pid,
            surface_token,
            crashed,
            instance: instance.to_string(),
            headless,
            health: BrowserHealth {
                mode: "launch",
                attach: None,
                user_data_dir: Some(user_data_dir),
                binary: chrome,
                login_source: prefs.login_source.clone(),
                notes,
            },
            console,
            backend_ids: std::sync::Mutex::new(std::collections::HashMap::new()),
            last_state_hash: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Replace the working tab with a fresh one on the SAME browser, keeping the
    /// session (and its cookies/logins) alive. Used to recover from a wedged
    /// navigation whose worker thread can't be cancelled: abandon the stuck tab
    /// rather than kill chrome. The old tab is closed off-thread so cleanup can
    /// never re-wedge this call. Returns Err only if a fresh tab can't be made
    /// (transport dead) — the caller then respawns the whole browser.
    /// Make `tab` the working tab with FULL watchdog coverage and rewire the
    /// session's crash/console state onto it, returning the PREVIOUS tab. EVERY
    /// path that swaps the working tab — open-in-new-tab, switch, recover — must
    /// go through here. Skip it and the new tab runs BLIND: no dialog
    /// auto-handling (a `beforeunload` wedges it), no keepalive, and worst,
    /// `self.crashed` still points at the OLD tab, so a renderer crash on the new
    /// one is never detected — exactly how a multi-tab session silently dies.
    pub(super) fn adopt_tab(&mut self, tab: Arc<Tab>) -> Result<()> {
        anyhow::ensure!(
            !super::native_surface_attached(&self.instance),
            "native browser surface owns one mounted tab; refusing to adopt a hidden target"
        );
        drop(self.adopt_tab_inner(tab, true));
        Ok(())
    }

    /// Adopt a tab that the Chromium shell has already mounted inside Phoenix.
    /// Unlike `adopt_tab`, this is valid while the native surface is attached:
    /// the authenticated shell operation is the authority that switched the
    /// visible WebContentsView first. Re-arm all watchdogs on the new target,
    /// but keep the JPEG fallback disabled because pixels are composited
    /// natively in the right sidebar.
    pub(super) fn adopt_native_tab(&mut self, tab: Arc<Tab>) -> Result<()> {
        anyhow::ensure!(
            self.health.mode == "embedded",
            "shared tab adoption requires the Phoenix in-app browser session"
        );
        // Electron has already switched or closed the old WebContentsView.
        // Calling a final CDP screencast command on that retired target emits
        // `No session with given id` and can poison the next status action.
        drop(self.adopt_tab_inner(tab, false));
        screencast::disarm(&self.tab, &self.instance);
        Ok(())
    }

    fn adopt_tab_inner(&mut self, tab: Arc<Tab>, stop_old_screencast: bool) -> Arc<Tab> {
        let old = std::mem::replace(&mut self.tab, tab);
        // A listener remains attached to an old Tab Arc until that tab is
        // finally dropped. Explicitly stop its producer before arming the new
        // working tab; otherwise every recovery can leave another duplicate
        // screencast encoding frames in the background.
        if stop_old_screencast {
            let _ = old.stop_screencast();
        }
        let tab = Arc::clone(&self.tab);
        tab.set_default_timeout(Duration::from_secs(20));
        self.console = attach_console_listener(&tab);
        attach_dialog_handler(&tab);
        self.crashed = attach_crash_detector(&tab);
        if self.health.mode != "embedded" {
            spawn_keepalive(&tab);
        }
        teaching_observer::arm(&tab, &self.instance);
        // The canvas browser view follows the WORKING tab — the screencast
        // listener holds a Weak of one tab only, so every swap re-arms.
        screencast::arm(&tab, &self.instance);
        old
    }

    pub(super) fn recover_tab(&mut self) -> Result<()> {
        anyhow::ensure!(
            !super::native_surface_attached(&self.instance),
            "native surface recovery must relaunch its mounted Chromium window instead of adopting a hidden tab"
        );
        const RECOVERY_OPERATION_BUDGET: Duration = Duration::from_secs(10);
        const RECOVERY_TAB_BUDGET: Duration = Duration::from_secs(8);
        // A wedged target call can also stall websocket send itself before the
        // transport reaches its response timeout. Put the complete recovery
        // operation behind an outer wall-clock fence. If it expires, the
        // caller terminates the owned Chrome process; that releases this
        // detached worker and the next action launches a clean browser.
        let browser = self._browser.clone();
        let old_for_close = Arc::clone(&self.tab);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let fresh = browser.new_tab_with_timeout(RECOVERY_TAB_BUDGET);
            if fresh.is_ok() {
                let _ = browser.close_tab_with_timeout(&old_for_close, Duration::from_secs(2));
            }
            let _ = sender.send(fresh);
        });
        let fresh = receiver
            .recv_timeout(RECOVERY_OPERATION_BUDGET)
            .map_err(|_| {
                anyhow::anyhow!(
                    "fresh-tab recovery did not complete within {}s",
                    RECOVERY_OPERATION_BUDGET.as_secs()
                )
            })??;
        // Do not synchronously stop the old tab's screencast here: it uses the
        // same wedged target transport and previously added another ~120s wait
        // after the nominal 30s navigation fence. Retire all old-tab protocol
        // work on the detached cleanup thread.
        let old = self.adopt_tab_inner(fresh, false);
        drop(old);
        Ok(())
    }
}

#[cfg(test)]
mod download_tests {
    use super::*;

    #[test]
    #[ignore = "launches a real local Chromium"]
    fn website_download_click_writes_the_completed_file() {
        let Some(chrome) = super::super::engine::which_chrome() else {
            return;
        };
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let prefs = BrowserPrefs {
            source: "phoenix".into(),
            attach: String::new(),
            attach_port: 9222,
            headless: true,
            binary: chrome.display().to_string(),
            extra_args: Vec::new(),
            login_source: String::new(),
            suspend_after: None,
        };
        let instance = "agent-download-test";
        let session = Session::launch(&prefs, Vec::new(), instance).unwrap();
        session.tab.navigate_to("about:blank").unwrap();
        session
            .tab
            .evaluate(
                r#"document.body.innerHTML='<a id="save" download="phoenix-teach-download.txt">Save</a>';
                   const blob=new Blob(['phoenix-download-ok'],{type:'text/plain'});
                   document.getElementById('save').href=URL.createObjectURL(blob);"#,
                false,
            )
            .unwrap();
        session.tab.find_element("#save").unwrap().click().unwrap();
        let target = download_dir_for_instance(instance)
            .unwrap()
            .join("phoenix-teach-download.txt");
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        while !target.is_file() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "phoenix-download-ok"
        );
        session.flush_and_close();
    }
}
