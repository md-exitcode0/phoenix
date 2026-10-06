//! Reference-driven design capture on the actor's existing managed browser.
//! The DOM audit and settle scripts are unchanged TasteCode implementations.
//! Copyright 2026 TasteCode contributors, Apache-2.0; see licenses/tastecode/.
//! Phoenix adaptation: exact owned target, isolated JS world, native capture
//! transport, checked bitmap dimensions and bounded private artifact writes.

use super::*;
use headless_chrome::protocol::cdp::{Page, Runtime};
use serde_json::json;

const DOM_AUDIT: &str = include_str!("design_preview_dom.js");
const SETTLE: &str = include_str!("design_preview_settle.js");

fn preview_origin(url: &str) -> Result<url::Url> {
    let parsed = url::Url::parse(url).context("invalid design preview URL")?;
    anyhow::ensure!(parsed.scheme() == "http" && parsed.host_str() == Some("127.0.0.1")
        && parsed.port().is_some() && parsed.username().is_empty() && parsed.password().is_none(),
        "design preview must use explicit loopback HTTP without credentials");
    Ok(parsed)
}

fn validate_viewports(viewports: &[(u32, u32)]) -> Result<()> {
    anyhow::ensure!(!viewports.is_empty() && viewports.len() <= 4, "design preview requires 1–4 viewports");
    let mut seen = std::collections::HashSet::new();
    for &(width, height) in viewports {
        anyhow::ensure!((320..=3840).contains(&width) && (240..=2160).contains(&height)
            && seen.insert((width, height)), "invalid or duplicate design viewport");
    }
    Ok(())
}

fn set_viewport(tab: &Tab, width: u32, height: u32) -> Result<()> {
    tab.call_method(Emulation::SetDeviceMetricsOverride {
        width, height, device_scale_factor: 1.0, mobile: false,
        scale: None, screen_width: Some(width), screen_height: Some(height),
        position_x: None, position_y: None, dont_set_visible_size: Some(false),
        screen_orientation: None, viewport: None, display_feature: None, device_posture: None,
    })?;
    Ok(())
}

fn evaluate(tab: &Tab, context_id: Runtime::ExecutionContextId, script: &str) -> Result<Value> {
    let result = tab.call_method(Runtime::Evaluate {
        expression: script.to_string(), context_id: Some(context_id),
        return_by_value: Some(true), generate_preview: Some(false), silent: Some(false),
        await_promise: Some(true), include_command_line_api: Some(false), user_gesture: Some(false),
        object_group: None, throw_on_side_effect: None, timeout: Some(10_000.0),
        disable_breaks: Some(true), repl_mode: None, allow_unsafe_eval_blocked_by_csp: None,
        unique_context_id: None, serialization_options: None,
    })?;
    anyhow::ensure!(result.exception_details.is_none(), "design preview audit script raised an exception");
    Ok(result.result.value.unwrap_or(Value::Null))
}

fn audit_is_valid(value: &Value) -> bool {
    value.get("h1Count").and_then(Value::as_u64).is_some_and(|n| n <= 10_000)
        && value.get("interactiveTargetViolations").and_then(Value::as_array).is_some_and(|rows| {
            rows.len() <= 200 && rows.iter().all(|row| {
                row.get("selector").and_then(Value::as_str).is_some_and(|s| s.chars().count() <= 512)
                    && row.get("label").and_then(Value::as_str).is_some_and(|s| s.chars().count() <= 200)
                    && ["width", "height"].iter().all(|key| row.get(key).and_then(Value::as_f64)
                        .is_some_and(|n| n.is_finite() && n >= 0.0))
            })
        })
}

