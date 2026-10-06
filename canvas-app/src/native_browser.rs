//! Same-Chromium browser surface for the Linux desktop.
//!
//! Phoenix automation owns a real, visible Chromium process and CDP tab.  The
//! desktop locates that exact X11 window using its PID plus an unguessable
//! `--class=phoenix-browser-<token>` launch marker, then reparents it into a
//! neutral GTK native child over the conversation. Chromium is not an XEmbed
//! client, so this deliberately avoids GtkSocket's protocol. No screenshots,
//! JPEG decode, or synthetic input are in the human interaction path.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::Manager;

const MAX_SURFACES: usize = 24;
const MAX_INSTANCE_LEN: usize = 128;
const MIN_WINDOW_TOKEN_LEN: usize = 16;
const MAX_WINDOW_TOKEN_LEN: usize = 128;
const MAX_SURFACE_DIMENSION: f64 = 16_384.0;
const MAX_SURFACE_OFFSET: f64 = 32_768.0;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SurfaceRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl SurfaceRect {
    fn validate(self) -> Result<Self, String> {
        if ![self.x, self.y, self.width, self.height]
            .into_iter()
            .all(f64::is_finite)
        {
            return Err("browser surface bounds must be finite".into());
        }
        if self.x < 0.0
            || self.y < 0.0
            || self.x > MAX_SURFACE_OFFSET
            || self.y > MAX_SURFACE_OFFSET
        {
            return Err("browser surface position is outside the Phoenix window".into());
        }
        if self.width < 64.0
            || self.height < 64.0
            || self.width > MAX_SURFACE_DIMENSION
            || self.height > MAX_SURFACE_DIMENSION
        {
            return Err(format!(
                "browser surface size must be between 64 and {MAX_SURFACE_DIMENSION:.0} CSS pixels"
            ));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SurfaceStatus {
    pub supported: bool,
    pub embedded: bool,
    pub visible: bool,
    pub fallback: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
struct SurfaceEntry {
    xid: u64,
    root_xid: u64,
    original_geometry: XGeometry,
    embedded: bool,
    visible: bool,
    rect: SurfaceRect,
}

#[derive(Debug, Clone, Copy, Default)]
struct XGeometry {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

#[derive(Default)]
pub(crate) struct BrowserSurfaceRegistry {
    entries: Mutex<HashMap<String, SurfaceEntry>>,
    hooks_installed: Mutex<bool>,
}

fn normalize_instance(instance: &str) -> Result<String, String> {
    let instance = instance.trim();
    let instance = if instance.is_empty() {
        "agent-phoenix"
    } else {
        instance
    };
    if instance.len() > MAX_INSTANCE_LEN
        || !instance
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return Err("invalid browser instance id".into());
    }
    Ok(instance.to_string())
}

fn validate_window_token(token: &str) -> Result<&str, String> {
    let token = token.trim();
    if !(MIN_WINDOW_TOKEN_LEN..=MAX_WINDOW_TOKEN_LEN).contains(&token.len())
        || !token
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return Err("invalid browser window token".into());
    }
    Ok(token)
}

fn expected_wm_class(token: &str) -> String {
    format!("phoenix-browser-{token}").to_ascii_lowercase()
}

fn expected_profile_dirs(instance: &str) -> Vec<std::path::PathBuf> {
    let browser = crate::phoenix_home().join("browser");
    if instance == "agent-phoenix" {
        vec![browser.join("profile"), browser.join("chrome-profile-copy")]
    } else {
        vec![browser.join("profiles").join(instance)]
    }
}

fn parse_cmdline(raw: &[u8]) -> Vec<String> {
    raw.split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect()
}

fn validate_chromium_args(
    args: &[String],
    instance: &str,
    window_token: &str,
) -> Result<(), String> {
    let binary = args
        .first()
        .and_then(|path| std::path::Path::new(path).file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !["chrome", "chromium", "brave", "cloak"]
        .iter()
        .any(|marker| binary.contains(marker))
    {
        return Err("PID is not a Chromium browser process".into());
    }
    if args.iter().any(|arg| arg.starts_with("--type=")) {
        return Err("PID belongs to a Chromium helper, not the browser process".into());
    }
    if args
        .iter()
        .any(|arg| arg == "--headless" || arg.starts_with("--headless="))
    {
        return Err("managed Chromium is headless; relaunch it as a visible surface".into());
    }
    if !args
        .iter()
        .any(|arg| arg.starts_with("--remote-debugging-port="))
    {
        return Err("Chromium is not the CDP-controlled Phoenix browser".into());
    }
    let expected_class = expected_wm_class(window_token);
    if !args.iter().any(|arg| {
        arg.strip_prefix("--class=")
            .is_some_and(|class| class.eq_ignore_ascii_case(&expected_class))
    }) {
        return Err("Chromium is missing the requested Phoenix window token".into());
    }
    let profile = args
        .iter()
        .find_map(|arg| arg.strip_prefix("--user-data-dir="))
        .map(std::path::PathBuf::from)
        .ok_or_else(|| "Chromium has no managed Phoenix profile".to_string())?;
    if !expected_profile_dirs(instance)
        .iter()
        .any(|path| path == &profile)
    {
        return Err(format!(
            "Chromium profile does not belong to browser instance `{instance}`"
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn validate_browser_process(pid: u32, instance: &str, window_token: &str) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    if pid == 0 || pid > i32::MAX as u32 {
        return Err("invalid Chromium PID".into());
    }
    let process = std::path::Path::new("/proc").join(pid.to_string());
    let metadata = std::fs::metadata(&process)
        .map_err(|_| format!("Chromium PID {pid} is no longer running"))?;
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err("refusing to attach another user's process".into());
    }
    let raw = std::fs::read(process.join("cmdline"))
        .map_err(|error| format!("could not inspect Chromium PID {pid}: {error}"))?;
    if raw.len() > 1024 * 1024 {
        return Err("Chromium command line is unexpectedly large".into());
    }
    validate_chromium_args(&parse_cmdline(&raw), instance, window_token)
}

#[cfg(not(unix))]
fn validate_browser_process(_pid: u32, _instance: &str, _window_token: &str) -> Result<(), String> {
    Err("native Chromium surfaces are currently available on Linux/X11".into())
}

fn registry_lock(
    registry: &BrowserSurfaceRegistry,
) -> Result<std::sync::MutexGuard<'_, HashMap<String, SurfaceEntry>>, String> {
    registry
        .entries
        .lock()
        .map_err(|_| "browser surface registry is unavailable".to_string())
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use gtk::gdk;
    use gtk::prelude::*;
    use std::collections::HashSet;
    use std::ffi::CString;
    use std::os::raw::{c_int, c_long, c_uchar, c_ulong};
    use std::ptr;
    use std::time::Duration;
    use tauri::Manager;
    use x11::xlib;

    const LAYER_NAME: &str = "phoenix-native-browser-layer";
    const HOST_PREFIX: &str = "phoenix-browser-host-";

    struct Display(*mut xlib::Display);

    impl Display {
        fn open() -> Result<Self, String> {
            let display = unsafe { xlib::XOpenDisplay(ptr::null()) };
            if display.is_null() {
                Err("X11 display is unavailable; native browser surface cannot attach".into())
            } else {
                Ok(Self(display))
            }
        }
    }

    impl Drop for Display {
        fn drop(&mut self) {
            unsafe { xlib::XCloseDisplay(self.0) };
        }
    }

    fn intern_atom(display: *mut xlib::Display, name: &str) -> xlib::Atom {
        let name = CString::new(name).expect("static X atom contains no NUL");
        unsafe { xlib::XInternAtom(display, name.as_ptr(), xlib::False) }
    }

    fn window_property(
        display: *mut xlib::Display,
        window: xlib::Window,
        atom: xlib::Atom,
    ) -> Vec<u8> {
        let mut actual_type = 0;
        let mut actual_format = 0;
        let mut count = 0;
        let mut remaining = 0;
        let mut value: *mut c_uchar = ptr::null_mut();
        let status = unsafe {
            xlib::XGetWindowProperty(
                display,
                window,
                atom,
                0,
                1024,
                xlib::False,
                xlib::AnyPropertyType as c_ulong,
                &mut actual_type,
                &mut actual_format,
                &mut count,
                &mut remaining,
                &mut value,
            )
        };
        if status != xlib::Success as c_int || value.is_null() || actual_format != 8 {
            if !value.is_null() {
                unsafe { xlib::XFree(value.cast()) };
            }
            return Vec::new();
        }
        let bytes = unsafe { std::slice::from_raw_parts(value, count as usize) }.to_vec();
        unsafe { xlib::XFree(value.cast()) };
        bytes
    }

    fn window_property_exists(
        display: *mut xlib::Display,
        window: xlib::Window,
        atom: xlib::Atom,
    ) -> bool {
        let mut actual_type = 0;
        let mut actual_format = 0;
        let mut count = 0;
        let mut remaining = 0;
        let mut value: *mut c_uchar = ptr::null_mut();
        let status = unsafe {
            xlib::XGetWindowProperty(
                display,
                window,
                atom,
                0,
                1,
                xlib::False,
                xlib::AnyPropertyType as c_ulong,
                &mut actual_type,
                &mut actual_format,
                &mut count,
                &mut remaining,
                &mut value,
            )
        };
        if !value.is_null() {
            unsafe { xlib::XFree(value.cast()) };
        }
        status == xlib::Success as c_int && actual_type != 0
    }

    fn window_pid(
        display: *mut xlib::Display,
        window: xlib::Window,
        atom: xlib::Atom,
    ) -> Option<u32> {
        let mut actual_type = 0;
        let mut actual_format = 0;
        let mut count = 0;
        let mut remaining = 0;
        let mut value: *mut c_uchar = ptr::null_mut();
        let status = unsafe {
            xlib::XGetWindowProperty(
                display,
                window,
                atom,
                0,
                1,
                xlib::False,
                xlib::XA_CARDINAL,
                &mut actual_type,
                &mut actual_format,
                &mut count,
                &mut remaining,
                &mut value,
            )
        };
        if status != xlib::Success as c_int || value.is_null() || actual_format != 32 || count == 0
        {
            if !value.is_null() {
                unsafe { xlib::XFree(value.cast()) };
            }
            return None;
        }
        let pid = unsafe { *(value.cast::<c_ulong>()) as u32 };
        unsafe { xlib::XFree(value.cast()) };
        Some(pid)
    }

    fn query_parent(display: *mut xlib::Display, window: xlib::Window) -> Option<(u64, u64)> {
        let mut root = 0;
        let mut parent = 0;
        let mut children = ptr::null_mut();
        let mut count = 0;
        let ok = unsafe {
            xlib::XQueryTree(
                display,
                window,
                &mut root,
                &mut parent,
                &mut children,
                &mut count,
            )
        };
        if !children.is_null() {
            unsafe { xlib::XFree(children.cast()) };
        }
        (ok != 0).then_some((root as u64, parent as u64))
    }

    fn geometry(display: *mut xlib::Display, window: xlib::Window) -> Option<XGeometry> {
        let mut root = 0;
        let mut x = 0;
        let mut y = 0;
        let mut width = 0;
        let mut height = 0;
        let mut border = 0;
        let mut depth = 0;
        let ok = unsafe {
            xlib::XGetGeometry(
                display,
                window,
                &mut root,
                &mut x,
                &mut y,
                &mut width,
                &mut height,
                &mut border,
                &mut depth,
            )
        };
        (ok != 0).then_some(XGeometry {
            x,
            y,
            width,
            height,
        })
    }

    fn children(display: *mut xlib::Display, window: xlib::Window) -> Vec<xlib::Window> {
        let mut root = 0;
        let mut parent = 0;
        let mut raw = ptr::null_mut();
        let mut count = 0;
        let ok = unsafe {
            xlib::XQueryTree(
                display,
                window,
                &mut root,
                &mut parent,
                &mut raw,
                &mut count,
            )
        };
        if ok == 0 || raw.is_null() {
            return Vec::new();
        }
        let values = unsafe { std::slice::from_raw_parts(raw, count as usize) }.to_vec();
        unsafe { xlib::XFree(raw.cast()) };
        values
    }

    fn discover_window(
        display: *mut xlib::Display,
        pid: u32,
        expected_class: &str,
    ) -> Option<(xlib::Window, xlib::Window, xlib::Window, XGeometry)> {
        let root = unsafe { xlib::XDefaultRootWindow(display) };
        let pid_atom = intern_atom(display, "_NET_WM_PID");
        let class_atom = intern_atom(display, "WM_CLASS");
        let wm_state_atom = intern_atom(display, "WM_STATE");
        let mut stack = vec![root];
        let mut visited = HashSet::new();
        let mut best = None;
        let mut best_area = 0u64;
        while let Some(window) = stack.pop() {
            if !visited.insert(window) || visited.len() > 100_000 {
                continue;
            }
            stack.extend(children(display, window));
            if window_pid(display, window, pid_atom) != Some(pid) {
                continue;
            }
            let class = String::from_utf8_lossy(&window_property(display, window, class_atom))
                .replace('\0', " ")
                .to_ascii_lowercase();
            if !class.split_whitespace().any(|part| part == expected_class) {
                continue;
            }
            // Chromium creates an early, viewable compositor/input window
            // carrying the browser PID and --class token before its actual
            // app window is managed. It has no ICCCM WM_STATE. Attaching it
            // leaves the real page as a second root window, defeating both
            // native input and popup containment. Wait for the WM-managed
            // client; the bounded desktop retry handles this startup race.
            if !window_property_exists(display, window, wm_state_atom) {
                continue;
            }
            let Some(size) = geometry(display, window) else {
                continue;
            };
            let Some((window_root, parent)) = query_parent(display, window) else {
                continue;
            };
            let area = u64::from(size.width) * u64::from(size.height);
            if area >= best_area {
                best_area = area;
                best = Some((
                    window,
                    window_root as xlib::Window,
                    parent as xlib::Window,
                    size,
                ));
            }
        }
        best
    }

    fn set_borderless(display: *mut xlib::Display, window: xlib::Window, borderless: bool) {
        #[repr(C)]
        struct MotifHints {
            flags: c_ulong,
            functions: c_ulong,
            decorations: c_ulong,
            input_mode: c_long,
            status: c_ulong,
        }
        const MWM_HINTS_DECORATIONS: c_ulong = 1 << 1;
        let atom = intern_atom(display, "_MOTIF_WM_HINTS");
        let hints = MotifHints {
            flags: MWM_HINTS_DECORATIONS,
            functions: 0,
            decorations: if borderless { 0 } else { 1 },
            input_mode: 0,
            status: 0,
        };
        unsafe {
            xlib::XChangeProperty(
                display,
                window,
                atom,
                atom,
                32,
                xlib::PropModeReplace,
                (&hints as *const MotifHints).cast::<u8>(),
                5,
            );
        }
    }

    fn host_name(instance: &str) -> String {
        format!("{HOST_PREFIX}{instance}")
    }

    fn ensure_layer(main_view: &webkit2gtk::WebView) -> Result<gtk::Fixed, String> {
        if let Some(parent) = main_view.parent() {
            if let Ok(overlay) = parent.clone().downcast::<gtk::Overlay>() {
                if let Some(layer) = overlay
                    .children()
                    .into_iter()
                    .find(|child| child.widget_name() == LAYER_NAME)
                    .and_then(|child| child.downcast::<gtk::Fixed>().ok())
                {
                    return Ok(layer);
                }
            }
        }
        let parent = main_view
            .parent()
            .and_then(|parent| parent.downcast::<gtk::Box>().ok())
            .ok_or_else(|| {
                "Phoenix main webview is not hosted in the expected GTK box".to_string()
            })?;
        let children = parent.children();
        let position = children
            .iter()
            .position(|child| child == main_view)
            .unwrap_or(children.len()) as i32;
        let (expand, fill, padding, pack_type) = parent.query_child_packing(main_view);
        parent.remove(main_view);

        let overlay = gtk::Overlay::new();
        overlay.set_hexpand(true);
        overlay.set_vexpand(true);
        overlay.add(main_view);
        let layer = gtk::Fixed::new();
        layer.set_widget_name(LAYER_NAME);
        layer.set_hexpand(true);
        layer.set_vexpand(true);
        layer.set_halign(gtk::Align::Fill);
        layer.set_valign(gtk::Align::Fill);
        overlay.add_overlay(&layer);
        overlay.set_overlay_pass_through(&layer, false);
        parent.pack_start(&overlay, expand, fill, padding);
        parent.set_child_packing(&overlay, expand, fill, padding, pack_type);
        parent.reorder_child(&overlay, position);
        overlay.show();
        main_view.show();
        layer.show();
        Ok(layer)
    }

    fn find_host(layer: &gtk::Fixed, instance: &str) -> Option<gtk::DrawingArea> {
        let name = host_name(instance);
        layer
            .children()
            .into_iter()
            .find(|child| child.widget_name() == name)
            .and_then(|child| child.downcast::<gtk::DrawingArea>().ok())
    }

    fn drawing_area_xid(host: &gtk::DrawingArea) -> Option<xlib::Window> {
        host.window()?
            .downcast::<gdkx11::X11Window>()
            .ok()
            .map(|window| window.xid())
    }

    fn window_is_viewable(display: *mut xlib::Display, window: xlib::Window) -> bool {
        let mut attributes = std::mem::MaybeUninit::<xlib::XWindowAttributes>::uninit();
        let ok = unsafe { xlib::XGetWindowAttributes(display, window, attributes.as_mut_ptr()) };
        ok != 0 && unsafe { attributes.assume_init() }.map_state == xlib::IsViewable
    }

    fn set_override_redirect(display: *mut xlib::Display, window: xlib::Window, enabled: bool) {
        let mut attributes = unsafe { std::mem::zeroed::<xlib::XSetWindowAttributes>() };
        attributes.override_redirect = if enabled { xlib::True } else { xlib::False };
        unsafe {
            xlib::XChangeWindowAttributes(
                display,
                window,
                xlib::CWOverrideRedirect,
                &mut attributes,
            );
        }
    }

    fn window_is_embedded(
        display: *mut xlib::Display,
        window: xlib::Window,
        host: xlib::Window,
    ) -> bool {
        query_parent(display, window).is_some_and(|(_, parent)| parent == host as u64)
            && window_is_viewable(display, window)
    }

    /// A compositor can acknowledge XReparentWindow and then hand the client
    /// back to the root a few frames later. The old immediate check called
    /// that success, leaving a full operating-system Chrome window aligned on
    /// top of Phoenix. Require the child relationship to survive a short
    /// compositor grace period before exposing it as an embedded surface.
    fn window_stays_embedded(
        display: *mut xlib::Display,
        window: xlib::Window,
        host: xlib::Window,
    ) -> bool {
        for _ in 0..8 {
            if !window_is_embedded(display, window, host) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(25));
            unsafe { xlib::XSync(display, xlib::False) };
        }
        window_is_embedded(display, window, host)
    }

    /// Keep the embedded Chromium child coupled to the Phoenix window.
    ///
    /// XWayland can leave an otherwise healthy raw child unmapped after the
    /// GTK toplevel is restored, so mirror the parent lifecycle explicitly.
    fn sync_native_surfaces(app: &tauri::AppHandle, parent_visible: bool) {
        let registry = app.state::<BrowserSurfaceRegistry>();
        let entries = match registry_lock(&registry) {
            Ok(entries) => entries.values().cloned().collect::<Vec<_>>(),
            Err(_) => return,
        };
        if entries.is_empty() {
            return;
        }
        let Ok(display) = Display::open() else {
            return;
        };
        for entry in entries {
            let visible = parent_visible && entry.visible;
            unsafe {
                if visible {
                    xlib::XMapRaised(display.0, entry.xid as xlib::Window);
                } else {
                    xlib::XUnmapWindow(display.0, entry.xid as xlib::Window);
                }
            }
        }
        unsafe { xlib::XFlush(display.0) };
    }

    fn install_window_hooks(
        app: &tauri::AppHandle,
        main_view: &webkit2gtk::WebView,
    ) -> Result<(), String> {
        let top = main_view
            .toplevel()
            .and_then(|widget| widget.downcast::<gtk::Window>().ok())
            .ok_or_else(|| "Phoenix main GTK window is unavailable".to_string())?;

        let configure_app = app.clone();
        top.connect_configure_event(move |window, _| {
            let visible = window.window().is_some_and(|gdk_window| {
                !gdk_window.state().contains(gdk::WindowState::ICONIFIED)
            });
            sync_native_surfaces(&configure_app, visible);
            false
        });

        let state_app = app.clone();
        top.connect_window_state_event(move |_, event| {
            let visible = !event
                .new_window_state()
                .contains(gdk::WindowState::ICONIFIED);
            sync_native_surfaces(&state_app, visible);
            gtk::glib::Propagation::Proceed
        });

        let map_app = app.clone();
        top.connect_map_event(move |_, _| {
            sync_native_surfaces(&map_app, true);
            gtk::glib::Propagation::Proceed
        });

        let unmap_app = app.clone();
        top.connect_unmap_event(move |_, _| {
            sync_native_surfaces(&unmap_app, false);
            gtk::glib::Propagation::Proceed
        });
        Ok(())
    }

    pub(super) async fn ensure_window_hooks(app: &tauri::AppHandle) -> Result<(), String> {
        let registry = app.state::<BrowserSurfaceRegistry>();
        {
            let mut installed = registry
                .hooks_installed
                .lock()
                .map_err(|_| "browser surface lifecycle hooks are unavailable".to_string())?;
            if *installed {
                return Ok(());
            }
            *installed = true;
        }
        let hook_app = app.clone();
        let result = with_main(app, move |main| install_window_hooks(&hook_app, &main)).await;
        if result.is_err() {
            if let Ok(mut installed) = registry.hooks_installed.lock() {
                *installed = false;
            }
        }
        result
    }

    fn attach(
        main_view: &webkit2gtk::WebView,
        instance: &str,
        pid: u32,
        token: &str,
        rect: SurfaceRect,
    ) -> Result<(SurfaceEntry, SurfaceStatus), String> {
        let display = Display::open()?;
        let class = expected_wm_class(token);
        let Some((xid, root, _original_parent, original_geometry)) =
            discover_window(display.0, pid, &class)
        else {
            return Err(format!(
                "visible Chromium window for PID {pid} and class `{class}` was not found"
            ));
        };

        let layer = ensure_layer(main_view)?;
        if let Some(previous) = find_host(&layer, instance) {
            layer.remove(&previous);
        }
        let host = gtk::DrawingArea::new();
        host.set_widget_name(&host_name(instance));
        host.set_size_request(rect.width.round() as i32, rect.height.round() as i32);
        layer.put(&host, rect.x.round() as i32, rect.y.round() as i32);
        host.show();
        host.realize();
        let host_xid = drawing_area_xid(&host)
            .ok_or_else(|| "native browser host has no X11 window".to_string())?;
        let scale = main_view.scale_factor().max(1) as f64;
        let width = (rect.width * scale).round().max(1.0) as u32;
        let height = (rect.height * scale).round().max(1.0) as u32;
        // Chromium top-level windows do not implement XEmbed. GtkSocket will
        // briefly reparent one and then leave it permanently unmapped. A raw
        // X11 child keeps Chromium's own compositor/input path intact while
        // the neutral GTK host provides clipping, stacking and movement.
        unsafe {
            // First withdraw the managed toplevel. Merely unmapping it races
            // Mutter/XWayland: the window manager can re-manage it immediately
            // after our reparent and turn it back into a loose desktop window.
            xlib::XWithdrawWindow(display.0, xid, xlib::XDefaultScreen(display.0));
            xlib::XSync(display.0, xlib::False);
            xlib::XUnmapWindow(display.0, xid);
        }
        set_override_redirect(display.0, xid, true);
        set_borderless(display.0, xid, true);
        unsafe {
            xlib::XReparentWindow(display.0, xid, host_xid, 0, 0);
            xlib::XMoveResizeWindow(display.0, xid, 0, 0, width, height);
            xlib::XMapRaised(display.0, xid);
            xlib::XSync(display.0, xlib::False);
        }
        let embedded = window_stays_embedded(display.0, xid, host_xid);
        if !embedded {
            // A root-owned Chromium window is a separate browser window, even
            // when it is borderless and aligned over Phoenix. Never expose it.
            // Park it unmapped while the UI closes the native lease and returns
            // to Phoenix's bounded in-app screencast fallback.
            unsafe {
                xlib::XUnmapWindow(display.0, xid);
                xlib::XReparentWindow(
                    display.0,
                    xid,
                    root,
                    original_geometry.x,
                    original_geometry.y,
                );
                xlib::XSync(display.0, xlib::False);
            }
            set_override_redirect(display.0, xid, false);
            set_borderless(display.0, xid, false);
            layer.remove(&host);
            return Err(
                "Chromium could not mount inside Phoenix; the external window was kept hidden"
                    .into(),
            );
        }
        let entry = SurfaceEntry {
            xid: xid as u64,
            root_xid: root as u64,
            original_geometry,
            embedded,
            visible: true,
            rect,
        };
        let status = SurfaceStatus {
            supported: true,
            embedded,
            visible: true,
            fallback: false,
            reason: None,
        };
        Ok((entry, status))
    }

    fn set_bounds(
        main_view: &webkit2gtk::WebView,
        instance: &str,
        entry: &SurfaceEntry,
        rect: SurfaceRect,
    ) -> Result<(), String> {
        let display = Display::open()?;
        let layer = ensure_layer(main_view)?;
        let host = find_host(&layer, instance)
            .ok_or_else(|| "embedded Chromium host is missing".to_string())?;
        let host_xid = drawing_area_xid(&host)
            .ok_or_else(|| "native browser host has no X11 window".to_string())?;
        if !window_is_embedded(display.0, entry.xid as xlib::Window, host_xid) {
            unsafe {
                // Once the compositor has returned this client to the root it
                // is an external browser window. Hide it before reporting the
                // lost mount so the UI can switch to its in-app frame stream.
                xlib::XUnmapWindow(display.0, entry.xid as xlib::Window);
                xlib::XSync(display.0, xlib::False);
            }
            return Err(
                "embedded Chromium escaped its Phoenix host; the external window was hidden".into(),
            );
        }
        host.set_size_request(rect.width.round() as i32, rect.height.round() as i32);
        layer.move_(&host, rect.x.round() as i32, rect.y.round() as i32);
        let scale = main_view.scale_factor().max(1) as f64;
        unsafe {
            xlib::XMoveResizeWindow(
                display.0,
                entry.xid as xlib::Window,
                0,
                0,
                (rect.width * scale).round().max(1.0) as u32,
                (rect.height * scale).round().max(1.0) as u32,
            );
            xlib::XFlush(display.0);
        }
        Ok(())
    }

    fn set_visible(
        main_view: &webkit2gtk::WebView,
        instance: &str,
        entry: &SurfaceEntry,
        visible: bool,
    ) -> Result<(), String> {
        let display = Display::open()?;
        if entry.embedded {
            let layer = ensure_layer(main_view)?;
            let host = find_host(&layer, instance)
                .ok_or_else(|| "embedded Chromium host is missing".to_string())?;
            if visible {
                host.show();
            } else {
                host.hide();
            }
        }
        unsafe {
            if visible {
                xlib::XMapRaised(display.0, entry.xid as xlib::Window);
            } else {
                xlib::XUnmapWindow(display.0, entry.xid as xlib::Window);
            }
            xlib::XFlush(display.0);
        }
        Ok(())
    }

    fn detach(
        main_view: &webkit2gtk::WebView,
        instance: &str,
        entry: &SurfaceEntry,
    ) -> Result<(), String> {
        let display = Display::open()?;
        // Detach is the terminal UI operation. Unmap first so a concurrent
        // hide+detach can never leave a free-floating Chrome window behind.
        unsafe {
            xlib::XUnmapWindow(display.0, entry.xid as xlib::Window);
            xlib::XSync(display.0, xlib::False);
        }
        if entry.embedded {
            unsafe {
                xlib::XReparentWindow(
                    display.0,
                    entry.xid as xlib::Window,
                    entry.root_xid as xlib::Window,
                    entry.original_geometry.x,
                    entry.original_geometry.y,
                );
                xlib::XMoveResizeWindow(
                    display.0,
                    entry.xid as xlib::Window,
                    entry.original_geometry.x,
                    entry.original_geometry.y,
                    entry.original_geometry.width.max(1),
                    entry.original_geometry.height.max(1),
                );
            }
            if let Ok(layer) = ensure_layer(main_view) {
                if let Some(host) = find_host(&layer, instance) {
                    layer.remove(&host);
                }
            }
        }
        set_override_redirect(display.0, entry.xid as xlib::Window, false);
        set_borderless(display.0, entry.xid as xlib::Window, false);
        unsafe {
            xlib::XSetTransientForHint(display.0, entry.xid as xlib::Window, 0);
            xlib::XFlush(display.0);
        }
        Ok(())
    }

    pub(super) fn shutdown_surfaces(entries: Vec<SurfaceEntry>) {
        let Ok(display) = Display::open() else {
            return;
        };
        for entry in entries {
            // Do not destroy the managed Chromium process. Park every foreign
            // X child, unmapped, back under the root before GTK tears down its
            // hosts; the next Phoenix launch can attach the same live tab.
            unsafe {
                xlib::XUnmapWindow(display.0, entry.xid as xlib::Window);
                xlib::XReparentWindow(
                    display.0,
                    entry.xid as xlib::Window,
                    entry.root_xid as xlib::Window,
                    entry.original_geometry.x,
                    entry.original_geometry.y,
                );
                xlib::XSetTransientForHint(display.0, entry.xid as xlib::Window, 0);
            }
            set_override_redirect(display.0, entry.xid as xlib::Window, false);
            set_borderless(display.0, entry.xid as xlib::Window, false);
        }
        unsafe { xlib::XFlush(display.0) };
    }

    async fn with_main<T, F>(app: &tauri::AppHandle, operation: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(webkit2gtk::WebView) -> Result<T, String> + Send + 'static,
    {
        let main = app
            .get_webview_window("main")
            .ok_or_else(|| "Phoenix main window is unavailable".to_string())?;
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        main.with_webview(move |platform| {
            let _ = sender.send(operation(platform.inner()));
        })
        .map_err(|error| error.to_string())?;
        tauri::async_runtime::spawn_blocking(move || {
            receiver
                .recv_timeout(Duration::from_secs(4))
                .map_err(|_| "native browser surface operation timed out".to_string())?
        })
        .await
        .map_err(|error| error.to_string())?
    }

    pub(super) async fn attach_surface(
        app: &tauri::AppHandle,
        instance: String,
        pid: u32,
        token: String,
        rect: SurfaceRect,
    ) -> Result<(SurfaceEntry, SurfaceStatus), String> {
        with_main(app, move |main| attach(&main, &instance, pid, &token, rect)).await
    }

    pub(super) async fn bounds_surface(
        app: &tauri::AppHandle,
        instance: String,
        entry: SurfaceEntry,
        rect: SurfaceRect,
    ) -> Result<(), String> {
        with_main(app, move |main| set_bounds(&main, &instance, &entry, rect)).await
    }

    pub(super) async fn visible_surface(
        app: &tauri::AppHandle,
        instance: String,
        entry: SurfaceEntry,
        visible: bool,
    ) -> Result<(), String> {
        with_main(app, move |main| {
            set_visible(&main, &instance, &entry, visible)
        })
        .await
    }

    pub(super) async fn detach_surface(
        app: &tauri::AppHandle,
        instance: String,
        entry: SurfaceEntry,
    ) -> Result<(), String> {
        with_main(app, move |main| detach(&main, &instance, &entry)).await
    }
}

fn unsupported(reason: &str) -> SurfaceStatus {
    SurfaceStatus {
        supported: false,
        embedded: false,
        visible: false,
        fallback: false,
        reason: Some(reason.into()),
    }
}

/// Called synchronously from the main-window close event, before GTK destroys
/// the GTK native hosts. It never terminates Chromium or exposes a window.
pub(crate) fn browser_surface_shutdown(app: &tauri::AppHandle) {
    let registry = app.state::<BrowserSurfaceRegistry>();
    let entries = match registry_lock(&registry) {
        Ok(mut entries) => entries.drain().map(|(_, entry)| entry).collect::<Vec<_>>(),
        Err(_) => return,
    };
    #[cfg(target_os = "linux")]
    linux::shutdown_surfaces(entries);
    #[cfg(not(target_os = "linux"))]
    let _ = entries;
}

#[tauri::command]
pub(crate) async fn browser_surface_attach(
    app: tauri::AppHandle,
    instance: String,
    pid: u32,
    window_token: String,
    rect: SurfaceRect,
) -> Result<SurfaceStatus, String> {
    let instance = normalize_instance(&instance)?;
    let token = validate_window_token(&window_token)?.to_string();
    let rect = rect.validate()?;
    validate_browser_process(pid, &instance, &token)?;

    #[cfg(target_os = "linux")]
    {
        if std::env::var("GDK_BACKEND")
            .ok()
            .is_some_and(|backend| !backend.split(',').any(|part| part.trim() == "x11"))
        {
            return Ok(unsupported(
                "native Chromium embedding needs the X11 GDK backend",
            ));
        }
        linux::ensure_window_hooks(&app).await?;
        let registry = app.state::<BrowserSurfaceRegistry>();
        let at_capacity = {
            let entries = registry_lock(&registry)?;
            entries.len() >= MAX_SURFACES && !entries.contains_key(&instance)
        };
        if at_capacity {
            return Err(format!(
                "at most {MAX_SURFACES} browser surfaces may be attached"
            ));
        }
        let previous = {
            let entries = registry_lock(&registry)?;
            entries.get(&instance).cloned()
        };
        if let Some(previous) = previous {
            linux::detach_surface(&app, instance.clone(), previous).await?;
            registry_lock(&registry)?.remove(&instance);
        }
        let (entry, status) =
            linux::attach_surface(&app, instance.clone(), pid, token, rect).await?;
        registry_lock(&registry)?.insert(instance, entry);
        Ok(status)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (app, instance, token, rect);
        Ok(unsupported(
            "same-Chromium embedding is currently implemented for Linux/X11",
        ))
    }
}

#[tauri::command]
pub(crate) async fn browser_surface_set_bounds(
    app: tauri::AppHandle,
    instance: String,
    rect: SurfaceRect,
) -> Result<SurfaceStatus, String> {
    let instance = normalize_instance(&instance)?;
    let rect = rect.validate()?;
    let registry = app.state::<BrowserSurfaceRegistry>();
    let entry = registry_lock(&registry)?
        .get(&instance)
        .cloned()
        .ok_or_else(|| format!("browser surface `{instance}` is not attached"))?;
    #[cfg(target_os = "linux")]
    linux::bounds_surface(&app, instance.clone(), entry.clone(), rect).await?;
    let mut updated = entry;
    updated.rect = rect;
    registry_lock(&registry)?.insert(instance, updated.clone());
    Ok(SurfaceStatus {
        supported: true,
        embedded: updated.embedded,
        visible: updated.visible,
        fallback: false,
        reason: None,
    })
}

async fn set_surface_visibility(
    app: tauri::AppHandle,
    instance: String,
    visible: bool,
) -> Result<SurfaceStatus, String> {
    let instance = normalize_instance(&instance)?;
    let registry = app.state::<BrowserSurfaceRegistry>();
    let entry = registry_lock(&registry)?
        .get(&instance)
        .cloned()
        .ok_or_else(|| format!("browser surface `{instance}` is not attached"))?;
    #[cfg(target_os = "linux")]
    linux::visible_surface(&app, instance.clone(), entry.clone(), visible).await?;
    let mut updated = entry;
    updated.visible = visible;
    registry_lock(&registry)?.insert(instance, updated.clone());
    Ok(SurfaceStatus {
        supported: true,
        embedded: updated.embedded,
        visible,
        fallback: false,
        reason: None,
    })
}

#[tauri::command]
pub(crate) async fn browser_surface_show(
    app: tauri::AppHandle,
    instance: String,
) -> Result<SurfaceStatus, String> {
    set_surface_visibility(app, instance, true).await
}

#[tauri::command]
pub(crate) async fn browser_surface_hide(
    app: tauri::AppHandle,
    instance: String,
) -> Result<SurfaceStatus, String> {
    set_surface_visibility(app, instance, false).await
}

#[tauri::command]
pub(crate) async fn browser_surface_detach(
    app: tauri::AppHandle,
    instance: String,
) -> Result<SurfaceStatus, String> {
    let instance = normalize_instance(&instance)?;
    let registry = app.state::<BrowserSurfaceRegistry>();
    let entry = registry_lock(&registry)?
        .get(&instance)
        .cloned()
        .ok_or_else(|| format!("browser surface `{instance}` is not attached"))?;
    #[cfg(target_os = "linux")]
    linux::detach_surface(&app, instance.clone(), entry).await?;
    registry_lock(&registry)?.remove(&instance);
    Ok(SurfaceStatus {
        supported: true,
        embedded: false,
        visible: false,
        fallback: false,
        reason: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_args(home: &std::path::Path, instance: &str, token: &str) -> Vec<String> {
        let profile = if instance == "agent-phoenix" {
            home.join("browser/profile")
        } else {
            home.join("browser/profiles").join(instance)
        };
        vec![
            "/opt/cloakbrowser/chrome".into(),
            "--remote-debugging-port=9229".into(),
            format!("--class={}", expected_wm_class(token)),
            format!("--user-data-dir={}", profile.display()),
        ]
    }

    #[test]
    fn validators_reject_path_traversal_and_weak_tokens() {
        assert_eq!(normalize_instance("").unwrap(), "agent-phoenix");
        assert!(normalize_instance("../phoenix").is_err());
        assert!(validate_window_token("short").is_err());
        assert!(validate_window_token("a1b2c3d4e5f6g7h8").is_ok());
    }

    #[test]
    fn bounds_are_finite_and_capped() {
        let rect = SurfaceRect {
            x: 310.0,
            y: 74.0,
            width: 1024.0,
            height: 720.0,
        };
        assert_eq!(rect.validate().unwrap(), rect);
        assert!(SurfaceRect { x: -1.0, ..rect }.validate().is_err());
        assert!(SurfaceRect {
            width: f64::NAN,
            ..rect
        }
        .validate()
        .is_err());
        assert!(SurfaceRect {
            height: 20_000.0,
            ..rect
        }
        .validate()
        .is_err());
    }

    #[test]
    fn chromium_validator_binds_pid_contract_to_instance_and_window_token() {
        let old_home = std::env::var_os("PHOENIX_HOME");
        let home = std::env::temp_dir().join("phoenix-surface-validator");
        std::env::set_var("PHOENIX_HOME", &home);
        let token = "0123456789abcdef0123456789abcdef";
        let args = valid_args(&home, "agent-nico", token);
        assert!(validate_chromium_args(&args, "agent-nico", token).is_ok());
        assert!(validate_chromium_args(&args, "agent-iris", token).is_err());
        assert!(validate_chromium_args(&args, "agent-nico", "ffffffffffffffff").is_err());
        let mut headless = args.clone();
        headless.push("--headless=new".into());
        assert!(validate_chromium_args(&headless, "agent-nico", token).is_err());
        match old_home {
            Some(value) => std::env::set_var("PHOENIX_HOME", value),
            None => std::env::remove_var("PHOENIX_HOME"),
        }
    }

    #[test]
    fn wm_class_marker_is_namespaced() {
        assert_eq!(
            expected_wm_class("0123456789abcdef"),
            "phoenix-browser-0123456789abcdef"
        );
    }
}
