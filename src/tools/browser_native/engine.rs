//! Engine plumbing: wedge/transport-death classification, bounded navigate,
//! page settling, ws discovery, profile management, chrome discovery.

use super::*;

pub(super) fn is_navigate_wedge(error: &anyhow::Error) -> bool {
    format!("{error:#}").contains("did not complete within")
}

/// Chromium occasionally commits a navigation but drops the lifecycle event
/// `headless_chrome` is waiting for (seen repeatedly on x.com). Treat that as a
/// successful navigation only when live page evidence proves the requested
/// destination, or a same-origin redirect from it, is already interactive.
/// This deliberately rejects an unrelated stale page left in the tab.
pub(super) fn navigation_wait_failure_is_usable(
    error: &str,
    before_url: &str,
    current_url: &str,
    requested_url: &str,
    ready_state: &str,
) -> bool {
    if !error
        .to_ascii_lowercase()
        .contains("event waited for never came")
        || !matches!(ready_state, "interactive" | "complete")
        || current_url.is_empty()
        || current_url == "about:blank"
    {
        return false;
    }

    if current_url == requested_url {
        return true;
    }

    let (Ok(current), Ok(requested)) =
        (url::Url::parse(current_url), url::Url::parse(requested_url))
    else {
        return false;
    };
    let same_origin = current.scheme() == requested.scheme()
        && current.host_str() == requested.host_str()
        && current.port_or_known_default() == requested.port_or_known_default();

    same_origin && current_url != before_url
}

/// Did the CDP transport itself die (vs. a normal action-level failure)? Only
/// these justify tearing down and respawning the browser. Kept deliberately
/// narrow — over-matching here is exactly what caused the constant relaunching.
pub(super) fn is_transport_dead(error: &anyhow::Error) -> bool {
    let text = format!("{error:#}").to_lowercase();
    [
        "connection closed",
        "connection is closed",
        "underlying connection is closed",
        "websocket",
        "sending on a closed channel",
        "receiving on an empty and disconnected channel",
        "the tab is gone",
        "target closed",
        "not connected to",
        "no browser is open",
        "broken pipe",
        "unexpected end of file",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

/// Returns `(human output, whether to append fresh browser state)`.
/// Navigate with a hard internal cap. `Page.navigate` (and the load wait)
/// are raw CDP `call_method`s with no timeout — a connection a VPN/proxy
/// black-holes can wedge them indefinitely (observed live: x.com over a VPN
/// froze the turn). Running them in a worker thread with a bounded
/// `recv_timeout` turns that infinite hang into a clean, fast, diagnostic
/// error. The happy path returns in seconds, unchanged; only the wedge path
/// differs. On timeout the worker is abandoned (its CDP call dies when the
/// caller drops the session), and the message names the likely cause.
pub(super) fn bounded_navigate(tab: &Arc<Tab>, url: &str) -> Result<()> {
    const NAV_BUDGET: Duration = Duration::from_secs(30);
    let (tx, rx) = std::sync::mpsc::channel();
    let worker_tab = Arc::clone(tab);
    let nav_url = url.to_string();
    std::thread::spawn(move || {
        let before_url = real_url(&worker_tab);
        let outcome = match worker_tab.navigate_to(&nav_url) {
            Ok(navigated_tab) => match navigated_tab.wait_until_navigated() {
                Ok(_) => Ok(()),
                Err(error) => {
                    let error = error.to_string();
                    let current_url = real_url(&worker_tab);
                    let ready_state =
                        eval_string(&worker_tab, "document.readyState").unwrap_or_default();
                    if navigation_wait_failure_is_usable(
                        &error,
                        &before_url,
                        &current_url,
                        &nav_url,
                        &ready_state,
                    ) {
                        Ok(())
                    } else {
                        Err(error)
                    }
                }
            },
            Err(error) => Err(error.to_string()),
        };
        let _ = tx.send(outcome);
    });
    match rx.recv_timeout(NAV_BUDGET) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => bail!("navigation to {url} failed: {error}"),
        Err(_) => bail!(
            "navigation to {url} did not complete within {}s — the site may be slow, rate-limiting \
automated browsers, or unreachable through a VPN/proxy (x.com in particular blocks many VPN exit \
IPs). The browser is still open and the page may have loaded anyway — check `browser_state` before \
retrying, and reach the data another way if it stays stuck (disabling a VPN for browser work helps).",
            NAV_BUDGET.as_secs()
        ),
    }
}

/// Let a freshly-navigated page reach a usable state before we read it.
/// `wait_until_navigated` only fires on the load event, but a modern JS page
/// keeps building its DOM after that — reading immediately yields a half-built
/// page, so the agent acts on interactive elements that don't exist yet (the
/// classic "the nav worked but the click failed"). browser-use waits for
/// `networkIdle`; we approximate it cheaply and without the CDP lifecycle
/// machinery: poll until `document.readyState` is `complete` AND the count of
/// interactive elements holds steady across ticks. Bounded so a page that
/// never quiets (ads, polling) still proceeds; early-exit keeps fast pages fast.
/// Best-effort — any eval error just ends the wait.
///
/// The wait floor matters: settle runs after EVERY click/keys/navigate, and
/// inside `browser_act` batches it multiplies. So probe IMMEDIATELY (no sleep
/// before the first look), and when the page was already `complete` at entry —
/// the common case for an SPA click that mutated in place — one confirming
/// tick is enough (~150ms floor). A page still loading at entry keeps the
/// two-stable-ticks bar (~300ms window once it completes).
pub(super) fn settle_page(tab: &Tab) {
    const PROBE: &str = "document.readyState+'|'+document.querySelectorAll('a,button,input,textarea,select,[role=button],[onclick]').length";
    let deadline = std::time::Instant::now() + Duration::from_secs(6);
    let Ok(mut last) = eval_string(tab, PROBE) else {
        return;
    };
    let needed: u8 = if last.split('|').next() == Some("complete") {
        1
    } else {
        2
    };
    let mut stable: u8 = 0;
    while std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(150));
        let Ok(snapshot) = eval_string(tab, PROBE) else {
            break;
        };
        let ready = snapshot.split('|').next() == Some("complete");
        if ready && snapshot == last {
            stable += 1;
            if stable >= needed {
                break;
            }
        } else {
            stable = 0;
            last = snapshot;
        }
    }
}

/// Turn an attach spec (`ws://...`, a port, or `1`/`true`) into a CDP ws URL.
pub(super) fn resolve_ws_url(spec: &str) -> Result<String> {
    if spec.starts_with("ws://") || spec.starts_with("wss://") {
        return Ok(spec.to_string());
    }
    let port: u16 = if spec == "1" || spec.eq_ignore_ascii_case("true") {
        9222
    } else {
        spec.parse().with_context(|| {
            format!("PHOENIX_BROWSER_ATTACH must be a ws:// URL or a port number, got '{spec}'")
        })?
    };
    devtools_ws_url(port)
}

