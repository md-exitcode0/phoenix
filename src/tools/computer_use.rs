//! computer_* tools — desktop control for the computer_use agent.
//!
//! Donor doctrine: open-computer-use (serial executor, unified action
//! vocabulary, hard floor below approvals). The execution backends:
//!
//! - **GNOME/Wayland (primary):** the bundled `phoenix-cursor@phoenix.dev`
//!   shell extension (desktop/gnome-extension/). It draws the separate
//!   animated agent cursor and injects input via compositor virtual devices;
//!   we drive it over DBus with `gdbus`. Synthetic events fire when the
//!   animated cursor ARRIVES at the target, so what the user sees is what
//!   actually happens. The user's own pointer is saved/restored.
//! - **X11 fallback:** `xdotool` (no overlay cursor; honest about it).
//!
//! All actions are serial by construction (one tool call at a time) and every
//! action's receipt is the tool output the orchestrator and gateway log see.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::{fd::AsRawFd, unix::process::CommandExt};

use super::{ToolCancellation, ToolOutput};

/// Desktop probes should normally finish in milliseconds. A missing DBus
/// daemon, wedged accessibility bridge, or hostile replacement executable
/// must not be able to pin the blocking tool worker indefinitely.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
/// `gdbus` and the AT-SPI helper return small JSON documents. Keep a generous
/// ceiling for large accessibility trees while making capture memory finite.
const COMMAND_STREAM_CAP: usize = 2 * 1024 * 1024;
const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(10);

const BUS: [&str; 6] = [
    "--session",
    "--dest",
    "dev.phoenix.Cursor",
    "--object-path",
    "/dev/phoenix/Cursor",
    "--method",
];

/// Whether the agent cursor has been deliberately placed (move, or an action
/// with explicit coordinates) since the gateway started. A coordinate-less
/// click/scroll/drag before any placement fires at an arbitrary stale spot —
/// reject it with guidance instead.
/// Placement is a property of one display, not of the gateway process.  A
/// global flag would let agent B issue a coordinate-less click on a fresh
/// desktop merely because agent A moved its cursor earlier.
static CURSOR_PLACED: std::sync::OnceLock<Mutex<std::collections::HashSet<String>>> =
    std::sync::OnceLock::new();

fn cursor_scope_key() -> String {
    crate::tools::isolated_desktop::current_scope_key()
        .unwrap_or_else(|| "host-desktop".to_string())
}

fn mark_cursor_placed() {
    let placed = CURSOR_PLACED.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    placed
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(cursor_scope_key());
}

fn require_cursor_placed(action: &str) -> Result<()> {
    let placed = CURSOR_PLACED.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    if !placed
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(&cursor_scope_key())
    {
        bail!(
            "{action} without coordinates, but the agent cursor has not been placed yet — \
             pass x/y from the LATEST screenshot, or computer_move first"
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    GnomeBridge,
    X11Tool,
}

fn run(cmd: &str, args: &[&str]) -> Result<String> {
    let output = run_bounded_command(
        cmd,
        args,
        COMMAND_TIMEOUT,
        COMMAND_STREAM_CAP,
        COMMAND_STREAM_CAP,
        None,
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if cmd == "xdotool" && stderr.contains("No such key name") {
        bail!("xdotool did not deliver the requested key: {}", stderr.trim());
    }
    match output.reason {
        CommandStopReason::Exited if output.status.success() && output.descendants_terminated == 0 => {
            Ok(stdout.trim().to_string())
        }
        CommandStopReason::Exited if output.status.success() => bail!(
            "{cmd} left {} background descendant(s); they were terminated and reaped before returning",
            output.descendants_terminated
        ),
        CommandStopReason::Exited => bail!("{cmd} failed: {}", stderr.trim()),
        CommandStopReason::TimedOut => bail!(
            "{cmd} timed out after {:.1}s; its process group was terminated and reaped",
            COMMAND_TIMEOUT.as_secs_f64()
        ),
        CommandStopReason::Cancelled => bail!(
            "{cmd} was cancelled; its process group was terminated and reaped"
        ),
        CommandStopReason::StdoutLimit => bail!(
            "{cmd} stdout exceeded the {} byte capture limit; its process group was terminated and reaped",
            COMMAND_STREAM_CAP
        ),
        CommandStopReason::StderrLimit => bail!(
            "{cmd} stderr exceeded the {} byte capture limit; its process group was terminated and reaped",
            COMMAND_STREAM_CAP
        ),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandStopReason {
    Exited,
    TimedOut,
    Cancelled,
    StdoutLimit,
    StderrLimit,
}

#[derive(Debug)]
struct BoundedCommandOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    reason: CommandStopReason,
    descendants_terminated: usize,
}

/// Execute one short-lived desktop helper with finite capture and an owned
/// process group. The direct child is deliberately observed without reaping
/// on Linux; that keeps its PGID reserved until same-group descendants are
/// terminated and prevents a background writer from holding our pipes open.
fn run_bounded_command(
    cmd: &str,
    args: &[&str],
    timeout: Duration,
    stdout_cap: usize,
    stderr_cap: usize,
    cancellation: Option<&ToolCancellation>,
) -> Result<BoundedCommandOutput> {
    if stdout_cap == 0 || stderr_cap == 0 {
        bail!("bounded command capture limits must be non-zero");
    }
    let mut command = Command::new(cmd);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Do not ever set DISPLAY globally. A scoped tool call gets an explicit
    // Xephyr/Xvfb environment for this one helper process; parallel agents
    // therefore cannot move/click/screenshot each other's desktops.
    if let Some(desktop) = crate::tools::isolated_desktop::current_environment()? {
        desktop.apply_to_command(&mut command);
    }
    #[cfg(unix)]
    command.process_group(0);

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn {cmd}"))?;
    let pid = i32::try_from(child.id()).context("desktop helper pid did not fit i32")?;
    let mut stdout = child
        .stdout
        .take()
        .context("desktop helper stdout pipe missing")?;
    let mut stderr = child
        .stderr
        .take()
        .context("desktop helper stderr pipe missing")?;
    if let Err(error) = set_capture_nonblocking(&stdout)
        .context("make desktop helper stdout nonblocking")
        .and_then(|_| {
            set_capture_nonblocking(&stderr).context("make desktop helper stderr nonblocking")
        })
    {
        let cleanup = terminate_owned_command(&mut child, pid);
        if cleanup.is_err() {
            if let Some(cancellation) = cancellation {
                cancellation.mark_unconfirmed();
            }
        }
        cleanup.context("terminate desktop helper after capture setup failure")?;
        return Err(error);
    }

    let started = Instant::now();
    let mut stdout_bytes = Vec::with_capacity(stdout_cap.min(64 * 1024));
    let mut stderr_bytes = Vec::with_capacity(stderr_cap.min(64 * 1024));
    let reason = loop {
        let stdout_limited = match drain_bounded_pipe(&mut stdout, &mut stdout_bytes, stdout_cap) {
            Ok(limited) => limited,
            Err(error) => {
                let cleanup = terminate_owned_command(&mut child, pid);
                if cleanup.is_err() {
                    if let Some(cancellation) = cancellation {
                        cancellation.mark_unconfirmed();
                    }
                }
                cleanup.context("terminate desktop helper after stdout capture failure")?;
                return Err(error).context("capture desktop helper stdout");
            }
        };
        let stderr_limited = match drain_bounded_pipe(&mut stderr, &mut stderr_bytes, stderr_cap) {
            Ok(limited) => limited,
            Err(error) => {
                let cleanup = terminate_owned_command(&mut child, pid);
                if cleanup.is_err() {
                    if let Some(cancellation) = cancellation {
                        cancellation.mark_unconfirmed();
                    }
                }
                cleanup.context("terminate desktop helper after stderr capture failure")?;
                return Err(error).context("capture desktop helper stderr");
            }
        };
        if stdout_limited {
            break CommandStopReason::StdoutLimit;
        }
        if stderr_limited {
            break CommandStopReason::StderrLimit;
        }
        if cancellation.is_some_and(ToolCancellation::is_requested) {
            break CommandStopReason::Cancelled;
        }
        if started.elapsed() >= timeout {
            break CommandStopReason::TimedOut;
        }
        match command_exited_without_reap(&mut child, pid) {
            Ok(true) => break CommandStopReason::Exited,
            Ok(false) => std::thread::sleep(COMMAND_POLL_INTERVAL),
            Err(error) => {
                let cleanup = terminate_owned_command(&mut child, pid);
                if cleanup.is_err() {
                    if let Some(cancellation) = cancellation {
                        cancellation.mark_unconfirmed();
                    }
                }
                cleanup.context("terminate desktop helper after status polling failure")?;
                return Err(error).context("poll desktop helper");
            }
        }
    };

    let (status, descendants_terminated) = match terminate_owned_command(&mut child, pid) {
        Ok(result) => result,
        Err(error) => {
            if let Some(cancellation) = cancellation {
                cancellation.mark_unconfirmed();
            }
            return Err(error)
                .context("desktop helper process-group termination was not confirmed");
        }
    };
    // Drain the finite bytes already buffered in the kernel after termination.
    // Reaching a cap here still takes precedence over an otherwise clean exit.
    let stdout_limited = drain_bounded_pipe(&mut stdout, &mut stdout_bytes, stdout_cap)
        .context("finish capturing desktop helper stdout")?;
    let stderr_limited = drain_bounded_pipe(&mut stderr, &mut stderr_bytes, stderr_cap)
        .context("finish capturing desktop helper stderr")?;
    let reason = if stdout_limited {
        CommandStopReason::StdoutLimit
    } else if stderr_limited {
        CommandStopReason::StderrLimit
    } else {
        reason
    };
    Ok(BoundedCommandOutput {
        status,
        stdout: stdout_bytes,
        stderr: stderr_bytes,
        reason,
        descendants_terminated,
    })
}

#[cfg(unix)]
fn set_capture_nonblocking(stream: &impl AsRawFd) -> Result<()> {
    let fd = stream.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error()).context("read capture pipe flags");
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error()).context("set capture pipe nonblocking");
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_capture_nonblocking(_stream: &impl Read) -> Result<()> {
    Ok(())
}

/// Drain at most `cap` bytes. One extra observed byte records overflow but is
/// never retained, so memory remains bounded even for an infinite producer.
fn drain_bounded_pipe(pipe: &mut impl Read, retained: &mut Vec<u8>, cap: usize) -> Result<bool> {
    let mut buffer = [0u8; 8192];
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) => return Ok(false),
            Ok(read) => {
                let remaining = cap.saturating_sub(retained.len());
                retained.extend_from_slice(&buffer[..read.min(remaining)]);
                if read > remaining {
                    return Ok(true);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) => return Err(error).context("read bounded capture pipe"),
        }
    }
}

#[cfg(target_os = "linux")]
fn command_exited_without_reap(_child: &mut std::process::Child, pid: i32) -> Result<bool> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result < 0 {
        return Err(std::io::Error::last_os_error()).context("waitid desktop helper");
    }
    Ok(info.si_signo == libc::SIGCHLD)
}

#[cfg(not(target_os = "linux"))]
fn command_exited_without_reap(child: &mut std::process::Child, _pid: i32) -> Result<bool> {
    Ok(child.try_wait().context("poll desktop helper")?.is_some())
}

#[cfg(target_os = "linux")]
fn live_command_group_members(pgid: i32) -> Result<Vec<i32>> {
    let mut members = Vec::new();
    for entry in std::fs::read_dir("/proc").context("scan /proc for desktop helper group")? {
        let entry = match entry {
            Ok(entry) => entry,
            // `/proc` is inherently racy: a PID can vanish after `read_dir`
            // yields it. Ignore only errors that prove that disappearance;
            // permission and I/O failures must keep cleanup unconfirmed.
            Err(error) if proc_entry_vanished(&error) => continue,
            Err(error) => return Err(error).context("read /proc entry"),
        };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        let stat = match std::fs::read_to_string(entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(error) if proc_entry_vanished(&error) => continue,
            Err(error) => return Err(error).context("read /proc process stat"),
        };
        let Some(after_name) = stat.rsplit_once(") ").map(|(_, rest)| rest) else {
            continue;
        };
        let mut fields = after_name.split_whitespace();
        let state = fields
            .next()
            .and_then(|field| field.as_bytes().first().copied());
        let _parent_pid = fields.next();
        let member_pgid = fields.next().and_then(|field| field.parse::<i32>().ok());
        if member_pgid == Some(pgid) && !matches!(state, Some(b'Z' | b'X')) {
            members.push(pid);
        }
    }
    Ok(members)
}

