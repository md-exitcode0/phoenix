//! The action interpreter: every `browser_*` verb, the DOM indexer and
//! page-state serializer.

use super::*;

fn is_embedded_session(session: &Session) -> bool {
    session.health.mode == "embedded"
}

fn embedded_target_id(session: &Session, requested: &str) -> Result<String> {
    session._browser.register_missing_tabs();
    session
        ._browser
        .get_tabs()
        .lock()
        .map_err(|_| anyhow::anyhow!("the embedded browser tab list is unavailable"))?
        .iter()
        .find(|tab| tab.get_target_id().contains(requested))
        .map(|tab| tab.get_target_id().to_string())
        .context("no shared sidebar tab matches that id")
}

fn open_embedded_tab(session: &mut Session, url: &str) -> Result<()> {
    let state = super::session::embedded_browser_surface_action(
        &session.instance,
        "new_tab",
        None,
        Some(url),
    )?
    .context("the Phoenix Chromium shell is unavailable")?;
    let target_id = state
        .target_id
        .filter(|value| !value.is_empty())
        .context("the new shared sidebar tab has no target id")?;
    match super::embedded_tab_by_target(session, &target_id) {
        Ok(next) => session.adopt_native_tab(next)?,
        Err(discovery_error) => {
            // Electron already opened and navigated the visible tab. A newly
            // created WebContentsView can be absent from an existing Browser
            // connection's Target registry even though a fresh connection
            // sees it immediately. Reattach to the shell's authoritative
            // active target instead of failing after the user can see the tab
            // or leaving the old CDP call wedged behind the session lock.
            let instance = session.instance.clone();
            let replacement = Session::open(&instance).with_context(|| {
                format!("the new sidebar tab opened, but browser control could not reattach after {discovery_error}")
            })?;
            *session = replacement;
        }
    }
    settle_page(&session.tab);
    Ok(())
}

fn switch_embedded_tab(session: &mut Session, requested: &str) -> Result<String> {
    let target_id = embedded_target_id(session, requested)?;
    let state = super::session::embedded_browser_surface_action(
        &session.instance,
        "switch_tab",
        Some(&target_id),
        None,
    )?
    .context("the Phoenix Chromium shell is unavailable")?;
    let active_id = state
        .target_id
        .filter(|value| !value.is_empty())
        .unwrap_or(target_id);
    let next = super::embedded_tab_by_target(session, &active_id).map_err(anyhow::Error::msg)?;
    session.adopt_native_tab(next)?;
    Ok(active_id)
}

fn close_embedded_tab(session: &mut Session, requested: &str) -> Result<String> {
    let target_id = embedded_target_id(session, requested)?;
    let state = super::session::embedded_browser_surface_action(
        &session.instance,
        "close_tab",
        Some(&target_id),
        None,
    )?
    .context("the Phoenix Chromium shell is unavailable")?;
    let active = state
        .tabs
        .iter()
        .find(|tab| tab.active)
        .context("the shared browser did not retain an active tab")?;
    let next = super::embedded_tab_by_target(session, &active.id).map_err(anyhow::Error::msg)?;
    session.adopt_native_tab(next)?;
    Ok(target_id)
}