/// Fetch webSocketDebuggerUrl from the DevTools HTTP endpoint. Plain
/// std::net so it is safe to call from any (sync or async) context.
pub(super) fn devtools_ws_url(port: u16) -> Result<String> {
    use std::io::{Read, Write};
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2))
        .with_context(|| format!(
            "no browser DevTools endpoint on 127.0.0.1:{port}. Start Chrome with --remote-debugging-port={port} (and close other Chrome windows first so the flag takes effect)."
        ))?;
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
    write!(
        stream,
        "GET /json/version HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )?;
    // Chrome's DevTools server may hold the connection open despite
    // Connection: close — read until the JSON body is balanced, not until EOF.
    let mut bytes = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                bytes.extend_from_slice(&buf[..n]);
                let depth: i64 = bytes.iter().fold(0, |d, b| match b {
                    b'{' => d + 1,
                    b'}' => d - 1,
                    _ => d,
                });
                if depth == 0 && bytes.contains(&b'{') {
                    break;
                }
            }
            Err(_) if !bytes.is_empty() => break,
            Err(e) => return Err(e).context("reading DevTools /json/version"),
        }
    }
    let raw = String::from_utf8_lossy(&bytes).into_owned();
    let start = raw
        .find('{')
        .context("DevTools /json/version returned no JSON")?;
    let end = raw
        .rfind('}')
        .context("DevTools /json/version returned no JSON")?;
    let parsed: Value = serde_json::from_str(&raw[start..=end])
        .context("DevTools /json/version returned invalid JSON")?;
    parsed
        .get("webSocketDebuggerUrl")
        .and_then(Value::as_str)
        .map(str::to_string)
        .context("DevTools /json/version had no webSocketDebuggerUrl")
}

/// Snapshot the essential auth data of a locked Chrome profile into a
/// Phoenix-owned copy (donor: browser-use locked-profile handling). Cookies,
/// saved logins, autofill, and preferences come along; caches and history
/// don't need to.
pub(super) fn copy_profile_essentials(src: &std::path::Path) -> Result<std::path::PathBuf> {
    let phoenix_home = crate::config::phoenix_home();
    crate::config::private_io::prepare_phoenix_home(&phoenix_home)?;
    crate::config::private_io::prepare_phoenix_directory(&phoenix_home.join("browser"))?;
    let dst = phoenix_home.join("browser/chrome-profile-copy");
    anyhow::ensure!(
        src.is_dir(),
        "browser profile source does not exist or is not a directory: {}",
        src.display()
    );
    anyhow::ensure!(
        src != dst,
        "refusing to refresh a browser-profile copy from itself"
    );
    let canonical_source = std::fs::canonicalize(src)
        .with_context(|| format!("failed to resolve browser profile source {}", src.display()))?;
    if let Ok(canonical_destination) = std::fs::canonicalize(&dst) {
        anyhow::ensure!(
            canonical_source != canonical_destination,
            "refusing to refresh a browser-profile copy through an alias of itself"
        );
    }
    anyhow::ensure!(
        !super::handoff::active_for_profile(&dst),
        "cannot refresh the managed browser-profile copy while its login handoff is open"
    );
    // Never merge a new donor into files left by an older donor. That could
    // expose stale cookies/logins after the user deliberately selected a
    // different or empty profile. Stop exact-profile leftovers first, then
    // rebuild the snapshot from an empty destination. Authentication can live
    // in Local Storage/IndexedDB as well as cookie databases, so selectively
    // deleting a few filenames is not a sufficient cross-donor isolation
    // boundary.
    kill_stale_profile_holders(&dst);
    match std::fs::symlink_metadata(&dst) {
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_file() => {
            std::fs::remove_file(&dst).with_context(|| {
                format!(
                    "failed to clear unsafe profile-copy entry {}",
                    dst.display()
                )
            })?;
        }
        Ok(metadata) if metadata.is_dir() => {
            std::fs::remove_dir_all(&dst).with_context(|| {
                format!(
                    "failed to clear previous browser-profile copy {}",
                    dst.display()
                )
            })?;
        }
        Ok(_) => anyhow::bail!(
            "profile-copy target has an unsupported type: {}",
            dst.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("inspect {}", dst.display())),
    }
    crate::config::private_io::prepare_phoenix_directory(&dst.join("Default"))
        .with_context(|| format!("failed to prepare profile copy dir {}", dst.display()))?;
    for file in ["Local State"] {
        let from = src.join(file);
        let to = dst.join(file);
        match std::fs::symlink_metadata(&to) {
            Ok(metadata) if metadata.is_file() || metadata.file_type().is_symlink() => {
                std::fs::remove_file(&to).with_context(|| {
                    format!("failed to clear stale profile file {}", to.display())
                })?;
            }
            Ok(_) => anyhow::bail!("profile-copy target is not a file: {}", to.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| format!("inspect {}", to.display())),
        }
        if from.is_file() {
            std::fs::copy(&from, &to).with_context(|| {
                format!("failed to copy browser profile file {}", from.display())
            })?;
        }
    }
    for file in [
        "Cookies",
        "Cookies-journal",
        "Login Data",
        "Login Data-journal",
        "Web Data",
        "Web Data-journal",
        "Preferences",
        "Secure Preferences",
    ] {
        let from = src.join("Default").join(file);
        let to = dst.join("Default").join(file);
        match std::fs::symlink_metadata(&to) {
            Ok(metadata) if metadata.is_file() || metadata.file_type().is_symlink() => {
                std::fs::remove_file(&to).with_context(|| {
                    format!("failed to clear stale profile file {}", to.display())
                })?;
            }
            Ok(_) => anyhow::bail!("profile-copy target is not a file: {}", to.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| format!("inspect {}", to.display())),
        }
        if from.is_file() {
            std::fs::copy(&from, &to).with_context(|| {
                format!("failed to copy browser profile file {}", from.display())
            })?;
        }
    }
    // Current Chromium stores cookies below Default/Network; older versions
    // used Default/Cookies. Support both layouts without ever merging donors.
    crate::config::private_io::prepare_phoenix_directory(&dst.join("Default/Network"))?;
    for file in ["Cookies", "Cookies-journal"] {
        let from = src.join("Default/Network").join(file);
        if from.is_file() {
            let to = dst.join("Default/Network").join(file);
            std::fs::copy(&from, &to).with_context(|| {
                format!("failed to copy browser profile file {}", from.display())
            })?;
        }
    }
    // A stale lock in the copy would block the launch we are enabling.
    std::fs::remove_file(dst.join("SingletonLock")).ok();
    Ok(dst)
}

/// `Browser::new` blocks until Chrome prints its DevTools port and has no hard
/// cap of its own: a profile-lock hand-off or a Chrome that dies on startup
/// wedges it indefinitely, freezing the whole turn. Run it on a worker thread
/// with a bounded wait so a failed launch becomes a fast, honest error instead
/// of an endless spinner.
pub(super) fn bounded_browser_launch(options: LaunchOptions<'static>) -> Result<Browser> {
    const LAUNCH_BUDGET: Duration = Duration::from_secs(45);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let outcome = Browser::new(options).map_err(|e| e.to_string());
        let _ = tx.send(outcome);
    });
    let deadline = std::time::Instant::now() + LAUNCH_BUDGET;
    loop {
        // Dropping the receiver makes a late Browser result get dropped on the
        // launch worker, which in turn kills/reaps the Chrome it owns. This is
        // what prevents an in-progress launch from escaping the gateway's
        // bounded shutdown pass as an orphaned profile holder.
        if super::shutdown_requested() {
            bail!("Chrome launch cancelled because the gateway is shutting down");
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            bail!(
                "Chrome did not finish starting within {}s — a previous Phoenix browser may still hold \
the profile lock. Run `phoenix restart` (or close stray Chrome windows) and try again.",
                LAUNCH_BUDGET.as_secs()
            );
        }
        match rx.recv_timeout(remaining.min(Duration::from_millis(100))) {
            Ok(Ok(browser)) => return Ok(browser),
            Ok(Err(error)) => bail!("Chrome failed to start: {error}"),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                bail!("Chrome launch worker stopped before reporting readiness")
            }
        }
    }
}