#[cfg(target_os = "linux")]
fn proc_entry_vanished(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::ENOENT) | Some(libc::ESRCH))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn live_command_group_members(pgid: i32) -> Result<Vec<i32>> {
    let result = unsafe { libc::kill(-pgid, 0) };
    if result == 0 {
        Ok(vec![pgid])
    } else if std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        Ok(Vec::new())
    } else {
        Err(std::io::Error::last_os_error()).context("probe desktop helper process group")
    }
}

#[cfg(unix)]
fn signal_command_group(pgid: i32, signal: i32) -> Result<()> {
    if pgid <= 1 || pgid == unsafe { libc::getpgrp() } {
        bail!("refusing to signal unsafe desktop helper process group {pgid}");
    }
    let actual = unsafe { libc::getpgid(pgid) };
    if actual != pgid {
        let error = std::io::Error::last_os_error();
        #[cfg(target_os = "linux")]
        if actual == -1
            && error.raw_os_error() == Some(libc::ESRCH)
            && live_command_group_members(pgid)?.is_empty()
        {
            return Ok(());
        }
        if actual == -1 {
            return Err(error).context(format!(
                "verify desktop helper process-group ownership for leader {pgid}"
            ));
        }
        bail!("desktop helper process group ownership changed: pid={pgid}, pgid={actual}");
    }
    if unsafe { libc::kill(-pgid, signal) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error).context("signal desktop helper process group")
    }
}