pub(super) fn run_action(
    session: &mut Session,
    action: &str,
    input: &Value,
) -> Result<(String, bool)> {
    // Prepare before dispatch, including each independently executed batch
    // step. Never retry an acknowledged gesture to compensate for a cold view.
    if matches!(action, "click" | "input" | "send_keys" | "scroll" | "select_dropdown" | "upload_file") {
        session.prepare_input()?;
    }
    let tab = session.tab.clone();
    match action {
        // Multi-action batch (donor: browser-use `multi_act`, its single biggest
        // speed lever — up to 5 actions per MODEL ROUND instead of one). Two
        // stale-DOM guards, ported 1:1: (1) navigating actions terminate the
        // sequence; (2) a URL change after any action aborts the remainder.
        // Page state attaches ONCE at the end (the per-action attach is what
        // made batching pointless before: N actions = N × 12KB of state).
        "act" => {
            let Some(steps) = input.get("actions").and_then(Value::as_array) else {
                bail!("browser_act needs `actions`: a JSON array of {{action, …params}} objects");
            };
            if steps.is_empty() {
                bail!("browser_act needs a non-empty `actions` list");
            }
            if steps.len() > 8 {
                bail!(
                    "browser_act batch too long ({}); cap is 8 — verify state between large batches",
                    steps.len()
                );
            }
            // Sub-actions a batch may carry. `act` itself, session control
            // (switch/close), and the sidecar-model reads (extract) stay solo.
            const BATCHABLE: [&str; 13] = [
                "navigate",
                "search",
                "go_back",
                "wait",
                "click",
                "input",
                "send_keys",
                "scroll",
                "find_text",
                "search_page",
                "dropdown_options",
                "select_dropdown",
                "upload_file",
            ];
            // Actions that load a new document — anything queued after them
            // would run against a page that no longer exists.
            const TERMINATING: [&str; 3] = ["navigate", "search", "go_back"];
            let mut log: Vec<String> = Vec::new();
            let total = steps.len();
            for (i, step) in steps.iter().enumerate() {
                let name = step.get("action").and_then(Value::as_str).unwrap_or("");
                if !BATCHABLE.contains(&name) {
                    bail!(
                        "browser_act step #{} has action {name:?}; batchable actions: {BATCHABLE:?}",
                        i + 1
                    );
                }
                let pre_url = real_url(&tab);
                match run_action(session, name, step) {
                    Ok((message, _)) => log.push(format!("{}. {name}: {message}", i + 1)),
                    Err(error) => {
                        log.push(format!("{}. {name}: FAILED — {error:#}", i + 1));
                        bail!(
                            "browser_act stopped at step #{} of {total}.
{}
The attached state is the page as it is NOW — continue from it; do not blind-retry the whole batch.",
                            i + 1,
                            log.join("
")
                        );
                    }
                }
                let last = i + 1 == total;
                if !last {
                    if TERMINATING.contains(&name) {
                        log.push(format!(
                            "-- {name} loads a new document; {} remaining action(s) skipped (queue them against the fresh state)",
                            total - i - 1
                        ));
                        break;
                    }
                    let post_url = real_url(&session.tab);
                    if post_url != pre_url {
                        log.push(format!(
                            "-- the page navigated to {post_url}; {} remaining action(s) skipped (stale-DOM guard)",
                            total - i - 1
                        ));
                        break;
                    }
                    // browser-use wait_between_actions — but only where the step
                    // handler didn't already wait. click/send_keys/scroll/input
                    // settle or sleep internally, and the read-only steps
                    // (find_text/search_page/dropdown_options) change nothing;
                    // stacking a blind 300ms on top of each was the batch's
                    // biggest hidden latency tax. select_dropdown is the one
                    // batchable step that fires change events and returns
                    // without settling — give the page a beat after it.
                    if name == "select_dropdown" {
                        std::thread::sleep(Duration::from_millis(300));
                    }
                }
            }
            Ok((
                format!(
                    "Ran batch:
{}",
                    log.join(
                        "
"
                    )
                ),
                true,
            ))
        }
        "navigate" => {
            let url = normalize_url(str_arg(input, "url")?);
            let new_tab = input
                .get("new_tab")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if new_tab && is_embedded_session(session) {
                open_embedded_tab(session, &url)?;
                Ok((format!("Opened {url} in a new shared sidebar tab."), true))
            } else if new_tab {
                let t = session._browser.new_tab()?;
                t.set_default_timeout(Duration::from_secs(20));
                bounded_navigate(&t, &url)?;
                settle_page(&t);
                session.adopt_tab(t)?;
                Ok((format!("Opened {url} in a new tab."), true))
            } else {
                bounded_navigate(&tab, &url)?;
                settle_page(&tab);
                Ok((format!("Navigated to {url}"), true))
            }
        }
        "search" => {
            let query = str_arg(input, "query")?;
            let url = format!("https://duckduckgo.com/?q={}", urlencode(&query));
            bounded_navigate(&tab, &url)?;
            settle_page(&tab);
            Ok((format!("Searched for: {query}"), true))
        }
        "go_back" => {
            tab.evaluate("history.back()", false)?;
            settle_page(&tab);
            Ok(("Went back one page.".into(), true))
        }
        "wait" => {
            let secs = input
                .get("seconds")
                .and_then(Value::as_u64)
                .unwrap_or(3)
                .min(10);
            sleep_secs(secs);
            Ok((format!("Waited {secs}s."), true))
        }
        "click" => {
            // Donor fallback: click by viewport coordinates when no index fits
            // (canvas, unindexed regions). Index remains the primary path.
            if input.get("index").is_none() {
                let x = input.get("x").and_then(Value::as_f64);
                let y = input.get("y").and_then(Value::as_f64);
                if let (Some(x), Some(y)) = (x, y) {
                    tab.click_point(Point { x, y })?;
                    settle_page(&tab);
                    return Ok((format!("Clicked at ({x:.0}, {y:.0})."), true));
                }
            }
            let index = int_arg(input, "index")
                .context("click needs an element index, or x+y coordinates as fallback")?;
            if input
                .get("new_tab")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                let href = eval_string(
                    &tab,
                    &format!(
                        "(function(){{var e=(window.__phx||[])[{index}];\
                         if(!e)return 'NOT_FOUND';if(!e.isConnected)return 'STALE';\
                         var a=e.closest('a[href]');\
                         return a?a.href:'NO_HREF';}})()"
                    ),
                )?;
                match href.as_str() {
                    "NOT_FOUND" => bail!("No element with index [{index}] in the current state. Get fresh browser_state — indexes change every navigation."),
                    "STALE" => bail!("Element [{index}] is STALE — the page re-rendered since that browser_state (its DOM node is detached). Get fresh browser_state and act on the new indexes."),

                    "NO_HREF" => bail!("[{index}] is not a link; new_tab only works on links."),
                    url => {
                        if is_embedded_session(session) {
                            open_embedded_tab(session, url)?;
                            return Ok((
                                format!("Opened [{index}] in a new shared sidebar tab."),
                                true,
                            ));
                        }
                        let t = session._browser.new_tab()?;
                        t.set_default_timeout(Duration::from_secs(20));
                        bounded_navigate(&t, url)?;
                        settle_page(&t);
                        session.adopt_tab(t)?;
                        return Ok((format!("Opened [{index}] in a new tab."), true));
                    }
                }
            }
            // Never send a coordinate click from a detached backend node. CDP
            // can retain its last content quad after a framework remount; a
            // click at that stale point may hit an unrelated replacement. The
            // JS registry is authoritative for liveness. A detached node goes
            // through the exact, unique semantic relocation path below.
            let indexed_status = eval_string(
                &tab,
                &format!(
                    "(function(){{var e=(window.__phx||[])[{index}];return !e?'NOT_FOUND':e.isConnected?'LIVE':'STALE';}})()"
                ),
            )?;
            if indexed_status == "NOT_FOUND" {
                bail!("No element with index [{index}] in the current state. Get fresh browser_state — indexes change every navigation.");
            }
            if indexed_status == "STALE" {
                if recover_stale_click(&tab, index as u64)? {
                    settle_page(&tab);
                    return Ok((
                        format!("Clicked [{index}] after safely relocating its unique remounted element."),
                        true,
                    ));
                }
                bail!("Element [{index}] is STALE — the page re-rendered and its identity is no longer unique. Get fresh browser_state and act on the new indexes.");
            }
            // Stable-id path (2026-07-02): act by Chrome's stable backendNodeId
            // so the [index] survives in-place re-renders that today's JS
            // `window.__phx[index]` ref would bail STALE on. Any failure (no
            // captured id, node remounted, CDP hiccup) falls through to the JS
            // path below, which preserves today's NOT_FOUND/STALE behavior — so
            // this is a strict upgrade, never a regression.
            if let Some(bid) = resolve_backend_id(session, index as u64) {
                if click_by_backend_id(&tab, bid).is_ok() {
                    settle_page(&tab);
                    return Ok((format!("Clicked [{index}]."), true));
                }
            }
            let result = eval_string(
                &tab,
                &format!(
                    "(function(){{var e=(window.__phx||[])[{index}];\
                     if(!e)return 'NOT_FOUND';if(!e.isConnected)return 'STALE';\
                     e.scrollIntoView({{block:'center',inline:'center'}});\
                     try{{e.focus();}}catch(_){{}}\
                     e.click();return 'OK';}})()"
                ),
            )?;
            if result == "NOT_FOUND" {
                bail!("No element with index [{index}] in the current state. Get fresh browser_state — indexes change every navigation.");
            }
            if result == "STALE" {
                bail!("Element [{index}] is STALE — the page re-rendered since that browser_state (its DOM node is detached). Get fresh browser_state and act on the new indexes.");
            }
            // browser-use doctrine: a click must NEVER block on a full navigation
            // lifecycle. Most SPA clicks (x.com, etc.) never fire one, so the old
            // `wait_until_navigated()` stalled for the entire 20s default timeout —
            // the live "clicked, then it froze and looked dead" the user reported.
            // Use the bounded networkIdle approximation instead: it early-exits in
            // ~0.5s on a page that didn't change and still waits out a real
            // navigation, hard-capped so a click can never wedge the session.
            settle_page(&tab);
            Ok((format!("Clicked [{index}]."), true))
        }
        "input" => {
            let index = int_arg(input, "index")?;
            let text = str_arg(input, "text")?;
            let clear = input.get("clear").and_then(Value::as_bool).unwrap_or(true);
            // Typing is browser-use's method, ported 1:1, NOT a per-site hack:
            // focus the real editable surface, then type with REAL per-character key
            // events. DOM writes (`e.value=`, execCommand, textContent) don't drive
            // React inputs or Draft.js/Lexical/ProseMirror editors (X, Reddit, Slack,
            // Notion) — their model is internal, so a DOM write leaves orphaned,
            // undeletable text with the Post/Send button stuck disabled. browser-use
            // types char-by-char with keyDown→char→keyUp because that's "exactly how
            // a human types, which modern websites expect"; that fires keydown/
            // keypress/beforeinput/input so EVERY editor registers it. So:
            // 1) focus the actual editable surface and set the selection (select-all
            //    to clear, caret-to-end to append) via JS; 2) tab.type_str() — the
            //    crate's port of that same real-key-event typing.
            let focus = eval_string(
                &tab,
                &FOCUS_FIELD_JS
                    .replace("__INDEX__", &index.to_string())
                    .replace("__CLEAR__", if clear { "true" } else { "false" })
                    .replace("__SECRET__", "false"),
            )?;
            match focus.as_str() {
                "NOT_FOUND" => bail!("No element with index [{index}] in the current state. Get fresh browser_state — indexes change every navigation."),
                "STALE" => bail!("Element [{index}] is STALE — the page re-rendered since that browser_state (its DOM node is detached). Get fresh browser_state and act on the new indexes."),

                "NOT_EDITABLE" => bail!("[{index}] has no editable text surface to type into. Pick the actual input/compose field from browser_state, or use computer-use."),
                _ => {}
            }
            // Real per-character key events (keyDown→char→keyUp with the char's text),
            // the crate's port of Puppeteer/browser-use human typing. The first
            // keystroke replaces the selection set above when clearing. Falls back to
            // Input.insertText only for chars with no key definition (e.g. emoji).
            type_at_human_pace(&tab, &text).map_err(|e| {
                anyhow::anyhow!("typing into [{index}] failed at the input layer: {e}")
            })?;
            // A beat for the editor's model→DOM render; a full second here was
            // pure latency tax on every keystroke-bearing round.
            std::thread::sleep(Duration::from_millis(300));
            // Verify against the editor's RENDERED state — valid now because real
            // input updates the model first, then the model re-renders to the DOM.
            let landed = eval_string(&tab, &VERIFY_FIELD_JS.replace("__VAL__", &js_string(&text)))?;
            if landed == "MISSING" {
                return Ok((
                    format!(
                        "Typed into [{index}], but the field still doesn't show the text — it may use a non-standard input layer. Try computer-use to type into it directly."
                    ),
                    true,
                ));
            }
            Ok((format!("Typed into [{index}]."), true))
        }
        "send_keys" => {
            let keys = str_arg(input, "keys")?;
            press_keys(&tab, &keys)?;
            // Keys often submit/navigate — settle (early-exits in ~0.5s on a
            // page that didn't change) instead of a blind 1s sleep.
            settle_page(&tab);
            Ok((format!("Sent keys: {keys}"), true))
        }
        "scroll" => {
            let down = input.get("down").and_then(Value::as_bool).unwrap_or(true);
            let pages = input.get("pages").and_then(Value::as_f64).unwrap_or(1.0);
            let dir = if down { 1.0 } else { -1.0 };
            // Verify the scroll MOVED something. `window.scrollBy` (and a bare
            // element scroll) silently no-op on modern SPA feeds (x.com, infinite
            // lists) where the scrollable region is an INNER container, not the
            // window — the "scroll did nothing / scroll fails" the user hit. An
            // explicit index scrolls that container; the windowless path hunts for
            // the element that truly scrolls (see SMART_SCROLL_JS).
            let outcome = match input.get("index").and_then(Value::as_i64) {
                Some(idx) => eval_string(
                    &tab,
                    &format!(
                        "(function(){{var e=(window.__phx||[])[{idx}];\
                         if(!e)return 'NOT_FOUND';if(!e.isConnected)return 'STALE';\
                         var b=e.scrollTop;\
                         e.scrollTop=b+{dir}*e.clientHeight*{pages};\
                         return e.scrollTop!==b?'OK':'NOSCROLL';}})()"
                    ),
                )?,
                None => eval_string(
                    &tab,
                    &SMART_SCROLL_JS
                        .replace("__DIR__", &dir.to_string())
                        .replace("__PAGES__", &pages.to_string()),
                )?,
            };
            if outcome == "NOT_FOUND" {
                bail!("No element with that index to scroll. Get fresh browser_state.");
            }
            if outcome == "STALE" {
                bail!("That element is STALE — the page re-rendered since that browser_state. Get fresh browser_state and scroll by the new index.");
            }
            std::thread::sleep(Duration::from_millis(300));
            if outcome == "NOSCROLL" {
                Ok((
                    format!(
                        "Nothing scrolled — already at the {} here, or no scrollable region at that spot. Re-read browser_state; the page may not have moved.",
                        if down { "bottom" } else { "top" }
                    ),
                    true,
                ))
            } else {
                Ok((
                    format!("Scrolled {}.", if down { "down" } else { "up" }),
                    true,
                ))
            }
        }
        "find_text" => {
            let text = str_arg(input, "text")?;
            let found = eval_string(
                &tab,
                &format!(
                    "(function(){{var w=document.createTreeWalker(document.body,NodeFilter.SHOW_TEXT);\
                     var n;while(n=w.nextNode()){{if(n.nodeValue&&n.nodeValue.indexOf({t})>=0){{\
                     n.parentElement.scrollIntoView({{block:'center'}});return 'OK';}}}}return 'NOT_FOUND';}})()",
                    t = js_string(&text)
                ),
            )?;
            if found == "NOT_FOUND" {
                bail!("Text not found on the page: {text}");
            }
            Ok((format!("Scrolled to: {text}"), true))
        }
        "search_page" => {
            let pattern = str_arg(input, "pattern")?;
            let out = eval_string(
                &tab,
                &format!(
                    "(function(){{try{{var re=new RegExp({p},'gi');var t=document.body.innerText||'';\
                     var m=t.match(re)||[];var u=[...new Set(m)].slice(0,30);\
                     return u.length? ('Matches ('+u.length+'):\\n'+u.join('\\n')) : 'NO_MATCH';}}catch(e){{return 'ERR:'+e;}}}})()",
                    p = js_string(&pattern)
                ),
            )?;
            if out == "NO_MATCH" {
                Ok((format!("No matches for /{pattern}/ on this page."), false))
            } else {
                Ok((out, false))
            }
        }
        "find_elements" => {
            let selector = str_arg(input, "selector")?;
            let limit = input.get("limit").and_then(Value::as_u64).unwrap_or(20);
            let out = eval_string(
                &tab,
                &format!(
                    "(function(){{try{{var els=[...document.querySelectorAll({s})].slice(0,{limit});\
                     return JSON.stringify(els.map(function(e){{return{{tag:e.tagName.toLowerCase(),\
                     text:(e.innerText||'').trim().slice(0,80),href:e.getAttribute('href')||undefined}};}}));}}\
                     catch(e){{return 'ERR:'+e;}}}})()",
                    s = js_string(&selector)
                ),
            )?;
            Ok((format!("{selector} =>\n{out}"), false))
        }
        "upload_file" => {
            let index = int_arg(input, "index")?;
            let path = shellexpand(&str_arg(input, "path")?);
            if !std::path::Path::new(&path).is_file() {
                bail!("file does not exist: {path}");
            }
            let element = tab
                .find_element(&format!("[data-phx-idx=\"{index}\"]"))
                .map_err(|_| {
                    anyhow::anyhow!(
                    "No element with index [{index}] in the current state. Get fresh browser_state."
                )
                })?;
            element.set_input_files(&[path.as_str()]).with_context(|| {
                format!("[{index}] did not accept a file — it must be an <input type=\"file\">")
            })?;
            sleep_secs(1);
            Ok((format!("Attached {path} to [{index}]."), true))
        }
        "extract" => {
            let links = input
                .get("extract_links")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let links_js = if links {
                "var a=[].slice.call(document.querySelectorAll('a[href]'),0,120)\
                 .map(function(x){return (x.innerText||'').trim().slice(0,60)+' -> '+x.href;});\
                 t+='\\n\\nLINKS:\\n'+a.join('\\n');"
            } else {
                ""
            };
            let body = eval_string(
                &tab,
                &format!(
                    "(function(){{var t=document.body?(document.body.innerText||''):'';\
                     document.querySelectorAll('iframe,frame').forEach(function(f){{\
                     try{{var d=f.contentDocument;if(d&&d.body){{t+='\\n[iframe '+(f.src||'')+']\\n'+(d.body.innerText||'');}}}}catch(_){{}}}});\
                     {links_js}return t;}})()"
                ),
            )?;
            // With a `query`, the runtime structures the full text through the
            // sidecar model (mesh hook) — give it the whole page. Without one
            // this is a bounded raw dump.
            let cap = if input.get("query").and_then(Value::as_str).is_some() {
                60_000
            } else {
                MAX_TEXT_CHARS
            };
            Ok((truncate(&body, cap), false))
        }
        "screenshot" => {
            let full = input
                .get("full_page")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let png = if is_embedded_session(session) {
                super::session::embedded_browser_capture(&session.instance, tab.get_target_id(), full)?
            } else {
                tab.capture_screenshot(CaptureScreenshotFormatOption::Png, None, None, !full)?
            };
            let path = write_unique_browser_artifact("png", &png)?;
            Ok((format!("Screenshot saved: {}", path.display()), false))
        }
        "save_as_pdf" => {
            let pdf = tab.print_to_pdf(None)?;
            let path = write_unique_browser_artifact("pdf", &pdf)?;
            Ok((format!("PDF saved: {}", path.display()), false))
        }
        // The ONLY reliable download path in a CDP-driven headless chrome.
        // Native downloads (`a.click()`, blob anchors, UI "Download" buttons)
        // are silently dropped — there is no browser UI to receive them; a
        // coworker burned 40 minutes rediscovering this on 2026-07-16. An in-page fetch
        // INHERITS the page's cookies/session (signed CDN URLs, logged-in
        // backends), comes back as base64, and Rust writes the real bytes.
        "download" => {
            let url = str_arg(input, "url")?;
            let js = format!(
                r#"(async function(){{
                    try {{
                        var r = await fetch({url}, {{credentials:'include'}});
                        if (!r.ok) return 'PHXERR HTTP ' + r.status + ' ' + r.statusText;
                        var b = await r.blob();
                        var fr = new FileReader();
                        var done = new Promise(function(res, rej){{
                            fr.onload = function(){{ res(fr.result); }};
                            fr.onerror = function(){{ rej(fr.error); }};
                        }});
                        fr.readAsDataURL(b);
                        return await done;
                    }} catch(e) {{ return 'PHXERR ' + String(e); }}
                }})()"#,
                url = js_string(&url)
            );
            let obj = tab.evaluate(&js, true)?;
            let data = match obj.value {
                Some(Value::String(s)) => s,
                other => bail!("download returned no data from the page ({other:?})"),
            };
            if let Some(err) = data.strip_prefix("PHXERR ") {
                bail!(
                    "download failed in page context: {err}. The fetch ran WITH this page's \
                     cookies — if it still failed, the URL may be expired (grab a fresh signed \
                     URL from the page) or blocked by CORS (navigate the tab to the file URL \
                     itself, then download from there)."
                );
            }
            let (mime, bytes) = decode_download_data_url(&data)?;
            validate_download_magic(&mime, &bytes)?;
            let target = match input.get("path").and_then(Value::as_str) {
                Some(p) if std::path::Path::new(p).is_absolute() => {
                    let path = std::path::PathBuf::from(p);
                    atomic_write_artifact_output(&path, &bytes)?;
                    path
                }
                Some(p) => bail!(
                    "`path` must be ABSOLUTE (got `{p}`) — the browser lane has no workspace \
                     anchor, a relative path would land somewhere surprising."
                ),
                None => {
                    let ext = match mime.as_str() {
                        "image/png" => "png",
                        "image/jpeg" => "jpg",
                        "image/webp" => "webp",
                        "video/mp4" => "mp4",
                        "application/pdf" => "pdf",
                        _ => "bin",
                    };
                    write_unique_browser_artifact(ext, &bytes)?
                }
            };
            let magic: String = bytes.iter().take(4).map(|b| format!("{b:02x}")).collect();
            Ok((
                format!(
                    "Downloaded {} bytes ({mime}) → {} (magic {magic})",
                    bytes.len(),
                    target.display()
                ),
                false,
            ))
        }
        "dropdown_options" => {
            let index = int_arg(input, "index")?;
            let mut out = eval_string(
                &tab,
                &format!(
                    "(function(){{var e=(window.__phx||[])[{index}];\
                     if(e&&!e.isConnected)return 'STALE';\
                     if(!e)return 'NOT_FOUND';\
                     if(e.tagName!=='SELECT')return 'CUSTOM';\
                     return JSON.stringify([...e.options].map(function(o){{return o.text;}}));}})()"
                ),
            )?;
            if out == "STALE" {
                bail!("Element [{index}] is STALE — the page re-rendered since that browser_state (its DOM node is detached). Get fresh browser_state and act on the new indexes.");
            }
            if out == "NOT_FOUND" {
                bail!("No element with index [{index}] in the current state. Get fresh browser_state — indexes change every navigation.");
            }
            if out == "CUSTOM" {
                // Modern apps usually render a button/combobox plus a portal
                // listbox instead of a native <select>. Opening that reversible
                // popup here is the useful equivalent of asking a native select
                // for its options; treating it as a hard type error caused the
                // historical dropdown retry loop.
                let opened = eval_string(
                    &tab,
                    &format!(
                        "(function(){{var e=(window.__phx||[])[{index}];\
                         if(!e)return 'NOT_FOUND';if(!e.isConnected)return 'STALE';\
                         if(e.getAttribute('aria-expanded')!=='true')e.click();return 'OPEN';}})()"
                    ),
                )?;
                if opened == "STALE" {
                    bail!("Element [{index}] is STALE — get fresh browser_state and use the new index.");
                }
                if opened == "NOT_FOUND" {
                    bail!("No element with index [{index}] in the current state. Get fresh browser_state.");
                }
                std::thread::sleep(Duration::from_millis(180));
                out = eval_string(
                    &tab,
                    &format!(
                        "(function(){{var e=(window.__phx||[])[{index}];if(!e||!e.isConnected)return 'STALE';\
                         var id=e.getAttribute('aria-controls')||e.getAttribute('aria-owns');\
                         var root=(id&&document.getElementById(id))||document;\
                         var visible=function(o){{var r=o.getBoundingClientRect(),s=getComputedStyle(o);return r.width>0&&r.height>0&&s.visibility!=='hidden'&&s.display!=='none';}};\
                         var nodes=[...root.querySelectorAll('[role=option],[role=menuitemradio],[role=menuitem]')].filter(visible);\
                         if(!nodes.length)nodes=[...document.querySelectorAll('[role=option],[role=menuitemradio],[role=menuitem]')].filter(visible);\
                         var labels=nodes.map(function(o){{return (o.innerText||o.textContent||'').trim();}}).filter(Boolean);\
                         return labels.length?JSON.stringify([...new Set(labels)]):'CUSTOM_NO_OPTIONS';}})()"
                    ),
                )?;
                if out == "CUSTOM_NO_OPTIONS" {
                    return Ok((
                        format!(
                            "[{index}] is a custom dropdown. Phoenix opened it, but the page exposes no ARIA option list. Read the fresh attached state and click the visible option directly; do not retry dropdown_options on the closed control."
                        ),
                        true,
                    ));
                }
                if out == "STALE" {
                    bail!("The custom dropdown re-rendered while opening. Use the fresh attached browser state and click the visible option directly.");
                }
                return Ok((
                    format!("Options for custom dropdown [{index}]:\n{out}"),
                    true,
                ));
            }
            Ok((format!("Options for [{index}]:\n{out}"), false))
        }
        "select_dropdown" => {
            let index = int_arg(input, "index")?;
            let text = str_arg(input, "text")?;
            // Native setter + selectedIndex + input/change, same reasoning as the
            // input fix: a bare `e.value=` is reverted on a React-controlled select.
            let out = eval_string(
                &tab,
                &format!(
                    "(function(){{var e=(window.__phx||[])[{index}];\
                     if(e&&!e.isConnected)return 'STALE';\
                     if(!e)return 'NOT_FOUND';\
                     if(e.tagName!=='SELECT')return 'CUSTOM';\
                     var i=[...e.options].findIndex(function(x){{return x.text.trim()==={t};}});\
                     if(i<0)return 'NO_OPTION';\
                     var setter=Object.getOwnPropertyDescriptor(window.HTMLSelectElement.prototype,'value').set;\
                     setter.call(e,e.options[i].value);e.selectedIndex=i;\
                     e.dispatchEvent(new Event('input',{{bubbles:true}}));\
                     e.dispatchEvent(new Event('change',{{bubbles:true}}));return 'OK';}})()",
                    t = js_string(text.trim())
                ),
            )?;
            match out.as_str() {
                "STALE" => bail!("Element [{index}] is STALE — the page re-rendered since that browser_state (its DOM node is detached). Get fresh browser_state and act on the new indexes."),
                "NOT_FOUND" => bail!("No element with index [{index}] in the current state. Get fresh browser_state — indexes change every navigation."),
                "CUSTOM" => {
                    let opened = eval_string(
                        &tab,
                        &format!(
                            "(function(){{var e=(window.__phx||[])[{index}];if(!e)return 'NOT_FOUND';if(!e.isConnected)return 'STALE';if(e.getAttribute('aria-expanded')!=='true')e.click();return 'OPEN';}})()"
                        ),
                    )?;
                    if opened != "OPEN" {
                        bail!("Custom dropdown [{index}] could not be opened ({opened}). Get fresh browser_state and click its replacement control.");
                    }
                    std::thread::sleep(Duration::from_millis(180));
                    let selected = eval_string(
                        &tab,
                        &format!(
                            "(function(){{var e=(window.__phx||[])[{index}];if(!e||!e.isConnected)return 'STALE';\
                             var id=e.getAttribute('aria-controls')||e.getAttribute('aria-owns');\
                             var root=(id&&document.getElementById(id))||document;\
                             var visible=function(o){{var r=o.getBoundingClientRect(),s=getComputedStyle(o);return r.width>0&&r.height>0&&s.visibility!=='hidden'&&s.display!=='none';}};\
                             var nodes=[...root.querySelectorAll('[role=option],[role=menuitemradio],[role=menuitem]')].filter(visible);\
                             if(!nodes.length)nodes=[...document.querySelectorAll('[role=option],[role=menuitemradio],[role=menuitem]')].filter(visible);\
                             var wanted={t};var o=nodes.find(function(n){{return (n.innerText||n.textContent||'').trim()===wanted;}});\
                             if(!o)return 'NO_CUSTOM_OPTION';o.click();return 'OK_CUSTOM';}})()",
                            t = js_string(text.trim())
                        ),
                    )?;
                    match selected.as_str() {
                        "OK_CUSTOM" => {
                            settle_page(&tab);
                            Ok((format!("Selected '{text}' in custom dropdown [{index}]."), true))
                        }
                        "STALE" => bail!("The custom dropdown re-rendered while opening. Read fresh browser_state and click the visible '{text}' option directly."),
                        _ => bail!("Custom dropdown [{index}] opened, but no visible option has exact text '{text}'. Read the fresh attached state and use the visible option's index instead of retrying this call."),
                    }
                },
                "NO_OPTION" => bail!("No option with exact text '{text}' in dropdown [{index}]."),
                _ => Ok((format!("Selected '{text}' in [{index}]."), true)),
            }
        }
        "switch" => {
            let tab_id = str_arg(input, "tab_id")?;
            if is_embedded_session(session) {
                let target_id = switch_embedded_tab(session, &tab_id)?;
                return Ok((format!("Switched to shared sidebar tab {target_id}."), true));
            }
            let tabs = session._browser.get_tabs().lock().map_err(|_| {
                anyhow::anyhow!("the browser tab list is unreadable; reopen the browser")
            })?;
            let target = tabs
                .iter()
                .find(|t| t.get_target_id().contains(&tab_id))
                .cloned();
            drop(tabs);
            match target {
                Some(t) => {
                    t.bring_to_front()?;
                    session.adopt_tab(t)?;
                    Ok((format!("Switched to tab {tab_id}."), true))
                }
                None => bail!("No open tab matches id '{tab_id}'."),
            }
        }
        "close" => {
            let tab_id = str_arg(input, "tab_id")?;
            if is_embedded_session(session) {
                let target_id = close_embedded_tab(session, &tab_id)?;
                return Ok((format!("Closed shared sidebar tab {target_id}."), true));
            }
            let tabs = session._browser.get_tabs().lock().map_err(|_| {
                anyhow::anyhow!("the browser tab list is unreadable; reopen the browser")
            })?;
            if tabs.len() <= 1 {
                drop(tabs);
                bail!("Refusing to close the last tab.");
            }
            let target = tabs
                .iter()
                .find(|t| t.get_target_id().contains(&tab_id))
                .cloned();
            drop(tabs);
            match target {
                Some(t) => {
                    t.close(false)?;
                    Ok((format!("Closed tab {tab_id}."), true))
                }
                None => bail!("No open tab matches id '{tab_id}'."),
            }
        }
        "evaluate" => {
            let code = str_arg(input, "code")?;
            let out = eval_string(
                &tab,
                &format!("JSON.stringify((function(){{return ({code});}})())"),
            )
            .unwrap_or_else(|e| format!("(evaluate error: {e})"));
            Ok((truncate(&out, 4000), true))
        }
        "console" => {
            let ring = session.console.lock().unwrap_or_else(|p| p.into_inner());
            if ring.is_empty() {
                Ok((
                    "Console is clean — no messages or exceptions captured since the session opened."
                        .to_string(),
                    false,
                ))
            } else {
                let lines: Vec<&str> = ring.iter().map(String::as_str).collect();
                let body = lines.join("\n");
                Ok((
                    format!(
                        "Captured console output ({} entries, oldest first):\n{}",
                        lines.len(),
                        truncate(&body, 8_000)
                    ),
                    false,
                ))
            }
        }
        "status" => Ok((browser_diagnostics(session), false)),
        "state" => Ok((String::new(), true)),
        other => bail!("unhandled browser action: {other}"),
    }
}