/// SIGKILL any process whose command line launched Chrome against this exact
/// profile dir. Matched strictly by the full `--user-data-dir=<path>` string, so
/// only Phoenix's own Chromium copies are ever hit — never the user's real
/// Chrome (which runs on a different profile dir). Linux-only (`/proc`); a no-op
/// elsewhere, where launch falls back to best-effort.
pub(super) fn kill_stale_profile_holders(profile_dir: &std::path::Path) {
    // The login handoff window (and its zygote/renderer children) matches the
    // `--user-data-dir` needle exactly like a stale chrome would — sweeping
    // now would SIGKILL the user's login mid-2FA. Launches are refused during
    // a handoff anyway; this guard is belt-and-braces for any path that
    // reaches the sweep directly.
    if super::handoff::active_for_profile(profile_dir) {
        tracing::info!("pre-launch sweep skipped: login handoff window is open");
        return;
    }
    let dir = profile_dir.display();
    let mut killed = false;
    #[cfg(target_os = "linux")]
    for pid in exact_profile_holder_pids_at(std::path::Path::new("/proc"), profile_dir, unsafe {
        libc::geteuid()
    }) {
        if signal_profile_holder(pid, profile_dir, libc::SIGKILL) {
            tracing::info!("killed stale Phoenix Chrome pid {pid} holding {dir}");
            super::blog(&format!(
                "killed stale Phoenix Chrome pid {pid} holding {dir} (pre-launch sweep)"
            ));
            killed = true;
        }
    }
    if killed {
        // Let the OS reap the process and release the singleton lock before we
        // relaunch on the same dir.
        std::thread::sleep(Duration::from_millis(400));
    }
}

pub(super) fn profile_holders(profile_dir: &std::path::Path) -> Option<String> {
    let dir = profile_dir.to_str()?;
    let needle = format!("--user-data-dir={dir}");
    let entries = std::fs::read_dir("/proc").ok()?;
    let mut holders = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str() else {
            continue;
        };
        if pid.parse::<u32>().is_err() {
            continue;
        }
        let Ok(raw) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let cmdline = String::from_utf8_lossy(&raw).replace('\0', " ");
        if !cmdline.contains(&needle) {
            continue;
        }
        let port = cmdline
            .split_whitespace()
            .find_map(|part| part.strip_prefix("--remote-debugging-port="))
            .unwrap_or("unknown");
        holders.push(format!("pid {pid} remote-debugging-port {port}"));
    }
    if holders.is_empty() {
        None
    } else {
        Some(holders.join("; "))
    }
}

/// Find only Chrome roots launched against Phoenix-owned profile directories.
/// The exact `--user-data-dir=` argument and same-UID check are both required;
/// the user's real browser profile is therefore outside the kill set even if
/// it uses the same Chrome binary.
#[cfg(target_os = "linux")]
pub(super) fn phoenix_profile_holder_pids_at(
    proc_root: &std::path::Path,
    browser_root: &std::path::Path,
    expected_uid: u32,
) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return Vec::new();
    };
    let mut pids = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        if pid_matches_phoenix_profile_at(proc_root, pid, browser_root, expected_uid) {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// Exact-profile counterpart used for one scoped browser. `starts_with` is
/// correct for a gateway-wide browser-root sweep, but would make `leaf-1`
/// accidentally match `leaf-10` during a targeted cleanup.
#[cfg(target_os = "linux")]
pub(super) fn exact_profile_holder_pids_at(
    proc_root: &std::path::Path,
    profile_dir: &std::path::Path,
    expected_uid: u32,
) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return Vec::new();
    };
    let mut pids = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        if pid_matches_exact_profile_at(proc_root, pid, profile_dir, expected_uid) {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    pids.dedup();
    pids
}

#[cfg(target_os = "linux")]
fn pid_matches_exact_profile_at(
    proc_root: &std::path::Path,
    pid: u32,
    profile_dir: &std::path::Path,
    expected_uid: u32,
) -> bool {
    use std::os::unix::fs::MetadataExt;

    let process = proc_root.join(pid.to_string());
    if process
        .metadata()
        .map(|metadata| metadata.uid() != expected_uid)
        .unwrap_or(true)
    {
        return false;
    }
    let Ok(raw) = std::fs::read(process.join("cmdline")) else {
        return false;
    };
    raw.split(|byte| *byte == 0)
        .filter_map(|argument| std::str::from_utf8(argument).ok())
        .filter_map(|argument| argument.strip_prefix("--user-data-dir="))
        .map(std::path::Path::new)
        .any(|profile| profile == profile_dir)
}

#[cfg(target_os = "linux")]
fn pid_matches_phoenix_profile_at(
    proc_root: &std::path::Path,
    pid: u32,
    browser_root: &std::path::Path,
    expected_uid: u32,
) -> bool {
    use std::os::unix::fs::MetadataExt;

    let process = proc_root.join(pid.to_string());
    if process
        .metadata()
        .map(|metadata| metadata.uid() != expected_uid)
        .unwrap_or(true)
    {
        return false;
    }
    let Ok(raw) = std::fs::read(process.join("cmdline")) else {
        return false;
    };
    raw.split(|byte| *byte == 0)
        .filter_map(|argument| std::str::from_utf8(argument).ok())
        .filter_map(|argument| argument.strip_prefix("--user-data-dir="))
        .map(std::path::Path::new)
        .any(|profile| profile.starts_with(browser_root))
}

#[cfg(target_os = "linux")]
fn phoenix_profile_holder_pids() -> Vec<u32> {
    phoenix_profile_holder_pids_at(
        std::path::Path::new("/proc"),
        &crate::config::phoenix_home().join("browser"),
        unsafe { libc::geteuid() },
    )
}

/// Open a stable kernel process handle before re-validating UID/cmdline, then
/// signal through that handle. If a PID exits and is reused anywhere in the
/// scan-to-signal window, the pidfd still refers to the original process and
/// can never hit the replacement.
#[cfg(target_os = "linux")]
fn signal_profile_holder(pid: u32, profile_root: &std::path::Path, signal: i32) -> bool {
    let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) } as i32;
    if pidfd < 0 {
        return false;
    }
    let verified =
        pid_matches_phoenix_profile_at(std::path::Path::new("/proc"), pid, profile_root, unsafe {
            libc::geteuid()
        });
    let signalled = verified
        && unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                pidfd,
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        } == 0;
    let _ = unsafe { libc::close(pidfd) };
    signalled
}