#[cfg(unix)]
fn terminate_owned_command(
    child: &mut std::process::Child,
    pgid: i32,
) -> Result<(ExitStatus, usize)> {
    let initial = live_command_group_members(pgid)?;
    let descendants = initial.iter().filter(|pid| **pid != pgid).count();
    if !initial.is_empty() {
        signal_command_group(pgid, libc::SIGTERM)?;
        let term_deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < term_deadline && !live_command_group_members(pgid)?.is_empty() {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !live_command_group_members(pgid)?.is_empty() {
            signal_command_group(pgid, libc::SIGKILL)?;
            let kill_deadline = Instant::now() + Duration::from_millis(500);
            while Instant::now() < kill_deadline && !live_command_group_members(pgid)?.is_empty() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    let remaining = live_command_group_members(pgid)?;
    if !remaining.is_empty() {
        bail!(
            "desktop helper process group still has live members: {}",
            remaining
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let status = child.wait().context("reap desktop helper leader")?;
    Ok((status, descendants))
}

#[cfg(not(unix))]
fn terminate_owned_command(
    child: &mut std::process::Child,
    _pid: i32,
) -> Result<(ExitStatus, usize)> {
    if child.try_wait().context("poll desktop helper")?.is_none() {
        child.kill().context("terminate desktop helper")?;
    }
    Ok((child.wait().context("reap desktop helper")?, 0))
}

/// Does the RUNNING bridge predate the layered-window methods? The loaded
/// extension only changes on a Wayland login, so introspect once per process.
/// The on-disk extension being newer than the loaded one was the root cause of
/// the Viber session: CaptureWindow missing, FocusWindow claiming ok without
/// raising — the agent acted blind for 40 rounds.
fn bridge_is_stale() -> bool {
    static STALE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *STALE.get_or_init(|| {
        run(
            "gdbus",
            &[
                "introspect",
                "--session",
                "--dest",
                "dev.phoenix.Cursor",
                "--object-path",
                "/dev/phoenix/Cursor",
            ],
        )
        .map(|xml| !xml.contains("CaptureWindow"))
        .unwrap_or(false)
    })
}

fn gdbus_call(method: &str, args: &[String]) -> Result<String> {
    let mut full: Vec<&str> = vec!["call"];
    full.extend(BUS);
    let method_full = format!("dev.phoenix.Cursor.{method}");
    full.push(&method_full);
    // Signed values (e.g. an upward Scroll delta) are GVariant parameters,
    // not gdbus options. End option parsing before passing any method args.
    full.push("--");
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    full.extend(arg_refs);
    run("gdbus", &full)
}

/// Format a free-text value as a GVariant **string literal** for `gdbus call`.
/// gdbus parses every argument as GVariant typed by the method signature, so a
/// bare `42` / `true` / `[x]` passed to a string-typed method (TypeText, Key,
/// Screenshot path) is read as an int/bool/array and errors or coerces wrong —
/// typing the literal text "42" would fail. Wrapping in quotes with C-style
/// escapes makes any text an unambiguous string. Numeric args (coords, ids)
/// stay bare so they parse as the int/uint the signature expects.
fn gvariant_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `gdbus call` prints a string return wrapped as `('<text>',)`. Pull the text
/// back out (best-effort) so the agent gets clean JSON, not GVariant syntax.
fn unwrap_gvariant_string(raw: &str) -> String {
    let trimmed = raw.trim();
    let inner = trimmed
        .strip_prefix("('")
        .and_then(|rest| rest.strip_suffix("',)"))
        .unwrap_or(trimmed);
    inner.replace("\\'", "'")
}

fn have(binary: &str) -> bool {
    executable_in_path(binary)
}

fn executable_in_path(binary: &str) -> bool {
    let candidate = std::path::Path::new(binary);
    if candidate.components().count() > 1 {
        return is_executable_file(candidate);
    }
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let dir = if dir.as_os_str().is_empty() {
            std::path::Path::new(".")
        } else {
            dir.as_path()
        };
        is_executable_file(&dir.join(binary))
    })
}

#[cfg(unix)]
fn is_executable_file(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &std::path::Path) -> bool {
    path.is_file()
}

const SETUP_HELP: &str = "Phoenix desktop bridge is not reachable. One-time setup on GNOME/Wayland:\n\
  1. cp -r phoenix_agent/desktop/gnome-extension/phoenix-cursor@phoenix.dev ~/.local/share/gnome-shell/extensions/\n\
     (install.sh does this automatically)\n\
  2. Log out and back in (Wayland reloads extensions on login), then:\n\
  3. gnome-extensions enable phoenix-cursor@phoenix.dev\n\
On X11 instead: install `xdotool` (sudo apt install xdotool).\n\
Report this setup requirement to the orchestrator — the user must do it once; do not retry until they have.";

/// Detect the working backend. Probed per call — cheap (one DBus ping) and a
/// user can enable the extension mid-session.
pub fn detect_backend() -> Result<Backend> {
    // A mesh agent with a DesktopScope is never allowed to fall through to
    // the host GNOME bridge. Ensure its own display first, then force the
    // deterministic X11 backend for xdotool/import/screenshot operations.
    if crate::tools::isolated_desktop::current_scope().is_some() {
        let desktop = crate::tools::isolated_desktop::current_environment()?
            .context("prepare this agent's isolated desktop")?;
        if desktop.native.is_some() {
            // The command runner applies this scope's private session bus;
            // this cannot fall through to the user's GNOME compositor.
            gdbus_call("Status", &[]).context("agent-owned GNOME cursor is unavailable")?;
            return Ok(Backend::GnomeBridge);
        }
        if have("xdotool") {
            return Ok(Backend::X11Tool);
        }
        bail!(
            "isolated desktop {} is ready, but xdotool is missing; install xdotool to control this agent's private display",
            desktop.display
        );
    }
    if gdbus_call("Status", &[]).is_ok() {
        return Ok(Backend::GnomeBridge);
    }
    if std::env::var("DISPLAY").is_ok() && have("xdotool") {
        return Ok(Backend::X11Tool);
    }
    Err(anyhow!(SETUP_HELP))
}

fn ok(summary: &str, content: String) -> Result<ToolOutput> {
    Ok(ToolOutput {
        summary: summary.to_string(),
        content,
    })
}

// --- inputs ---------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct StatusInput {}

#[derive(Debug, Deserialize)]
pub struct ScreenshotInput {}

#[derive(Debug, Deserialize)]
pub struct MoveInput {
    pub x: i64,
    pub y: i64,
    #[serde(default)]
    pub duration_ms: u64,
}

#[derive(Debug, Deserialize)]
pub struct ClickInput {
    pub x: Option<i64>,
    pub y: Option<i64>,
    #[serde(default = "default_button")]
    pub button: String,
    #[serde(default)]
    pub double: bool,
}

fn default_button() -> String {
    "left".to_string()
}

#[derive(Debug, Deserialize)]
pub struct DragInput {
    pub from_x: i64,
    pub from_y: i64,
    pub to_x: i64,
    pub to_y: i64,
    #[serde(default)]
    pub duration_ms: u64,
}

#[derive(Debug, Deserialize)]
pub struct ScrollInput {
    #[serde(default)]
    pub dx: i64,
    #[serde(default)]
    pub dy: i64,
}

#[derive(Debug, Deserialize)]
pub struct TypeInput {
    pub text: String,
}

#[derive(Debug, Deserialize)]
pub struct KeyInput {
    pub combo: String,
}

#[derive(Debug, Deserialize)]
pub struct OpenInput {
    pub target: String,
}

#[derive(Debug, Deserialize)]
pub struct WaitInput {
    pub ms: u64,
}

// --- tools -----------------------------------------------------------------

pub fn status(_input: StatusInput) -> Result<ToolOutput> {
    match detect_backend() {
        Ok(Backend::GnomeBridge) => {
            let raw = gdbus_call("Status", &[])?;
            if bridge_is_stale() {
                return ok(
                    "desktop backend status — STALE BRIDGE (window control degraded until the user logs out/in)",
                    format!("backend: gnome-bridge, but the LOADED extension is an OLD version (missing CaptureWindow/WindowBatch — Wayland only reloads extensions on login). Layered window control (capture_window / window_act) is unavailable and focus/raise may silently fail. Full-screen tools (screenshot, move/click/type on the focused window) still work. If the task needs a specific app window, tell the user ONE logout/login enables reliable window control — don't burn rounds fighting it.\n{raw}"),
                );
            }
            ok(
                "desktop backend status",
                format!("backend: gnome-bridge (animated agent cursor active — full screen control). The coordinate space spans the FULL virtual desktop (all monitors, origin top-left) and equals the screenshot's pixel space. Take a computer_screenshot to see the exact dimensions and ground every click/move from it — never assume a fixed resolution.\n{raw}"),
            )
        }
        Ok(Backend::X11Tool) => {
            let geometry = run("xdotool", &["getdisplaygeometry"]).unwrap_or_default();
            ok(
                "desktop backend status",
                format!("backend: x11/xdotool\nscreen: {geometry}\nAvailable: computer_open, computer_screenshot, computer_act, computer_move, computer_click, computer_drag, computer_scroll, computer_type, computer_key, computer_wait, computer_list_windows, computer_focus_window.\nUnavailable in this private desktop: computer_capture_window, computer_window_act, computer_lower_window, all computer_app_* accessibility tools. Do not call those as fallback; they target a different backend. Use computer_screenshot and computer_act for Blender and other native apps. Custom overlay cursor: unavailable on X11."),
            )
        }
        // Launching non-browser apps and local files never needed the bridge — don't block the
        // whole desktop surface because screen CONTROL isn't set up yet.
        Err(_) => ok(
            "desktop backend status (launcher-only)",
            format!(
                "backend: launcher-only — screen control is not set up, but launching WORKS.\n\
                 WORKS NOW: computer_open (non-browser apps by name and local files), computer_wait.\n\
                 NOT AVAILABLE yet: computer_screenshot / move / click / drag / scroll / type / key.\n\
                 For 'open X' tasks: just use computer_open — do not stop for setup.\n\
                 Only when the task needs seeing or clicking the screen, relay this one-time setup:\n{SETUP_HELP}"
            ),
        ),
    }
}

pub fn local_status_line() -> String {
    match status(StatusInput {}) {
        Ok(output) => format!("computer · {}", output.summary),
        Err(error) => format!("computer · status unavailable: {error:#}"),
    }
}

/// Process-cached form of [`local_status_line`] for the runtime-context
/// preflight, which re-assembles on every turn. `status` spawns a
/// `gdbus`/`xdotool` backend probe; the desktop backend is a machine-setup
/// fact that does not change mid-run, so we compute the line once and reuse
/// it. Keeps the injected line off the per-turn hot path and byte-stable
/// across rounds (provider prefix-cache safe).
pub fn cached_status_line() -> String {
    static CACHE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CACHE.get_or_init(local_status_line).clone()
}

pub fn screenshot(_input: ScreenshotInput) -> Result<ToolOutput> {
    let backend = detect_backend()?;
    let (path, path_str) = shot_path("desktop")?;
    let mut pending = PendingShot::new(path.clone());

    match backend {
        Backend::GnomeBridge => {
            let result = gdbus_call("Screenshot", &[gvariant_str(&path_str)])?;
            if result.contains("error") {
                bail!("shell screenshot failed: {result}");
            }
        }
        Backend::X11Tool => {
            if have("import") {
                run("import", &["-window", "root", &path_str])?;
            } else if have("scrot") {
                run("scrot", &[&path_str])?;
            } else {
                bail!("no X11 screenshot tool found (install imagemagick or scrot)");
            }
        }
    }
    verify_shot_written(&path, &path_str)?;
    pending.keep();
    if let Some((width,height))=png_dimensions(&path) {
        crate::tools::isolated_desktop::record_observation(&path,"desktop",width,height);
    }
    // Tell the agent the EXACT pixel space it is looking at, so a vision-grounded
    // (or reasoned) coordinate maps 1:1 to where a click lands. The bridge
    // captures the FULL virtual stage (all monitors, origin top-left) and clicks
    // use that same global space; at scale 1.0 the PNG pixels ARE the click
    // coordinates. (If fractional scaling is ever enabled, physical PNG pixels
    // would differ from logical click coords by the scale factor — handle then.)
    let dims_line = match png_dimensions(&path) {
        Some((w, h)) => format!(
            "\nScreen image: {w}x{h} px, origin top-left. computer_click/computer_move x,y are in THESE exact pixels — read coordinates off this screenshot, never from memory or an assumed resolution."
        ),
        None => String::new(),
    };
    // First line must stay parseable by the vision caption hook (mesh.rs).
    ok(
        "desktop screenshot",
        format!("Screenshot saved: {path_str}{dims_line}"),
    )
}

/// Read a PNG's pixel dimensions straight from its IHDR header (no image-decode
/// dependency). Layout: 8-byte signature, 4-byte chunk length, "IHDR", then
/// width and height as big-endian u32.
fn png_dimensions(path: &std::path::Path) -> Option<(u32, u32)> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = [0u8; 24];
    file.read_exact(&mut bytes).ok()?;
    if &bytes[..8] != b"\x89PNG\r\n\x1a\n"
        || u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) != 13
        || &bytes[12..16] != b"IHDR"
    {
        return None;
    }
    let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    Some((w, h))
}

fn png_has_terminal_iend(path: &std::path::Path) -> bool {
    const PNG_IEND: &[u8; 12] = b"\0\0\0\0IEND\xaeB`\x82";

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let Ok(mut file) = options.open(path) else {
        return false;
    };
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    if file.seek(SeekFrom::End(-(PNG_IEND.len() as i64))).is_err() {
        return false;
    }
    let mut tail = [0u8; 12];
    if file.read_exact(&mut tail).is_err() {
        return false;
    }
    &tail == PNG_IEND
}

#[derive(Debug, Deserialize)]
pub struct LocateInput {
    /// Precise visual description of the element to find, e.g.
    /// "the blue Submit button below the email field".
    pub target: String,
}

/// Screenshot + target description; the runtime's vision hook (mesh.rs)
/// appends the grounded `(x, y)` for the agent's next move/click.
pub fn locate(input: LocateInput) -> Result<ToolOutput> {
    let target = input.target.trim().to_string();
    if target.is_empty() {
        bail!("computer_locate needs `target` — describe the element to find");
    }
    let shot = screenshot(ScreenshotInput {})?;
    Ok(ToolOutput {
        summary: format!("locating: {target}"),
        content: format!("{}\nLocate target: {target}", shot.content),
    })
}

#[derive(Debug, Deserialize)]
pub struct ReadTextInput {}

/// Screenshot + an OCR marker; the runtime's vision hook (mesh.rs) transcribes
/// the on-screen text verbatim and appends it. The Coasty `ocr` capability:
/// use it when you need exact strings (error dialogs, table values, codes)
/// rather than a visual description.
pub fn read_text(_input: ReadTextInput) -> Result<ToolOutput> {
    let shot = screenshot(ScreenshotInput {})?;
    Ok(ToolOutput {
        summary: "reading screen text".to_string(),
        content: format!("{}\nOCR: transcribe all visible text", shot.content),
    })
}

#[derive(Debug, Deserialize)]
pub struct ListWindowsInput {}

/// Map a raw `gdbus` failure to a clearer message when the cause is that the
/// running GNOME extension predates these methods (it only reloads on a Wayland
/// logout/login). The exact e529a013 failure: ListWindows on the old extension.
fn window_method_error(error: anyhow::Error) -> anyhow::Error {
    let text = error.to_string();
    if text.contains("UnknownMethod")
        || text.contains("No such method")
        || text.contains("not implemented")
        || text.contains("is not a valid method")
    {
        anyhow!(
            "window targeting isn't loaded yet: the Phoenix GNOME cursor extension must be reloaded to pick up ListWindows/FocusWindow — log out and back in (Wayland only reloads extensions on login). Until then, act on the focused window with computer_act / computer_screenshot. (bridge said: {text})"
        )
    } else {
        error
    }
}

/// List open application windows (id, title, app, focus, geometry) so the agent
/// can target a SPECIFIC window deterministically instead of alt-tab guessing.
/// Wayland-native via the GNOME bridge, with a real X11/xdotool implementation
/// rather than forcing agents into alt-tab guessing on X11.
pub fn list_windows(_input: ListWindowsInput) -> Result<ToolOutput> {
    match detect_backend()? {
        Backend::GnomeBridge => {
            let raw = gdbus_call("ListWindows", &[]).map_err(window_method_error)?;
            ok(
                "windows",
                format!(
                    "Open windows (use `id` with computer_focus_window):\n{}",
                    unwrap_gvariant_string(&raw)
                ),
            )
        }
        Backend::X11Tool => list_x11_windows(),
    }
}

fn list_x11_windows() -> Result<ToolOutput> {
    let ids = run("xdotool", &["search", "--onlyvisible", "--name", "."])
        .context("xdotool could not enumerate visible X11 windows")?;
    let active = run("xdotool", &["getactivewindow"])
        .ok()
        .and_then(|value| value.trim().parse::<i64>().ok());
    let mut windows = Vec::new();

    for raw_id in ids.lines() {
        let Ok(id) = raw_id.trim().parse::<i64>() else {
            continue;
        };
        let id_text = id.to_string();
        let title = run("xdotool", &["getwindowname", &id_text]).unwrap_or_default();
        let app = run("xdotool", &["getwindowclassname", &id_text])
            .unwrap_or_default()
            .lines()
            .last()
            .unwrap_or_default()
            .to_string();
        let pid = run("xdotool", &["getwindowpid", &id_text])
            .ok()
            .and_then(|value| value.trim().parse::<i64>().ok())
            .unwrap_or_default();
        let geometry = run("xdotool", &["getwindowgeometry", "--shell", &id_text])
            .ok()
            .and_then(|value| parse_xdotool_geometry(&value));
        let Some((x, y, width, height)) = geometry else {
            continue;
        };
        windows.push(serde_json::json!({
            "id": id,
            "title": title,
            "app": app,
            "pid": pid,
            "focused": active == Some(id),
            "minimized": false,
            "x": x,
            "y": y,
            "width": width,
            "height": height,
        }));
    }

    let payload = serde_json::json!({"ok": true, "windows": windows});
    ok(
        "windows",
        format!(
            "Open windows (use `id` with computer_focus_window):\n{}",
            payload
        ),
    )
}

fn parse_xdotool_geometry(raw: &str) -> Option<(i64, i64, i64, i64)> {
    let mut values = std::collections::HashMap::new();
    for line in raw.lines() {
        let (key, value) = line.split_once('=')?;
        if matches!(key, "X" | "Y" | "WIDTH" | "HEIGHT") {
            values.insert(key, value.trim().parse::<i64>().ok()?);
        }
    }
    Some((
        *values.get("X")?,
        *values.get("Y")?,
        *values.get("WIDTH")?,
        *values.get("HEIGHT")?,
    ))
}

#[derive(Debug, Deserialize)]
pub struct FocusWindowInput {
    /// `id` from computer_list_windows.
    pub id: i64,
}

/// Activate a window by its `computer_list_windows` id — un-minimizes, switches
/// to its workspace, raises and focuses it. The deterministic way to act on a
/// window that isn't currently in front (instead of alt-tab roulette).
///
/// TRUST BUT VERIFY: a stale bridge (or compositor focus-steal denial) can
/// report ok while the window never surfaces — the exact failure that had the
/// agent screenshotting YouTube while "focused" on Viber and clicking blind.
/// So after the bridge's claim, re-list the windows and require `focused:true`
/// before reporting success.
pub fn focus_window(input: FocusWindowInput) -> Result<ToolOutput> {
    reject_browser_window_target(input.id)?;
    match detect_backend()? {
        Backend::GnomeBridge => {
            let raw =
                gdbus_call("FocusWindow", &[input.id.to_string()]).map_err(window_method_error)?;
            let parsed = unwrap_gvariant_string(&raw);
            if parsed.contains("\"ok\":false") || parsed.contains("\"ok\": false") {
                bail!("focus_window: {parsed}");
            }
            // Activation is async in the compositor — give it a beat, then
            // verify against the live window list instead of trusting the claim.
            std::thread::sleep(std::time::Duration::from_millis(400));
            let verified = gdbus_call("ListWindows", &[])
                .ok()
                .map(|raw| unwrap_gvariant_string(&raw))
                .map(|list| window_is_focused(&list, input.id));
            match verified {
                Some(true) | None => {
                    // None = the verify probe itself failed; keep the original
                    // claim rather than inventing a failure.
                    ok("window focused (verified)", parsed)
                }
                Some(false) => bail!(
                    "focus_window: the bridge reported ok but window {id} did NOT take focus (it is still not the focused window). Do NOT act on coordinates yet — the screen shows something else. Causes: the loaded GNOME extension is stale (user must log out/in to reload it) or the compositor denied focus stealing. Options: computer_list_windows to see what IS focused, ask the user to click the window once, or (stale bridge) tell the user a logout/login enables reliable window control.",
                    id = input.id
                ),
            }
        }
        Backend::X11Tool => {
            let id = input.id.to_string();
            run("xdotool", &["windowactivate", "--sync", &id])
                .with_context(|| format!("xdotool could not focus window {}", input.id))?;
            let active = run("xdotool", &["getactivewindow"])
                .ok()
                .and_then(|value| value.trim().parse::<i64>().ok());
            anyhow::ensure!(
                active == Some(input.id),
                "focus_window: xdotool activated window {} but live focus verification reported {:?}",
                input.id,
                active
            );
            ok(
                "window focused (verified)",
                serde_json::json!({"ok": true, "focused": {"id": input.id}}).to_string(),
            )
        }
    }
}

/// Does the ListWindows JSON show `id` as the focused window? Serde-parsed —
/// substring matching against `"focused":true` cannot tie the flag to the id.
fn window_is_focused(list_json: &str, id: i64) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(list_json) else {
        return false;
    };
    value["windows"]
        .as_array()
        .map(|windows| {
            windows
                .iter()
                .any(|w| w["id"].as_i64() == Some(id) && w["focused"].as_bool() == Some(true))
        })
        .unwrap_or(false)
}

fn validate_screen_point(action: &str, x: i64, y: i64) -> Result<()> {
    anyhow::ensure!(
        x >= 0 && y >= 0,
        "{action} coordinates must be non-negative screen pixels (got {x}, {y})"
    );
    Ok(())
}

/// Record cursor placement only after the backend confirms the initial move.
/// Keeping this transition in one helper prevents move/click/drag call sites
/// from accidentally marking a failed move as safe for coordinate-less input.
fn move_then_mark(move_action: impl FnOnce() -> Result<()>) -> Result<()> {
    move_action()?;
    mark_cursor_placed();
    Ok(())
}