/// Recover the common SPA case where a framework remounts an element between
/// `browser_state` and `browser_click`. The detached node still retains the
/// exact semantic identity the agent saw. We relocate only when that identity
/// has at least one stable signal and matches exactly one currently visible
/// element; ambiguity fails closed. The final click is driven through Chrome's
/// Element API so the page receives a real trusted mouse event, not `e.click()`.
fn recover_stale_click(tab: &Tab, index: u64) -> Result<bool> {
    let status = eval_string(
        tab,
        &STALE_CLICK_RECOVERY_JS.replace("__INDEX__", &index.to_string()),
    )?;
    if status != "UNIQUE" {
        return Ok(false);
    }
    let element = match tab.find_element("[data-phx-recovered-click=\"1\"]") {
        Ok(element) => element,
        Err(_) => return Ok(false),
    };
    let clicked = element.click().is_ok();
    let _ = tab.evaluate(
        "document.querySelectorAll('[data-phx-recovered-click]').forEach(function(e){e.removeAttribute('data-phx-recovered-click')})",
        false,
    );
    Ok(clicked)
}

/// Fill a stored secret without placing it in a model-authored JSON value or
/// returning it through CDP. Chromium's text-input command updates the focused
/// editable surface atomically and emits the page's normal input event;
/// verification reports only empty/non-empty.
pub(super) fn input_secret(session: &mut Session, index: i64, secret: &str) -> Result<()> {
    anyhow::ensure!(!secret.is_empty(), "stored credential secret is empty");
    let tab = session.tab.clone();
    let focus = eval_string(
        &tab,
        &FOCUS_FIELD_JS
            .replace("__INDEX__", &index.to_string())
            .replace("__CLEAR__", "true")
            .replace("__SECRET__", "true"),
    )?;
    match focus.as_str() {
        "NOT_FOUND" => bail!("No element with index [{index}] in the current state. Get fresh browser_state — indexes change every navigation."),
        "STALE" => bail!("Element [{index}] is STALE — the page re-rendered since that browser_state. Get fresh browser_state and use its new index."),
        "NOT_EDITABLE" => bail!("[{index}] has no editable text surface for the stored credential."),
        _ => {}
    }
    // Do not race the first framework hydration tick. Moodle, React, and Vue
    // login pages can index one input and replace it immediately afterward.
    // Waiting briefly and re-finding the exact unique semantic field BEFORE
    // typing avoids sending a real secret to a detached node in the first
    // place. Only public input metadata is used for the re-find.
    std::thread::sleep(Duration::from_millis(200));
    if eval_string(&tab, SECRET_FIELD_TARGET_STATUS_JS)? != "READY" {
        let recovered = eval_string(&tab, REFIND_SECRET_FIELD_JS)?;
        anyhow::ensure!(
            recovered == "READY",
            "credential field [{index}] changed before input ({recovered}); get fresh browser_state"
        );
    }
    // `type_str` expands a password into one CDP keydown/keyup pair per
    // character. On an Electron-owned WebContents target, login helpers can
    // react between those pairs and clear or remount the password field. Send
    // the exact text through Chromium's focused-editor path in one bounded
    // command instead. The secret still exists only inside this native call.
    tab.send_character(secret)
        .map_err(|error| anyhow::anyhow!("credential input into [{index}] failed: {error}"))?;
    let mut landed = secret_field_remained_set(&tab)?;
    if !landed {
        // Login pages commonly finish hydrating after their inputs are first
        // indexed. The original node can accept real CDP key events and then
        // be replaced by the framework before verification, leaving a fresh
        // empty password field. Recover that exact semantic field inside this
        // secret-safe call; never make the model request or carry the secret a
        // second time. A full navigation, ambiguity, or changed field identity
        // still fails closed.
        let recovered = eval_string(&tab, REFIND_SECRET_FIELD_JS)?;
        if recovered == "READY" {
            tab.send_character(secret).map_err(|error| {
                anyhow::anyhow!("credential retry into remounted [{index}] failed: {error}")
            })?;
            landed = secret_field_remained_set(&tab)?;
        }
    }
    // Keep the permanent vault-secret marker (browser state must redact this
    // field), but remove the short-lived typing pointer/fingerprint once the
    // protected operation has finished.
    let _ = eval_string(&tab, CLEAR_SECRET_TRACKING_JS);
    anyhow::ensure!(
        landed,
        "credential input into [{index}] did not remain stable in the field"
    );
    Ok(())
}