/// PIDFD-signalled variant for one exact disposable profile. Unlike the
/// gateway-wide sweep, a sibling such as `agent-coder-job-a` must never match
/// `agent-coder-job-ab`; validate the full `--user-data-dir=` argument again
/// after opening the stable kernel process handle.
#[cfg(target_os = "linux")]
fn signal_exact_profile_holder(pid: u32, profile_dir: &std::path::Path, signal: i32) -> bool {
    let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) } as i32;
    if pidfd < 0 {
        return false;
    }
    let verified =
        pid_matches_exact_profile_at(std::path::Path::new("/proc"), pid, profile_dir, unsafe {
            libc::geteuid()
        });
    let signalled = verified
        && unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                pidfd,
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        } == 0;
    let _ = unsafe { libc::close(pidfd) };
    signalled
}

#[cfg(not(target_os = "linux"))]
fn signal_exact_profile_holder(_pid: u32, _profile_dir: &std::path::Path, _signal: i32) -> bool {
    false
}

/// Force-close only Chromium roots that still hold this one Phoenix-owned
/// disposable profile. This path is deliberately independent of the in-memory
/// browser session lock: a timed-out action can keep that lock forever, but it
/// must not keep a worker profile/process alive after the worker is cancelled.
///
/// Returns true only after a final exact `/proc` scan confirms no holder
/// remains. Callers must not delete the profile directory if this returns
/// false.
pub(super) fn force_close_exact_profile_holders(
    profile_dir: &std::path::Path,
    deadline: std::time::Instant,
) -> bool {
    #[cfg(target_os = "linux")]
    {
        let scan = || {
            exact_profile_holder_pids_at(std::path::Path::new("/proc"), profile_dir, unsafe {
                libc::geteuid()
            })
        };
        let term_pids = scan();
        for pid in &term_pids {
            let _ = signal_exact_profile_holder(*pid, profile_dir, libc::SIGTERM);
        }
        let gentle_deadline = std::cmp::min(
            deadline,
            std::time::Instant::now() + Duration::from_millis(750),
        );
        while std::time::Instant::now() < gentle_deadline && !scan().is_empty() {
            std::thread::sleep(Duration::from_millis(20));
        }
        let kill_pids = scan();
        for pid in &kill_pids {
            let _ = signal_exact_profile_holder(*pid, profile_dir, libc::SIGKILL);
        }
        while std::time::Instant::now() < deadline && !scan().is_empty() {
            std::thread::sleep(Duration::from_millis(20));
        }
        scan().is_empty()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (profile_dir, deadline);
        // There is no PIDFD/proc identity fence on this target; do not claim
        // that an unverified process was stopped. The profile is retained.
        false
    }
}

#[cfg(not(target_os = "linux"))]
fn signal_profile_holder(_pid: u32, _profile_root: &std::path::Path, _signal: i32) -> bool {
    false
}

#[cfg(not(target_os = "linux"))]
fn phoenix_profile_holder_pids() -> Vec<u32> {
    Vec::new()
}

/// Start graceful termination even for a session whose mutex is held by an
/// abandoned action. The slot workers still perform the normal storage-flush
/// wait and drop/reap path in parallel.
pub(super) fn signal_phoenix_profile_holders_for_shutdown() {
    let browser_root = crate::config::phoenix_home().join("browser");
    for pid in phoenix_profile_holder_pids() {
        if signal_profile_holder(pid, &browser_root, libc::SIGTERM) {
            super::blog(&format!(
                "shutdown: signalled Phoenix-profile Chrome pid {pid} for graceful exit"
            ));
        }
    }
}