/// Caller has already admitted an execute-mode design job and owns `instance`.
/// Does not create a second browser or manipulate an attached daily browser.
pub(crate) fn capture_design_preview(instance: &str, url: &str, viewports: &[(u32, u32)], cancel: Arc<std::sync::atomic::AtomicBool>) -> Result<Vec<Value>> {
    let active = || -> Result<()> {
        anyhow::ensure!(!shutdown_requested() && !cancel.load(std::sync::atomic::Ordering::Acquire),
            "design preview capture cancelled");
        Ok(())
    };
    validate_viewports(viewports)?;
    let origin = preview_origin(url)?.origin();
    anyhow::ensure!(!instance.is_empty() && instance.len() <= 256, "design capture needs a scoped browser instance");
    active()?;
    let slot = session_slot(instance);
    let mut guard = try_lock_for(&slot, Duration::from_secs(10))
        .context("design browser is busy; no duplicate capture was started")?;
    active()?;
    let _activity = BrowserActivity::begin(instance);
    if guard.is_none() { *guard = Some(Session::open(instance)?); }
    let session = guard.as_mut().context("design browser is unavailable")?;
    anyhow::ensure!(session.health.mode == "embedded" || session.owned_pid.is_some(),
        "design preview will not navigate an externally attached browser");
    let tab = Arc::clone(&session.tab);
    let prior = tab.call_method(Page::GetLayoutMetrics(None))?.css_layout_viewport;
    let restore = (prior.client_width, prior.client_height);
    let result = (|| -> Result<Vec<Value>> {
        active()?;
        bounded_navigate(&tab, url)?;
        active()?;
        anyhow::ensure!(preview_origin(&real_url(&tab))?.origin() == origin, "design preview navigated outside its local origin");
        let frame = tab.call_method(Page::GetFrameTree(None))?.frame_tree.frame.id;
        let context_id = tab.call_method(Page::CreateIsolatedWorld {
            frame_id: frame, world_name: Some("phoenix-design-review".into()), grant_univeral_access: Some(false),
        })?.execution_context_id;
        let mut screenshots = Vec::new();
        for &(width, height) in viewports {
            active()?;
            set_viewport(&tab, width, height)?;
            session.prepare_input()?;
            active()?;
            evaluate(&tab, context_id, SETTLE)?;
            active()?;
            let metrics = evaluate(&tab, context_id,
                "({width:innerWidth,height:innerHeight,pageHeight:Math.max(document.documentElement.scrollHeight,document.body?.scrollHeight||0),url:location.href})")?;
            anyhow::ensure!(metrics["width"].as_u64() == Some(u64::from(width)) && metrics["height"].as_u64() == Some(u64::from(height)),
                "design preview did not reach its requested CSS viewport");
            anyhow::ensure!(preview_origin(metrics["url"].as_str().context("missing preview URL")?)?.origin() == origin,
                "design preview navigated outside its local origin");
            let page_height = metrics["pageHeight"].as_u64().context("invalid preview document height")?;
            let capture_height = page_height.max(u64::from(height)).min(12_000) as u32;
            let audit = evaluate(&tab, context_id, DOM_AUDIT)?;
            anyhow::ensure!(audit_is_valid(&audit), "invalid design DOM audit; no pass was inferred");
            active()?;
            let png = if session.health.mode == "embedded" {
                session::embedded_design_capture(instance, tab.get_target_id(), width, capture_height)?
            } else {
                use base64::Engine as _;
                let capture = tab.call_method(Page::CaptureScreenshot {
                    format: Some(CaptureScreenshotFormatOption::Png), quality: None,
                    clip: Some(Page::Viewport { x: 0.0, y: 0.0, width: f64::from(width), height: f64::from(capture_height), scale: 1.0 }),
                    from_surface: Some(true), capture_beyond_viewport: Some(true), optimize_for_speed: None,
                })?;
                base64::engine::general_purpose::STANDARD.decode(capture.data)?
            };
            anyhow::ensure!(png.len() >= 33 && png.len() <= 24 * 1024 * 1024 && png.starts_with(b"\x89PNG\r\n\x1a\n")
                && &png[12..16] == b"IHDR" && u32::from_be_bytes(png[16..20].try_into()?) == width
                && u32::from_be_bytes(png[20..24].try_into()?) == capture_height,
                "design screenshot has invalid or unexpected pixels");
            active()?;
            anyhow::ensure!(preview_origin(&real_url(&tab))?.origin() == origin,
                "design preview navigated away while capturing");
            let path = write_unique_browser_artifact("png", &png)?;
            screenshots.push(json!({"path":path,"width":width,"height":height,"domAudit":audit,
                "captureHeight":capture_height,"fullDocument":page_height <= 12_000}));
        }
        Ok(screenshots)
    })();
    let restored = set_viewport(&tab, restore.0 as u32, restore.1 as u32);
    match (result, restored) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error.context("design preview viewport restoration failed")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_nonlocal_previews_and_invalid_viewports_before_browser_work() {
        for url in ["https://example.com/", "http://localhost:4173/", "http://127.0.0.1/", "http://user:secret@127.0.0.1:4173/"] {
            assert!(preview_origin(url).is_err(), "{url}");
        }
        assert!(preview_origin("http://127.0.0.1:4173/path").is_ok());
        for viewports in [vec![], vec![(0, 844)], vec![(390, 844); 2], vec![(4000, 844)], vec![(390, 2300)]] {
            assert!(validate_viewports(&viewports).is_err());
        }
        assert!(validate_viewports(&[(1440, 1000), (390, 844)]).is_ok());
    }

    #[test]
    fn dom_audit_accepts_real_findings_without_turning_them_into_success() {
        assert!(audit_is_valid(&json!({"h1Count":0,"interactiveTargetViolations":[{"selector":"#small","label":"Small","width":12,"height":9}]})));
        for value in [json!({}), json!({"h1Count":1,"interactiveTargetViolations":"clean"}), json!({"h1Count":1,"interactiveTargetViolations":[{"selector":"#x","label":"x","width":-1,"height":40}]})] {
            assert!(!audit_is_valid(&value));
        }
    }

    /// Explicit opt-in integration driven by a credential-free private browser
    /// fixture. A normal unit-test run never opens a browser or user's profile.
    #[test]
    #[ignore = "requires the owned local design-capture fixture"]
    fn native_design_capture_preserves_viewports_and_real_dom_failures() {
        let url = std::env::var("PHOENIX_DESIGN_CAPTURE_TEST_URL").expect("fixture URL required");
        let destination = std::env::var("PHOENIX_DESIGN_CAPTURE_TEST_OUTPUT").expect("fixture output required");
        let home = crate::config::phoenix_home();
        assert!(home.join("design-capture-fixture.marker").is_file(), "private fixture marker required");
        assert_eq!(std::env::var("PHOENIX_BROWSER_LOGIN_SOURCE").unwrap(), "none");
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let screens = capture_design_preview("agent-iris-capture-fixture", &url,
            &[(1440, 1000), (390, 844)], Arc::clone(&cancel)).expect("native design capture");
        assert_eq!(screens.len(), 2);
        for screen in &screens {
            assert_eq!(screen["domAudit"]["h1Count"], 1);
            assert!(screen["domAudit"]["interactiveTargetViolations"].as_array().unwrap()
                .iter().any(|row| row["selector"] == "#small"));
            assert!(screen["captureHeight"].as_u64().unwrap() >= 2400);
            assert_eq!(screen["fullDocument"], true);
        }
        cancel.store(true, std::sync::atomic::Ordering::Release);
        assert!(capture_design_preview("agent-iris-capture-fixture", &url, &[(390, 844)], cancel).is_err());
        std::fs::write(destination, serde_json::to_vec_pretty(&json!({"passed":true,"screenshots":screens,
            "scope":"Actual private Chromium capture and DOM audit. No model task or user browser."})).unwrap()).unwrap();
    }
}