/// Confirm the field remains populated across multiple browser ticks. A single
/// immediate non-empty read is not enough: framework-controlled login forms
/// often replace or clear the DOM node a few hundred milliseconds later.
/// This helper only receives the boolean SET/EMPTY state, never field content.
fn secret_field_remained_set(tab: &Tab) -> Result<bool> {
    const REQUIRED_STABLE_READS: u8 = 3;
    const PROBE_INTERVAL: Duration = Duration::from_millis(175);
    let mut stable = 0_u8;
    for _ in 0..5 {
        std::thread::sleep(PROBE_INTERVAL);
        if eval_string(tab, SECRET_FIELD_STATUS_JS)? == "SET" {
            stable += 1;
            if stable >= REQUIRED_STABLE_READS {
                return Ok(true);
            }
        } else {
            return Ok(false);
        }
    }
    Ok(false)
}

fn decode_download_data_url(data: &str) -> Result<(String, Vec<u8>)> {
    let raw = data
        .strip_prefix("data:")
        .context("unexpected data-URL shape from page fetch")?;
    let (metadata, encoded) = raw
        .split_once(";base64,")
        .context("unexpected data-URL shape from page fetch")?;
    if metadata.len() > 512
        || metadata
            .bytes()
            .any(|byte| byte.is_ascii_control() || !byte.is_ascii())
    {
        bail!("download data URL has invalid or oversized media metadata");
    }
    let mime = metadata
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if mime.len() > 255 || mime.bytes().any(|byte| byte.is_ascii_whitespace()) {
        bail!("download data URL has an invalid media type");
    }
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|error| anyhow::anyhow!("base64 decode failed: {error}"))?;
    Ok((mime, bytes))
}