pub fn move_cursor(input: MoveInput) -> Result<ToolOutput> {
    validate_screen_point("computer_move", input.x, input.y)?;
    let backend = detect_backend()?;
    move_then_mark(|| match backend {
        Backend::GnomeBridge => gdbus_call(
            "MoveTo",
            &[
                input.x.to_string(),
                input.y.to_string(),
                input.duration_ms.to_string(),
            ],
        )
        .map(|_| ()),
        Backend::X11Tool => run(
            "xdotool",
            &["mousemove", &input.x.to_string(), &input.y.to_string()],
        )
        .map(|_| ()),
    })?;
    ok(
        "agent cursor moved",
        format!("Agent cursor at ({}, {}).", input.x, input.y),
    )
}

pub fn click(input: ClickInput) -> Result<ToolOutput> {
    if !matches!(input.button.as_str(), "left" | "right" | "middle") {
        bail!("button must be left, right, or middle");
    }
    if input.x.is_some() != input.y.is_some() {
        bail!("computer_click coordinates must provide both x and y, or neither");
    }
    if let (Some(x), Some(y)) = (input.x, input.y) {
        validate_screen_point("computer_click", x, y)?;
    }
    let backend = detect_backend()?;
    if let (Some(x), Some(y)) = (input.x, input.y) {
        move_then_mark(|| match backend {
            Backend::GnomeBridge => {
                gdbus_call("MoveTo", &[x.to_string(), y.to_string(), "0".to_string()]).map(|_| ())
            }
            Backend::X11Tool => {
                run("xdotool", &["mousemove", &x.to_string(), &y.to_string()]).map(|_| ())
            }
        })?;
    } else {
        require_cursor_placed("computer_click")?;
    }
    match backend {
        Backend::GnomeBridge => {
            let method = if input.double { "DoubleClick" } else { "Click" };
            gdbus_call(method, &[input.button.clone()])?;
        }
        Backend::X11Tool => {
            let btn = match input.button.as_str() {
                "right" => "3",
                "middle" => "2",
                _ => "1",
            };
            let mut args = vec!["click"];
            if input.double {
                args.extend(["--repeat", "2"]);
            }
            args.push(btn);
            run("xdotool", &args)?;
        }
    }
    let kind = if input.double {
        "double-click"
    } else {
        "click"
    };
    let at = match (input.x, input.y) {
        (Some(x), Some(y)) => format!(" at ({x}, {y})"),
        _ => " at current agent-cursor position".to_string(),
    };
    ok("clicked", format!("{} {kind}{at}.", input.button))
}

pub fn drag(input: DragInput) -> Result<ToolOutput> {
    validate_screen_point("computer_drag start", input.from_x, input.from_y)?;
    validate_screen_point("computer_drag end", input.to_x, input.to_y)?;
    match detect_backend()? {
        Backend::GnomeBridge => {
            move_then_mark(|| {
                gdbus_call(
                    "MoveTo",
                    &[
                        input.from_x.to_string(),
                        input.from_y.to_string(),
                        "0".to_string(),
                    ],
                )
                .map(|_| ())
            })?;
            gdbus_call(
                "Drag",
                &[
                    input.to_x.to_string(),
                    input.to_y.to_string(),
                    input.duration_ms.to_string(),
                ],
            )?;
        }
        Backend::X11Tool => {
            move_then_mark(|| {
                run(
                    "xdotool",
                    &[
                        "mousemove",
                        &input.from_x.to_string(),
                        &input.from_y.to_string(),
                    ],
                )
                .map(|_| ())
            })?;
            run("xdotool", &["mousedown", "1"])?;
            run(
                "xdotool",
                &[
                    "mousemove",
                    &input.to_x.to_string(),
                    &input.to_y.to_string(),
                ],
            )?;
            run("xdotool", &["mouseup", "1"])?;
        }
    }
    ok(
        "dragged",
        format!(
            "Dragged ({}, {}) -> ({}, {}).",
            input.from_x, input.from_y, input.to_x, input.to_y
        ),
    )
}

pub fn scroll(input: ScrollInput) -> Result<ToolOutput> {
    if input.dx == 0 && input.dy == 0 {
        bail!("scroll needs a non-zero dx or dy (positive dy scrolls down)");
    }
    require_cursor_placed("computer_scroll")?;
    match detect_backend()? {
        Backend::GnomeBridge => {
            gdbus_call("Scroll", &[input.dx.to_string(), input.dy.to_string()])?;
        }
        Backend::X11Tool => {
            let (button, count) = if input.dy != 0 {
                (if input.dy > 0 { "5" } else { "4" }, input.dy.abs())
            } else {
                (if input.dx > 0 { "7" } else { "6" }, input.dx.abs())
            };
            run(
                "xdotool",
                &["click", "--repeat", &count.min(30).to_string(), button],
            )?;
        }
    }
    ok(
        "scrolled",
        format!("Scrolled dx={} dy={}.", input.dx, input.dy),
    )
}

pub fn type_text(input: TypeInput) -> Result<ToolOutput> {
    if input.text.is_empty() {
        bail!("text is empty");
    }
    if input.text.chars().count() > 2000 {
        bail!("text too long for one type action (2000 char cap) — split it");
    }
    match detect_backend()? {
        Backend::GnomeBridge => {
            gdbus_call("TypeText", &[gvariant_str(&input.text)])?;
        }
        Backend::X11Tool => {
            run("xdotool", &["type", "--delay", "12", "--", &input.text])?;
        }
    }
    ok(
        "typed",
        format!("Typed {} characters.", input.text.chars().count()),
    )
}

pub fn key(input: KeyInput) -> Result<ToolOutput> {
    if input.combo.trim().is_empty() {
        bail!("combo is empty (e.g. \"ctrl+l\", \"enter\", \"alt+tab\")");
    }
    match detect_backend()? {
        Backend::GnomeBridge => {
            gdbus_call("Key", &[gvariant_str(&input.combo)])?;
        }
        Backend::X11Tool => {
            let combo = x11_key_combo(&input.combo)?;
            let combo = x11_function_keycodes(&combo)?;
            run("xdotool", &["key", "--", &combo])?;
        }
    }
    ok("key sent", format!("Sent key combo: {}.", input.combo))
}

fn x11_function_keycodes(combo: &str) -> Result<String> {
    let is_function = |key: &str| key.strip_prefix('F')
        .and_then(|n| n.parse::<u8>().ok()).is_some_and(|n| (1..=35).contains(&n));
    if !combo.split('+').any(is_function) { return Ok(combo.to_string()); }
    // libxdo may choose an alternate level for duplicate F-key keysyms and
    // synthesize Alt even though the caller requested plain F3. Resolve the
    // unmodified symbol on this owned display and send its physical keycode.
    let mapping = run("xmodmap", &["-pk"])?;
    combo.split('+').map(|key| {
        if !is_function(key) { return Ok(key.to_string()); }
        mapping.lines().find_map(|line| {
            let mut fields = line.split_whitespace();
            let code = fields.next()?.parse::<u8>().ok()?;
            let _hex = fields.next()?;
            (fields.next()? == format!("({key})")).then(|| code.to_string())
        }).with_context(|| format!("no unmodified X11 keycode for {key}"))
    }).collect::<Result<Vec<_>>>().map(|keys| keys.join("+"))
}

fn x11_key_combo(combo: &str) -> Result<String> {
    combo.trim().split('+').map(|part| {
        let part = part.trim();
        anyhow::ensure!(!part.is_empty(), "key combination contains an empty key");
        let lower = part.to_ascii_lowercase();
        let mapped = match lower.as_str() {
            "enter" | "return" => "Return",
            "esc" | "escape" => "Escape",
            "tab" => "Tab",
            "space" | "spacebar" => "space",
            "backspace" => "BackSpace",
            "delete" | "del" => "Delete",
            "insert" | "ins" => "Insert",
            "home" => "Home", "end" => "End",
            "pageup" | "page_up" | "pgup" => "Prior",
            "pagedown" | "page_down" | "pgdn" => "Next",
            "left" | "arrowleft" => "Left", "right" | "arrowright" => "Right",
            "up" | "arrowup" => "Up", "down" | "arrowdown" => "Down",
            "ctrl" | "control" => "ctrl", "shift" => "shift", "alt" => "alt",
            "super" | "meta" | "win" => "super",
            "decimal" | "numdecimal" | "numpaddecimal" => "KP_Decimal",
            "numenter" | "numpadenter" => "KP_Enter",
            "numplus" | "numpadadd" => "KP_Add",
            "numminus" | "numpadsubtract" => "KP_Subtract",
            _ => {
                if let Some(number) = lower.strip_prefix("numpad")
                    .or_else(|| lower.strip_prefix("num"))
                    .and_then(|n| n.parse::<u8>().ok()) {
                    anyhow::ensure!(number <= 9, "numeric keypad key must be 0 through 9");
                    return Ok(format!("KP_{number}"));
                }
                if let Some(number) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                    anyhow::ensure!((1..=35).contains(&number), "function key must be F1 through F35");
                    return Ok(format!("F{number}"));
                }
                // Preserve case-sensitive native X keysyms such as KP_1 and
                // ISO_Left_Tab. The command wrapper rejects unknown symbols.
                part
            }
        };
        Ok(mapped.to_string())
    }).collect::<Result<Vec<_>>>().map(|keys| keys.join("+"))
}

// --- batched multi-action (the Codex / Coasty pattern) --------------------
//
// One model round-trip drives a whole UI sequence and we screenshot ONCE at
// the end, instead of a model round-trip + screenshot per single action. This
// is the biggest computer-use speed win (Codex's GPT-5.5 emits batched actions
// per screenshot; Coasty's v3 "lean" loop does the same). Each step reuses the
// existing single-action handler, so backend dispatch stays in one place.

/// One step in a [`act`] batch. Internally tagged by `type`, so the model emits
/// e.g. `{"type":"click","x":100,"y":200}` — the same action vocabulary as
/// Codex / OpenAI computer-use and Coasty. `click_element`/`type_into` are
/// a11y-grounded: they locate by visible text at EXECUTION time, so a whole
/// locate → click → type flow is ONE batch step with no coordinate reading.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    Move(MoveInput),
    Click(ClickInput),
    DoubleClick(ClickInput),
    ClickElement(ClickElementInput),
    TypeInto(TypeIntoInput),
    Type(TypeInput),
    Key(KeyInput),
    Scroll(ScrollInput),
    Drag(DragInput),
    Wait(WaitInput),
}