/// Last-resort gateway-exit fence. A wedged action can make its Session
/// unreachable through the slot lock; after the shared graceful deadline,
/// kill only the exact same-UID Phoenix profile holders so no orphaned Chrome
/// overlaps the replacement gateway or keeps its profile lock.
pub(super) fn force_close_phoenix_profile_holders(deadline: std::time::Instant) {
    let browser_root = crate::config::phoenix_home().join("browser");
    let pids = phoenix_profile_holder_pids();
    for pid in &pids {
        if signal_profile_holder(*pid, &browser_root, libc::SIGKILL) {
            super::blog(&format!(
                "shutdown: force-terminated unresponsive Phoenix-profile Chrome pid {pid}"
            ));
        }
    }
    for pid in pids {
        while !chrome_exited(pid) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// SIGTERM a chrome process and wait (bounded) for it to exit, so Chrome runs
/// its graceful shutdown and FLUSHES cookies + localStorage to the profile
/// before we relaunch. This is the fix for "logins never persist": headless_
/// chrome's `TemporaryProcess::drop` (and our `kill_stale_profile_holders`)
/// SIGKILL Chrome, which loses everything not yet flushed — including the
/// localStorage token Discord keeps its session in, so a login the user just
/// completed in the visible window evaporated the moment we dropped that chrome
/// to flip back to headless. A cleanly-closed profile persists logins exactly
/// like a normal browser. Best-effort, Linux /proc; a dead pid returns fast.
pub(super) fn sigterm_and_wait(pid: u32) -> bool {
    sigterm_and_wait_until(pid, std::time::Instant::now() + Duration::from_secs(5))
}

/// Deadline-sharing form used by gateway shutdown so every browser instance
/// consumes the same global drain budget instead of receiving a fresh five
/// seconds after it finally acquires its slot.
pub(super) fn sigterm_and_wait_until(pid: u32, deadline: std::time::Instant) -> bool {
    #[cfg(unix)]
    let _ = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    #[cfg(not(unix))]
    let _ = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status();
    while std::time::Instant::now() < deadline {
        if chrome_exited(pid) {
            return true; // exited — storage flushed
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        std::thread::sleep(remaining.min(Duration::from_millis(100)));
    }
    chrome_exited(pid)
}

/// Has this chrome finished exiting? Chrome is OUR child process, and nothing
/// reaps it until `TemporaryProcess::drop` runs `wait()` — so after a clean
/// SIGTERM exit it lingers as a ZOMBIE and `/proc/<pid>` still exists. Checking
/// bare existence made every successful flush look like a timeout. A zombie
/// (state `Z` in /proc/<pid>/stat) HAS exited: its storage is flushed; drop's
/// later kill()+wait() merely reaps it.
fn chrome_exited(pid: u32) -> bool {
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(s) => s,
        Err(error) => return error.kind() == std::io::ErrorKind::NotFound,
    };
    // State is the first field after the parenthesised comm (which may itself
    // contain spaces/parens — split on the LAST ')').
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .is_some_and(|state| matches!(state, "Z" | "X" | "x"))
}

/// Remove Chrome's singleton guard files so a relaunch on this profile starts a
/// fresh instance instead of trying to hand off to a (now dead) one.
pub(super) fn clear_singletons(profile_dir: &std::path::Path) {
    for file in ["SingletonLock", "SingletonCookie", "SingletonSocket"] {
        std::fs::remove_file(profile_dir.join(file)).ok();
    }
}

/// First open page tab (http/about) in an attached browser, if any.
/// The `--remote-debugging-port` of the (single) chrome still holding this
/// profile dir, by scanning /proc. Used to RECONNECT to a live chrome whose CDP
/// websocket died, instead of killing it. Skips renderer/zygote/gpu helper
/// processes (they share the cmdline but aren't the browser) by requiring the
/// process to NOT be a `--type=` child.
pub(super) fn chrome_debug_port(profile_dir: &std::path::Path) -> Option<u16> {
    let dir = profile_dir.to_str()?;
    let needle = format!("--user-data-dir={dir}");
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str() else { continue };
        if pid.parse::<u32>().is_err() {
            continue;
        }
        let Ok(raw) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let cmdline = String::from_utf8_lossy(&raw).replace('\0', " ");
        if !cmdline.contains(&needle) || cmdline.contains("--type=") {
            continue; // not our profile, or a helper child process
        }
        if let Some(port) = cmdline
            .split_whitespace()
            .find_map(|part| part.strip_prefix("--remote-debugging-port="))
            .and_then(|p| p.parse::<u16>().ok())
        {
            return Some(port);
        }
    }
    None
}

/// Reconnect to a chrome process that is still alive but whose CDP websocket died
/// (the common idle-stale case). Returns a fresh Session driving the SAME chrome —
/// page, tabs, and cookies intact — so a transport death never has to kill the
/// browser the user is watching. None if no live chrome is reachable on this
/// profile (then the caller relaunches). The reconnected Browser is in connect
/// mode (does not own the process), so dropping it later won't kill chrome.
pub(super) fn reconnect_to_live_chrome(
    profile_dir: &std::path::Path,
    prev_target: Option<&str>,
    // The same chrome process the dying session launched — its visibility is
    // whatever that session's was.
    headless: bool,
    owned_pid: Option<u32>,
    surface_token: Option<String>,
) -> Option<Session> {
    let port = chrome_debug_port(profile_dir)?;
    let ws = resolve_ws_url(&port.to_string()).ok()?;
    // IDLE_SOCKET_BUDGET, not 5s: this Duration is the new connection's IDLE
    // timeout — 5s meant the reconnected socket died again before the first
    // 20s keepalive ping, so every reconnect-mode session churned.
    let browser = Browser::connect_with_timeout(ws.clone(), IDLE_SOCKET_BUDGET).ok()?;
    // A freshly-connected Browser only knows the targets it opened itself; the
    // tabs chrome already had (the user's real page — the whole point of
    // reconnecting) are absent from get_tabs() until we enumerate them. Without
    // this, prev_target is never found and we land on a stray about:blank ("went
    // back to twitter"). register_missing_tabs() runs Target.getTargets and adds
    // every pre-existing page as a usable Tab.
    browser.register_missing_tabs();
    // Reconnect to the SAME tab by its stable CDP target id (chrome-side, survives
    // the reconnect) — picking "the first http tab" grabbed a stray about:blank.
    let previous_tab = prev_target.and_then(|id| {
        browser
            .get_tabs()
            .lock()
            .ok()?
            .iter()
            .find(|t| t.get_target_id() == id)
            .cloned()
    });
    // A native --app window has one mounted WebContents. If that exact target
    // disappeared, choosing another existing page (or creating a fresh one)
    // would make the agent control a hidden root window while the user keeps
    // looking at the stale mounted pane. Fail closed so the caller relaunches
    // and remounts a single verified window instead.
    let tab = match previous_tab {
        Some(tab) => tab,
        None if surface_token.is_some() => {
            super::blog(
                "reconnect: mounted native target disappeared — refusing to adopt a hidden target; clean relaunch required",
            );
            return None;
        }
        None => existing_page_tab(&browser).or_else(|| browser.new_tab().ok())?,
    };
    super::blog(&format!(
        "reconnect: attached to live chrome on port {port}, tab {} ({})",
        tab.get_target_id(),
        if prev_target.is_some_and(|id| tab.get_target_id() == id) {
            "SAME tab as before the drop"
        } else {
            "previous tab not found — picked/opened another"
        }
    ));
    tab.set_default_timeout(Duration::from_secs(20));
    let console = attach_console_listener(&tab);
    attach_dialog_handler(&tab);
    let crashed = attach_crash_detector(&tab);
    spawn_keepalive(&tab);
    Some(Session {
        _browser: browser,
        tab,
        owned_pid,
        surface_token,
        crashed,
        // The caller (mod.rs transport-recovery) stamps the real instance and
        // re-arms the screencast before storing the session in its slot.
        instance: String::new(),
        headless,
        console,
        backend_ids: std::sync::Mutex::new(std::collections::HashMap::new()),
        last_state_hash: std::sync::atomic::AtomicU64::new(0),
        health: BrowserHealth {
            mode: "reconnect",
            attach: Some(ws),
            user_data_dir: Some(profile_dir.to_path_buf()),
            binary: None,
            login_source: String::new(),
            notes: vec!["reconnected to the live chrome after a transport drop".to_string()],
        },
    })
}

pub(super) fn existing_page_tab(browser: &Browser) -> Option<Arc<Tab>> {
    let tabs = browser.get_tabs().lock().ok()?.clone();
    tabs.into_iter().find(|t| {
        let url = t.get_url();
        url.starts_with("http") || url.starts_with("about:")
    })
}

/// The tab's URL, preferring the LIVE location when the cached value looks stale.
/// `tab.get_url()` is a cache updated from CDP events; right after a reconnect to a
/// live chrome it can still read `about:blank` even though the page is loaded —
/// reporting that would make the agent needlessly re-navigate ("went back to
/// twitter"). Only eval when the cache is suspicious, so normal calls stay cheap.
pub(super) fn real_url(tab: &Tab) -> String {
    let cached = tab.get_url();
    if cached.is_empty() || cached == "about:blank" {
        if let Ok(live) = eval_string(tab, "location.href") {
            if !live.is_empty() && live != "about:blank" {
                return live;
            }
        }
    }
    cached
}

pub(super) fn eval_string(tab: &Tab, expr: &str) -> Result<String> {
    let obj = tab.evaluate(expr, false)?;
    match obj.value {
        Some(Value::String(s)) => Ok(s),
        Some(other) => Ok(other.to_string()),
        None => Ok(String::new()),
    }
}

pub(super) fn press_keys(tab: &Tab, keys: &str) -> Result<()> {
    if let Some((mods, key)) = parse_combo(keys) {
        tab.press_key_with_modifiers(&key, Some(&mods))?;
        return Ok(());
    }
    let canonical = canonical_special_key(keys).unwrap_or(keys);
    // send_keys is for SPECIAL keys ONLY — never for typing text. A whole
    // sentence errored ("Key not found: <sentence>"), but a single printable
    // char like "d" slipped through, so a coworker typed an entire prompt ONE
    // CHARACTER PER CALL — ~200 rounds, ~15s of grok each, ~50 minutes to type
    // one line (2026-07-15). Reject anything that isn't a recognized special
    // key and point at browser_input, which types the whole string in one call.
    if !is_special_key(canonical) {
        anyhow::bail!(
            "`{keys}` is not a special key, so send_keys will not press it. \
             send_keys is ONLY for special keys (Enter, Tab, Escape, Backspace, \
             Delete, ArrowUp/Down/Left/Right, Home, End, PageUp, PageDown) and \
             combos (Ctrl+A, Shift+Tab). To type TEXT — even a single character — \
             use `browser_input` with the field's index; it types the entire string \
             in ONE call. NEVER type character by character."
        );
    }
    tab.press_key(canonical)?;
    Ok(())
}

/// CDP's key table is case-sensitive even though Phoenix's public tool and old
/// taught routines have historically accepted uppercase spellings. Canonicalize
/// at the engine boundary so `BACKSPACE`, `Backspace`, and `backspace` all press
/// the same real key instead of surfacing "Key not found" in the browser UI.
fn canonical_special_key(key: &str) -> Option<&'static str> {
    match key.trim().to_ascii_uppercase().as_str() {
        "ENTER" | "RETURN" => Some("Enter"),
        "TAB" => Some("Tab"),
        "ESC" | "ESCAPE" => Some("Escape"),
        "BACKSPACE" => Some("Backspace"),
        "DELETE" => Some("Delete"),
        "SPACE" => Some("Space"),
        "ARROWUP" | "UP" => Some("ArrowUp"),
        "ARROWDOWN" | "DOWN" => Some("ArrowDown"),
        "ARROWLEFT" | "LEFT" => Some("ArrowLeft"),
        "ARROWRIGHT" | "RIGHT" => Some("ArrowRight"),
        "HOME" => Some("Home"),
        "END" => Some("End"),
        "PAGEUP" => Some("PageUp"),
        "PAGEDOWN" => Some("PageDown"),
        "INSERT" => Some("Insert"),
        "F1" => Some("F1"),
        "F2" => Some("F2"),
        "F3" => Some("F3"),
        "F4" => Some("F4"),
        "F5" => Some("F5"),
        "F6" => Some("F6"),
        "F7" => Some("F7"),
        "F8" => Some("F8"),
        "F9" => Some("F9"),
        "F10" => Some("F10"),
        "F11" => Some("F11"),
        "F12" => Some("F12"),
        _ => None,
    }
}