fn validate_download_magic(mime: &str, bytes: &[u8]) -> Result<()> {
    let valid = match mime {
        "image/png" => validate_png_magic(bytes).is_ok(),
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/webp" => bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP",
        "application/pdf" => validate_pdf_magic(bytes).is_ok(),
        "video/mp4" => bytes.len() >= 12 && &bytes[4..8] == b"ftyp",
        _ => return Ok(()),
    };
    if !valid {
        bail!("download declared {mime} but its bytes do not have the expected file signature");
    }
    Ok(())
}

/// Robust page scroll. `window.scrollBy` silently no-ops on SPA feeds (x.com,
/// infinite lists) where scrolling lives in an inner overflow container, not the
/// window — the top cause of "scroll did nothing." This finds the element that
/// ACTUALLY scrolls — the document scroller, else the scrollable under the
/// viewport center (or its nearest scrollable ancestor), else the tallest
/// scrollable container — and verifies it moved (scrollTop changed), returning
/// 'OK' or 'NOSCROLL'. `__DIR__` is ±1, `__PAGES__` the page count; one page ==
/// one viewport height. Setting scrollTop fires real scroll events, so infinite
/// feeds lazy-load just as they would for a human wheel.
/// Resolve the real editable surface from the indexed element, focus it, and set
/// the selection (select-all to clear, caret-to-end to append) so the subsequent
/// CDP Input.insertText lands correctly. Tags the resolved node with
/// `data-phx-typing` so VERIFY_FIELD_JS reads the SAME node. Secret fills also
/// leave a persistent `data-phx-vault-secret` marker so every later indexed
/// browser-state snapshot masks the value, including API-key fields that use a
/// normal text input. Does NOT write any text itself — real input events do
/// that (DOM writes don't drive Draft.js/Lexical). __INDEX__, __CLEAR__, and
/// __SECRET__ are substituted.
pub(super) const FOCUS_FIELD_JS: &str = r#"
(function(){
  var e=(window.__phx||[])[__INDEX__];
  if(!e)return 'NOT_FOUND';
  if(!e.isConnected)return 'STALE';
  e.scrollIntoView({block:'center'});
  // The indexed element may be a wrapper — find the actual editable surface.
  var t=null;
  if(e.isContentEditable||(e.tagName&&/^(INPUT|TEXTAREA)$/.test(e.tagName)))t=e;
  else t=e.querySelector('[contenteditable="true"],input,textarea')||e.closest('[contenteditable="true"]');
  if(!t)return 'NOT_EDITABLE';
  var clear=__CLEAR__;
  t.focus();
  if(t.isContentEditable){
    var sel=window.getSelection();var r=document.createRange();r.selectNodeContents(t);
    if(!clear)r.collapse(false); // caret to end -> append
    sel.removeAllRanges();sel.addRange(r);
  }else{
    if(clear){try{t.select();}catch(_){}}
    else{var n=(t.value||'').length;try{t.setSelectionRange(n,n);}catch(_){}}
  }
  if(window.__phxTyping&&window.__phxTyping.removeAttribute){try{window.__phxTyping.removeAttribute('data-phx-typing');}catch(_){}}
  document.querySelectorAll('[data-phx-typing]').forEach(function(x){x.removeAttribute('data-phx-typing');});
  t.setAttribute('data-phx-typing','1');
  if(__SECRET__)t.setAttribute('data-phx-vault-secret','1');
  if(__SECRET__)window.__phxSecretFingerprint={
    tag:String(t.tagName||'').toLowerCase(),
    id:t.getAttribute('id')||'',
    name:t.getAttribute('name')||'',
    type:t.getAttribute('type')||'',
    autocomplete:t.getAttribute('autocomplete')||'',
    placeholder:t.getAttribute('placeholder')||''
  };
  window.__phxTyping=t;
  return t.isContentEditable?'CE':'INPUT';
})()
"#;