impl Action {
    fn label(&self) -> &'static str {
        match self {
            Action::Move(_) => "move",
            Action::Click(_) => "click",
            Action::DoubleClick(_) => "double_click",
            Action::ClickElement(_) => "click_element",
            Action::TypeInto(_) => "type_into",
            Action::Type(_) => "type",
            Action::Key(_) => "key",
            Action::Scroll(_) => "scroll",
            Action::Drag(_) => "drag",
            Action::Wait(_) => "wait",
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ClickElementInput {
    /// Application name from computer_app_targets, e.g. "Zen".
    pub app: String,
    /// Visible text of the element to click (name or text content).
    pub query: String,
    #[serde(default = "default_button")]
    pub button: String,
    #[serde(default)]
    pub double: bool,
}

#[derive(Debug, Deserialize)]
pub struct TypeIntoInput {
    /// Application name from computer_app_targets, e.g. "Zen".
    pub app: String,
    /// Visible text/label of the field to type into.
    pub query: String,
    pub text: String,
}

/// Resolve a visible text to its on-screen click point (single query, used by
/// the element-grounded act steps). A miss carries the closest visible texts.
fn locate_point(app: &str, query: &str) -> Result<(i64, i64)> {
    reject_browser_app_target(app)?;
    let json = atspi_run(&["locate", app.trim(), query.trim()])?;
    let parsed = serde_json::from_str::<serde_json::Value>(&json).unwrap_or_default();
    if atspi_failed(&json) {
        bail!(
            "no visible element matching {query:?} in {app}{}",
            format_closest(&parsed)
        );
    }
    parsed
        .get("cx")
        .and_then(|v| v.as_i64())
        .zip(parsed.get("cy").and_then(|v| v.as_i64()))
        .ok_or_else(|| anyhow!("locate returned no coordinates: {json}"))
}

fn click_element(input: ClickElementInput) -> Result<ToolOutput> {
    reject_browser_app_target(&input.app)?;
    if input.double || input.button != "left" {
        let (cx, cy) = locate_point(&input.app, &input.query)?;
        click(ClickInput {
            x: Some(cx),
            y: Some(cy),
            button: input.button,
            double: input.double,
        })?;
        return ok(
            "clicked element",
            format!("Clicked {:?} at ({cx}, {cy}).", input.query),
        );
    }
    let json = atspi_run(&["activate", input.app.trim(), input.query.trim()])?;
    let parsed = serde_json::from_str::<serde_json::Value>(&json).unwrap_or_default();
    if atspi_failed(&json) {
        bail!(
            "no visible element matching {:?} in {}{}",
            input.query,
            input.app,
            format_closest(&parsed)
        );
    }
    if parsed.get("acted").and_then(|value| value.as_bool()) == Some(true) {
        return ok("activated element", format!("Activated {:?}.", input.query));
    }
    let (cx, cy) = parsed
        .get("cx")
        .and_then(|value| value.as_i64())
        .zip(parsed.get("cy").and_then(|value| value.as_i64()))
        .ok_or_else(|| anyhow!("accessibility activation returned no action or point: {json}"))?;
    click(ClickInput {
        x: Some(cx),
        y: Some(cy),
        button: input.button,
        double: input.double,
    })?;
    ok(
        "clicked element",
        format!("Clicked {:?} at ({cx}, {cy}).", input.query),
    )
}

fn type_into(input: TypeIntoInput) -> Result<ToolOutput> {
    reject_browser_app_target(&input.app)?;
    let json = atspi_run(&[
        "settext-query",
        input.app.trim(),
        input.query.trim(),
        input.text.as_str(),
    ])?;
    let parsed = serde_json::from_str::<serde_json::Value>(&json).unwrap_or_default();
    if atspi_failed(&json) {
        bail!(
            "no visible field matching {:?} in {}{}",
            input.query,
            input.app,
            format_closest(&parsed)
        );
    }
    let chars = input.text.chars().count();
    if parsed.get("set").and_then(|value| value.as_bool()) == Some(true) {
        return ok(
            "filled element",
            format!("Set {:?} to {chars} character(s).", input.query),
        );
    }
    let (cx, cy) = parsed
        .get("cx")
        .and_then(|value| value.as_i64())
        .zip(parsed.get("cy").and_then(|value| value.as_i64()))
        .ok_or_else(|| anyhow!("accessibility text action returned no field point: {json}"))?;
    click(ClickInput {
        x: Some(cx),
        y: Some(cy),
        button: "left".to_string(),
        double: false,
    })?;
    type_text(TypeInput { text: input.text })?;
    ok(
        "typed into element",
        format!(
            "Clicked {:?} at ({cx}, {cy}) and typed {chars} character(s).",
            input.query
        ),
    )
}

fn run_one(action: Action) -> Result<ToolOutput> {
    match action {
        Action::Move(input) => move_cursor(input),
        Action::Click(input) => click(input),
        Action::DoubleClick(mut input) => {
            input.double = true;
            click(input)
        }
        Action::ClickElement(input) => click_element(input),
        Action::TypeInto(input) => type_into(input),
        Action::Type(input) => type_text(input),
        Action::Key(input) => key(input),
        Action::Scroll(input) => scroll(input),
        Action::Drag(input) => drag(input),
        Action::Wait(input) => wait(input),
    }
}

const ACT_BATCH_CAP: usize = 25;

#[derive(Debug, Deserialize)]
pub struct ActInput {
    /// Ordered actions to execute in one batch.
    pub actions: Vec<Action>,
    /// Take one screenshot after the batch (default true):
    /// the agent sees the resulting screen without spending a separate round.
    #[serde(default = "default_true")]
    pub screenshot: bool,
}

fn default_true() -> bool {
    true
}

/// Execute an ordered batch of UI actions in one tool call, then screenshot
/// once so the agent can verify the result. Stops at the first failing action
/// (a wrong step usually invalidates the rest) and reports exactly where, so
/// the agent re-grounds instead of blind-retrying the whole sequence.
pub fn act(input: ActInput) -> Result<ToolOutput> {
    let total = input.actions.len();
    if total == 0 {
        bail!("computer_act needs a non-empty `actions` list");
    }
    if total > ACT_BATCH_CAP {
        bail!(
            "computer_act batch too long ({total}); cap is {ACT_BATCH_CAP} — verify state between large batches"
        );
    }

    let mut log = Vec::with_capacity(total);
    for (idx, action) in input.actions.into_iter().enumerate() {
        let step = idx + 1;
        let label = action.label();
        match run_one(action) {
            Ok(out) => log.push(format!("{step}. {label}: {}", out.summary)),
            Err(error) => {
                log.push(format!("{step}. {label}: FAILED — {error:#}"));
                bail!(
                    "computer_act stopped at action #{step} of {total}.\n{}\nCall computer_screenshot to see the current state, then continue from where it stopped — do not blind-retry the whole batch.",
                    log.join("\n")
                );
            }
        }
    }

    let mut content = format!("Ran {total} action(s):\n{}", log.join("\n"));
    if input.screenshot {
        // Include the screenshot's "Screenshot saved: <path>" line so the mesh
        // vision hook (keyed on the computer_act tool name) captions the end
        // state — the agent sees the result without a separate screenshot call.
        match screenshot(ScreenshotInput {}) {
            Ok(shot) => content = format!("{}\n\n{}", shot.content, content),
            Err(error) => content.push_str(&format!(
                "\n\n(end-of-batch screenshot unavailable: {error:#} — call computer_screenshot to verify)"
            )),
        }
    }
    ok(&format!("ran {total} action(s)"), content)
}

pub fn open(input: OpenInput) -> Result<ToolOutput> {
    let target = input.target.trim();
    if target.is_empty() {
        bail!("target is empty (a non-browser application name, local file path, or directory)");
    }
    let is_url = target.starts_with("http://")
        || target.starts_with("https://")
        || target.starts_with("file://");
    if is_url {
        bail!("web and file URLs must open inside Phoenix; use browser_navigate with this coworker's managed browser instead of computer_open");
    }
    let path = std::path::Path::new(target);
    let is_html = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "html" | "htm" | "mhtml" | "xhtml"
            )
        });
    if is_html {
        bail!("HTML files must open inside Phoenix; use browser_navigate with an absolute file:// URL instead of computer_open");
    }
    if path.exists() {
        if is_executable_file(path) {
            if let Some(name) = path.file_name().and_then(|value| value.to_str()) {
                reject_browser_app_target(name)?;
            }
            launch_app(target, &[])?;
            return ok(
                "application launched",
                format!("Launched {target}. Observe the desktop to confirm the application is ready.{}",native_app_guidance(target)),
            );
        }
        launch_app("xdg-open", &[target])?;
        return ok(
            "opened on desktop",
            format!("Requested {target} in its default application. Observe the desktop to confirm it opened.{}",native_app_guidance(target)),
        );
    }
    if is_browser_application_name(target) {
        bail!("separate browser windows are disabled; use Phoenix's managed browser tools instead of computer_open");
    }
    // Not a path — treat it as an application name and launch its
    // .desktop entry. This works with NO bridge: "open my VPN app" must not
    // fail because screen control isn't set up.
    if let Some(app) = find_desktop_app(target, &desktop_entry_dirs()) {
        launch_desktop_app(&app)?;
        return ok(
            "application launched",
            format!(
                "Launched {} ({}). Observe this agent's desktop to confirm the application is ready.{}",
                app.display_name, app.id, native_app_guidance(&app.display_name)
            ),
        );
    }
    let suggestions = suggest_apps(target, &desktop_entry_dirs(), 6);
    let hint = if suggestions.is_empty() {
        String::new()
    } else {
        format!(
            " Installed apps that may match: {}.",
            suggestions.join(", ")
        )
    };
    bail!(
        "`{target}` is not an existing file/URL and no installed application matches it.{hint} \
         For files, verify the path first; for apps, use one of the names above or as it appears in the app grid."
    )
}

fn native_app_guidance(_target:&str)->String {
    let mut guidance=String::new();
    if let Some(scope)=crate::tools::isolated_desktop::current_scope_key() {
        if let Some(view)=crate::tools::isolated_desktop::existing_desktops().into_iter().find(|view|view.scope_key==scope) {
            guidance.push_str(if view.backend=="gnome" {
                "\nDesktop backend: GNOME with the Phoenix cursor. Use computer_list_windows, then computer_capture_window and window-relative computer_window_act for this app."
            } else {
                "\nDesktop backend: private X11. Use computer_screenshot and screen-relative computer_act. Window capture and window batches are unavailable on this backend."
            });
        }
    }
    guidance
}

fn is_browser_application_name(target: &str) -> bool {
    let lower = target.to_ascii_lowercase();
    let tokens = lower
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    let compact = tokens.concat();

    // Browser desktop ids and WM classes are normally tokenized names such as
    // `org.mozilla.firefox`, `google-chrome`, `zen-browser`, or
    // `microsoft-edge`. Match those identities, not arbitrary substrings:
    // `knowledge` and `ledger` both contain "edge" but are native apps.
    const BROWSER_TOKENS: [&str; 17] = [
        "browser",
        "firefox",
        "chrome",
        "chromium",
        "brave",
        "vivaldi",
        "librewolf",
        "floorp",
        "waterfox",
        "opera",
        "safari",
        "epiphany",
        "qutebrowser",
        "falkon",
        "midori",
        "thorium",
        "duckduckgo",
    ];
    if tokens.iter().any(|token| BROWSER_TOKENS.contains(token)) {
        return true;
    }
    matches!(compact.as_str(), "zen" | "edge" | "arc" | "msedge")
        || compact.starts_with("zenbrowser")
        || compact.starts_with("zenalpha")
        || compact.starts_with("operagx")
        || compact.contains("googlechrome")
        || compact.contains("microsoftedge")
        || compact.contains("cloakbrowser")
        || compact.contains("ungoogledchromium")
}

fn reject_browser_app_target(app: &str) -> Result<()> {
    if is_browser_application_name(app) {
        bail!(
            "browser windows are not a computer-use surface. Use this coworker's private managed browser_* tools; never drive the user's daily browser ({app}) with computer_*"
        );
    }
    Ok(())
}

fn is_browser_window_identity(app: &str, title: &str) -> bool {
    // Titles describe documents and dialogs, not application identity. Blender's
    // own file chooser is titled "File Browser"; editors can open browser-named
    // files. Use the title only when the desktop supplies no application class.
    if app.trim().is_empty() {
        is_browser_application_name(title)
    } else {
        is_browser_application_name(app)
    }
}

fn reject_browser_window_target(id: i64) -> Result<()> {
    let identity = match detect_backend()? {
        Backend::GnomeBridge => {
            let raw = gdbus_call("ListWindows", &[]).map_err(window_method_error)?;
            let json = unwrap_gvariant_string(&raw);
            let parsed: serde_json::Value = serde_json::from_str(&json)
                .context("desktop bridge returned an invalid window list")?;
            parsed["windows"].as_array().and_then(|windows| {
                windows.iter().find_map(|window| {
                    (window["id"].as_i64() == Some(id)).then(|| {
                        (
                            window["app"].as_str().unwrap_or_default().to_string(),
                            window["title"].as_str().unwrap_or_default().to_string(),
                        )
                    })
                })
            })
        }
        Backend::X11Tool => {
            let id = id.to_string();
            let app = run("xdotool", &["getwindowclassname", &id]).unwrap_or_default();
            let title = run("xdotool", &["getwindowname", &id]).unwrap_or_default();
            Some((app, title))
        }
    };
    if let Some((app, title)) = identity {
        if is_browser_window_identity(&app, &title) {
            bail!(
                "window {id} is a browser ({app}). Use this coworker's private managed browser_* tools; never drive the user's daily browser with computer_*"
            );
        }
    }
    Ok(())
}

/// A resolved .desktop application entry.
#[derive(Debug, Clone)]
pub struct DesktopApp {
    /// Desktop id (filename without `.desktop`) — what `gtk-launch` takes.
    pub id: String,
    pub display_name: String,
    pub path: std::path::PathBuf,
    /// Raw Desktop Entry `Exec=` command. Scoped desktops launch this directly
    /// (without host DBus/GApplication activation) so an existing host app
    /// cannot absorb the request onto the user's real display.
    pub exec: String,
}