/// Keys `send_keys` is allowed to press. Everything else is text → browser_input.
fn is_special_key(k: &str) -> bool {
    const SPECIAL: &[&str] = &[
        "Enter",
        "Return",
        "Tab",
        "Escape",
        "Esc",
        "Backspace",
        "Delete",
        "Space",
        "ArrowUp",
        "ArrowDown",
        "ArrowLeft",
        "ArrowRight",
        "Up",
        "Down",
        "Left",
        "Right",
        "Home",
        "End",
        "PageUp",
        "PageDown",
        "Insert",
        "F1",
        "F2",
        "F3",
        "F4",
        "F5",
        "F6",
        "F7",
        "F8",
        "F9",
        "F10",
        "F11",
        "F12",
    ];
    SPECIAL.iter().any(|s| s.eq_ignore_ascii_case(k))
}

pub(super) fn parse_combo(keys: &str) -> Option<(Vec<ModifierKey>, String)> {
    if !keys.contains('+') {
        return None;
    }
    let parts: Vec<&str> = keys.split('+').map(str::trim).collect();
    let (mods_in, key) = parts.split_at(parts.len() - 1);
    let mut mods = Vec::new();
    for m in mods_in {
        match m.to_ascii_lowercase().as_str() {
            "control" | "ctrl" => mods.push(ModifierKey::Ctrl),
            "alt" => mods.push(ModifierKey::Alt),
            "shift" => mods.push(ModifierKey::Shift),
            "meta" | "cmd" | "command" => mods.push(ModifierKey::Meta),
            _ => {}
        }
    }
    Some((mods, key[0].to_string()))
}

#[cfg(test)]
mod key_name_tests {
    use super::canonical_special_key;

    #[test]
    fn legacy_uppercase_keys_are_canonicalized_for_cdp() {
        assert_eq!(canonical_special_key("BACKSPACE"), Some("Backspace"));
        assert_eq!(canonical_special_key("delete"), Some("Delete"));
        assert_eq!(canonical_special_key("ARROWDOWN"), Some("ArrowDown"));
        assert_eq!(canonical_special_key("Escape"), Some("Escape"));
        assert_eq!(canonical_special_key("ordinary text"), None);
    }
}

pub(super) fn str_arg(input: &Value, key: &str) -> Result<String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .with_context(|| format!("missing required string argument: {key}"))
}

pub(super) fn int_arg(input: &Value, key: &str) -> Result<i64> {
    input
        .get(key)
        .and_then(Value::as_i64)
        .with_context(|| format!("missing required integer argument: {key}"))
}

pub(super) fn js_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

pub(super) fn normalize_url(url: String) -> String {
    if url.starts_with("http://")
        || url.starts_with("https://")
        || url.starts_with("about:")
        || url.starts_with("file://")
        || url.starts_with("chrome://")
        || url.starts_with("data:")
    {
        url
    } else {
        format!("https://{url}")
    }
}

pub(super) fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub(super) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}\n…(truncated)")
    }
}

pub(super) fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() > 120 {
        let t: String = line.chars().take(120).collect();
        format!("{t}...")
    } else {
        line.to_string()
    }
}

pub(super) fn sleep_secs(secs: u64) {
    std::thread::sleep(Duration::from_secs(secs));
}

static ARTIFACT_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(super) fn validate_png_magic(bytes: &[u8]) -> Result<()> {
    const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if !bytes.starts_with(PNG_SIGNATURE) {
        bail!("Chrome returned data without a PNG signature");
    }
    Ok(())
}

pub(super) fn validate_pdf_magic(bytes: &[u8]) -> Result<()> {
    if !bytes.starts_with(b"%PDF-") {
        bail!("Chrome returned data without a PDF signature");
    }
    Ok(())
}

struct StagedArtifact(std::path::PathBuf);

impl Drop for StagedArtifact {
    fn drop(&mut self) {
        if !self.0.as_os_str().is_empty() {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

fn open_artifact_stage(target: &std::path::Path) -> Result<(StagedArtifact, std::fs::File)> {
    let parent = target.parent().unwrap_or_else(|| std::path::Path::new("."));
    for _ in 0..128 {
        let sequence = ARTIFACT_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = parent.join(format!(
            ".phx-artifact-{}-{stamp}-{sequence}.tmp",
            std::process::id()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        match options.open(&path) {
            Ok(file) => {
                let metadata = file.metadata().with_context(|| {
                    format!("failed to inspect staged artifact {}", path.display())
                })?;
                if !metadata.is_file() {
                    bail!("staged artifact is not a regular file: {}", path.display());
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::{MetadataExt, PermissionsExt};
                    if metadata.uid() != unsafe { libc::geteuid() } || metadata.nlink() != 1 {
                        bail!(
                            "refusing unsafe staged artifact {} (owner/link count mismatch)",
                            path.display()
                        );
                    }
                    file.set_permissions(std::fs::Permissions::from_mode(0o600))
                        .with_context(|| {
                            format!("failed to secure staged artifact {}", path.display())
                        })?;
                }
                return Ok((StagedArtifact(path), file));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to stage artifact beside {}", target.display())
                });
            }
        }
    }
    bail!(
        "could not allocate a unique staging file beside {}",
        target.display()
    )
}

fn validate_existing_artifact_target(path: &std::path::Path) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "refusing to replace non-regular or symlink artifact target {}",
            path.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.nlink() != 1 {
            bail!(
                "refusing unsafe artifact target {} (owner/link count mismatch)",
                path.display()
            );
        }
    }
    Ok(())
}

fn sync_artifact_parent(path: &std::path::Path) {
    if let Some(parent) = path.parent() {
        if let Ok(directory) = std::fs::File::open(parent) {
            let _ = directory.sync_all();
        }
    }
}

/// Atomically replace an explicit output without following a symlink target.
/// The old file remains intact until the fully-written, synced 0600 staging
/// inode is renamed over it.
pub(crate) fn atomic_write_artifact_output(path: &std::path::Path, contents: &[u8]) -> Result<()> {
    use std::io::Write as _;

    if path.file_name().is_none() {
        bail!("artifact output must name a file: {}", path.display());
    }
    crate::config::private_io::prepare_private_parent(path)?;
    validate_existing_artifact_target(path)?;
    let (mut staged, mut file) = open_artifact_stage(path)?;
    file.write_all(contents)
        .with_context(|| format!("failed to stage artifact {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync staged artifact {}", path.display()))?;
    drop(file);
    // Re-check after the potentially long write. A final-component symlink
    // appearing after this check is still replaced by rename, never followed.
    validate_existing_artifact_target(path)?;
    std::fs::rename(&staged.0, path)
        .with_context(|| format!("failed to atomically publish artifact {}", path.display()))?;
    staged.0 = std::path::PathBuf::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to secure artifact {}", path.display()))?;
    }
    sync_artifact_parent(path);
    Ok(())
}