const STALE_CLICK_RECOVERY_JS: &str = r#"
(function(){
  var old=(window.__phx||[])[__INDEX__];
  if(!old||old.isConnected||!old.tagName)return 'UNAVAILABLE';
  document.querySelectorAll('[data-phx-recovered-click]').forEach(function(e){e.removeAttribute('data-phx-recovered-click');});
  function norm(v){return String(v||'').trim().replace(/\s+/g,' ');}
  function visible(e){
    if(!e||!e.isConnected||e.disabled)return false;
    var r=e.getBoundingClientRect(),s=getComputedStyle(e);
    return r.width>0&&r.height>0&&r.bottom>=0&&r.right>=0&&r.top<=innerHeight&&r.left<=innerWidth&&s.visibility!=='hidden'&&s.display!=='none';
  }
  var names=['id','role','aria-label','name','placeholder','title','href','type'];
  var attrs={},signals=0;
  names.forEach(function(name){var value=old.getAttribute(name);if(value){attrs[name]=value;signals++;}});
  var text=norm(old.innerText||old.textContent||(old.value||''));
  if(text){text=text.slice(0,240);signals++;}
  if(!signals)return 'NO_SIGNAL';
  var candidates=Array.prototype.filter.call(document.getElementsByTagName(old.tagName),function(candidate){
    if(!visible(candidate))return false;
    for(var name in attrs){if(candidate.getAttribute(name)!==attrs[name])return false;}
    if(text&&norm(candidate.innerText||candidate.textContent||(candidate.value||'')).slice(0,240)!==text)return false;
    return true;
  });
  if(candidates.length!==1)return candidates.length?'AMBIGUOUS':'MISSING';
  candidates[0].setAttribute('data-phx-recovered-click','1');
  candidates[0].scrollIntoView({block:'center',inline:'center'});
  return 'UNIQUE';
})()
"#;

/// Read back the field that FOCUS_FIELD_JS tagged and confirm the typed text is
/// now in its RENDERED state. Valid because real CDP input updates the editor
/// model first; the model then renders to textContent/value. __VAL__ is the
/// js_string-quoted text.
pub(super) const VERIFY_FIELD_JS: &str = r#"
(function(){
  var t=window.__phxTyping||document.querySelector('[data-phx-typing="1"]');
  if(!t)return 'MISSING';
  t.removeAttribute('data-phx-typing');
  window.__phxTyping=null;
  var txt=t.isContentEditable?(t.textContent||''):(t.value||'');
  return txt.indexOf(__VAL__)>=0?'OK':'MISSING';
})()
"#;

/// Secret-safe companion to VERIFY_FIELD_JS. It reports only empty/non-empty;
/// the value never crosses back from Chrome. The caller probes repeatedly and
/// clears transient tracking only after the stability window ends.
const SECRET_FIELD_STATUS_JS: &str = r#"
(function(){
  var t=window.__phxTyping||document.querySelector('[data-phx-typing="1"]');
  if(!t||!t.isConnected)return 'MISSING';
  var populated=t.isContentEditable?!!(t.textContent||'').length:!!(t.value||'').length;
  return populated?'SET':'EMPTY';
})()
"#;

const SECRET_FIELD_TARGET_STATUS_JS: &str = r#"
(function(){
  var t=window.__phxTyping||document.querySelector('[data-phx-typing="1"]');
  if(!t||!t.isConnected||t.disabled)return 'MISSING';
  var r=t.getBoundingClientRect(),s=getComputedStyle(t);
  return r.width>0&&r.height>0&&s.visibility!=='hidden'&&s.display!=='none'?'READY':'HIDDEN';
})()
"#;

const CLEAR_SECRET_TRACKING_JS: &str = r#"
(function(){
  document.querySelectorAll('[data-phx-typing]').forEach(function(x){x.removeAttribute('data-phx-typing');});
  window.__phxTyping=null;
  window.__phxSecretFingerprint=null;
  return 'CLEARED';
})()
"#;

/// Recover one login field that was remounted after it accepted the first
/// protected fill. The fingerprint contains only public DOM metadata. It never
/// stores, reads, compares, or returns the credential value.
const REFIND_SECRET_FIELD_JS: &str = r#"
(function(){
  var f=window.__phxSecretFingerprint;
  if(!f)return 'NO_FINGERPRINT';
  function visible(e){
    if(!e||!e.isConnected||e.disabled)return false;
    var r=e.getBoundingClientRect(),s=getComputedStyle(e);
    return r.width>0&&r.height>0&&s.visibility!=='hidden'&&s.display!=='none';
  }
  function same(e){
    if(String(e.tagName||'').toLowerCase()!==f.tag)return false;
    var attrs=['id','name','type','autocomplete','placeholder'];
    for(var i=0;i<attrs.length;i++){
      var k=attrs[i], expected=f[k]||'';
      if(expected && (e.getAttribute(k)||'')!==expected)return false;
    }
    return visible(e);
  }
  var candidates=Array.prototype.filter.call(
    document.querySelectorAll('input,textarea,[contenteditable="true"]'),same);
  if(candidates.length!==1)return candidates.length?'AMBIGUOUS':'MISSING';
  var t=candidates[0];
  t.focus();
  if(t.isContentEditable){
    var sel=window.getSelection(),r=document.createRange();r.selectNodeContents(t);
    sel.removeAllRanges();sel.addRange(r);
  }else{try{t.select();}catch(_){}}
  document.querySelectorAll('[data-phx-typing]').forEach(function(x){x.removeAttribute('data-phx-typing');});
  t.setAttribute('data-phx-typing','1');
  t.setAttribute('data-phx-vault-secret','1');
  window.__phxTyping=t;
  return 'READY';
})()
"#;

pub(super) const SMART_SCROLL_JS: &str = r#"
(function(dir,pages){
  var d=dir*window.innerHeight*pages;
  function tryScroll(e){if(!e)return false;var b=e.scrollTop;e.scrollTop=b+d;return e.scrollTop!==b;}
  var doc=document.scrollingElement||document.documentElement;
  if(tryScroll(doc))return 'OK';
  var el=document.elementFromPoint(Math.floor(window.innerWidth/2),Math.floor(window.innerHeight/2));
  while(el){if(el.scrollHeight>el.clientHeight+4&&tryScroll(el))return 'OK';el=el.parentElement;}
  var all=document.querySelectorAll('*'),best=null,h=0;
  for(var i=0;i<all.length;i++){var e=all[i];if(e.scrollHeight>e.clientHeight+4){var oy=getComputedStyle(e).overflowY;if((oy==='auto'||oy==='scroll')&&e.scrollHeight>h){best=e;h=e.scrollHeight;}}}
  if(best&&tryScroll(best))return 'OK';
  return 'NOSCROLL';
})(__DIR__,__PAGES__)
"#;