fn desktop_entry_dirs() -> Vec<std::path::PathBuf> {
    desktop_entry_dirs_for(
        std::env::var_os("XDG_DATA_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
        std::env::var_os("XDG_DATA_DIRS").as_deref(),
    )
}

fn desktop_entry_dirs_for(
    data_home: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
    data_dirs: Option<&std::ffi::OsStr>,
) -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;
    let mut dirs = Vec::new();
    let configured_home = data_home.map(PathBuf::from).filter(|path| path.is_absolute());
    if let Some(root) = configured_home.or_else(|| home.map(|root| PathBuf::from(root).join(".local/share"))) {
        dirs.push(root.join("applications"));
    }
    if let Some(home) = home {
        dirs.push(PathBuf::from(home).join(".local/share/flatpak/exports/share/applications"));
    }
    if let Some(roots) = data_dirs.filter(|value| !value.is_empty()) {
        dirs.extend(std::env::split_paths(roots)
            .filter(|path| path.is_absolute())
            .map(|path| path.join("applications")));
    }
    for fixed in [
        "/usr/share/applications",
        "/usr/local/share/applications",
        "/var/lib/snapd/desktop/applications",
        "/var/lib/flatpak/exports/share/applications",
    ] {
        let path = PathBuf::from(fixed);
        if !dirs.contains(&path) {
            dirs.push(path);
        }
    }
    dirs
}

/// Find the installed application best matching a human name ("expressvpn",
/// "Express VPN", "firefox"). Filename and `Name=` are both matched,
/// case-insensitively, ignoring spaces/dashes; exact beats prefix beats
/// substring.
pub fn find_desktop_app(name: &str, dirs: &[std::path::PathBuf]) -> Option<DesktopApp> {
    let needle = normalize_app_name(name);
    if needle.is_empty() {
        return None;
    }
    // Individual query words (e.g. "system", "monitor") for matching against
    // Keywords/GenericName, where stripping spaces (the `needle`) would miss.
    let lower = name.to_lowercase();
    let query_words: Vec<String> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 2)
        .map(str::to_string)
        .collect();
    let mut best: Option<(u8, DesktopApp)> = None;
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            if contents.contains("NoDisplay=true") || contents.contains("Type=Link") {
                continue;
            }
            let field = |key: &str| {
                contents
                    .lines()
                    .find_map(|line| line.strip_prefix(key))
                    .unwrap_or("")
                    .to_string()
            };
            let name_field = field("Name=");
            let display_name = if name_field.is_empty() {
                stem.to_string()
            } else {
                name_field
            };
            // Exact / prefix / substring on filename + Name (spaces stripped).
            let candidates = [normalize_app_name(stem), normalize_app_name(&display_name)];
            let norm_score = candidates
                .iter()
                .filter_map(|candidate| {
                    if *candidate == needle {
                        Some(3u8)
                    } else if candidate.starts_with(&needle) {
                        Some(2)
                    } else if candidate.contains(&needle) {
                        Some(1)
                    } else {
                        None
                    }
                })
                .max()
                .unwrap_or(0);
            // Word-coverage over Name + GenericName + Keywords — this is what lets
            // "system monitor" resolve an app whose Keywords list System;Monitor
            // even though its Name is "Resources".
            let haystack = format!(
                "{} {} {} {}",
                stem,
                display_name,
                field("GenericName="),
                field("Keywords=")
            )
            .to_lowercase();
            let word_score = if !query_words.is_empty()
                && query_words.iter().all(|w| haystack.contains(w.as_str()))
            {
                2
            } else {
                0
            };
            let score = norm_score.max(word_score);
            if score == 0 {
                continue;
            }
            let app = DesktopApp {
                id: stem.to_string(),
                display_name,
                path: path.clone(),
                exec: field("Exec="),
            };
            if best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
                best = Some((score, app));
            }
        }
    }
    best.map(|(_, app)| app)
}

/// Up to `max` installed app names that share a word with `query` (else a few
/// arbitrary ones), so a failed computer_open can point the agent at apps that
/// actually exist instead of dead-ending.
fn suggest_apps(query: &str, dirs: &[std::path::PathBuf], max: usize) -> Vec<String> {
    let lower = query.to_lowercase();
    let words: Vec<String> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3)
        .map(str::to_string)
        .collect();
    let mut shared: Vec<String> = Vec::new();
    let mut any: Vec<String> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            if contents.contains("NoDisplay=true") || contents.contains("Type=Link") {
                continue;
            }
            let Some(name) = contents.lines().find_map(|line| line.strip_prefix("Name=")) else {
                continue;
            };
            let keywords = contents
                .lines()
                .find_map(|line| line.strip_prefix("Keywords="))
                .unwrap_or("");
            let hay = format!("{name} {keywords}").to_lowercase();
            if !words.is_empty() && words.iter().any(|w| hay.contains(w.as_str())) {
                if !shared.iter().any(|n| n == name) {
                    shared.push(name.to_string());
                }
            } else if any.len() < max && !any.iter().any(|n| n == name) {
                any.push(name.to_string());
            }
        }
    }
    shared.truncate(max);
    if shared.is_empty() {
        any.truncate(max);
        any
    } else {
        shared
    }
}

fn normalize_app_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_lowercase()
}

/// Launch a process WITHOUT blocking on it. `run`/`Command::output()` reads the
/// child's stdout/stderr to EOF — and a launcher (gtk-launch/gio/xdg-open) hands
/// off to a long-lived GUI app that INHERITS those pipes, so `output()` hangs
/// until the app exits (the e529a013 freeze on `computer_open`). Here we detach
/// the std streams to /dev/null and wait only briefly for the LAUNCHER's own
/// exit code (it returns in well under a second) — never for the GUI child.
fn launch_app(cmd: &str, args: &[&str]) -> Result<()> {
    use std::process::Stdio;
    let mut command = Command::new(cmd);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(desktop) = crate::tools::isolated_desktop::current_environment()? {
        desktop.apply_to_command(&mut command);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn {cmd}"))?;
    // Catch an immediate launcher failure (bad id), but never block on the app.
    for _ in 0..15 {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => bail!("{cmd} exited with {status}"),
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
            Err(error) => return Err(error.into()),
        }
    }
    // Still running after ~1.5s — fine for a launcher; the app is starting.
    Ok(())
}

fn launch_desktop_app(app: &DesktopApp) -> Result<()> {
    if crate::tools::isolated_desktop::current_scope().is_some() {
        return launch_desktop_exec_in_scope(app);
    }
    if have("gtk-launch") {
        return launch_app("gtk-launch", &[&app.id]);
    }
    if have("gio") {
        return launch_app("gio", &["launch", &app.path.to_string_lossy()]);
    }
    bail!(
        "neither gtk-launch nor gio is available to launch {}",
        app.id
    )
}

/// Launch a `.desktop` entry without `gtk-launch`/`gio` when an agent is on a
/// private desktop. Those helpers can use the inherited session bus to ask a
/// host single-instance app to present a window on the user's desktop. The
/// scoped environment removes that bus and this parser starts the declared
/// executable directly inside Xephyr instead.
fn launch_desktop_exec_in_scope(app: &DesktopApp) -> Result<()> {
    let mut argv =
        parse_desktop_exec(&app.exec).with_context(|| format!("parse Exec= for {}", app.id))?;
    let program = argv
        .first()
        .cloned()
        .context("desktop entry has no executable")?;
    let args = argv.split_off(1);
    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    launch_app(&program, &refs)
}

/// Small, strict Desktop Entry Exec parser. Field codes are dropped because
/// `computer_open` launches an app without user-provided file/URL arguments;
/// `%%` keeps one literal percent. It deliberately never invokes a shell.
fn parse_desktop_exec(raw: &str) -> Result<Vec<String>> {
    let raw = raw.trim();
    anyhow::ensure!(
        !raw.is_empty() && raw.len() <= 16 * 1024,
        "desktop Exec= is empty or oversized"
    );
    let mut words = Vec::<String>::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for character in raw.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if matches!(character, '\'' | '"') {
            if quote == Some(character) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(character);
            } else {
                current.push(character);
            }
            continue;
        }
        if character.is_whitespace() && quote.is_none() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }
        current.push(character);
    }
    anyhow::ensure!(
        !escaped && quote.is_none(),
        "desktop Exec= has an unterminated escape or quote"
    );
    if !current.is_empty() {
        words.push(current);
    }
    let mut argv = Vec::new();
    for word in words {
        let mut expanded = String::new();
        let mut chars = word.chars().peekable();
        while let Some(character) = chars.next() {
            if character != '%' {
                expanded.push(character);
                continue;
            }
            match chars.next() {
                Some('%') => expanded.push('%'),
                Some(
                    'f' | 'F' | 'u' | 'U' | 'i' | 'c' | 'k' | 'd' | 'D' | 'n' | 'N' | 'v' | 'm',
                ) => {}
                Some(other) => anyhow::bail!("unsupported desktop Exec= field code %{other}"),
                None => anyhow::bail!("unterminated desktop Exec= field code"),
            }
        }
        if !expanded.is_empty() {
            argv.push(expanded);
        }
    }
    anyhow::ensure!(!argv.is_empty(), "desktop Exec= contains only field codes");
    Ok(argv)
}

pub fn wait(input: WaitInput) -> Result<ToolOutput> {
    let ms = input.ms.min(10_000);
    std::thread::sleep(std::time::Duration::from_millis(ms));
    ok("waited", format!("Waited {ms} ms."))
}

// --- layered window control (legacy unscoped fallback) --------------------
//
// An unscoped local CLI call may work on REAL windows on the user's desktop, layered UNDER
// whatever the user is doing. Perception is per-window compositor capture
// (`computer_capture_window` — the compositor keeps every mapped window's
// texture live, so it works while the window is fully covered). Action is
// `computer_window_act`: a window-targeted batch delivered at a pause in the
// user's own input — keyboard focus moves to the target for the batch (no
// workspace switch), pointer actions briefly raise the window, and the user's
// focus/stacking/pointer are restored afterwards. The user can drag anything
// over the agent's workspace and keep working; they reveal it again by moving
// or closing what's on top. Mesh agents do not use this path: their scoped
// helpers run on a private Xephyr/Xvfb display first.

const WINDOW_TOOL_HINT: &str =
    "layered window control needs the Phoenix GNOME bridge (see computer_status for the one-time setup); the X11 fallback doesn't support it";

fn require_bridge(tool: &str) -> Result<()> {
    match detect_backend()? {
        Backend::GnomeBridge => Ok(()),
        Backend::X11Tool => bail!("{tool}: {WINDOW_TOOL_HINT}"),
    }
}

/// A capture that "succeeded" but wrote zero bytes means the temp filesystem
/// rejected the write (full disk / over quota) — the compositor reports ok
/// and leaves an empty file behind. Passing that to the vision caption hook
/// blinds the agent SILENTLY (live 2026-07-06: /tmp over quota, every shot
/// 0 bytes, every caption "empty base64" — the agent drove Discord blind),
/// so fail the tool call loudly and name the disk as the cause.
fn verify_shot_written(path: &std::path::Path, path_str: &str) -> Result<()> {
    const MAX_DESKTOP_SHOT_BYTES: u64 = 64 * 1024 * 1024;
    const MAX_DESKTOP_SHOT_EDGE: u32 = 32_768;
    const MAX_DESKTOP_SHOT_PIXELS: u64 = 100_000_000;

    let metadata = match std::fs::symlink_metadata(path) {
        Err(_) => bail!("screenshot file was not written: {path_str}"),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.file_type().is_file() => {
            bail!("screenshot output is not a regular file: {path_str}")
        }
        Ok(metadata) if metadata.len() == 0 => {
            let _ = std::fs::remove_file(path);
            bail!(
                "screenshot came back EMPTY (0 bytes): {path_str} — the temp \
                 filesystem is almost certainly full or over quota (check \
                 `df -h {}`). Desktop vision is blind until space is freed; \
                 report this blocker instead of retrying captures.",
                std::env::temp_dir().display()
            )
        }
        Ok(metadata) if metadata.len() > MAX_DESKTOP_SHOT_BYTES => {
            let _ = std::fs::remove_file(path);
            bail!(
                "screenshot is too large ({} bytes; max {MAX_DESKTOP_SHOT_BYTES}): {path_str}",
                metadata.len()
            )
        }
        Ok(metadata) => metadata,
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.nlink() != 1 {
            bail!("screenshot output has an unsafe owner/link count: {path_str}");
        }
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("securing screenshot {path_str}"))?;
    }
    let Some((width, height)) = png_dimensions(path) else {
        let _ = std::fs::remove_file(path);
        bail!("screenshot is not a valid PNG: {path_str}");
    };
    if !png_has_terminal_iend(path) {
        let _ = std::fs::remove_file(path);
        bail!("screenshot PNG is truncated or missing its final IEND chunk: {path_str}");
    }
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if width == 0
        || height == 0
        || width > MAX_DESKTOP_SHOT_EDGE
        || height > MAX_DESKTOP_SHOT_EDGE
        || pixels > MAX_DESKTOP_SHOT_PIXELS
    {
        let _ = std::fs::remove_file(path);
        bail!("screenshot dimensions are unsafe ({width}x{height}, {pixels} pixels): {path_str}");
    }
    Ok(())
}