fn atomic_create_artifact_output(path: &std::path::Path, contents: &[u8]) -> Result<bool> {
    use std::io::Write as _;

    crate::config::private_io::prepare_private_parent(path)?;
    let (mut staged, mut file) = open_artifact_stage(path)?;
    file.write_all(contents)
        .with_context(|| format!("failed to stage artifact {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync staged artifact {}", path.display()))?;
    drop(file);
    match std::fs::hard_link(&staged.0, path) {
        Ok(()) => {
            std::fs::remove_file(&staged.0).with_context(|| {
                format!("failed to remove artifact stage {}", staged.0.display())
            })?;
            staged.0 = std::path::PathBuf::new();
            sync_artifact_parent(path);
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error)
            .with_context(|| format!("failed to atomically create artifact {}", path.display())),
    }
}

fn validate_artifact_extension(ext: &str) -> Result<()> {
    if ext.is_empty()
        || ext.len() > 12
        || !ext
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        bail!("invalid browser artifact extension: {ext:?}");
    }
    Ok(())
}

fn secure_artifact_directory(dir: &std::path::Path) -> Result<()> {
    // The synthetic child lets the shared no-symlink directory preparer create
    // `dir` without ever creating a probe file.
    crate::config::private_io::prepare_private_parent(&dir.join(".artifact-probe"))?;
    let metadata = std::fs::symlink_metadata(dir)
        .with_context(|| format!("failed to inspect artifact directory {}", dir.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("unsafe browser artifact directory: {}", dir.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() } {
            bail!(
                "browser artifact directory is not owned by the current user: {}",
                dir.display()
            );
        }
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("failed to secure artifact directory {}", dir.display()))?;
    }
    Ok(())
}

fn artifact_candidate(
    dir: &std::path::Path,
    ext: &str,
    stamp: u128,
    process_id: u32,
    sequence: u64,
) -> std::path::PathBuf {
    dir.join(format!("artifact-{stamp}-{process_id}-{sequence}.{ext}"))
}

fn write_unique_browser_artifact_in(
    dir: &std::path::Path,
    ext: &str,
    contents: &[u8],
    stamp: u128,
    sequence: &std::sync::atomic::AtomicU64,
) -> Result<std::path::PathBuf> {
    validate_artifact_extension(ext)?;
    secure_artifact_directory(dir)?;
    for _ in 0..128 {
        let next = sequence.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = artifact_candidate(dir, ext, stamp, std::process::id(), next);
        if atomic_create_artifact_output(&path, contents)? {
            return Ok(path);
        }
    }
    bail!(
        "could not allocate a collision-free browser artifact below {}",
        dir.display()
    )
}

pub(super) fn write_unique_browser_artifact(
    ext: &str,
    contents: &[u8],
) -> Result<std::path::PathBuf> {
    let dir = crate::config::phoenix_home().join("browser/artifacts");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    write_unique_browser_artifact_in(&dir, ext, contents, stamp, &ARTIFACT_SEQUENCE)
}

pub(super) fn home_join(rel: &str) -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
        .join(rel)
}

pub(super) fn shellexpand(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        return home_join(rest).to_string_lossy().into_owned();
    }
    p.to_string()
}

/// One-shot, ISOLATED screenshot of a URL — the design lanes' self-serve
/// slop-gate eye (2026-07-16: Iris once restyled blind for 30+ min because
/// the old browser worker owned the only browser). Launches a throwaway
/// headless chrome on a temp profile — never touches a coworker's private
/// browser, its logins, or its lane lock —
/// navigates, waits for render, captures, and tears everything down.
pub(crate) fn snap_url(
    url: &str,
    width: u32,
    height: u32,
    full_page: bool,
    wait_ms: u64,
) -> Result<Vec<u8>> {
    if !(320..=3_840).contains(&width) || !(480..=2_400).contains(&height) {
        bail!(
            "ui_snap viewport {width}x{height} is outside the safe 320..=3840 by 480..=2400 range"
        );
    }
    if wait_ms > 15_000 {
        bail!("ui_snap wait {wait_ms}ms exceeds the 15000ms cap");
    }
    let profile_guard = tempfile::Builder::new()
        .prefix("phx-uisnap-")
        .tempdir()
        .context("failed to create the isolated ui_snap profile")?;
    let profile = profile_guard.path().to_path_buf();
    let mut builder = LaunchOptionsBuilder::default();
    builder
        .headless(true)
        .sandbox(false)
        // Same WebGL story as the main lane: a design check that can't render
        // 3D/canvas content grades a blank box instead of the page.
        .enable_gpu(true)
        .args(vec![
            std::ffi::OsStr::new("--use-gl=angle"),
            std::ffi::OsStr::new("--use-angle=gl"),
        ])
        .window_size(Some((width, height)))
        .idle_browser_timeout(Duration::from_secs(60))
        .user_data_dir(Some(profile.clone()));
    if let Some(path) = which_chrome() {
        builder.path(Some(path));
    }
    let options = builder.build().map_err(|e| anyhow::anyhow!(e))?;
    let browser = bounded_browser_launch(options)?;
    let tab = first_browser_tab(&browser).context("ui_snap: no tab after launch")?;
    tab.set_default_timeout(Duration::from_secs(30));
    tab.navigate_to(url)?;
    tab.wait_until_navigated()?;
    // Let client-side rendering + entry animations settle before looking.
    std::thread::sleep(Duration::from_millis(wait_ms));
    let png = tab.capture_screenshot(CaptureScreenshotFormatOption::Png, None, None, !full_page)?;
    drop(tab);
    drop(browser);
    drop(profile_guard);
    Ok(png)
}