/// The injected DOM indexer — ports browser-use's interactive-element heuristics
/// and emits the `[i]<tag attrs>text</tag>` state the agent prompt reasons over.
///
/// Donor-parity coverage: walks open shadow roots and same-origin iframes
/// (cross-origin iframes are surfaced with their center coordinates for
/// coordinate clicks), emits page stats + skeleton/empty-page detection, and
/// [Start/End of page] + pages-above/below scroll context. Indexed elements
/// live in `window.__phx` (works across shadow/iframe boundaries where CSS
/// selectors cannot reach); a `data-phx-idx` attribute is set additionally for
/// CDP-side lookups. JS dialogs (alert/confirm/prompt/onbeforeunload) are
/// suppressed each pass so the agent can never hang on a modal dialog.
pub(super) const INDEXER_JS: &str = r#"
(function(){
  try{window.alert=function(){};window.confirm=function(){return true;};window.prompt=function(){return null;};window.onbeforeunload=null;}catch(_){}
  var reg=window.__phx=[];
  var INTER_ROLES=['button','link','checkbox','radio','menuitem','menuitemcheckbox','tab','switch','option','combobox','searchbox','textbox','slider','spinbutton'];
  var INTER_TAGS=['a','button','input','select','textarea','summary','details'];
  var stats={links:0,inter:0,iframes:0,cross:0,images:0,total:0,shadow:0};
  function win(el){return (el.ownerDocument&&el.ownerDocument.defaultView)||window;}
  function visible(el){
    var r=el.getBoundingClientRect();
    if(r.width<=0||r.height<=0)return false;
    var s=win(el).getComputedStyle(el);
    if(s.visibility==='hidden'||s.display==='none'||parseFloat(s.opacity||'1')===0)return false;
    return true;
  }
  function interactive(el){
    var tag=el.tagName.toLowerCase();
    if(INTER_TAGS.indexOf(tag)>=0)return true;
    var role=el.getAttribute('role');
    if(role&&INTER_ROLES.indexOf(role)>=0)return true;
    if(el.hasAttribute('onclick'))return true;
    if(el.isContentEditable)return true;
    var ti=el.getAttribute('tabindex');
    if(ti!==null&&ti!=='-1')return true;
    if(win(el).getComputedStyle(el).cursor==='pointer'){
      // cursor:pointer over-collects nested wrappers; only count if no
      // interactive ancestor already owns this region.
      var p=el.parentElement,d=0;
      while(p&&d<4){var pr=p.getAttribute&&p.getAttribute('role');
        if(['a','button'].indexOf(p.tagName.toLowerCase())>=0||(pr&&INTER_ROLES.indexOf(pr)>=0))return false;
        p=p.parentElement;d++;}
      return true;
    }
    return false;
  }
  function repr(el,i){
    var tag=el.tagName.toLowerCase();
    var a=[];
    var ac=(el.getAttribute('autocomplete')||'').toLowerCase();
    var secretField=el.hasAttribute('data-phx-vault-secret')||(tag==='input'&&((el.type||'').toLowerCase()==='password'||/^(current-password|new-password|one-time-code)$/.test(ac)));
    ['type','name','placeholder','aria-label','value'].forEach(function(k){
      var v=(k==='value'&&'value' in el)?(secretField?(el.value?'[secret set]':''):el.value):el.getAttribute(k);
      if(v)a.push(k+'="'+String(v).slice(0,40)+'"');});
    // Native HTML labels are not input.innerText and need not be repeated in
    // aria-label. Keep the name next to the actionable index, not field values.
    if(!el.getAttribute('aria-label')&&el.labels&&el.labels.length){
      var label=Array.from(el.labels).map(function(node){return node.innerText||node.textContent||'';}).join(' ').trim().replace(/\s+/g,' ').slice(0,120);
      if(label)a.push('label='+JSON.stringify(label));
    }
    if(el.getAttribute('href'))a.push('href="'+el.getAttribute('href').slice(0,50)+'"');
    var txt=(secretField?'':(el.innerText||el.value||'')).trim().replace(/\s+/g,' ').slice(0,120);
    var attrs=a.length?' '+a.join(' '):'';
    return '['+i+']<'+tag+attrs+'>'+txt+'</'+tag+'>';
  }
  var lines=[];
  function walk(root,label,depth){
    if(!root||depth>3)return;
    var all=root.querySelectorAll('*');
    for(var k=0;k<all.length;k++){
      if(reg.length>=250)return;
      var el=all[k];stats.total++;
      var tag=el.tagName.toLowerCase();
      if(tag==='a')stats.links++;
      if(tag==='img')stats.images++;
      if(tag==='iframe'||tag==='frame'){
        stats.iframes++;
        var cd=null;try{cd=el.contentDocument;}catch(_){}
        if(cd&&cd.body){walk(cd.body,label+'[iframe]',depth+1);}
        else if(visible(el)){stats.cross++;var r=el.getBoundingClientRect();
          lines.push('<cross-origin iframe src="'+(el.getAttribute('src')||'').slice(0,60)+'" center=('+Math.round(r.left+r.width/2)+','+Math.round(r.top+r.height/2)+') - not indexable, click by coordinates>');}
        continue;
      }
      if(el.shadowRoot){stats.shadow++;walk(el.shadowRoot,label+'[shadow]',depth+1);}
      if(!visible(el))continue;
      if(!interactive(el))continue;
      var i=reg.length;reg.push(el);
      try{el.setAttribute('data-phx-idx',i);}catch(_){}
      stats.inter++;
      var r2=el.getBoundingClientRect();
      var pos=r2.top<0?' (above)':(r2.top>window.innerHeight?' (below)':'');
      lines.push(repr(el,i)+pos+label);
    }
  }
  walk(document.body||document.documentElement,'',0);
  var textChars=((document.body&&document.body.innerText)||'').length;
  var health='';
  if(stats.total<10)health='Page appears empty (SPA not loaded?). ';
  else if(stats.total>20&&textChars<stats.total*5)health='Page looks like skeleton/placeholder content (still loading?). ';
  var pstats=health+stats.links+' links, '+stats.inter+' interactive, '+stats.iframes+' iframes'+(stats.cross?' ('+stats.cross+' cross-origin)':'')+(stats.shadow?', '+stats.shadow+' shadow roots':'')+', '+stats.images+' images, '+stats.total+' elements';
  var y=window.scrollY,ih=window.innerHeight,maxY=Math.max(0,(document.body?document.body.scrollHeight:0)-ih);
  var body=lines.join('\n');
  if(y<10)body='[Start of page]\n'+body;
  if(maxY-y<10)body=body+'\n[End of page]';
  var scroll=maxY>10?('\nScroll: '+(y/ih).toFixed(1)+' pages above, '+((maxY-y)/ih).toFixed(1)+' pages below'+((maxY-y)>ih*0.2?' - scroll down for more':'')):'';
  return JSON.stringify({title:document.title||'',count:stats.inter,text:body,scroll:scroll,stats:pstats});
})()
"#;

pub(super) fn index_state(session: &Session) -> Result<String> {
    let tab = &session.tab;
    let raw = eval_string(tab, INDEXER_JS)?;
    // Capture each indexed element's stable Chrome backendNodeId in ONE CDP
    // call, so actions can drive by a stable id instead of the position index
    // that goes stale on re-render. Best-effort: a CDP failure leaves the prior
    // map in place and actions fall back to the JS window.__phx path. Never fatal.
    capture_backend_ids(session);
    let parsed: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let title = parsed.get("title").and_then(Value::as_str).unwrap_or("");
    let count = parsed.get("count").and_then(Value::as_u64).unwrap_or(0);
    let body = parsed.get("text").and_then(Value::as_str).unwrap_or("");
    let scroll = parsed.get("scroll").and_then(Value::as_str).unwrap_or("");
    let stats = parsed.get("stats").and_then(Value::as_str).unwrap_or("");
    let url = real_url(tab);
    // Donor parity: every state lists the open tabs with the short ids that
    // `browser_switch` / `browser_close` take — without this the agent cannot
    // know what to pass.
    let tabs_text = open_tabs_text(session);
    let mut out = format!("URL: {url}\nTitle: {title}\nPage: {stats}\n{tabs_text}Interactive elements ({count}) — act with the [index]:\n{body}{scroll}");
    out = truncate(&out, MAX_STATE_CHARS);
    Ok(out)
}