/// A compositor/helper can create its destination and still report failure.
/// Keep successful screenshots for the vision hook, but unlink every partial
/// output on an error path before returning it to the agent.
struct PendingShot {
    path: std::path::PathBuf,
    keep: bool,
}

impl PendingShot {
    fn new(path: std::path::PathBuf) -> Self {
        Self { path, keep: false }
    }

    fn keep(&mut self) {
        self.keep = true;
    }
}

impl Drop for PendingShot {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn shot_path(prefix: &str) -> Result<(std::path::PathBuf, String)> {
    #[cfg(unix)]
    let suffix = unsafe { libc::geteuid() }.to_string();
    #[cfg(not(unix))]
    let suffix = std::process::id().to_string();
    let mut dir = std::env::temp_dir().join(format!("phoenix-desktop-shots-{suffix}"));
    // Separate screenshot namespaces prevent one agent's vision artifact
    // from ever being selected by another agent's concurrent desktop turn.
    // The scope key is hash-derived and filesystem-safe.
    if let Some(scope) = crate::tools::isolated_desktop::current_scope_key() {
        dir.push(scope);
    }
    prepare_shot_directory(&dir)?;
    let safe_prefix: String = prefix
        .chars()
        .take(64)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    let path = dir.join(format!(
        "{safe_prefix}-{}.png",
        uuid::Uuid::new_v4().simple()
    ));
    let path_str = path.to_string_lossy().to_string();
    Ok((path, path_str))
}

fn prepare_shot_directory(dir: &std::path::Path) -> Result<()> {
    // A new agent scope has two absent levels: the per-user screenshot root
    // and the scope directory. Secure the whole chain before creating either;
    // create_dir alone only happened to work after an unscoped screenshot.
    crate::config::private_io::prepare_private_parent(&dir.join(".capture"))
        .context("creating screenshot directory chain")?;
    let metadata = std::fs::symlink_metadata(&dir).context("inspecting screenshot dir")?;
    if !metadata.file_type().is_dir() {
        bail!(
            "screenshot directory is not a real directory: {}",
            dir.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() } {
            bail!(
                "screenshot directory is not owned by this user: {}",
                dir.display()
            );
        }
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .context("securing screenshot dir")?;
    }
    Ok(())
}

fn bridge_json_failed(json: &str) -> bool {
    json.contains("\"ok\":false") || json.contains("\"ok\": false")
}

#[derive(Debug, Deserialize)]
pub struct CaptureWindowInput {
    /// `id` from computer_list_windows.
    pub id: i64,
}

/// Capture ONE window's live content — even when other windows fully cover
/// it. The image is window-aligned: coordinates read off it are
/// window-relative and feed `computer_window_act` directly.
pub fn capture_window(input: CaptureWindowInput) -> Result<ToolOutput> {
    reject_browser_window_target(input.id)?;
    require_bridge("computer_capture_window")?;
    let (path, path_str) = shot_path(&format!("window-{}", input.id))?;
    let mut pending = PendingShot::new(path.clone());
    let raw = gdbus_call(
        "CaptureWindow",
        &[input.id.to_string(), gvariant_str(&path_str)],
    )
    .map_err(window_method_error)?;
    let json = unwrap_gvariant_string(&raw);
    if bridge_json_failed(&json) {
        bail!("computer_capture_window: {json}");
    }
    verify_shot_written(&path, &path_str)?;
    pending.keep();
    let dims_line = match png_dimensions(&path) {
        Some((w, h)) => {
            crate::tools::isolated_desktop::record_observation(&path,"window",w,h);
            format!(
            "\nWindow image: {w}x{h} px, origin top-left OF THE WINDOW. Coordinates read off this image are window-relative — pass them straight to computer_window_act for window {}; they stay valid wherever the window sits on screen, even fully covered.",
            input.id
        )},
        None => String::new(),
    };
    // First line stays parseable by the vision caption hook (mesh).
    ok(
        "window captured",
        format!("Screenshot saved: {path_str}{dims_line}\n{json}"),
    )
}

const WINDOW_ACT_CAP: usize = 25;

#[derive(Debug, Deserialize)]
pub struct WindowActInput {
    /// `id` from computer_list_windows.
    pub id: i64,
    /// Ordered window-relative actions:
    /// {"type":"click","x":..,"y":..,"button"?,"double"?} · {"type":"type","text":..}
    /// · {"type":"key","combo":..} · {"type":"scroll","x":..,"y":..,"dx":..,"dy":..}
    /// · {"type":"wait","ms":..}
    pub actions: serde_json::Value,
    /// Capture the window after the batch (default true) so the result is seen
    /// without a separate round.
    #[serde(default = "default_true")]
    pub capture: bool,
}

/// Execute a batch of window-relative actions against one window while the
/// user keeps working — the layered path. The compositor waits for a pause in
/// the user's own input, takes keyboard focus for the batch (raising only for
/// pointer actions), then restores the user's focus, stacking, and pointer.
pub fn window_act(input: WindowActInput) -> Result<ToolOutput> {
    let Some(actions) = input.actions.as_array() else {
        bail!("computer_window_act needs `actions` as a JSON array");
    };
    if actions.is_empty() {
        bail!("computer_window_act needs a non-empty `actions` list");
    }
    if actions.len() > WINDOW_ACT_CAP {
        bail!(
            "computer_window_act batch too long ({}); cap is {WINDOW_ACT_CAP} — capture and verify between large batches",
            actions.len()
        );
    }
    const KINDS: [&str; 8] = ["move", "click", "double_click", "type", "key", "scroll", "wait", "drag"];
    for (i, action) in actions.iter().enumerate() {
        let kind = action.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if !KINDS.contains(&kind) {
            bail!(
                "computer_window_act action #{} has type {kind:?}; supported: {KINDS:?}",
                i + 1
            );
        }
    }
    reject_browser_window_target(input.id)?;
    require_bridge("computer_window_act")?;
    let payload = serde_json::to_string(&input.actions).context("serializing actions")?;
    let raw = gdbus_call(
        "WindowBatch",
        &[input.id.to_string(), gvariant_str(&payload)],
    )
    .map_err(window_method_error)?;
    let json = unwrap_gvariant_string(&raw);
    if bridge_json_failed(&json) {
        bail!(
            "computer_window_act stopped partway: {json}\nObserve the current windows with computer_list_windows or computer_screenshot before continuing; the previous target may have closed or opened a dialog. Do not blind-retry the whole batch."
        );
    }
    let receipt:serde_json::Value=serde_json::from_str(&json).context("invalid window batch receipt")?;
    let completed=receipt.get("completed_actions").map(|value|value.as_u64()
        .context("invalid completed action count")).transpose()?.unwrap_or(actions.len() as u64);
    anyhow::ensure!(completed<=actions.len() as u64,"window batch receipt exceeds requested actions");
    let focus_changed=receipt["focus_changed"]==true;
    let mut content = format!(
        "Completed {completed} of {} window action(s) on window {}:\n{json}", actions.len(),
        input.id
    );
    if focus_changed {
        content.push_str("\nAll input actions were delivered, then focus changed; any remaining waits ended early. Discover the current window if needed, then capture the actual new dialog before clicking its controls. A window title or the previous screenshot does not ground new button coordinates. The previous window was not refocused or recaptured.");
    }
    if input.capture && !focus_changed {
        match capture_window(CaptureWindowInput { id: input.id }) {
            Ok(shot) => content = format!("{}\n\n{}", shot.content, content),
            Err(error) => content.push_str(&format!(
                "\n\n(end-of-batch window capture unavailable: {error:#} — call computer_capture_window to verify)"
            )),
        }
    }
    let summary=if completed==actions.len() as u64 {format!("ran {completed} window action(s)")}
        else {format!("completed {completed} window action(s); final waits ended")};
    ok(&summary, content)
}

#[derive(Debug, Deserialize)]
pub struct LowerWindowInput {
    /// `id` from computer_list_windows.
    pub id: i64,
}

/// Push a window below the others — park the agent's workspace under the
/// user's windows so it never blocks their work; capture/act keep working.
pub fn lower_window(input: LowerWindowInput) -> Result<ToolOutput> {
    reject_browser_window_target(input.id)?;
    require_bridge("computer_lower_window")?;
    let raw = gdbus_call("LowerWindow", &[input.id.to_string()]).map_err(window_method_error)?;
    let json = unwrap_gvariant_string(&raw);
    if bridge_json_failed(&json) {
        bail!("computer_lower_window: {json}");
    }
    ok(
        "window lowered",
        format!(
            "Window {} pushed below the user's windows. It keeps running; computer_capture_window still sees it and computer_window_act still drives it.",
            input.id
        ),
    )
}

// --- AT-SPI: act on the user's real apps without taking over input ---------
//
// AT-SPI is the Linux accessibility bus. It lets Phoenix act on named widgets
// in the user's actual apps without moving the mouse or stealing keyboard
// focus, which is the background-control layer for normal desktop apps.

const ATSPI_HELPER: &str = include_str!("../../desktop/atspi/phoenix_atspi.py");

fn atspi_helper_path() -> Result<std::path::PathBuf> {
    let dir = crate::config::phoenix_home().join("atspi");
    crate::config::private_io::prepare_phoenix_directory(&dir)
        .context("preparing private AT-SPI helper directory")?;
    let path = dir.join("phoenix_atspi.py");
    let stale = match crate::config::private_io::read_private_file_limited(
        &path,
        ATSPI_HELPER.len().saturating_add(1),
    ) {
        Ok(Some(existing)) => existing != ATSPI_HELPER.as_bytes(),
        Ok(None) => true,
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    if stale {
        crate::config::private_io::atomic_write_private(&path, ATSPI_HELPER.as_bytes())
            .with_context(|| format!("writing AT-SPI helper to {}", path.display()))?;
    }
    Ok(path)
}

fn atspi_run(args: &[&str]) -> Result<String> {
    if crate::tools::isolated_desktop::current_scope().is_some() {
        bail!(
            "AT-SPI targets the host accessibility bus and is unavailable inside an isolated agent desktop. Use computer_screenshot and computer_act, or window capture/batches when the active backend supports them."
        );
    }
    if !std::path::Path::new("/usr/bin/python3").is_file() && !have("python3") {
        bail!("AT-SPI needs python3 + GTK bindings — install: sudo apt install python3-gi gir1.2-atspi-2.0");
    }
    let path = atspi_helper_path()?;
    let lock = ATSPI_BRIDGE.get_or_init(|| Mutex::new(None));
    let mut slot = lock
        .lock()
        .map_err(|_| anyhow!("AT-SPI bridge lock was poisoned"))?;
    for attempt in 0..2 {
        if slot.is_none() {
            *slot = Some(AtspiBridge::spawn(&path)?);
        }
        let result = slot.as_mut().expect("bridge inserted").request(args);
        match result {
            Ok(value) => return Ok(value),
            Err(error) if attempt == 0 => {
                *slot = None;
                let _ = error;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("bounded AT-SPI retry")
}

static ATSPI_BRIDGE: OnceLock<Mutex<Option<AtspiBridge>>> = OnceLock::new();

struct AtspiBridge {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl AtspiBridge {
    fn spawn(path: &std::path::Path) -> Result<Self> {
        // The system interpreter owns the system PyGObject/AT-SPI bindings;
        // user pyenv/venv shims commonly do not. Fall back only off Linux.
        let python = if std::path::Path::new("/usr/bin/python3").is_file() {
            "/usr/bin/python3"
        } else {
            "python3"
        };
        let mut command = Command::new(python);
        command
            .arg(path)
            .arg("serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(unix)]
        unsafe {
            command.pre_exec(|| {
                libc::setpgid(0, 0);
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let mut child = command
            .spawn()
            .context("starting persistent AT-SPI bridge")?;
        let stdin = child.stdin.take().context("AT-SPI bridge stdin missing")?;
        let stdout = child
            .stdout
            .take()
            .context("AT-SPI bridge stdout missing")?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }

    fn request(&mut self, args: &[&str]) -> Result<String> {
        if let Some(status) = self.child.try_wait().context("checking AT-SPI bridge")? {
            bail!("AT-SPI bridge exited early: {status}");
        }
        let request = serde_json::to_string(args).context("encoding AT-SPI request")?;
        anyhow::ensure!(request.len() <= 256 * 1024, "AT-SPI request is too large");
        self.stdin
            .write_all(request.as_bytes())
            .and_then(|_| self.stdin.write_all(b"\n"))
            .and_then(|_| self.stdin.flush())
            .context("sending AT-SPI request")?;

        #[cfg(unix)]
        {
            let mut descriptor = libc::pollfd {
                fd: self.stdout.get_ref().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe {
                libc::poll(
                    &mut descriptor,
                    1,
                    COMMAND_TIMEOUT.as_millis().min(i32::MAX as u128) as i32,
                )
            };
            if ready == 0 {
                bail!(
                    "AT-SPI bridge timed out after {}s",
                    COMMAND_TIMEOUT.as_secs()
                );
            }
            if ready < 0 {
                return Err(std::io::Error::last_os_error()).context("waiting for AT-SPI bridge");
            }
        }

        let mut response = String::new();
        let bytes = self
            .stdout
            .read_line(&mut response)
            .context("reading AT-SPI response")?;
        anyhow::ensure!(bytes > 0, "AT-SPI bridge closed its output");
        anyhow::ensure!(bytes <= COMMAND_STREAM_CAP, "AT-SPI response is too large");
        Ok(response.trim().to_string())
    }
}

impl Drop for AtspiBridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn atspi_failed(json: &str) -> bool {
    json.contains("\"ok\": false") || json.contains("\"ok\":false")
}

#[derive(Debug, Deserialize)]
pub struct AppTargetsInput {}

pub fn app_targets(_input: AppTargetsInput) -> Result<ToolOutput> {
    let json = atspi_run(&["apps"])?;
    ok(
        "a11y apps",
        format!("Apps reachable via AT-SPI (use the name with computer_app_inspect):\n{json}"),
    )
}

#[derive(Debug, Deserialize)]
pub struct AppInspectInput {
    /// Application name from computer_app_targets, e.g. "Zen" or "Firefox".
    pub app: String,
    #[serde(default)]
    pub max: Option<usize>,
}

pub fn app_inspect(input: AppInspectInput) -> Result<ToolOutput> {
    if input.app.trim().is_empty() {
        bail!("computer_app_inspect needs `app` (a name from computer_app_targets)");
    }
    reject_browser_app_target(&input.app)?;
    let max = input.max.unwrap_or(120).clamp(10, 400).to_string();
    let json = atspi_run(&["inspect", input.app.trim(), "--max", &max])?;
    if atspi_failed(&json) {
        bail!("computer_app_inspect: {json}");
    }
    ok(
        "app elements",
        format!(
            "UI elements in {} — each has on-screen `bounds` (cx,cy = its center). \
             To act, MOVE the mouse to (cx,cy) and computer_click — accessibility \
             locates, the mouse acts. `computer_app_locate` does this lookup for \
             you from a text query.\n{json}",
            input.app.trim()
        ),
    )
}

#[derive(Debug, Deserialize)]
pub struct AppLocateInput {
    /// Application name from computer_app_targets, e.g. "Zen" or "Firefox".
    pub app: String,
    /// Visible text/label of ONE element to find, e.g. "Sign in" or "Compose".
    #[serde(default)]
    pub query: Option<String>,
    /// MANY texts to check/locate in ONE call (a verification checklist).
    #[serde(default)]
    pub queries: Option<Vec<String>>,
}

/// Format a missed query's `closest` list (visible texts near the query,
/// with click points) so the agent's next step is grounded in what IS on
/// screen instead of guessing phrasings blind — the 2026-07-08 Pixel run
/// burned three model round-trips guessing "no games" / "No games found" /
/// "no results" against a bare miss.
fn format_closest(result: &serde_json::Value) -> String {
    let Some(items) = result.get("closest").and_then(|v| v.as_array()) else {
        return String::new();
    };
    if items.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = items
        .iter()
        .filter_map(|item| {
            let text = item.get("text")?.as_str()?;
            let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let cx = item.get("cx")?.as_i64()?;
            let cy = item.get("cy")?.as_i64()?;
            Some(format!("  · {text:?} ({role}) at ({cx}, {cy})"))
        })
        .collect();
    format!("\nClosest visible texts:\n{}", lines.join("\n"))
}

/// Resolve on-screen elements by their visible text (accessible name OR text
/// content) and return the point the MOUSE should click. This is the
/// a11y-grounded twin of the vision `computer_locate`: perception via the
/// accessibility tree (exact, layout-independent), action via the cursor.
/// One call resolves a whole LIST via `queries` — never call this once per
/// string; a miss reports the closest visible texts so the next step is
/// grounded.
pub fn app_locate(input: AppLocateInput) -> Result<ToolOutput> {
    let app = input.app.trim().to_string();
    let mut queries: Vec<String> = match (&input.query, &input.queries) {
        (_, Some(list)) if !list.is_empty() => list.clone(),
        (Some(single), _) => vec![single.clone()],
        _ => vec![],
    };
    queries.retain(|q| !q.trim().is_empty());
    if app.is_empty() || queries.is_empty() {
        bail!(
            "computer_app_locate needs `app` and `query` (one element's visible text) \
             or `queries` (a list to check in one call)"
        );
    }
    reject_browser_app_target(&app)?;
    if queries.len() == 1 {
        let json = atspi_run(&["locate", &app, queries[0].trim()])?;
        let parsed = serde_json::from_str::<serde_json::Value>(&json).unwrap_or_default();
        if atspi_failed(&json) {
            let closest = format_closest(&parsed);
            bail!(
                "computer_app_locate: {json}{closest}\n\
                 If one of those is the element (different wording), locate/click it by \
                 THAT text. To see everything on screen at once, use computer_app_read — \
                 do not guess phrasings one call at a time."
            );
        }
        let coords = parsed
            .get("cx")
            .and_then(|v| v.as_i64())
            .zip(parsed.get("cy").and_then(|v| v.as_i64()));
        let guidance = match coords {
            Some((cx, cy)) => format!(
                "Found it at ({cx}, {cy}). Click it with `computer_click` x={cx} y={cy} \
                 (the mouse glides there and clicks). To type into it, click first, then \
                 computer_type.\n{json}"
            ),
            None => json,
        };
        return ok("located element", guidance);
    }

    let payload = serde_json::to_string(&queries).context("serializing queries")?;
    let json = atspi_run(&["locate-many", &app, &payload])?;
    if atspi_failed(&json) {
        bail!("computer_app_locate: {json}");
    }
    let parsed = serde_json::from_str::<serde_json::Value>(&json).unwrap_or_default();
    let results = parsed
        .get("results")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut visible = Vec::new();
    let mut missing = Vec::new();
    for result in &results {
        let query = result.get("query").and_then(|v| v.as_str()).unwrap_or("?");
        if result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
            let cx = result.get("cx").and_then(|v| v.as_i64()).unwrap_or(0);
            let cy = result.get("cy").and_then(|v| v.as_i64()).unwrap_or(0);
            let matched = result
                .pointer("/matched/role")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            visible.push(format!("  ✓ {query:?} at ({cx}, {cy}) [{matched}]"));
        } else {
            missing.push(format!("  ✗ {query:?}{}", format_closest(result)));
        }
    }
    let summary = format!(
        "checked {} — {} visible, {} missing",
        queries.len(),
        visible.len(),
        missing.len()
    );
    let mut content = String::new();
    if !visible.is_empty() {
        content.push_str(&format!(
            "VISIBLE ({}):\n{}\n",
            visible.len(),
            visible.join("\n")
        ));
    }
    if !missing.is_empty() {
        content.push_str(&format!(
            "MISSING ({}):\n{}\n",
            missing.len(),
            missing.join("\n")
        ));
        content.push_str(
            "A missing entry may just be worded differently — check its closest texts \
             above before treating it as absent.\n",
        );
    }
    ok(&summary, content)
}

#[derive(Debug, Deserialize)]
pub struct AppReadInput {
    /// Application name from computer_app_targets, e.g. "Zen" or "Firefox".
    pub app: String,
    /// Cap on returned text (default 6000, clamp 500..20000).
    #[serde(default)]
    pub max_chars: Option<usize>,
}

/// Read EVERYTHING visible in an app in one call: the window's text in
/// document order via the accessibility tree. The one-call perception
/// primitive — a verification checklist is answered by reading this once,
/// not by a locate call per string.
pub fn app_read(input: AppReadInput) -> Result<ToolOutput> {
    if input.app.trim().is_empty() {
        bail!("computer_app_read needs `app` (a name from computer_app_targets)");
    }
    reject_browser_app_target(&input.app)?;
    let max_chars = input
        .max_chars
        .unwrap_or(6000)
        .clamp(500, 20_000)
        .to_string();
    let json = atspi_run(&["read", input.app.trim(), "--max-chars", &max_chars])?;
    if atspi_failed(&json) {
        bail!("computer_app_read: {json}");
    }
    let parsed = serde_json::from_str::<serde_json::Value>(&json).unwrap_or_default();
    let lines: Vec<String> = parsed
        .get("lines")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|line| line.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let truncated = parsed
        .get("truncated")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut content = format!(
        "Visible text in {} ({} line(s), document order — every line is on screen NOW):\n{}",
        input.app.trim(),
        lines.len(),
        lines.join("\n")
    );
    if truncated {
        content.push_str("\n(truncated — raise max_chars if something you need is cut off)");
    }
    content.push_str(
        "\nTo click any of these, computer_app_locate its text for the (cx, cy) — \
         or use it directly in a computer_act click_element step.",
    );
    ok(&format!("read {} visible line(s)", lines.len()), content)
}

#[cfg(all(test, unix))]
mod bounded_command_tests {
    use super::*;

    #[test]
    fn bounded_command_times_out_and_reaps_its_group() {
        let started = Instant::now();
        let output = run_bounded_command(
            "/bin/sh",
            &["-c", "sleep 30"],
            Duration::from_millis(100),
            4096,
            4096,
            None,
        )
        .unwrap();
        assert_eq!(output.reason, CommandStopReason::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "timeout cleanup took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn bounded_command_stops_stdout_and_stderr_floods_at_the_cap() {
        for (script, expected) in [
            (
                "while :; do printf 0123456789; done",
                CommandStopReason::StdoutLimit,
            ),
            (
                "while :; do printf 0123456789 >&2; done",
                CommandStopReason::StderrLimit,
            ),
        ] {
            let output = run_bounded_command(
                "/bin/sh",
                &["-c", script],
                Duration::from_secs(2),
                4096,
                4096,
                None,
            )
            .unwrap();
            assert_eq!(output.reason, expected);
            assert!(output.stdout.len() <= 4096);
            assert!(output.stderr.len() <= 4096);
        }
    }

    #[test]
    fn bounded_command_terminates_background_descendants_after_leader_exit() {
        let output = run_bounded_command(
            "/bin/sh",
            &["-c", "(sleep 30) & echo $!"],
            Duration::from_secs(2),
            4096,
            4096,
            None,
        )
        .unwrap();
        assert_eq!(output.reason, CommandStopReason::Exited);
        assert!(output.status.success());
        assert!(output.descendants_terminated >= 1);
        let descendant = String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        assert!(
            !process_is_live(descendant),
            "background descendant {descendant} survived bounded command cleanup"
        );
    }

    #[test]
    fn bounded_command_observes_an_available_tool_cancellation() {
        let cancellation = ToolCancellation::default();
        let requester = cancellation.clone();
        let request = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            requester.request();
        });
        let output = run_bounded_command(
            "/bin/sh",
            &["-c", "sleep 30"],
            Duration::from_secs(2),
            4096,
            4096,
            Some(&cancellation),
        )
        .unwrap();
        request.join().unwrap();
        assert_eq!(output.reason, CommandStopReason::Cancelled);
    }

    fn process_is_live(pid: i32) -> bool {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        stat.rsplit_once(") ")
            .and_then(|(_, rest)| rest.as_bytes().first().copied())
            .is_some_and(|state| !matches!(state, b'Z' | b'X'))
    }
}

#[cfg(test)]
#[path = "computer_use_tests.rs"]
mod computer_use_tests;