/// Adopt Chrome's launch tab without using headless_chrome's deprecated
/// `wait_for_initial_tab`. The crate recommends `new_tab`, but doing that
/// leaves Chrome's own launch tab beside ours. Polling the public tab list
/// preserves the verified one-tab contract and falls back only when Chrome
/// genuinely failed to publish its initial target.
pub(super) fn first_browser_tab(browser: &Browser) -> Result<Arc<Tab>> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let first = browser
            .get_tabs()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .first()
            .cloned();
        if let Some(tab) = first {
            return Ok(tab);
        }
        if Instant::now() >= deadline {
            return browser
                .new_tab()
                .context("Chrome did not publish an initial tab; fallback tab creation failed");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub(super) fn which_chrome() -> Option<std::path::PathBuf> {
    std::env::var_os("PATH")
        .as_deref()
        .and_then(find_chrome_in_path)
}

const CHROME_CANDIDATES: [&str; 4] = [
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
];
const MAX_BROWSER_PATH_BYTES: usize = 64 * 1024;
const MAX_BROWSER_PATH_COMPONENTS: usize = 256;
const MAX_BROWSER_PATH_COMPONENT_BYTES: usize = 4096;

fn find_chrome_in_path(path: &std::ffi::OsStr) -> Option<std::path::PathBuf> {
    if path.len() > MAX_BROWSER_PATH_BYTES {
        return None;
    }

    let mut directories = Vec::new();
    for (index, directory) in std::env::split_paths(path).enumerate() {
        if index >= MAX_BROWSER_PATH_COMPONENTS {
            break;
        }
        // An empty PATH field conventionally means the current directory.
        // Phoenix deliberately rejects it: a gateway must not select a
        // workspace-provided executable merely because PATH contains `::`.
        if directory.as_os_str().is_empty()
            || directory.as_os_str().len() > MAX_BROWSER_PATH_COMPONENT_BYTES
        {
            continue;
        }
        // Directory symlinks are allowed because /bin and managed toolchains
        // commonly use them. Resolution must still end at a directory; FIFOs,
        // sockets, devices, and dangling links are rejected without opening.
        if !std::fs::metadata(&directory).is_ok_and(|metadata| metadata.is_dir()) {
            continue;
        }
        directories.push(directory);
    }

    // Preserve the old discovery order exactly: browser name priority wins
    // over PATH directory priority (google-chrome anywhere precedes chromium).
    for candidate in CHROME_CANDIDATES {
        for directory in &directories {
            let path = directory.join(candidate);
            if is_executable_browser_file(&path) {
                return Some(path);
            }
        }
    }
    None
}

fn is_executable_browser_file(path: &std::path::Path) -> bool {
    let link_metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return false,
    };
    // Final-component symlinks are explicitly allowed: distro Chrome
    // launchers are routinely symlinked. Follow only for metadata and accept
    // the result solely when it is a regular executable file. No candidate is
    // opened, so a FIFO/device/socket cannot block discovery.
    let metadata = if link_metadata.file_type().is_symlink() {
        match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(_) => return false,
        }
    } else {
        link_metadata
    };
    if !metadata.is_file() {
        return false;
    }
    is_executable_metadata(&metadata)
}

#[cfg(unix)]
fn is_executable_metadata(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable_metadata(_metadata: &std::fs::Metadata) -> bool {
    true
}

#[cfg(all(test, unix))]
mod artifact_tests {
    use super::*;
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};

    #[test]
    fn returned_capture_magic_is_strict() {
        assert!(validate_png_magic(b"\x89PNG\r\n\x1a\nrest").is_ok());
        assert!(validate_png_magic(b"not png").is_err());
        assert!(validate_pdf_magic(b"%PDF-1.7\n").is_ok());
        assert!(validate_pdf_magic(b"not pdf").is_err());
    }

    #[test]
    fn unique_artifacts_are_private_and_preserve_collisions() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("artifacts");
        secure_artifact_directory(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        let stamp = 123u128;
        let sequence = std::sync::atomic::AtomicU64::new(0);
        let collision = artifact_candidate(&dir, "png", stamp, std::process::id(), 0);
        std::fs::write(&collision, b"keep me").unwrap();
        let created =
            write_unique_browser_artifact_in(&dir, "png", b"new bytes", stamp, &sequence).unwrap();

        assert_ne!(created, collision);
        assert_eq!(std::fs::read(&collision).unwrap(), b"keep me");
        assert_eq!(std::fs::read(&created).unwrap(), b"new bytes");
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let metadata = std::fs::metadata(&created).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        assert_eq!(metadata.nlink(), 1);
        assert_eq!(
            std::fs::read_dir(&dir)
                .unwrap()
                .flatten()
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
                .count(),
            0
        );
    }

    #[test]
    fn explicit_artifact_write_is_atomic_private_and_refuses_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("download.bin");
        std::fs::write(&target, b"old").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        atomic_write_artifact_output(&target, b"complete replacement").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"complete replacement");
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let external = root.path().join("external");
        let linked = root.path().join("linked.bin");
        std::fs::write(&external, b"outside").unwrap();
        symlink(&external, &linked).unwrap();
        assert!(atomic_write_artifact_output(&linked, b"replacement").is_err());
        assert_eq!(std::fs::read(&external).unwrap(), b"outside");
        assert!(std::fs::symlink_metadata(&linked)
            .unwrap()
            .file_type()
            .is_symlink());
    }
}

#[cfg(all(test, unix))]
mod chrome_path_tests {
    use super::*;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    fn executable(path: &std::path::Path) {
        std::fs::write(path, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn injected_path_preserves_browser_name_then_directory_order() {
        let dir = tempfile::tempdir().unwrap();
        let early = dir.path().join("early");
        let late = dir.path().join("late");
        std::fs::create_dir_all(&early).unwrap();
        std::fs::create_dir_all(&late).unwrap();
        executable(&early.join("chromium"));
        executable(&late.join("google-chrome"));
        let path = std::env::join_paths([&early, &late]).unwrap();
        assert_eq!(find_chrome_in_path(&path), Some(late.join("google-chrome")));

        executable(&early.join("google-chrome"));
        assert_eq!(
            find_chrome_in_path(&path),
            Some(early.join("google-chrome"))
        );
    }

    #[test]
    fn injected_path_accepts_executable_symlink_but_rejects_special_inodes() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let real = dir.path().join("real-chrome");
        executable(&real);
        std::os::unix::fs::symlink(&real, bin.join("google-chrome")).unwrap();
        let path = std::env::join_paths([&bin]).unwrap();
        assert_eq!(find_chrome_in_path(&path), Some(bin.join("google-chrome")));

        std::fs::remove_file(bin.join("google-chrome")).unwrap();
        let fifo = bin.join("google-chrome");
        let fifo_c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o700) }, 0);
        let started = std::time::Instant::now();
        assert_eq!(find_chrome_in_path(&path), None);
        assert!(started.elapsed() < Duration::from_secs(1));

        std::fs::remove_file(&fifo).unwrap();
        let special = dir.path().join("special.pipe");
        let special_c = CString::new(special.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(special_c.as_ptr(), 0o700) }, 0);
        std::os::unix::fs::symlink(&special, bin.join("google-chrome")).unwrap();
        assert_eq!(find_chrome_in_path(&path), None);
    }

    #[test]
    fn injected_path_rejects_non_executable_empty_and_out_of_bound_components() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        std::fs::write(bin.join("google-chrome"), b"not executable").unwrap();
        let path = std::env::join_paths([&bin]).unwrap();
        assert_eq!(find_chrome_in_path(&path), None);
        assert_eq!(find_chrome_in_path(std::ffi::OsStr::new("")), None);

        executable(&bin.join("chromium"));
        let mut components = vec![dir.path().join("missing"); MAX_BROWSER_PATH_COMPONENTS];
        components.push(bin);
        let path = std::env::join_paths(components).unwrap();
        assert_eq!(find_chrome_in_path(&path), None);
    }
}