/// After `INDEXER_JS` has tagged each interactive element with
/// `data-phx-idx="<i>"`, resolve every index to its stable Chrome
/// `backendNodeId` in ONE CDP call (`DOM.getFlattenedDocument`, pierced so it
/// reaches shadow roots + iframes the same way the indexer walks them). The map
/// is stored on the Session (replaced wholesale — it always reflects the
/// latest index pass). depth is capped (not -1) so a giant DOM doesn't ship a
/// huge payload — elements deeper than the cap simply aren't in the map and
/// fall back to the JS path, which is the honest no-regression behavior.
fn capture_backend_ids(session: &Session) {
    use headless_chrome::protocol::cdp::DOM;
    let tab = &session.tab;
    // The DOM agent must be enabled before any DOM.* query on this tab —
    // without it GetFlattenedDocument fails with CDP -32000 "DOM agent hasn't
    // been enabled" (the exact live failure this line fixed). Idempotent and
    // cheap; running it per capture also self-heals recovered/adopted tabs,
    // which arrive with a fresh, un-enabled DOM agent.
    let _ = tab.call_method(DOM::Enable {
        include_whitespace: None,
    });
    let flat = match tab.call_method(DOM::GetFlattenedDocument {
        depth: Some(50),
        pierce: Some(true),
    }) {
        Ok(flat) => flat,
        Err(error) => {
            tracing::debug!("browser: backend-id capture skipped ({error:#})");
            return;
        }
    };
    let mut map = std::collections::HashMap::<u64, u32>::new();
    for node in &flat.nodes {
        // `attributes` is a flat Vec<String> of alternating name/value pairs.
        let Some(attrs) = node.attributes.as_ref() else {
            continue;
        };
        let mut i = 0;
        while i + 1 < attrs.len() {
            if attrs[i] == "data-phx-idx" {
                if let Ok(idx) = attrs[i + 1].parse::<u64>() {
                    map.insert(idx, node.backend_node_id);
                }
                break;
            }
            i += 2;
        }
    }
    tracing::debug!(
        "browser: captured {} index→backendNodeId entries",
        map.len()
    );
    if let Ok(mut slot) = session.backend_ids.lock() {
        *slot = map;
    }
}

/// Resolve an agent-visible `[index]` to its stable Chrome `backendNodeId`
/// from the most recent index pass. `None` if the index wasn't captured (element
/// not indexed, or the GetFlattenedDocument call failed) — callers fall back to
/// the JS `window.__phx[index]` path, which is today's behavior.
fn resolve_backend_id(session: &Session, index: u64) -> Option<u32> {
    session
        .backend_ids
        .lock()
        .ok()
        .and_then(|m| m.get(&index).copied())
}

/// Centroid of a CDP content quad — a flat `[x1,y1, x2,y2, x3,y3, x4,y4]` list
/// of 4 corner points. Mirrors headless_chrome's `ElementQuad::from_raw_points`
/// midpoint (`(top_left + bottom_right) / 2`).
pub(super) fn midpoint_from_quad(quad: &[f64]) -> Option<Point> {
    if quad.len() < 8 {
        return None;
    }
    let xs = [quad[0], quad[2], quad[4], quad[6]];
    let ys = [quad[1], quad[3], quad[5], quad[7]];
    let min_x = xs.iter().copied().fold(f64::INFINITY, f64::min);
    let max_x = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let min_y = ys.iter().copied().fold(f64::INFINITY, f64::min);
    let max_y = ys.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    Some(Point {
        x: (min_x + max_x) / 2.0,
        y: (min_y + max_y) / 2.0,
    })
}

/// Click by stable Chrome `backendNodeId`: scroll into view, read the content
/// quad, click the midpoint. Survives in-place re-renders (style/text/attr
/// changes, SPA route changes that reuse the node) because Chrome keeps the
/// backendNodeId live across those. A remount destroys the id → this returns
/// Err and the caller falls back to the JS path (which bails STALE honestly).
fn click_by_backend_id(tab: &Arc<Tab>, backend_node_id: u32) -> Result<()> {
    use headless_chrome::protocol::cdp::DOM;
    let _ = tab.call_method(DOM::ScrollIntoViewIfNeeded {
        node_id: None,
        backend_node_id: Some(backend_node_id),
        object_id: None,
        rect: None,
    })?;
    let quads = tab.call_method(DOM::GetContentQuads {
        node_id: None,
        backend_node_id: Some(backend_node_id),
        object_id: None,
    })?;
    let mid = quads
        .quads
        .first()
        .and_then(|q| midpoint_from_quad(q))
        .context("element has no visible quad to click")?;
    tab.click_point(mid)?;
    Ok(())
}

pub(super) fn open_tabs_text(session: &Session) -> String {
    if is_embedded_session(session) {
        // Electron's CDP endpoint contains the shell and every coworker's
        // targets. Only the instance-scoped directory may list this agent's
        // tabs; never fall back to the global CDP list on a read failure.
        let state = match super::session::embedded_browser_surface_action(
            &session.instance, "status", None, None,
        ) {
            Ok(Some(state)) => state,
            _ => return "Open tabs unavailable for this managed browser; no other profile's tabs are listed.\n".into(),
        };
        if state.tabs.len() < 2 { return String::new(); }
        let mut text = String::from("Open tabs (switch/close by id):\n");
        let mut seen = std::collections::HashSet::new();
        for tab in state.tabs {
            if !seen.insert(tab.id.clone()) { continue; }
            let marker = if tab.id == session.tab.get_target_id().as_str() { " (current)" } else { "" };
            let title: String = tab.title.chars().take(30).collect();
            text.push_str(&format!("  Tab {}: {} - {title}{marker}\n", tab.id, tab.url));
        }
        return text;
    }
    let Ok(tabs) = session._browser.get_tabs().lock() else {
        return String::new();
    };
    if tabs.len() < 2 {
        return String::new();
    }
    let mut text = String::from("Open tabs (switch/close by id):\n");
    for tab in tabs.iter() {
        let id = tab.get_target_id();
        let short = &id[id.len().saturating_sub(4)..];
        let marker = if std::sync::Arc::ptr_eq(tab, &session.tab) {
            " (current)"
        } else {
            ""
        };
        let title: String = tab
            .get_title()
            .unwrap_or_default()
            .chars()
            .take(30)
            .collect();
        text.push_str(&format!(
            "  Tab {short}: {} - {title}{marker}\n",
            tab.get_url()
        ));
    }
    text
}

// --- helpers ---

/// Type like a quick human: about 100 words per minute (~120 ms a character)
/// with natural variation, never a burst. One field is capped at ~45 s so a
/// long passage speeds up instead of blowing the 120 s browser tool budget.
fn type_at_human_pace(tab: &Tab, text: &str) -> Result<()> {
    use rand::Rng;
    const HUMAN_MS: f64 = 120.0;
    const FIELD_BUDGET_MS: f64 = 45_000.0;
    let count = text.chars().count().max(1) as f64;
    let mean_ms = HUMAN_MS.min(FIELD_BUDGET_MS / count);
    let mut rng = rand::thread_rng();
    let started = std::time::Instant::now();
    let mut due_ms = 0.0;
    let mut buffer = [0u8; 4];
    for ch in text.chars() {
        tab.type_str(ch.encode_utf8(&mut buffer))?;
        let pause = if ch == ' ' || ch == '\n' { 1.3 } else { 1.0 };
        due_ms += mean_ms * pause * rng.gen_range(0.6..1.4);
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        if due_ms > elapsed {
            std::thread::sleep(Duration::from_secs_f64((due_ms - elapsed) / 1000.0));
        }
    }
    Ok(())
}

#[cfg(test)]
mod download_artifact_tests {
    use super::*;

    #[test]
    fn data_url_decoder_normalizes_mime() {
        let (mime, bytes) =
            decode_download_data_url("data:IMAGE/PNG;charset=utf-8;base64,iVBORw0KGgo=").unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(bytes, b"\x89PNG\r\n\x1a\n");
        assert!(validate_download_magic(&mime, &bytes).is_ok());
        assert!(decode_download_data_url("data:image/png,not-base64").is_err());
        assert!(decode_download_data_url("data:image/png;base64,AAA").is_err());
    }

    #[test]
    fn declared_download_types_must_match_basic_magic() {
        assert!(validate_download_magic("image/jpeg", b"\xff\xd8\xffrest").is_ok());
        assert!(validate_download_magic("image/webp", b"RIFF1234WEBPrest").is_ok());
        assert!(validate_download_magic("application/pdf", b"%PDF-1.7").is_ok());
        assert!(validate_download_magic("video/mp4", b"1234ftyprest").is_ok());
        assert!(validate_download_magic("image/png", b"<html>error</html>").is_err());
        assert!(validate_download_magic("application/octet-stream", b"").is_ok());
    }
}
