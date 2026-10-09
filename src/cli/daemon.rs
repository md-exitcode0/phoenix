//! Gateway daemon — the long-running Phoenix process every session routes through.
//!
//! Bare `phoenix` boots this daemon: it binds a unix socket in the Phoenix home
//! and serves turns forever. `phoenix start` is a thin client — it connects,
//! submits the user's turn, renders the streamed `CliEvent`s, and prints the
//! final summary. Without a running gateway, normal product turns refuse to
//! run; explicit scaffold/real diagnostics may run the same mesh in-process.
//!
//! Wire protocol: newline-delimited JSON. One `WireRequest` per connection,
//! then a stream of `WireResponse` lines ending in `Done` or `Error`.
//! Turns are executed one at a time PER SESSION (each session's file-backed
//! state has one writer); different sessions run concurrently — a wake or a
//! user turn never queues behind another session's work.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;
use zeroize::{Zeroize, Zeroizing};

use crate::runtime::agent_conversation::ConversationOwnerRef;
use crate::runtime::CliEvent;

/// Re-export the shared desktop/gateway compatibility token for existing CLI
/// callers and tests. The definition lives in one source file compiled by both
/// crates, so the two release artifacts cannot silently drift.
pub use crate::wire_protocol::GATEWAY_WIRE_PROTOCOL;

/// Informational build/source identity carried in the handshake. Compatibility
/// is deliberately decided by `GATEWAY_WIRE_PROTOCOL`, not executable bytes:
/// debug and release builds of the same wire contract must coexist without
/// repeatedly replacing one another.
const GATEWAY_BUILD_ID: &str = concat!("phoenix-agent/", env!("CARGO_PKG_VERSION"));

const DETACHED_LOG_MAX_BYTES: u64 = 20 * 1024 * 1024;
const PROTOCOL_PROBE_TIMEOUT: Duration = Duration::from_millis(750);
const PROTOCOL_PROBE_ATTEMPTS: usize = 3;
const LIFECYCLE_LOCK_TIMEOUT: Duration = Duration::from_secs(35);
pub(crate) const MAX_WIRE_REQUEST_BYTES: usize = 2 * 1024 * 1024;

fn background_automation_paused(value: Option<&str>) -> bool {
    value == Some("1")
}

#[cfg(test)]
mod background_pause_tests {
    #[test]
    fn inspection_pause_requires_explicit_process_opt_in() {
        assert!(super::background_automation_paused(Some("1")));
        for value in [None, Some(""), Some("0"), Some("false"), Some("true")] {
            assert!(!super::background_automation_paused(value));
        }
    }
}

pub fn socket_path() -> PathBuf {
    crate::config::phoenix_home().join("gateway.sock")
}

pub fn log_path() -> PathBuf {
    crate::config::phoenix_home().join("gateway.log")
}

pub fn pid_path() -> PathBuf {
    crate::config::phoenix_home().join("gateway.pid")
}

/// Autostart log — detached spawn stdout/stderr when a client boots the gateway.
pub fn autostart_log_path() -> PathBuf {
    crate::config::phoenix_home().join("gateway-autostart.log")
}

fn lifecycle_lock_path() -> PathBuf {
    crate::config::phoenix_home().join("gateway-lifecycle")
}

/// Cross-process lease for the complete gateway lifecycle transaction.
///
/// The Unix socket is intentionally kept published while a daemon drains its
/// browsers, but an older daemon used to unlink it before that drain. Without
/// a lease, two Canvas/CLI supervisors could observe that transient gap and
/// spawn a replacement while the verified stop caller was still waiting for
/// the old WS/browser/memory process to exit. `flock` is released by the kernel
/// on crash, so this serializes stop/probe/spawn without creating a stale-lock
/// recovery problem of its own.
struct GatewayLifecycleLock(std::fs::File);

impl GatewayLifecycleLock {
    async fn acquire() -> Result<Self> {
        Self::acquire_at(&lifecycle_lock_path(), LIFECYCLE_LOCK_TIMEOUT).await
    }

    async fn acquire_at(path: &Path, timeout: Duration) -> Result<Self> {
        crate::config::private_io::prepare_private_parent(path)?;
        let file = open_private_append_file(path)
            .with_context(|| format!("open gateway lifecycle lock {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;

            let deadline = tokio::time::Instant::now() + timeout;
            loop {
                if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                    break;
                }
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::EWOULDBLOCK)
                    && error.raw_os_error() != Some(libc::EAGAIN)
                {
                    return Err(error)
                        .with_context(|| format!("lock gateway lifecycle {}", path.display()));
                }
                if tokio::time::Instant::now() >= deadline {
                    anyhow::bail!(
                        "another Phoenix client held the gateway lifecycle lock {} for more than {:.0}s; refusing to overlap gateway stop/start",
                        path.display(),
                        timeout.as_secs_f32()
                    );
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        Ok(Self(file))
    }
}

impl Drop for GatewayLifecycleLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

/// Is this executable a cargo TEST/bench binary rather than the real CLI?
///
/// **This is a fork-bomb guard, not a tidiness check.** `phoenix_binary_for_spawn`
/// used to accept anything whose filename merely `starts_with("phoenix")`, and
/// under `cargo test` `current_exe()` is `target/debug/deps/phoenix-<16 hex>`.
/// So a test that reached gateway autostart spawned *the test binary itself*,
/// detached via setsid + double-fork and reparented to init — where it ran the
/// whole suite again, reached autostart again, and spawned again. Exponentially,
/// with every generation escaping the test harness's process group so neither
/// `cargo` nor Ctrl-C could reap it.
///
/// Observed 2026-07-25: **2,465 orphaned `phoenix-b858c4053d38a2d2` processes
/// holding 10.4 GB**, which took the machine to 166 MB free and killed the
/// session. This is almost certainly the true cause of the repeated "melted
/// box" incidents previously blamed on concurrent `cargo test`/`build` — the
/// standing advice to "sweep `deps/phoenix*` orphans afterwards" was treating
/// the symptom.
///
/// Two independent signals, because either alone can be defeated: cargo places
/// these in a `deps/` directory, AND gives them a 16-hex-digit suffix.
fn is_cargo_test_binary(exe: &Path, name: &str) -> bool {
    if exe.parent().is_some_and(|p| p.ends_with("deps")) {
        return true;
    }
    name.rsplit_once('-').is_some_and(|(_, suffix)| {
        suffix.len() == 16 && suffix.chars().all(|c| c.is_ascii_hexdigit())
    })
}

/// Resolve the phoenix binary to spawn as a detached gateway.
/// Prefer an explicitly matched build, then the current executable. Under
/// `cargo test`, use the sibling real CLI rather than falling through to an
/// arbitrarily old installed binary (and never spawn the test harness itself).
fn phoenix_binary_for_spawn() -> PathBuf {
    if let Ok(explicit) = std::env::var("PHOENIX_GATEWAY_BINARY") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("");
            if !is_cargo_test_binary(&path, name) {
                return path;
            }
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(name) = exe.file_name().and_then(|n| n.to_str()) {
            // Accept phoenix, phoenix-agent, or debug/release target names that
            // contain phoenix — but never a test binary (see the guard's doc:
            // that path is a fork bomb, not a mis-spawn).
            if name.starts_with("phoenix") && !is_cargo_test_binary(&exe, name) {
                return exe;
            }
            if is_cargo_test_binary(&exe, name) {
                if let Some(profile_dir) = exe.parent().and_then(Path::parent) {
                    let sibling = profile_dir.join("phoenix");
                    if sibling.is_file() {
                        return sibling;
                    }
                }
            }
        }
    }
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/"));
    let local = home.join(".local/bin/phoenix");
    if local.exists() {
        return local;
    }
    PathBuf::from("phoenix")
}

fn command_path_for_metadata(command: &Path) -> Option<PathBuf> {
    if command.is_absolute() || command.components().count() > 1 {
        return command.is_file().then(|| command.to_path_buf());
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|directory| directory.join(command))
            .find(|candidate| candidate.is_file())
    })
}

fn selected_gateway_binary_path() -> Option<PathBuf> {
    let command = phoenix_binary_for_spawn();
    command_path_for_metadata(&command).map(|path| path.canonicalize().unwrap_or(path))
}

#[cfg(target_os = "linux")]
fn process_uses_selected_executable_at(
    proc_root: &Path,
    pid: i32,
    selected: &Path,
) -> Option<bool> {
    use std::os::unix::fs::MetadataExt;

    let running = std::fs::metadata(proc_root.join(pid.to_string()).join("exe")).ok()?;
    let selected = std::fs::metadata(selected).ok()?;
    Some(running.dev() == selected.dev() && running.ino() == selected.ino())
}

#[cfg(not(target_os = "linux"))]
fn process_uses_selected_executable_at(
    _proc_root: &Path,
    _pid: i32,
    _selected: &Path,
) -> Option<bool> {
    None
}

#[derive(Debug, PartialEq, Eq)]
enum GatewayProbe {
    Down,
    Compatible,
    Incompatible(String),
    Unresponsive(String),
}

async fn gateway_probe_at(path: &Path) -> GatewayProbe {
    let stream = match UnixStream::connect(path).await {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return GatewayProbe::Down;
        }
        Err(error) => {
            return GatewayProbe::Unresponsive(format!("protocol probe connect failed: {error}"));
        }
    };
    let exchange = async {
        let (read_half, mut write_half) = stream.into_split();
        let mut payload = serde_json::to_string(&WireRequest::ProtocolInfo)?;
        payload.push('\n');
        write_half.write_all(payload.as_bytes()).await?;
        let mut lines = BufReader::new(read_half).lines();
        let line = lines
            .next_line()
            .await?
            .ok_or_else(|| anyhow::anyhow!("gateway closed the protocol probe"))?;
        serde_json::from_str::<WireResponse>(&line).map_err(anyhow::Error::from)
    };
    let response = match tokio::time::timeout(PROTOCOL_PROBE_TIMEOUT, exchange).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            return GatewayProbe::Unresponsive(format!("protocol probe failed: {error:#}"));
        }
        Err(_) => {
            return GatewayProbe::Unresponsive("protocol probe timed out".to_string());
        }
    };
    match response {
        WireResponse::ProtocolInfo { protocol, .. } if protocol == GATEWAY_WIRE_PROTOCOL => {
            GatewayProbe::Compatible
        }
        WireResponse::ProtocolInfo { protocol, .. } => GatewayProbe::Incompatible(format!(
            "expected protocol {GATEWAY_WIRE_PROTOCOL}, got {protocol}"
        )),
        WireResponse::Error { message }
            if message.contains("ProtocolInfo")
                && (message.contains("unknown variant") || message.contains("unknown request")) =>
        {
            GatewayProbe::Incompatible(format!(
                "gateway does not support the {GATEWAY_WIRE_PROTOCOL} handshake: {message}"
            ))
        }
        other => GatewayProbe::Unresponsive(format!(
            "protocol probe returned an unexpected response: {other:?}"
        )),
    }
}

async fn gateway_probe_confirmed_at(path: &Path) -> GatewayProbe {
    let mut last = GatewayProbe::Unresponsive("protocol probe was not attempted".to_string());
    for attempt in 0..PROTOCOL_PROBE_ATTEMPTS {
        last = gateway_probe_at(path).await;
        if !matches!(last, GatewayProbe::Unresponsive(_)) {
            return last;
        }
        if attempt + 1 < PROTOCOL_PROBE_ATTEMPTS {
            tokio::time::sleep(Duration::from_millis(100 * (attempt as u64 + 1))).await;
        }
    }
    last
}

async fn gateway_probe() -> GatewayProbe {
    let socket = socket_path();
    // Build replacement is not a wire incompatibility. A compatible live
    // gateway remains available until an explicit restart.
    gateway_probe_confirmed_at(&socket).await
}

fn rotated_log_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".1");
    PathBuf::from(name)
}

/// Open a gateway log without ever creating it with group/world permissions,
/// then repair an older permissive file through the already-open handle. The
/// parent helper creates a fresh Phoenix-owned root privately but deliberately
/// leaves the mode of an existing custom `PHOENIX_HOME` alone.
fn open_gateway_log_append(path: &Path) -> Result<std::fs::File> {
    crate::config::private_io::prepare_private_parent(path)?;
    open_private_append_file(path)
        .with_context(|| format!("open private gateway log {}", path.display()))
}

fn open_private_append_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

#[cfg(unix)]
fn repair_private_file_mode(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let file = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn repair_private_file_mode(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

fn rotate_log_if_oversized(path: &Path, max_bytes: u64) -> std::io::Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("refusing to rotate non-file log {}", path.display()),
        ));
    }
    if metadata.len() < max_bytes {
        return Ok(());
    }
    let rotated = rotated_log_path(path);
    match std::fs::remove_file(&rotated) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::fs::rename(path, &rotated)?;
    repair_private_file_mode(&rotated)
}

#[cfg(target_os = "linux")]
fn socket_inode_from_proc(proc_root: &Path, socket: &Path) -> Option<String> {
    let table = std::fs::read_to_string(proc_root.join("net/unix")).ok()?;
    let wanted = socket.to_string_lossy();
    table.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        (fields.len() >= 8 && fields[7..].join(" ") == wanted.as_ref())
            .then(|| fields[6].to_string())
    })
}

/// The pidfile is only a hint.  Before signalling it, prove that the PID is a
/// same-user Phoenix executable and owns the exact Unix listener named by the
/// socket path.  This prevents a stale/reused pidfile from killing an unrelated
/// process.  On platforms without Linux procfs we refuse automatic replacement.
#[cfg(target_os = "linux")]
fn pid_is_verified_gateway_at(
    proc_root: &Path,
    pid: i32,
    socket: &Path,
    selected_executable: Option<&Path>,
) -> bool {
    use std::os::unix::fs::MetadataExt;

    if pid <= 1 {
        return false;
    }
    let process_dir = proc_root.join(pid.to_string());
    let Ok(metadata) = std::fs::metadata(&process_dir) else {
        return false;
    };
    if metadata.uid() != unsafe { libc::geteuid() } {
        return false;
    }
    let Ok(executable) = std::fs::read_link(process_dir.join("exe")) else {
        return false;
    };
    let executable_text = executable.to_string_lossy();
    let executable_without_deleted = executable_text.trim_end_matches(" (deleted)");
    let executable_path = Path::new(executable_without_deleted);
    let executable_name = executable_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    // Standard installs retain the narrow name allowlist.  A differently
    // named executable is accepted only when it is the exact canonical path
    // the caller explicitly selected. Same-UID and exact-listener ownership
    // are still independently required below.
    let matches_selected = selected_executable
        .is_some_and(|selected| selected.to_string_lossy() == executable_without_deleted);
    if (!matches!(executable_name, "phoenix" | "phoenix-agent") && !matches_selected)
        || is_cargo_test_binary(executable_path, executable_name)
    {
        return false;
    }
    let Some(inode) = socket_inode_from_proc(proc_root, socket) else {
        return false;
    };
    let Ok(descriptors) = std::fs::read_dir(process_dir.join("fd")) else {
        return false;
    };
    let expected = format!("socket:[{inode}]");
    descriptors.flatten().any(|entry| {
        std::fs::read_link(entry.path())
            .ok()
            .is_some_and(|target| target.as_os_str() == expected.as_str())
    })
}

#[cfg(not(target_os = "linux"))]
fn pid_is_verified_gateway_at(
    _proc_root: &Path,
    _pid: i32,
    _socket: &Path,
    _selected_executable: Option<&Path>,
) -> bool {
    false
}

fn pid_is_verified_gateway(pid: i32, socket: &Path) -> bool {
    let selected = selected_gateway_binary_path();
    pid_is_verified_gateway_at(Path::new("/proc"), pid, socket, selected.as_deref())
}

/// Remove leftover gateway.sock / gateway.pid when nothing is listening.
/// Safe to call from ensure/stop paths; never kills a live process.
fn reclaim_stale_gateway_files() {
    let sock = socket_path();
    // Only remove the socket file if connect fails (dead listener left a file).
    if sock.exists() {
        // Best-effort sync probe — ensure/stop are the only callers.
        if std::os::unix::net::UnixStream::connect(&sock).is_err() {
            let _ = std::fs::remove_file(&sock);
        }
    }
    let pidf = pid_path();
    if let Ok(raw) = std::fs::read_to_string(&pidf) {
        if let Ok(pid) = raw.trim().parse::<i32>() {
            // Signal 0 probes existence without spawning a helper that could
            // hang in a gateway lifecycle transaction. EPERM still means the
            // process exists (though a valid Phoenix pid is same-UID).
            let alive = if unsafe { libc::kill(pid, 0) } == 0 {
                true
            } else {
                std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
            };
            if !alive {
                let _ = std::fs::remove_file(&pidf);
            }
        } else {
            let _ = std::fs::remove_file(&pidf);
        }
    }
}

/// Spawn a fully detached bare `phoenix` (daemon mode). Logs to gateway-autostart.log.
///
/// Unix detach: `setsid` + double-fork so the gateway is session leader and
/// reparented to init — not a child of Canvas/CLI. The intermediate child is
/// reaped immediately so the spawner never leaves `[phoenix] <defunct>` zombies.
fn spawn_detached_gateway() -> Result<PathBuf> {
    let log = autostart_log_path();
    crate::config::private_io::prepare_private_parent(&log)
        .with_context(|| format!("prepare autostart log parent {}", log.display()))?;
    rotate_log_if_oversized(&log, DETACHED_LOG_MAX_BYTES)
        .with_context(|| format!("rotate autostart log {}", log.display()))?;
    let log_file = open_gateway_log_append(&log)
        .with_context(|| format!("open autostart log {}", log.display()))?;
    let err_file = log_file
        .try_clone()
        .with_context(|| format!("clone autostart log {}", log.display()))?;
    let bin = phoenix_binary_for_spawn();
    let mut cmd = std::process::Command::new(&bin);
    cmd.stdin(std::process::Stdio::null())
        .stdout(log_file)
        .stderr(err_file);
    // Full detach: session leader + double-fork so we are not the parent of the
    // long-lived gateway (avoids Canvas zombies when Child is dropped).
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: pre_exec runs in the child between fork and exec; only async-signal-safe
        // calls (setsid/fork/_exit). Grandchild returns Ok and continues to exec.
        unsafe {
            cmd.pre_exec(|| {
                // Cargo and some GUI launchers deliberately ignore SIGINT.
                // POSIX preserves an ignored disposition across exec, which
                // made the detached gateway immune to `phoenix stop`. Give
                // the daemon a clean signal contract before it installs its
                // own Tokio handlers.
                for signal in [libc::SIGINT, libc::SIGTERM] {
                    if libc::signal(signal, libc::SIG_DFL) == libc::SIG_ERR {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                match libc::fork() {
                    -1 => Err(std::io::Error::last_os_error()),
                    0 => Ok(()), // grandchild → exec
                    _ => {
                        // Intermediate child exits so grandchild is reparented to init.
                        libc::_exit(0);
                    }
                }
            });
        }
    }
    #[cfg(not(unix))]
    {
        // Non-unix: best-effort spawn without session detach.
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to launch gateway binary {}", bin.display()))?;
    // Reap the intermediate child (exits immediately after the second fork).
    // The real gateway is the grandchild, already reparented — do NOT wait on it.
    #[cfg(unix)]
    {
        let _ = child.wait();
    }
    #[cfg(not(unix))]
    {
        // Intentionally forget Child so the process can outlive us.
        std::mem::forget(child);
    }
    Ok(log)
}

fn process_start_time_at(proc_root: &Path, pid: i32) -> Option<String> {
    let stat = std::fs::read_to_string(proc_root.join(pid.to_string()).join("stat")).ok()?;
    let (_, rest) = stat.rsplit_once(')')?;
    // The remainder begins with field 3 (state); field 22 (starttime) is index 19.
    rest.split_whitespace().nth(19).map(str::to_string)
}

fn process_start_time(pid: i32) -> Option<String> {
    process_start_time_at(Path::new("/proc"), pid)
}

fn process_instance_exited_at(proc_root: &Path, pid: i32, start_time: Option<&str>) -> bool {
    let stat = match std::fs::read_to_string(proc_root.join(pid.to_string()).join("stat")) {
        Ok(stat) => stat,
        Err(error) => return error.kind() == std::io::ErrorKind::NotFound,
    };
    let Some((_, rest)) = stat.rsplit_once(')') else {
        return false;
    };
    let mut fields = rest.split_whitespace();
    let Some(state) = fields.next() else {
        return false;
    };
    if matches!(state, "Z" | "X" | "x") {
        return true;
    }
    start_time.is_some_and(|expected| fields.nth(18).is_some_and(|actual| actual != expected))
}

fn process_instance_exited(pid: i32, start_time: Option<&str>) -> bool {
    process_instance_exited_at(Path::new("/proc"), pid, start_time)
}

fn remove_pidfile_if_matches(pid: i32) {
    let path = pid_path();
    let matches = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| raw.trim().parse::<i32>().ok())
        == Some(pid);
    if matches {
        let _ = std::fs::remove_file(path);
    }
}

/// A daemon that is already retiring deliberately leaves its pidfile as an
/// exit fence after withdrawing the Unix socket. Wait for that exact process
/// instance (PID + /proc start time, with zombie handling) before binding a
/// replacement. No signal is sent here: even a stale/reused pidfile can at
/// worst fail closed for 15 seconds, never kill an unrelated process.
async fn wait_for_retiring_gateway_pidfile() -> Result<()> {
    let path = pid_path();
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("read gateway pidfile {}", path.display()));
        }
    };
    let pid: i32 = raw
        .trim()
        .parse()
        .with_context(|| format!("gateway pidfile {} is corrupt", path.display()))?;
    if pid <= 1 {
        anyhow::bail!(
            "gateway pidfile {} contains unsafe pid {pid}; refusing automatic replacement",
            path.display()
        );
    }
    if pid == std::process::id() as i32 {
        return Ok(());
    }
    let start_time = process_start_time(pid);
    if process_instance_exited(pid, start_time.as_deref()) {
        remove_pidfile_if_matches(pid);
        return Ok(());
    }
    for _ in 0..150 {
        if process_instance_exited(pid, start_time.as_deref()) {
            remove_pidfile_if_matches(pid);
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    anyhow::bail!(
        "gateway pidfile {} still names live process instance {pid} after 15s while the Unix socket is down; refusing to overlap its wire, browser, or memory cleanup",
        path.display()
    )
}

async fn stop_verified_gateway() -> Result<Option<i32>> {
    let socket = socket_path();
    let pid: i32 = match std::fs::read_to_string(pid_path()) {
        Ok(raw) => raw.trim().parse().context("gateway.pid is corrupt")?,
        Err(_) => {
            if UnixStream::connect(&socket).await.is_err() {
                reclaim_stale_gateway_files();
                return Ok(None);
            }
            anyhow::bail!(
                "a process is listening on {} but there is no Phoenix pidfile; refusing to signal an unverified process",
                socket.display()
            );
        }
    };
    if !pid_is_verified_gateway(pid, &socket) {
        if UnixStream::connect(&socket).await.is_err() {
            reclaim_stale_gateway_files();
            return Ok(None);
        }
        anyhow::bail!(
            "pidfile names pid {pid}, but that same-user Phoenix process could not be proven to own {}; refusing to signal it",
            socket.display()
        );
    }
    let start_time = process_start_time(pid);
    if unsafe { libc::kill(pid, libc::SIGINT) } == -1 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            reclaim_stale_gateway_files();
            return Ok(None);
        }
        return Err(error).context(format!("failed to signal verified gateway pid {pid}"));
    }
    for _ in 0..150 {
        if process_instance_exited(pid, start_time.as_deref()) {
            remove_pidfile_if_matches(pid);
            reclaim_stale_gateway_files();
            return Ok(Some(pid));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    anyhow::bail!(
        "verified gateway pid {pid} did not finish cleanup within 15s — retry or Ctrl-C its terminal"
    )
}

/// Ensure a compatible gateway from the explicitly selected binary is ready.
/// A socket owned by an older build is gracefully stopped only after its
/// pidfile is tied to the exact listener through procfs; otherwise ensure fails
/// closed instead of risking an unrelated process or spawning into a collision.
pub async fn ensure_gateway_running() -> Result<()> {
    // Hold this across the initial probe, any verified retirement, detached
    // spawn, and the final protocol-ready probe. A second supervisor may wait,
    // but it can never use the old daemon's shutdown gap as permission to
    // overlap a new WS/browser/Cognee process.
    let lifecycle = GatewayLifecycleLock::acquire().await?;
    match gateway_probe().await {
        GatewayProbe::Compatible => return Ok(()),
        GatewayProbe::Down => wait_for_retiring_gateway_pidfile().await?,
        GatewayProbe::Incompatible(reason) => {
            eprintln!("phoenix: replacing incompatible gateway ({reason})");
            stop_verified_gateway().await.with_context(|| {
                format!("incompatible gateway could not be safely replaced ({reason})")
            })?;
        }
        GatewayProbe::Unresponsive(reason) => {
            anyhow::bail!(
                "gateway accepted connections but did not complete a protocol handshake after {PROTOCOL_PROBE_ATTEMPTS} attempts ({reason}); refusing to stop or replace it"
            );
        }
    }

    // Dead socket file / stale pid would race with bind — clear them first.
    reclaim_stale_gateway_files();
    let log = spawn_detached_gateway()?;
    // The spawned daemon must take this same lease and re-check before bind.
    // Release only after fork/exec; competing starters may spawn an extra
    // short-lived child, but exactly one child can claim the socket and the
    // losers observe it under-lock instead of unlinking its live endpoint.
    drop(lifecycle);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
    loop {
        match gateway_probe().await {
            GatewayProbe::Compatible => return Ok(()),
            GatewayProbe::Down => {}
            GatewayProbe::Unresponsive(_) => {
                // A new daemon can accept before its request loop is ready.
                // Keep waiting to the deadline without treating a transient
                // failure as authority to stop or replace that process.
            }
            GatewayProbe::Incompatible(reason) => {
                anyhow::bail!(
                    "spawned gateway is not compatible with this client ({reason}) — check log at {}",
                    log.display()
                );
            }
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "gateway did not become ready within 12s after autostart — check log at {}",
                log.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Stop only the process proven to own Phoenix's Unix listener.  The pidfile
/// alone is never trusted because a stale PID can be reused by another app.
pub async fn stop_daemon() -> Result<()> {
    let _lifecycle = GatewayLifecycleLock::acquire().await?;
    match stop_verified_gateway().await? {
        Some(pid) => println!("gateway stopped (pid {pid})."),
        None => println!("no gateway running."),
    }
    Ok(())
}

/// Log one timestamped line: live to the gateway terminal AND appended to the
/// log file. Millisecond stamps make stall diagnosis possible: the gap between
/// `thinking` and the next event IS the provider call duration.
pub(crate) fn glog(line: &str) {
    let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let rendered = format!("[{stamp}] {line}\n");
    // A desktop-launched gateway can outlive the shell/PTY that originally
    // owned stdout. `println!` panics on EPIPE; because turns log before their
    // first response, one orphaned output pipe made every real Turn task die
    // silently while Ping still answered — the canvas then reported
    // "gateway closed before the turn completed". Logging is diagnostic and
    // must never be allowed to take down request handling.
    {
        use std::io::Write;
        let _ = std::io::stdout().lock().write_all(rendered.as_bytes());
    }
    append_gateway_log_line(&rendered);
}

/// Append an already-rendered line through the gateway's one rotation and
/// permission gate. Browser lifecycle logging uses this too; giving it a
/// separate append handle used to let a browser-heavy run grow past the cap
/// and race a `glog` rename onto a stale inode.
pub(crate) fn append_gateway_log_line(rendered: &str) {
    // Rotation and append are one intra-process critical section. Without
    // this, concurrent turn events could rename/open different inodes and
    // silently bypass the size bound.
    static LOG_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    let _guard = LOG_LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let path = log_path();
    let _ = crate::config::private_io::with_private_lock(&path, || {
        rotate_log_if_oversized(&path, DETACHED_LOG_MAX_BYTES)
            .with_context(|| format!("rotate gateway log {}", path.display()))?;
        let mut file = open_gateway_log_append(&path)?;
        use std::io::Write;
        file.write_all(rendered.as_bytes())
            .with_context(|| format!("append gateway log {}", path.display()))?;
        Ok(())
    });
}

/// Compact one-line description of a runtime event for the gateway log.
/// Returns None for events too noisy/voluminous to log.
fn event_brief(event: &CliEvent) -> Option<String> {
    Some(match event {
        CliEvent::Thinking => "thinking (provider call started)".to_string(),
        CliEvent::ToolCallStarted {
            agent,
            tool_name,
            input_summary,
        } => format!("{agent}: {tool_name}({input_summary}) started"),
        CliEvent::ToolCallCompleted {
            agent,
            tool_name,
            success,
            ..
        } => format!(
            "{agent}: {tool_name} {}",
            if *success { "ok" } else { "FAILED" }
        ),
        CliEvent::SpecialistDelegated { agent, subject } => {
            format!("delegated to {agent}: {subject}")
        }
        CliEvent::VolumeWorkerLifecycle {
            batch_id,
            label,
            item_id,
            status,
            ..
        } => format!("volume batch {batch_id}: {label} ({item_id}) {status:?}"),
        CliEvent::AgentHandoff {
            from, to, subject, ..
        } => format!("{from} handed off to {to}: {subject}"),
        CliEvent::SpecialistCompleted { agent, ok, .. } => {
            format!("{agent} {}", if *ok { "completed" } else { "failed" })
        }
        CliEvent::Routing { target, .. } => format!("routing -> {target}"),
        // The ask's birth certificate: without it the log shows an answer
        // being delivered to an ask that was never seen being asked.
        CliEvent::AskUser {
            id,
            agent,
            questions,
            ..
        } => format!(
            "ask {id} posted by {agent}: {}",
            questions.first().map(|q| q.question.as_str()).unwrap_or("")
        ),
        // Librarian pass is the deterministic Cognee recall, not a teammate —
        // don't log it as a "librarian" turn in the gateway log.
        CliEvent::LibrarianPass { .. } => return None,
        CliEvent::SessionResolved {
            session_id, status, ..
        } => format!("session {session_id} ({status})"),
        CliEvent::WatcherCard { from, subject, .. } => format!("{from}: {subject}"),
        CliEvent::SteerQueued {
            from, to, subject, ..
        } => {
            format!("message queued for busy {to} from {from}: {subject}")
        }
        CliEvent::SteerDelivered { to, subject } => {
            format!("{to} read injected message: {subject}")
        }
        CliEvent::Done => "turn events done".to_string(),
        _ => return None,
    })
}

/// A canvas sticky note — the user's pinned context on the canvas board.
/// Passed into the turn so the agent can read and reference the notes.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StickyNoteData {
    pub id: String,
    pub text: String,
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub color: Option<String>,
}

/// The canvas viewport — where the user is currently looking on the infinite
/// canvas board. Used for spatial context scoping: sticky notes near the
/// viewport get full priority, distant notes get a summary, far notes are
/// omitted. `None` (old clients) = no spatial filtering, all notes injected.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ViewportData {
    /// Center of the viewport in world coordinates.
    pub x: f64,
    pub y: f64,
    /// Zoom level (1.0 = 100%).
    pub z: f64,
    /// Viewport dimensions in world coordinates.
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TurnDelivery {
    /// Default wire value. Group rooms: if the room is working, run the prompt
    /// as the next independent turn in FIFO order. One-to-one conversations
    /// are upgraded to `Steer` by `effective_turn_delivery` (no queueing).
    #[default]
    Queue,
    /// Inject the message into the current model loop at its next round
    /// boundary (normal Send in a one-to-one conversation while it works).
    Steer,
}

/// Settings-only credential-vault operations. Secret-bearing fields are
/// wiped when the command leaves scope and Debug never prints their values.
#[derive(Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum VaultCommand {
    Status,
    Initialize {
        master_password: String,
    },
    UnlockWithPassword {
        master_password: String,
    },
    UnlockWithRecoveryKey {
        recovery_key: String,
    },
    Lock,
    ChangeMasterPassword {
        current_password: String,
        new_password: String,
    },
    RotateRecoveryKey,
    List,
    Store {
        scope: crate::security::vault::CredentialScope,
        site: String,
        label: String,
        #[serde(default)]
        username: Option<String>,
        kind: String,
        #[serde(default = "default_json_object")]
        metadata_json: String,
        secret: String,
        /// Secondary secret fields (card cvc/expiry/name/billing_zip, login
        /// totp). Sealed with `secret`; never logged.
        #[serde(default)]
        fields: std::collections::BTreeMap<String, String>,
    },
    Update {
        credential_id: String,
        scope: crate::security::vault::CredentialScope,
        site: String,
        label: String,
        #[serde(default)]
        username: Option<String>,
        kind: String,
        #[serde(default = "default_json_object")]
        metadata_json: String,
        #[serde(default)]
        replacement_secret: Option<String>,
        /// With `replacement_secret`, the complete new set of secondary
        /// secret fields.
        #[serde(default)]
        replacement_fields: Option<std::collections::BTreeMap<String, String>>,
    },
    /// Answer an agent's `ask_for_pass` popup: seal the typed secret straight
    /// into Passes under the runtime-bound owner of that request and return
    /// the metadata-only receipt the UI then submits as the ask's answer.
    FulfillRequest {
        ask_id: String,
        kind: String,
        #[serde(default)]
        site: Option<String>,
        #[serde(default)]
        label: Option<String>,
        #[serde(default)]
        username: Option<String>,
        #[serde(default = "default_json_object")]
        metadata_json: String,
        secret: String,
        #[serde(default)]
        fields: std::collections::BTreeMap<String, String>,
    },
    Reveal {
        credential_id: String,
        #[serde(default)]
        master_password: Option<String>,
    },
    Delete {
        credential_id: String,
    },
}

fn default_json_object() -> String {
    "{}".to_string()
}

impl VaultCommand {
    fn action_name(&self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Initialize { .. } => "initialize",
            Self::UnlockWithPassword { .. } => "unlock_with_password",
            Self::UnlockWithRecoveryKey { .. } => "unlock_with_recovery_key",
            Self::Lock => "lock",
            Self::ChangeMasterPassword { .. } => "change_master_password",
            Self::RotateRecoveryKey => "rotate_recovery_key",
            Self::List => "list",
            Self::Store { .. } => "store",
            Self::Update { .. } => "update",
            Self::FulfillRequest { .. } => "fulfill_request",
            Self::Reveal { .. } => "reveal",
            Self::Delete { .. } => "delete",
        }
    }
}

impl std::fmt::Debug for VaultCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VaultCommand")
            .field("action", &self.action_name())
            .finish_non_exhaustive()
    }
}

impl Drop for VaultCommand {
    fn drop(&mut self) {
        match self {
            Self::Initialize { master_password } | Self::UnlockWithPassword { master_password } => {
                master_password.zeroize()
            }
            Self::UnlockWithRecoveryKey { recovery_key } => recovery_key.zeroize(),
            Self::ChangeMasterPassword {
                current_password,
                new_password,
            } => {
                current_password.zeroize();
                new_password.zeroize();
            }
            Self::Store { secret, fields, .. } | Self::FulfillRequest { secret, fields, .. } => {
                secret.zeroize();
                fields.values_mut().for_each(Zeroize::zeroize);
            }
            Self::Update {
                replacement_secret,
                replacement_fields,
                ..
            } => {
                if let Some(secret) = replacement_secret.as_mut() {
                    secret.zeroize();
                }
                if let Some(fields) = replacement_fields.as_mut() {
                    fields.values_mut().for_each(Zeroize::zeroize);
                }
            }
            Self::Reveal {
                master_password: Some(password),
                ..
            } => password.zeroize(),
            _ => {}
        }
    }
}

/// Vault results returned only to the local Settings client. Recovery keys
/// and revealed credential values are wiped on drop and redacted from Debug.
#[derive(Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum VaultReply {
    Status {
        status: String,
    },
    RecoveryKey {
        recovery_key: String,
    },
    Unlocked,
    Locked,
    PasswordChanged,
    Credentials {
        credentials: Vec<crate::security::vault::CredentialMetadata>,
    },
    Stored {
        credential: crate::security::vault::CredentialMetadata,
    },
    Revealed {
        credential: crate::security::vault::CredentialMetadata,
        secret: String,
        #[serde(default)]
        fields: std::collections::BTreeMap<String, String>,
    },
    Deleted {
        deleted: bool,
    },
    /// Metadata-only receipt for an answered `ask_for_pass` popup. `answer`
    /// is exactly what the UI submits through AnswerAsk.
    PassRequestFulfilled {
        credential: crate::security::vault::CredentialMetadata,
        answer: String,
    },
}

impl std::fmt::Debug for VaultReply {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let result = match self {
            Self::Status { .. } => "status",
            Self::RecoveryKey { .. } => "recovery_key",
            Self::Unlocked => "unlocked",
            Self::Locked => "locked",
            Self::PasswordChanged => "password_changed",
            Self::Credentials { .. } => "credentials",
            Self::Stored { .. } => "stored",
            Self::Revealed { .. } => "revealed",
            Self::Deleted { .. } => "deleted",
            Self::PassRequestFulfilled { .. } => "pass_request_fulfilled",
        };
        formatter
            .debug_struct("VaultReply")
            .field("result", &result)
            .finish_non_exhaustive()
    }
}

impl Drop for VaultReply {
    fn drop(&mut self) {
        match self {
            Self::RecoveryKey { recovery_key } => recovery_key.zeroize(),
            Self::Revealed { secret, fields, .. } => {
                secret.zeroize();
                fields.values_mut().for_each(Zeroize::zeroize);
            }
            _ => {}
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum WireRequest {
    /// Lightweight readiness/compatibility handshake.  Clients must use this
    /// instead of treating an accepted socket as proof that the daemon speaks
    /// their wire contract or came from their selected executable.
    ProtocolInfo,
    Ping,
    /// Authoritative company-runtime projection. Unlike story events, this is
    /// safe to use for reconnect reconciliation and cannot leave stale work
    /// animations behind when a terminal frame was missed.
    CompanySnapshot {
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        owner: Option<ConversationOwnerRef>,
    },
    /// Local Settings credential manager. This is deliberately handled in the
    /// long-lived gateway so an unlock applies to subsequent agent actions.
    Vault(VaultCommand),
    /// Resumable first-run product setup. Provider and vault secrets remain in
    /// their dedicated control paths; this carries only non-secret choices.
    Onboarding(crate::onboarding::OnboardingCommand),
    /// Authoritative coworkers, groups, canonical threads, and sidebar state.
    /// Mutations and reads share this typed boundary so the desktop cannot
    /// drift from the event-sourced company directory.
    CompanyDirectory(crate::runtime::company_control::CompanyDirectoryCommand),
    /// "Teach agent" control plane. The desktop records direct human browser
    /// actions against the selected coworker's private profile and saves a
    /// secret-safe semantic workflow in ~/.phoenix.
    TeachWorkflow(crate::runtime::workflow_teaching::TeachWorkflowCommand),
    /// Searchable, scoped, dependency-aware product settings. Secrets stay in
    /// the Vault request path and never cross this non-secret boundary.
    Settings(crate::settings::SettingsCommand),
    /// Resolve the exact stable coworker ids a group prompt would wake. The
    /// returned roster fingerprint must accompany the eventual Turn so a
    /// membership change between preview and send is recoverable, not silent.
    GroupActivationPreview {
        group_id: String,
        user_request: String,
    },
    Turn {
        session_id: String,
        /// Client-minted identity for this authored turn. It is optional only
        /// for legacy/internal clients; Canvas supplies it on every send and
        /// reuses it if a socket reconnect retries the same submission.
        #[serde(default)]
        turn_id: Option<String>,
        user_request: String,
        /// Execute is the normal tool-using turn. Plan permits discovery and
        /// questions but mechanically blocks mutations and external effects.
        #[serde(default)]
        interaction_mode: crate::runtime::InteractionMode,
        /// Three-tier product permission. New clients send this; old clients
        /// continue to use `yolo` below and stored anchors migrate on recall.
        #[serde(default)]
        permission_mode: Option<crate::tools::PermissionMode>,
        /// Run tools unconfined (CLI `/yolo`). `None` = the client doesn't
        /// know (canvas/browser faces) — the daemon replays the session's
        /// anchored mode; a session that never had one runs confined.
        #[serde(default)]
        yolo: Option<bool>,
        /// The CLIENT's working directory — the workspace the user is actually
        /// in. The daemon must never substitute its own process cwd (it may
        /// have been started anywhere, e.g. $HOME). `None` = the client
        /// doesn't know (canvas/browser faces) — the daemon replays the
        /// session's anchored workspace, exactly like an internal wake; the
        /// Globe-prompt incident (2026-07-10) was canvas turns falling to
        /// daemon cwd + confined mode in a yolo session anchored elsewhere.
        #[serde(default)]
        workspace: Option<PathBuf>,
        /// Journal mode (plan 015 phase 0): stream this turn's events as
        /// daemon-reduced `Story` rows instead of raw events — the lane GUI
        /// faces render. `AskUser` (full question set for the popup) and
        /// `Done` still cross raw. Default off: the TUI keeps raw events.
        #[serde(default)]
        journal: bool,
        /// Route this turn directly to a specialist (e.g. "frontend", "coder")
        /// instead of the orchestrator. `None` or `"orchestrator"` = the
        /// existing behavior (orchestrator receives the turn). The name is
        /// resolved via `AgentAddress::from_talk_name()`.
        #[serde(default)]
        target_agent: Option<String>,
        /// Address a first-class company group. Mutually exclusive with
        /// `target_agent`; the group's own canonical thread is `session_id`.
        #[serde(default)]
        target_group: Option<String>,
        /// Authoritative activation decision produced by
        /// `GroupActivationPreview`. Old clients may omit it; the daemon then
        /// resolves once at receipt for backwards compatibility.
        #[serde(default)]
        group_activation: Option<crate::runtime::group_conversation::GroupActivationIntent>,
        /// Requested delivery. One-to-one conversations always steer a
        /// message sent while they work; groups keep their ordered queue.
        #[serde(default)]
        delivery: TurnDelivery,
        /// Canvas sticky notes the user pinned on the board — injected into
        /// the agent's context so it can read and reference them.
        #[serde(default)]
        sticky_notes: Option<Vec<StickyNoteData>>,
        /// The canvas viewport — where the user is currently looking. Used
        /// for spatial context scoping: nearby sticky notes get full
        /// priority, distant notes get a summary, far notes are omitted.
        /// `None` (old clients) = no spatial filtering.
        #[serde(default)]
        viewport: Option<ViewportData>,
        /// Images, files, or folders the user attached to THIS message.
        /// Pasted images are copied to ~/.phoenix/attachments first;
        /// explicitly selected files/folders retain their absolute path. The
        /// turn builder injects guidance for the matching tool kind.
        /// `None`/empty = no attachments.
        #[serde(default)]
        attachments: Option<Vec<String>>,
    },
    /// Composer queue shown in the narrow expandable block above the input.
    QueuedTurns {
        session_id: String,
        #[serde(default)]
        owner: Option<ConversationOwnerRef>,
    },
    /// Authoritative structured task list for the selected canonical thread.
    /// This is ephemeral runtime state by design, but it must never be
    /// reconstructed from prose or a compact tool receipt in the desktop UI.
    TodoList {
        session_id: String,
        #[serde(default)]
        owner: Option<ConversationOwnerRef>,
    },
    /// Durable question cards for the canonical conversation, including
    /// pending asks and their resolved transcript rows. This is the restart
    /// recovery path for questions that are not yet a ToolResult.
    ConversationAsks {
        session_id: String,
        #[serde(default)]
        owner: Option<ConversationOwnerRef>,
    },
    CancelQueuedTurn {
        session_id: String,
        queue_id: String,
        #[serde(default)]
        owner: Option<ConversationOwnerRef>,
    },
    /// Convert a waiting direct-agent/Phoenix prompt into an explicit steer.
    /// Group discussions keep their next-turn ordering and reject this action.
    SteerQueuedTurn {
        session_id: String,
        queue_id: String,
        #[serde(default)]
        owner: Option<ConversationOwnerRef>,
    },
    /// Stop the running turn for a session (user pressed Esc). The turn task
    /// is aborted; when target_agent is present, only that specialist's
    /// foreground/background task is eligible.
    Cancel {
        session_id: String,
        #[serde(default)]
        target_agent: Option<String>,
        #[serde(default)]
        owner: Option<ConversationOwnerRef>,
    },
    /// Irreversibly remove one authored prompt boundary from the canonical
    /// transcript. `turns_from_end=0` selects the newest user turn. Removing a
    /// prompt also removes its complete response; `Agent` keeps the prompt and
    /// removes every assistant/tool/talk message until the next user turn.
    DeleteTranscriptTurn {
        session_id: String,
        turns_from_end: usize,
        expected_prompt: String,
        scope: TranscriptDeleteScope,
        /// Exact durable question cards rendered inside this turn.  The
        /// desktop derives these from the selected display-turn id; older
        /// clients omit the list safely.
        #[serde(default)]
        ask_ids: Vec<String>,
        #[serde(default)]
        owner: Option<ConversationOwnerRef>,
    },
    /// Long-lived event stream for a session: background-agent spawns,
    /// returns, and their working steps — pushed the moment they happen, even
    /// when no turn is running. The TUI holds one of these per session.
    Subscribe {
        session_id: String,
        #[serde(default)]
        owner: Option<ConversationOwnerRef>,
    },
    /// The same stream reduced daemon-side into the calm journal
    /// (`runtime::story`): narration + aggregated receipts at narrative
    /// beats, handoffs, return/watcher cards. New faces (canvas, mobile)
    /// subscribe here so every UI tells the same story (plan 015 phase 0).
    SubscribeJournal {
        session_id: String,
        #[serde(default)]
        owner: Option<ConversationOwnerRef>,
    },
    /// Live browser view: the screencast frames of every managed browser tab
    /// (the page the current coworker is driving), pushed as they render. The app draws
    /// them into an in-canvas browser window — the browser itself stays
    /// headless. Frames bypass the postbox/story lanes entirely (no replay,
    /// no persistence); this stream exists only while a client holds it.
    SubscribeBrowser,
    /// Canvas click-to-help: forward one user click into a managed browser
    /// instance's live page (page CSS-pixel coordinates).
    BrowserClick {
        instance: String,
        x: f64,
        y: f64,
    },
    /// Direct human control of a coworker's private browser profile. This is
    /// also the login-window path; typed secrets inherit BrowserUserAction's
    /// redacted Debug and zeroizing Drop behavior and are never journaled.
    BrowserInteract {
        instance: String,
        browser_action: crate::tools::browser_native::BrowserUserAction,
    },
    /// Lifecycle for the compositor-backed Chromium window embedded by the
    /// desktop. This controls the same managed CDP/profile session as agent
    /// tools; it never creates a separate browser or cookie jar.
    BrowserSurface {
        instance: String,
        action: crate::tools::browser_native::BrowserSurfaceAction,
    },
    /// List already-created agent desktops without opening or controlling any.
    DesktopWorkspaces,
    DesktopObservation {
        scope_key: String,
        #[serde(default)]
        after_ms: Option<u64>,
    },
    /// The user explicitly ended a session (`/quit`): digest it into memory
    /// NOW instead of waiting for the daemon's idle-threshold timer, so a
    /// session started minutes later already remembers where this one left off.
    DigestSession {
        session_id: String,
        #[serde(default)]
        owner: Option<ConversationOwnerRef>,
    },
    /// A fresh start on the client (`/new`, `/clear`): index the saved-but-
    /// unprocessed memory backlog NOW so the fresh context can recall
    /// everything already saved. Digest-free — `/clear` wipes the screen,
    /// not the session, so there is no ending session to digest.
    IndexMemory,
    /// The user answered an `ask_user` popup: resolve the awaiting agent turn.
    AnswerAsk {
        ask_id: String,
        answer: String,
        /// Current Canvas binds decisions to the conversation that rendered
        /// the card. Optional only for legacy CLI/TUI clients.
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        owner: Option<ConversationOwnerRef>,
    },
    /// The user closed an ask card without selecting an answer. This is a
    /// typed cancellation, never a synthetic answer and never a wake turn.
    DismissAsk {
        ask_id: String,
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        owner: Option<ConversationOwnerRef>,
    },
}

fn validate_wire_request_session_ids(request: &WireRequest) -> Result<()> {
    let validate = |session_id: &str| {
        crate::session::SessionStore::validate_session_id(session_id).context("invalid session_id")
    };
    match request {
        WireRequest::CompanySnapshot {
            session_id: Some(session_id),
            ..
        }
        | WireRequest::Turn { session_id, .. }
        | WireRequest::QueuedTurns { session_id, .. }
        | WireRequest::TodoList { session_id, .. }
        | WireRequest::ConversationAsks { session_id, .. }
        | WireRequest::CancelQueuedTurn { session_id, .. }
        | WireRequest::SteerQueuedTurn { session_id, .. }
        | WireRequest::Cancel { session_id, .. }
        | WireRequest::DeleteTranscriptTurn { session_id, .. }
        | WireRequest::Subscribe { session_id, .. }
        | WireRequest::SubscribeJournal { session_id, .. }
        | WireRequest::DigestSession { session_id, .. } => validate(session_id),
        WireRequest::AnswerAsk {
            session_id: Some(session_id),
            ..
        }
        | WireRequest::DismissAsk {
            session_id: Some(session_id),
            ..
        } => validate(session_id),
        WireRequest::ProtocolInfo
        | WireRequest::Ping
        | WireRequest::CompanySnapshot {
            session_id: None, ..
        }
        | WireRequest::Vault(_)
        | WireRequest::Onboarding(_)
        | WireRequest::CompanyDirectory(_)
        | WireRequest::TeachWorkflow(_)
        | WireRequest::Settings(_)
        | WireRequest::GroupActivationPreview { .. }
        | WireRequest::SubscribeBrowser
        | WireRequest::BrowserClick { .. }
        | WireRequest::BrowserInteract { .. }
        | WireRequest::BrowserSurface { .. }
        | WireRequest::DesktopWorkspaces
        | WireRequest::DesktopObservation { .. }
        | WireRequest::IndexMemory
        | WireRequest::AnswerAsk {
            session_id: None, ..
        }
        | WireRequest::DismissAsk {
            session_id: None, ..
        } => Ok(()),
    }
}

fn scoped_request_owner(
    request: &WireRequest,
) -> Result<Option<(&str, Option<&ConversationOwnerRef>)>> {
    let scoped = match request {
        WireRequest::CompanySnapshot {
            session_id: Some(session_id),
            owner,
        }
        | WireRequest::QueuedTurns { session_id, owner }
        | WireRequest::TodoList { session_id, owner }
        | WireRequest::ConversationAsks { session_id, owner }
        | WireRequest::CancelQueuedTurn {
            session_id, owner, ..
        }
        | WireRequest::SteerQueuedTurn {
            session_id, owner, ..
        }
        | WireRequest::Cancel {
            session_id, owner, ..
        }
        | WireRequest::DeleteTranscriptTurn {
            session_id, owner, ..
        }
        | WireRequest::Subscribe {
            session_id, owner, ..
        }
        | WireRequest::SubscribeJournal {
            session_id, owner, ..
        }
        | WireRequest::DigestSession {
            session_id, owner, ..
        } => Some((session_id.as_str(), owner.as_ref())),
        WireRequest::CompanySnapshot {
            session_id: None,
            owner: Some(_),
        } => anyhow::bail!("a conversation owner requires a scoped company snapshot"),
        _ => None,
    };
    Ok(scoped)
}

fn authorize_wire_request_owner_against_snapshot(
    request: &WireRequest,
    snapshot: &crate::runtime::company_directory::DirectorySnapshot,
) -> Result<()> {
    let Some((session_id, owner)) = scoped_request_owner(request)? else {
        return Ok(());
    };
    crate::runtime::agent_conversation::authorize_canonical_session(snapshot, session_id, owner)
}

/// Ask ids are opaque action handles, not conversation authorization. Bind a
/// current Canvas decision back to both the canonical session that rendered
/// the card and its immutable owner before the pending receiver/history can be
/// changed. Legacy clients omit both optional fields and retain compatibility.
fn ask_request_scope(
    request: &WireRequest,
) -> Option<(&str, Option<&str>, Option<&ConversationOwnerRef>)> {
    match request {
        WireRequest::AnswerAsk {
            ask_id,
            session_id,
            owner,
            ..
        }
        | WireRequest::DismissAsk {
            ask_id,
            session_id,
            owner,
        } => Some((ask_id.as_str(), session_id.as_deref(), owner.as_ref())),
        _ => None,
    }
}

fn authorize_ask_request_owner_against_snapshot(
    request: &WireRequest,
    durable_ask_session_id: Option<&str>,
    snapshot: &crate::runtime::company_directory::DirectorySnapshot,
) -> Result<bool> {
    let Some((ask_id, session_id, owner)) = ask_request_scope(request) else {
        return Ok(false);
    };
    // Legacy clients omit both fields. Once a caller asserts a session, it
    // must match the durable question even without a newer owner assertion.
    if owner.is_none() && session_id.is_none() {
        return Ok(true);
    }
    let session_id = session_id.context("a conversation owner requires an ask session_id")?;
    let durable_ask_session_id = durable_ask_session_id
        .with_context(|| format!("ask `{ask_id}` has no durable conversation scope"))?;
    anyhow::ensure!(
        durable_ask_session_id == session_id,
        "ask `{ask_id}` belongs to `{durable_ask_session_id}`, not `{session_id}`"
    );
    if let Some(owner) = owner {
        crate::runtime::agent_conversation::authorize_canonical_session(
            snapshot,
            session_id,
            Some(owner),
        )?;
    }
    Ok(true)
}

fn authorize_ask_request_owner(request: &WireRequest) -> Result<bool> {
    let Some((ask_id, session_id, owner)) = ask_request_scope(request) else {
        return Ok(false);
    };
    if owner.is_none() && session_id.is_none() {
        return Ok(true);
    }
    let record = crate::runtime::asks::decision_record_for(ask_id)?;
    let snapshot = if owner.is_some() {
        crate::runtime::company::global()?.directory_snapshot()?
    } else {
        crate::runtime::company_directory::DirectorySnapshot::default()
    };
    authorize_ask_request_owner_against_snapshot(
        request,
        record.as_ref().map(|record| record.session_id.as_str()),
        &snapshot,
    )
}

/// Current Canvas supplies an immutable owner assertion on every scoped
/// conversation operation. Legacy CLI/TUI requests remain compatible when the
/// field is absent, while a supplied mismatch is rejected before any queue,
/// transcript, ask, or subscription state is touched.
fn authorize_wire_request_owner(request: &WireRequest) -> Result<()> {
    if authorize_ask_request_owner(request)? {
        return Ok(());
    }
    let Some((_session_id, owner)) = scoped_request_owner(request)? else {
        return Ok(());
    };
    if owner.is_none() {
        return Ok(());
    }
    let snapshot = crate::runtime::company::global()?.directory_snapshot()?;
    authorize_wire_request_owner_against_snapshot(request, &snapshot)
}

/// Validate a group wake decision against the current authoritative roster
/// and against the actual prompt being submitted. This closes both stale-send
/// and payload-substitution gaps: a preview for one set of mentions cannot be
/// reused with different text, and a queued turn cannot wake newly added
/// coworkers after its composer preview.
fn validated_group_activation_intent(
    target_group: Option<&str>,
    user_request: &str,
    supplied: Option<&crate::runtime::group_conversation::GroupActivationIntent>,
) -> Result<Option<crate::runtime::group_conversation::GroupActivationIntent>> {
    let Some(group_id) = target_group else {
        anyhow::ensure!(
            supplied.is_none(),
            "group activation was supplied for a non-group conversation"
        );
        return Ok(None);
    };
    let snapshot = crate::runtime::company::global()?.directory_snapshot()?;
    validated_group_activation_intent_against_snapshot(&snapshot, group_id, user_request, supplied)
}

/// Every delivery value is accepted for every conversation kind. Group rooms
/// used to reject `Steer` because a room message was queued as one ordered
/// turn; a room message is now heard by every running member and acted on by
/// the addressed ones (see `route_room_message`), so there is nothing to
/// reject. The function stays as the wire seam for future delivery values.
fn validate_turn_delivery(_target_group: Option<&str>, _delivery: TurnDelivery) -> Result<()> {
    Ok(())
}

/// What a Send actually does when its conversation is already working: it is
/// never queued behind the running turn. One-to-one conversations deliver it
/// into that turn's steering inbox; group rooms write it to the canonical
/// room transcript and route it to the running/idle members (see
/// `route_room_message`). An idle conversation runs the message as a normal
/// turn either way. Old clients that still send `queue` get the same.
fn effective_turn_delivery(_target_group: Option<&str>, _requested: TurnDelivery) -> TurnDelivery {
    TurnDelivery::Steer
}

/// Acknowledgement for a user message delivered into a running turn.
///
/// A steer acknowledgement is a delivery receipt, never the agent's answer:
/// its `final_markdown` stays empty so no client can render the receipt as a
/// reply ("message delivered to Avery" once showed up as Avery's answer).
/// The receipt text goes to the gateway log instead.
fn steered_turn_summary(session_id: &str, receipt: String) -> TurnSummary {
    glog(&format!("turn [{session_id}]: steer acknowledged ({receipt})"));
    TurnSummary {
        completion: TurnCompletion::Steered,
        final_markdown: String::new(),
        main_session_id: session_id.to_string(),
        run_id: String::new(),
        trace_path: String::new(),
        route: "talk".to_string(),
        total_tokens: 0,
        orchestrator_tokens: None,
        coder_tokens: None,
        compression_saved_tokens: 0,
        compression_raw_tokens: 0,
        context_window: None,
        background_work_pending: crate::runtime::postbox::has_background_work(session_id),
    }
}

fn validated_group_activation_intent_against_snapshot(
    snapshot: &crate::runtime::company_directory::DirectorySnapshot,
    group_id: &str,
    user_request: &str,
    supplied: Option<&crate::runtime::group_conversation::GroupActivationIntent>,
) -> Result<Option<crate::runtime::group_conversation::GroupActivationIntent>> {
    let prompt_preview = crate::runtime::group_conversation::preview_group_activation(
        snapshot,
        group_id,
        user_request,
    )?;
    let mut intent = supplied.cloned().unwrap_or_else(|| prompt_preview.intent());
    anyhow::ensure!(
        intent.group_id == group_id,
        "group activation belongs to a different group"
    );
    let validated =
        crate::runtime::group_conversation::validate_group_activation(snapshot, &intent)?;
    anyhow::ensure!(
        validated.selection == prompt_preview.selection
            && validated.active_agent_ids == prompt_preview.active_agent_ids
            && validated.execution_mode == prompt_preview.execution_mode
            && validated.execution_waves == prompt_preview.execution_waves,
        "group mentions changed or explicit dependency order changed after the activation preview; refresh and send again"
    );
    if intent.execution_dependencies.is_some() {
        anyhow::ensure!(intent.execution_dependencies == prompt_preview.execution_dependencies,
            "group dependencies changed after preview; refresh and send again");
    } else {
        // Upgrade a freshly submitted old-client preview from the same checked
        // prompt. Already queued legacy plans keep their stored wave meaning.
        intent.execution_dependencies = prompt_preview.execution_dependencies;
    }
    Ok(Some(intent))
}

struct RunningTurn {
    handle: tokio::task::AbortHandle,
    /// None is Phoenix/orchestrator. Some(base) is a foreground turn submitted
    /// directly through that specialist's Canvas window.
    target_agent: Option<String>,
    /// First-class group id when this is a group discussion. Group messages
    /// sent while it runs enter that group's durable follow-up queue.
    target_group: Option<String>,
    /// Exact composer/queued prompt for a user-authored foreground turn. An
    /// internal routine or return wake leaves this empty.
    authored_prompt: Option<String>,
    /// Cheap generation fence captured before the runtime synchronously saves
    /// the new user boundary. Permanent deletion uses it to distinguish an
    /// immediately-aborted, never-persisted duplicate prompt from an older
    /// identical turn already on disk.
    session_file_before: Option<SessionFileVersion>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionFileVersion {
    len: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    inode: u64,
}

fn session_file_version(session_id: &str) -> Option<SessionFileVersion> {
    crate::session::SessionStore::validate_session_id(session_id).ok()?;
    let path = crate::config::phoenix_home()
        .join("sessions")
        .join(format!("{session_id}.json"));
    let metadata = std::fs::metadata(path).ok()?;
    Some(SessionFileVersion {
        len: metadata.len(),
        modified: metadata.modified().ok(),
        #[cfg(unix)]
        inode: {
            use std::os::unix::fs::MetadataExt;
            metadata.ino()
        },
    })
}

/// The conversation is a routing scope, not an execution identity. Keep every
/// live handle and remove only the execution which actually settled. This is
/// necessary before admitting independently resumable group assignments.
#[derive(Default)]
struct RunningTurns {
    sessions: std::collections::HashMap<String, std::collections::HashMap<tokio::task::Id, RunningTurn>>,
}

impl RunningTurns {
    fn insert(&mut self, session: String, turn: RunningTurn) -> tokio::task::Id {
        let id = turn.handle.id();
        self.sessions.entry(session).or_default().insert(id, turn);
        id
    }

    fn remove(&mut self, session: &str, execution: tokio::task::Id) {
        if let Some(turns) = self.sessions.get_mut(session) {
            turns.remove(&execution);
            if turns.is_empty() {
                self.sessions.remove(session);
            }
        }
    }

    fn cancel(&self, session: &str, target: Option<&str>) -> bool {
        let mut aborted = false;
        for turn in self.sessions.get(session).into_iter().flat_map(|turns| turns.values()) {
            if target.is_none_or(|agent| turn.target_agent.as_deref() == Some(agent))
                && !turn.handle.is_finished()
            {
                turn.handle.abort();
                aborted = true;
            }
        }
        aborted
    }

    fn abort_for_deletion(&self, session: &str) -> Vec<(Option<String>, Option<SessionFileVersion>)> {
        self.sessions.get(session).into_iter().flat_map(|turns| turns.values()).map(|turn| {
            turn.handle.abort();
            (turn.authored_prompt.clone(), turn.session_file_before.clone())
        }).collect()
    }
}

fn running_turns() -> &'static std::sync::Mutex<RunningTurns> {
    static REGISTRY: std::sync::OnceLock<std::sync::Mutex<RunningTurns>> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| std::sync::Mutex::new(RunningTurns::default()))
}

#[derive(Debug, Serialize, Deserialize)]
pub enum WireResponse {
    ProtocolInfo {
        protocol: String,
        package_version: String,
        binary_id: Option<String>,
    },
    Pong,
    CompanySnapshot(crate::runtime::company::CompanySnapshot),
    Vault(VaultReply),
    Onboarding(crate::onboarding::OnboardingReply),
    CompanyDirectory(crate::runtime::company_control::CompanyDirectoryView),
    TeachWorkflow(crate::runtime::workflow_teaching::TeachWorkflowReply),
    Settings(crate::settings::SettingsReply),
    GroupActivationPreview(crate::runtime::group_conversation::GroupActivationPreview),
    QueuedTurns(Vec<super::turn_queue::QueuedTurnSummary>),
    TodoList(Vec<crate::tools::TodoItem>),
    ConversationAsks(Vec<crate::runtime::asks::AskRecord>),
    AskAnswered {
        disposition: String,
        /// Exact durable successor for a detached question. Clients can
        /// subscribe before answering, then match its final story by turn ID.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        continuation_turn_id: Option<String>,
    },
    TranscriptDeleted {
        removed_messages: usize,
        deleted_prompt: bool,
    },
    Event(CliEvent),
    /// One calm journal row from the daemon-side story reduction
    /// (`SubscribeJournal` connections only).
    Story(crate::runtime::story::StoryEvent),
    /// A missed row from the bounded current-turn replay sent when a journal
    /// subscriber reattaches after visiting another conversation.
    StoryReplay(crate::runtime::story::StoryEvent),
    /// One live screencast frame from a managed browser tab
    /// (`SubscribeBrowser` connections only).
    BrowserFrame(crate::tools::browser_native::BrowserFrame),
    BrowserInteraction(crate::tools::browser_native::BrowserInteractionReceipt),
    BrowserSurface(crate::tools::browser_native::BrowserSurfaceReply),
    DesktopWorkspaces(Vec<crate::tools::isolated_desktop::DesktopView>),
    DesktopObservation(crate::tools::isolated_desktop::DesktopObservation),
    Done(TurnSummary),
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TranscriptDeleteScope {
    Prompt,
    Agent,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TurnCompletion {
    #[default]
    Unknown,
    Completed,
    Incomplete,
    Queued,
    Steered,
    Canceled,
}

impl From<crate::runtime::OutcomeCompletion> for TurnCompletion {
    fn from(value: crate::runtime::OutcomeCompletion) -> Self {
        match value {
            crate::runtime::OutcomeCompletion::Completed => Self::Completed,
            crate::runtime::OutcomeCompletion::Incomplete => Self::Incomplete,
            crate::runtime::OutcomeCompletion::Unknown => Self::Unknown,
        }
    }
}

/// Everything the CLI needs to print the end-of-turn answer and footer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnSummary {
    /// Completion of this request, independent of session-wide background work.
    #[serde(default)]
    pub completion: TurnCompletion,
    pub final_markdown: String,
    pub main_session_id: String,
    pub run_id: String,
    pub trace_path: String,
    pub route: String,
    pub total_tokens: u32,
    pub orchestrator_tokens: Option<(u32, u32)>,
    pub coder_tokens: Option<(u32, u32)>,
    /// Tokens (estimated, bytes/4) the tool-output compressor kept out of model
    /// context this turn. Durable session auto-compaction reports separately as
    /// gateway notices.
    #[serde(default)]
    pub compression_saved_tokens: u64,
    /// Tokens (estimated, bytes/4) of RAW tool output that entered the
    /// compressor this turn — the denominator that makes the saved figure
    /// provable (saved/raw = the ratio the TUI shows).
    #[serde(default)]
    pub compression_raw_tokens: u64,
    /// Context window usage: (used_tokens, limit_tokens) — the model's context
    /// window capacity and how much was used this turn. `used` is the
    /// orchestrator's input (prompt) tokens; `limit` is the model's context
    /// window from the provider catalog. None if either is unknown.
    #[serde(default)]
    pub context_window: Option<(u32, u32)>,
    /// Detached work is still running, or a completed result is waiting in
    /// the postbox. This is session-wide activity, not completion of the
    /// request described by this summary. Correlated clients use `completion`.
    #[serde(default)]
    pub background_work_pending: bool,
}

// ── Server ────────────────────────────────────────────────────────────

fn publish_gateway_pid(
    listener: UnixListener,
    socket: &Path,
    pidfile: &Path,
) -> Result<UnixListener> {
    if let Err(error) = crate::config::private_io::atomic_write_private(
        pidfile,
        std::process::id().to_string().as_bytes(),
    ) {
        // Binding without publishing an identity must never leave a listener
        // that looks startable but cannot later be verified and stopped.
        drop(listener);
        if let Err(cleanup_error) = std::fs::remove_file(socket) {
            if cleanup_error.kind() != std::io::ErrorKind::NotFound {
                return Err(anyhow::anyhow!(
                    "gateway bound {} but could not publish its pidfile {} ({error:#}); socket cleanup also failed: {cleanup_error}",
                    socket.display(),
                    pidfile.display()
                ));
            }
        }
        return Err(error).with_context(|| {
            format!(
                "gateway bound {} but could not publish its pidfile {}",
                socket.display(),
                pidfile.display()
            )
        });
    }
    Ok(listener)
}

/// Claim and publish the gateway listener while the caller holds the lifecycle
/// lease. The final connect check and stale-socket removal must be in the same
/// locked transaction as bind + pid publication: otherwise two bare daemon
/// starters can both observe a stale endpoint and the loser can unlink the
/// winner's newly-bound socket.
async fn claim_gateway_listener_locked(socket: &Path, pidfile: &Path) -> Result<UnixListener> {
    match UnixStream::connect(socket).await {
        Ok(_) => {
            anyhow::bail!(
                "a phoenix gateway is already running ({}). Open a session with `phoenix start`.",
                socket.display()
            );
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "gateway endpoint {} could not be safely classified as stale; refusing to unlink it",
                    socket.display()
                )
            });
        }
    }

    // A failed connect plus an existing directory entry is the normal stale
    // Unix-socket case. Because the lifecycle lease is still held, no Phoenix
    // starter can publish a replacement between this check and removal.
    match std::fs::remove_file(socket) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("remove stale gateway socket {}", socket.display()));
        }
    }

    if let Some(parent) = socket.parent() {
        crate::config::private_io::prepare_private_parent(socket)
            .with_context(|| format!("failed to prepare gateway home {}", parent.display()))?;
    }
    let listener = UnixListener::bind(socket)
        .with_context(|| format!("failed to bind gateway socket at {}", socket.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) = std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))
        {
            drop(listener);
            let cleanup = std::fs::remove_file(socket);
            if let Err(cleanup_error) = cleanup {
                if cleanup_error.kind() != std::io::ErrorKind::NotFound {
                    return Err(anyhow::anyhow!(
                        "failed to secure gateway socket {} ({error}); cleanup also failed: {cleanup_error}",
                        socket.display()
                    ));
                }
            }
            return Err(error).with_context(|| {
                format!(
                    "failed to make gateway socket {} owner-only",
                    socket.display()
                )
            });
        }
    }
    publish_gateway_pid(listener, socket, pidfile)
}

/// Run the gateway daemon until Ctrl-C/SIGTERM. Bare `phoenix` lands here.
pub async fn run_daemon() -> Result<()> {
    // Pin the daemon's process CWD to Phoenix's shared company workspace.
    //
    // The gateway inherits its CWD from whoever launched it — the canvas app
    // (→ `phoenix_agent/canvas-app`), a terminal, `setsid` from a build dir,
    // etc. That inherited dir leaks in two places that fall back to
    // `env::current_dir()`: the Oracle sweep/skill-card pass, and the
    // per-turn workspace resolver in `spawn_event_loop` (when a turn arrives
    // with no workspace AND no session anchor). The failure mode is agents
    // silently operating on Phoenix's OWN source (canvas-app has a Cargo.toml,
    // so it even reads as a "project root" to the sweep). A daemon has no
    // meaningful CWD of its own, so anchor it at ~/.phoenix/workspace. Every real
    // turn still carries its own workspace (canvas folder-picker / CLI cwd),
    // which overrides this — this only governs the no-workspace fallback.
    let default_workspace =
        crate::config::ensure_phoenix_home().map(|_| crate::config::phoenix_workspace_root());
    match default_workspace {
        Ok(workspace) => match std::env::set_current_dir(&workspace) {
            Ok(()) => glog(&format!(
                "gateway: pinned working dir to {}",
                workspace.display()
            )),
            Err(error) => glog(&format!(
                "gateway: could NOT pin working dir to {} ({error:#}) — workspace fallback may use the launch dir",
                workspace.display()
            )),
        },
        Err(error) => glog(&format!(
            "gateway: could NOT prepare the shared workspace ({error:#})"
        )),
    }
    let mut terminate_signal =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .context("install gateway SIGTERM handler")?;
    let path = socket_path();
    let pidfile = pid_path();
    let startup_lifecycle = GatewayLifecycleLock::acquire().await?;
    // Fail quickly for the common already-running case. The second check in
    // `claim_gateway_listener_locked` is the authoritative one after any
    // retiring-process fence has completed.
    if UnixStream::connect(&path).await.is_ok() {
        anyhow::bail!(
            "a phoenix gateway is already running ({}). Open a session with `phoenix start`.",
            path.display()
        );
    }
    wait_for_retiring_gateway_pidfile().await?;
    let listener = claim_gateway_listener_locked(&path, &pidfile).await?;
    // The daemon is externally identifiable before another supervisor enters
    // the lifecycle transaction. From here onward, contenders observe a live
    // listener and cannot reclaim it as stale.
    drop(startup_lifecycle);
    match recover_pending_transcript_deletions() {
        Ok(0) => {}
        Ok(count) => glog(&format!(
            "transcript deletion: recovered {count} interrupted permanent deletion(s)"
        )),
        Err(error) => {
            anyhow::bail!(
                "an interrupted permanent transcript deletion could not be recovered safely: {error:#}"
            )
        }
    }
    println!("phoenix gateway listening on {}", path.display());
    println!(
        "Log: {}  |  open a session in another terminal with `phoenix start`. Ctrl-C stops the gateway.",
        log_path().display()
    );
    glog("gateway started");
    // Reap only positively-identified Xephyr/Xvfb leases left by a prior
    // gateway before this process starts accepting agent work. This is
    // separate from browser shutdown and never changes the user's DISPLAY.
    crate::tools::isolated_desktop::reclaim_stale_scopes_on_startup();
    match crate::runtime::workflow_teaching::purge_expired_deleted_routines() {
        Ok(0) => {}
        Ok(purged) => glog(&format!(
            "workflows: permanently removed {purged} routine(s) after their 30-day recovery window"
        )),
        Err(error) => glog(&format!(
            "workflows: expired routine cleanup failed ({error:#})"
        )),
    }
    // Plan 019: agents live in ~/.phoenix/agents — materialize the built-in
    // manifests on boot (no-op when present) and load the registry so custom
    // specialists resolve from the very first turn.
    match crate::sub_agents::registry::export_builtin_manifests() {
        Ok(0) => {}
        Ok(written) => glog(&format!(
            "agents: exported {written} built-in manifest(s) to ~/.phoenix/agents"
        )),
        Err(error) => glog(&format!("agents: manifest export failed ({error:#})")),
    }
    crate::sub_agents::registry::refresh();
    {
        let custom = crate::sub_agents::registry::production_custom_roster();
        if !custom.is_empty() {
            let names: Vec<&str> = custom.iter().map(|(role, _)| role.as_str()).collect();
            glog(&format!(
                "agents: {} production custom specialist(s) registered: {}",
                names.len(),
                names.join(", ")
            ));
        }
    }
    // Long provider generations stream for minutes; surface their heartbeat
    // ("model streaming… N KB") in the gateway terminal/log so they never
    // look hung.
    // Requests with a live execution observer deliver progress themselves.
    // Unowned/internal requests may log diagnostics, but must not impersonate
    // a foreground task or broadcast activity into unrelated conversations.
    crate::providers::set_stream_progress_hook(Box::new(|_session_id, line| {
        glog(line);
    }));
    // Plan 015 phase 0: the WebSocket face for webview/browser/mobile
    // clients — a localhost relay onto this same socket, token-gated. Never
    // fatal: if the port is taken the bridge logs and stays down while the
    // unix socket keeps serving.
    tokio::spawn(crate::cli::ws_bridge::run_ws_bridge());

    // One turn at a time PER SESSION: a session's turns and wakes serialize
    // against each other (its file-backed session state is not concurrent-
    // safe), but sessions never block EACH OTHER. This was one GLOBAL mutex
    // until 2026-07-09 — a user's quick question sat queued for minutes
    // behind another session's autonomous goal turn. Connections are still
    // accepted immediately; a same-session collision gets a "queued" notice.
    let turn_locks: TurnLocks = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    // Only the gateway owns group activation executors. A read-only CLI or
    // desktop helper may also open CompanyStore, so recovery belongs here at
    // the actual process restart boundary rather than in CompanyStore::open.
    match crate::runtime::company::global().and_then(|company| company.recover_interrupted_jobs()) {
        Ok(0) => {}
        Ok(recovered) => glog(&format!("company jobs: preserved {recovered} interrupted job(s) as stale")),
        Err(error) => glog(&format!("company jobs: restart recovery failed ({error:#})")),
    }
    match crate::runtime::company::global()
        .and_then(|company| company.recover_stale_group_activations())
    {
        Ok(0) => {}
        Ok(recovered) => glog(&format!(
            "group turns: preserved {recovered} interrupted activation(s) as blocked/stale"
        )),
        Err(error) => glog(&format!(
            "group turns: restart recovery failed; activation receipts left untouched ({error:#})"
        )),
    }
    // Inspection launches must not spend provider quota by replaying saved work
    // or firing overdue schedules. This process-only opt-in never rewrites the
    // user's schedules, queue, permissions, or ordinary interactive task route.
    let background_paused = background_automation_paused(
        std::env::var("PHOENIX_PAUSE_BACKGROUND").ok().as_deref(),
    );
    if background_paused {
        glog("background automation paused for this gateway process; saved schedules and queued work are retained");
    }
    if !background_paused {
        if let Err(error) = enqueue_ready_group_continuations(None) {
            glog(&format!("group continuations remain saved; enqueue recovery failed ({error:#})"));
        }
        match super::turn_queue::recover_interrupted()
            .and_then(|_| super::turn_queue::pending_sessions())
        {
            Ok(sessions) => {
                for session_id in sessions {
                    fire_queued_turn_wake(session_id, Arc::clone(&turn_locks));
                }
            }
            Err(error) => glog(&format!(
                "queued prompts: recovery failed; queue left untouched ({error:#})"
            )),
        }
        match crate::tools::agent_forge::pending_agent_provisioning_roles() {
            Ok(roles) => {
                for role in roles {
                    fire_agent_provisioning_wake(role, Arc::clone(&turn_locks));
                }
            }
            Err(error) => glog(&format!(
                "agent provisioning: restart recovery skipped ({error:#})"
            )),
        }
    }
    // Idle-wake is reserved for user-authored steering. Detached coworker
    // returns remain durable and are absorbed by the next natural turn; they
    // must never manufacture a second answer after the owner already finished.
    let (wake_tx, mut wake_rx) = tokio::sync::mpsc::unbounded_channel();
    crate::runtime::postbox::set_wake_notifier(wake_tx);
    if let Err(error)=crate::tools::terminal_jobs::recover() {
        glog(&format!("terminal jobs: recovery failed without rerunning commands ({error:#})"));
    }
    match crate::runtime::postbox::recover_durable_returns() {
        Ok(_) => {}
        Err(error) => glog(&format!(
            "coworker returns: restart recovery failed; receipts remain in the company store ({error:#})"
        )),
    }
    let mut cron_tick = tokio::time::interval(std::time::Duration::from_secs(30));
    cron_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Full-graph maintenance gets one predictable 12-hour tick per gateway
    // lifetime. Attempts (including failures) are also durably rate-limited:
    // a broken provider or stale graph must never become a retry loop, and a
    // freshly opened desktop must never start a surprise whole-store rebuild.
    // The memory coordinator and the job-local lock prevent overlap.
    let maintain_period = std::time::Duration::from_secs(12 * 60 * 60);
    let mut maintain_tick = tokio::time::interval_at(
        tokio::time::Instant::now() + maintain_period,
        maintain_period,
    );
    maintain_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Session-close digests: every 5 min, scan for main sessions that went
    // idle with undigested activity and remember a project-state note for
    // each (see `runtime::session_digest`). interval_at(now + 5m) doubles as
    // the crash catch-up — a restart digests whatever the crash orphaned.
    let digest_period = std::time::Duration::from_secs(5 * 60);
    let mut digest_tick =
        tokio::time::interval_at(tokio::time::Instant::now() + digest_period, digest_period);
    digest_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let workflow_purge_period = std::time::Duration::from_secs(60 * 60);
    let mut workflow_purge_tick = tokio::time::interval_at(
        tokio::time::Instant::now() + workflow_purge_period,
        workflow_purge_period,
    );
    workflow_purge_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Backlog indexing is already triggered at session boundaries and after a
    // new digest. Do not duplicate it two minutes after every launch: that old
    // catch-up path was the source of an immediate 500%-CPU, 2 GiB spike.
    loop {
        tokio::select! {
            _ = cron_tick.tick(), if !background_paused => {
                fire_due_crons(Arc::clone(&turn_locks));
            }
            _ = maintain_tick.tick(), if !background_paused => {
                if maintenance_elapsed_since_attempt() >= 12 * 60 * 60 {
                    run_librarian_maintenance_job(Arc::clone(&turn_locks));
                }
            }
            _ = digest_tick.tick(), if !background_paused => {
                run_session_digest_job(Arc::clone(&turn_locks));
            }
            _ = workflow_purge_tick.tick(), if !background_paused => {
                match crate::runtime::workflow_teaching::purge_expired_deleted_routines() {
                    Ok(0) => {}
                    Ok(purged) => glog(&format!(
                        "workflows: permanently removed {purged} routine(s) after their 30-day recovery window"
                    )),
                    Err(error) => glog(&format!(
                        "workflows: expired routine cleanup failed ({error:#})"
                    )),
                }
            }
            Some(wake) = wake_rx.recv() => {
                match wake {
                    crate::runtime::postbox::WakeRequest::UserSteer(session_id) =>
                        fire_background_wake(session_id, Arc::clone(&turn_locks)),
                    crate::runtime::postbox::WakeRequest::TerminalCompletion(session_id) =>
                        fire_terminal_wake(session_id, Arc::clone(&turn_locks)),
                }
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted.context("gateway accept failed")?;
                let locks = Arc::clone(&turn_locks);
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(stream, locks).await {
                        glog(&format!("connection error: {error:#}"));
                        eprintln!("gateway: connection error: {error:#}");
                    }
                });
            }
            _ = tokio::signal::ctrl_c() => {
                println!("\nphoenix gateway shutting down.");
                glog("gateway stopped (ctrl-c)");
                break;
            }
            _ = terminate_signal.recv() => {
                println!("\nphoenix gateway shutting down.");
                glog("gateway stopped (sigterm)");
                break;
            }
        }
    }
    // Full quit: wipe every unlocked Passes key before anything else drains.
    crate::security::vault::Vault::lock_all();
    let unsettled=tokio::task::spawn_blocking(||
        crate::tools::terminal_jobs::cancel_all_and_wait(std::time::Duration::from_secs(2))).await.unwrap_or(1);
    if unsettled>0 {glog(&format!("terminal shutdown: {unsettled} job(s) did not confirm termination before the bound"));}
    // Keep the Unix listener and pidfile published while the bounded browser
    // pass flushes profile storage.  A replacement must continue to see this
    // process as the socket owner until its last browser has either cleanly
    // closed or exhausted the shutdown budget.  Withdrawing readiness first
    // allowed a new daemon (and its WS/browser/Cognee state) to start while the
    // old process was still cleaning up.
    glog("gateway shutdown: flushing browser profiles before withdrawing Unix readiness");
    crate::tools::browser_native::shutdown();
    crate::tools::isolated_desktop::shutdown_all();
    glog("gateway shutdown: browser and isolated desktop cleanup finished; withdrawing Unix readiness");
    drop(listener);
    std::fs::remove_file(&path).ok();
    // Leave gateway.pid as an exit fence until the process has actually
    // crossed into exited/zombie state. A stop client removes it after its
    // verified wait; a self-initiated Ctrl-C/SIGTERM leaves it for the next
    // starter's `wait_for_retiring_gateway_pidfile` pass. Removing both files
    // here recreated a small but real WS/browser/Cognee overlap window during
    // Tokio/runtime teardown.
    Ok(())
}

/// A failed memory-indexing pass means every save since the last success is
/// invisible to recall — safe on disk, unsearchable. That must reach the
/// USER, not just the gateway log: one notice per gateway run, into every
/// active session, naming the fix. (The two-day silent write-only store of
/// 2026-07-06 is the scar this guards.)
fn notice_cognify_failure(kind: &str, error: &anyhow::Error) {
    glog(&format!("memory: {kind} indexing FAILED: {error:#}"));
    static NOTICED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !NOTICED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        crate::runtime::postbox::forward_to_active(crate::runtime::CliEvent::GatewayNotice(
            format!(
                "memory indexing is failing — saves are safe but UNSEARCHABLE until it \
                 recovers ({error:#}). Check `phoenix configure` → Model roles → Memory \
                 graph; probe with `cargo test --lib probe_memory_cognify -- --ignored`."
            ),
        ));
    }
}

/// Durable last-run marker for memory maintenance across gateway restarts.
/// Per-session turn locks. Entries are created on first use and live for the
/// gateway's life — a few dozen sessions of `Arc<Mutex<()>>` is nothing.
type TurnLocks = Arc<std::sync::Mutex<std::collections::HashMap<String, Arc<super::session_gate::SessionGate>>>>;

/// The lock that serializes ONE session's turns and wakes.
fn turn_lock_for(locks: &TurnLocks, session_id: &str) -> Arc<super::session_gate::SessionGate> {
    locks
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .entry(session_id.to_string())
        .or_default()
        .clone()
}

fn is_authored_transcript_user(message: &crate::session::Message) -> bool {
    let crate::session::Message::User { content } = message else {
        return false;
    };
    // A message delivered into a running turn is part of THAT turn (the
    // Canvas display gives it the running turn's id), not a new boundary.
    if crate::runtime::postbox::strip_user_steer_marker(content).is_some() {
        return false;
    }
    let normalized = content.trim().to_ascii_lowercase();
    !(normalized.starts_with("[late ask answer]")
        || normalized.starts_with("[queued wake]")
        || normalized.starts_with("[background return]")
        || normalized.starts_with("goal heartbeat")
        || normalized.starts_with("[watcher wake]")
        || (normalized.starts_with("queued prompt queued_")
            && (normalized.ends_with(" is starting") || normalized.contains(" failed:"))))
}

fn transcript_prompt_identity(value: &str) -> String {
    let mut words = value.split_whitespace().peekable();
    while words.peek().is_some_and(|word| word.starts_with('@')) {
        words.next();
    }
    words.collect::<Vec<_>>().join(" ").to_lowercase()
}

fn delete_transcript_turn_from_session(
    session: &mut crate::session::Session,
    turns_from_end: usize,
    expected_prompt: &str,
    scope: TranscriptDeleteScope,
) -> Result<(usize, bool)> {
    let authored = session
        .messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| is_authored_transcript_user(message).then_some(index))
        .collect::<Vec<_>>();
    let selected = authored
        .len()
        .checked_sub(turns_from_end.saturating_add(1))
        .and_then(|ordinal| authored.get(ordinal).copied())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "that prompt is no longer in the live canonical transcript; nothing was deleted"
            )
        })?;
    let selected_prompt = match &session.messages[selected] {
        crate::session::Message::User { content } => content,
        _ => unreachable!("authored indexes contain only user messages"),
    };
    anyhow::ensure!(
        transcript_prompt_identity(selected_prompt) == transcript_prompt_identity(expected_prompt),
        "the visible prompt no longer matches that canonical turn; nothing was deleted"
    );
    let end = authored
        .iter()
        .copied()
        .find(|index| *index > selected)
        .unwrap_or(session.messages.len());
    let start = if scope == TranscriptDeleteScope::Prompt {
        selected
    } else {
        selected.saturating_add(1)
    };
    let removed = end.saturating_sub(start);
    if removed == 0 {
        return Ok((0, false));
    }
    let mut replacement = session.messages.clone();
    replacement.drain(start..end);
    session.replace_messages(replacement);
    Ok((removed, scope == TranscriptDeleteScope::Prompt))
}

fn delete_transcript_turn_blocking(
    session_id: &str,
    turns_from_end: usize,
    expected_prompt: &str,
    scope: TranscriptDeleteScope,
    active_prompt_not_persisted: bool,
    ask_ids: Vec<String>,
) -> Result<(usize, bool)> {
    let root = crate::config::phoenix_home().join("sessions");
    let mut session = crate::session::SessionStore::read_one_from_disk(&root, session_id)?
        .ok_or_else(|| anyhow::anyhow!("session {session_id} does not exist"))?;
    let archive_path = root.join(format!("{session_id}.archive.jsonl"));
    let archive = load_transcript_archive(&archive_path)?;
    if active_prompt_not_persisted && turns_from_end == 0 {
        // The UI can beat the turn's first atomic session save by a few
        // milliseconds. The file generation is still exactly the one
        // captured before this matching active prompt started, so the newest
        // visible turn was never persisted. Do not run ordinal selection: an
        // older turn may have byte-identical text. Preserve canonical history
        // and scrub only the active turn's auxiliary recovery stores.
        let archive_replacement = encode_transcript_archive(&archive)?;
        let manifest = stage_transcript_deletion(
            &root,
            &session,
            &archive_replacement,
            PendingTranscriptDeletion {
                version: 1,
                session_id: session_id.to_string(),
                turns_from_end,
                expected_prompt: expected_prompt.to_string(),
                scope,
                ask_ids,
            },
        )?;
        apply_staged_transcript_deletion(&root, &manifest)?;
        return Ok((0, scope == TranscriptDeleteScope::Prompt));
    }
    let receipt = match delete_transcript_turn_across_storage(
        &mut session,
        archive.clone(),
        turns_from_end,
        expected_prompt,
        scope,
    ) {
        Ok(receipt) => receipt,
        Err(error) => return Err(error),
    };

    let (removed, deleted_prompt, archive_replacement) = receipt;
    let manifest = stage_transcript_deletion(
        &root,
        &session,
        &archive_replacement,
        PendingTranscriptDeletion {
            version: 1,
            session_id: session_id.to_string(),
            turns_from_end,
            expected_prompt: expected_prompt.to_string(),
            scope,
            ask_ids,
        },
    )?;
    apply_staged_transcript_deletion(&root, &manifest)?;
    Ok((removed, deleted_prompt))
}

const DELETE_ARCHIVE_MAX_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum TranscriptStorage {
    Archive,
    Live,
}

#[derive(Clone)]
struct StoredTranscriptMessage {
    storage: TranscriptStorage,
    message: crate::session::Message,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingTranscriptDeletion {
    version: u32,
    session_id: String,
    turns_from_end: usize,
    expected_prompt: String,
    scope: TranscriptDeleteScope,
    #[serde(default)]
    ask_ids: Vec<String>,
}

fn load_transcript_archive(path: &Path) -> Result<Vec<crate::session::Message>> {
    let Some(bytes) =
        crate::config::private_io::read_private_file_limited(path, DELETE_ARCHIVE_MAX_BYTES)?
    else {
        return Ok(Vec::new());
    };
    let text = std::str::from_utf8(&bytes).context("transcript archive is not UTF-8")?;
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line).context("transcript archive contains an invalid row")
        })
        .collect()
}

fn encode_transcript_archive(messages: &[crate::session::Message]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for message in messages {
        bytes.extend_from_slice(&serde_json::to_vec(message)?);
        bytes.push(b'\n');
    }
    anyhow::ensure!(
        bytes.len() <= DELETE_ARCHIVE_MAX_BYTES,
        "rewritten transcript archive exceeds its safety limit"
    );
    Ok(bytes)
}

fn rebuild_surviving_reference_pins(
    messages: impl IntoIterator<Item = crate::session::Message>,
) -> Vec<crate::session::PinnedRef> {
    let mut pins = Vec::<crate::session::PinnedRef>::new();
    for message in messages {
        let crate::session::Message::ToolResult {
            tool_name,
            input,
            success: true,
            ..
        } = message
        else {
            continue;
        };
        let Some(key) = crate::session::reference_pin_key(&tool_name, &input) else {
            continue;
        };
        pins.retain(|pin| !(pin.tool == tool_name && pin.key == key));
        pins.push(crate::session::PinnedRef {
            tool: tool_name,
            key,
        });
    }
    pins
}

fn delete_transcript_turn_across_storage(
    session: &mut crate::session::Session,
    archive: Vec<crate::session::Message>,
    turns_from_end: usize,
    expected_prompt: &str,
    scope: TranscriptDeleteScope,
) -> Result<(usize, bool, Vec<u8>)> {
    let mut rows = archive
        .into_iter()
        .filter(|message| !crate::runtime::compaction::is_continuation_message(message))
        .map(|message| StoredTranscriptMessage {
            storage: TranscriptStorage::Archive,
            message,
        })
        .chain(
            session
                .messages
                .iter()
                .filter(|message| !crate::runtime::compaction::is_continuation_message(message))
                .cloned()
                .map(|message| StoredTranscriptMessage {
                    storage: TranscriptStorage::Live,
                    message,
                }),
        )
        .collect::<Vec<_>>();
    let authored = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| is_authored_transcript_user(&row.message).then_some(index))
        .collect::<Vec<_>>();
    let selected = authored
        .len()
        .checked_sub(turns_from_end.saturating_add(1))
        .and_then(|ordinal| authored.get(ordinal).copied())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "that prompt is no longer in the canonical transcript; nothing was deleted"
            )
        })?;
    let selected_prompt = match &rows[selected].message {
        crate::session::Message::User { content } => content,
        _ => unreachable!("authored indexes contain only user messages"),
    };
    anyhow::ensure!(
        transcript_prompt_identity(selected_prompt) == transcript_prompt_identity(expected_prompt),
        "the visible prompt no longer matches that canonical turn; nothing was deleted"
    );
    let end = authored
        .iter()
        .copied()
        .find(|index| *index > selected)
        .unwrap_or(rows.len());
    let start = if scope == TranscriptDeleteScope::Prompt {
        selected
    } else {
        selected.saturating_add(1)
    };
    anyhow::ensure!(start < end, "that agent turn contains no persisted rows");
    let removed = end - start;
    rows.drain(start..end);

    let archive_messages = rows
        .iter()
        .filter(|row| row.storage == TranscriptStorage::Archive)
        .map(|row| row.message.clone())
        .collect::<Vec<_>>();
    let live_messages = rows
        .iter()
        .filter(|row| row.storage == TranscriptStorage::Live)
        .map(|row| row.message.clone())
        .collect::<Vec<_>>();
    let mut replacement = Vec::with_capacity(live_messages.len().saturating_add(1));
    if let Some(continuation) =
        crate::runtime::compaction::rebuild_continuation_after_deletion(&archive_messages)
    {
        replacement.push(continuation);
    }
    replacement.extend(live_messages.iter().cloned());
    session.replace_messages(replacement);
    session.pinned_refs = rebuild_surviving_reference_pins(
        archive_messages.iter().chain(live_messages.iter()).cloned(),
    );
    Ok((
        removed,
        scope == TranscriptDeleteScope::Prompt,
        encode_transcript_archive(&archive_messages)?,
    ))
}

fn transcript_delete_paths(root: &Path, session_id: &str) -> (PathBuf, PathBuf, PathBuf) {
    (
        root.join(format!("{session_id}.delete-pending.json")),
        root.join(format!("{session_id}.delete-session.json")),
        root.join(format!("{session_id}.delete-archive.jsonl")),
    )
}

fn stage_transcript_deletion(
    root: &Path,
    session: &crate::session::Session,
    archive: &[u8],
    manifest: PendingTranscriptDeletion,
) -> Result<PendingTranscriptDeletion> {
    crate::session::SessionStore::validate_session_id(&manifest.session_id)?;
    anyhow::ensure!(
        session.id == manifest.session_id,
        "deletion stage session mismatch"
    );
    let (manifest_path, session_stage, archive_stage) =
        transcript_delete_paths(root, &manifest.session_id);
    crate::config::private_io::atomic_write_private(
        &session_stage,
        &serde_json::to_vec_pretty(session)?,
    )?;
    crate::config::private_io::atomic_write_private(&archive_stage, archive)?;
    crate::config::private_io::atomic_write_private(
        &manifest_path,
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(manifest)
}

fn apply_staged_transcript_deletion(
    root: &Path,
    manifest: &PendingTranscriptDeletion,
) -> Result<()> {
    anyhow::ensure!(
        manifest.version == 1,
        "unsupported transcript deletion manifest"
    );
    crate::session::SessionStore::validate_session_id(&manifest.session_id)?;
    let (manifest_path, session_stage, archive_stage) =
        transcript_delete_paths(root, &manifest.session_id);
    let session_bytes =
        crate::config::private_io::read_private_file_limited(&session_stage, 32 * 1024 * 1024)?
            .ok_or_else(|| anyhow::anyhow!("staged transcript replacement is missing"))?;
    let session: crate::session::Session = serde_json::from_slice(&session_bytes)?;
    anyhow::ensure!(
        session.id == manifest.session_id,
        "staged session identity mismatch"
    );
    let archive_bytes = crate::config::private_io::read_private_file_limited(
        &archive_stage,
        DELETE_ARCHIVE_MAX_BYTES,
    )?
    .ok_or_else(|| anyhow::anyhow!("staged archive replacement is missing"))?;

    // Validate and scrub the auxiliary recovery stores first. Each operation
    // is idempotent, and the manifest remains authoritative until the
    // canonical session/archive replacements below are durable. In
    // particular, a stale/malicious ask id must fail ownership validation
    // before any transcript file is changed.
    crate::runtime::asks::purge_records(&manifest.session_id, &manifest.ask_ids)?;
    crate::runtime::company::global()?.purge_job_returns(&manifest.session_id)?;
    super::channel_receipts::purge_session(&manifest.session_id)?;
    crate::runtime::journal::purge_turn(
        &manifest.session_id,
        manifest.turns_from_end,
        &manifest.expected_prompt,
        manifest.scope == TranscriptDeleteScope::Agent,
    )?;

    let mut store = crate::session::SessionStore::new(root.to_path_buf());
    store.upsert(session);
    store.save_one(&manifest.session_id)?;
    let archive_path = root.join(format!("{}.archive.jsonl", manifest.session_id));
    if archive_bytes.is_empty() {
        crate::config::private_io::remove_private_file(&archive_path)?;
    } else {
        crate::config::private_io::atomic_write_private(&archive_path, &archive_bytes)?;
    }

    // The manifest is the recovery authority. Remove it only after every
    // canonical target is durable; leftover stage files are then harmless.
    crate::config::private_io::remove_private_file(&manifest_path)?;
    let _ = crate::config::private_io::remove_private_file(&session_stage);
    let _ = crate::config::private_io::remove_private_file(&archive_stage);
    Ok(())
}

fn recover_pending_transcript_deletions() -> Result<usize> {
    let root = crate::config::phoenix_home().join("sessions");
    if !root.is_dir() {
        return Ok(0);
    }
    let mut recovered = 0usize;
    for entry in std::fs::read_dir(&root)?.take(4_096) {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.ends_with(".delete-pending.json") {
            continue;
        }
        let Some(bytes) = crate::config::private_io::read_private_file_limited(&path, 1024 * 1024)?
        else {
            continue;
        };
        let manifest: PendingTranscriptDeletion = serde_json::from_slice(&bytes)?;
        let expected_name = format!("{}.delete-pending.json", manifest.session_id);
        anyhow::ensure!(
            name == expected_name,
            "transcript deletion manifest identity mismatch"
        );
        apply_staged_transcript_deletion(&root, &manifest)?;
        recovered += 1;
    }
    Ok(recovered)
}

/// True while ANY session's turn is running (the digest job's "machine is
/// quiet" check).
fn any_turn_running(locks: &TurnLocks) -> bool {
    locks
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .values()
        .any(|lock| lock.try_lock().is_err())
}

fn maintenance_ts_path() -> PathBuf {
    crate::config::phoenix_home().join(".maintenance_ts")
}

fn maintenance_attempt_ts_path() -> PathBuf {
    crate::config::phoenix_home().join(".maintenance_attempt_ts")
}

fn last_maintenance_ts() -> anyhow::Result<Option<i64>> {
    let path = maintenance_ts_path();
    let Some(bytes) = crate::config::private_io::read_private_file(&path)? else {
        return Ok(None);
    };
    let raw = std::str::from_utf8(&bytes).context("maintenance marker is not UTF-8")?;
    let timestamp = raw
        .trim()
        .parse::<i64>()
        .context("maintenance marker is not an integer timestamp")?;
    Ok(Some(timestamp))
}

#[cfg(test)]
fn maintenance_elapsed_since_success() -> i64 {
    let now = chrono::Utc::now().timestamp();
    match last_maintenance_ts() {
        Ok(None) => i64::MAX,
        Ok(Some(timestamp)) if timestamp <= now.saturating_add(5 * 60) => {
            now.saturating_sub(timestamp)
        }
        Ok(Some(timestamp)) => {
            glog(&format!(
                "maintenance: success marker is implausibly in the future ({timestamp}); treating it as stale"
            ));
            i64::MAX
        }
        Err(error) => {
            glog(&format!(
                "maintenance: could not read the success marker ({error:#}); treating it as stale"
            ));
            i64::MAX
        }
    }
}

fn maintenance_elapsed_since_attempt() -> i64 {
    let now = chrono::Utc::now().timestamp();
    let attempt = read_maintenance_timestamp(&maintenance_attempt_ts_path());
    let success = last_maintenance_ts();
    [attempt, success]
        .into_iter()
        .filter_map(Result::ok)
        .flatten()
        .filter(|timestamp| *timestamp <= now.saturating_add(5 * 60))
        .max()
        .map_or(i64::MAX, |timestamp| now.saturating_sub(timestamp))
}

fn read_maintenance_timestamp(path: &std::path::Path) -> anyhow::Result<Option<i64>> {
    let Some(bytes) = crate::config::private_io::read_private_file(path)? else {
        return Ok(None);
    };
    let raw = std::str::from_utf8(&bytes).context("maintenance marker is not UTF-8")?;
    Ok(Some(raw.trim().parse::<i64>().context(
        "maintenance marker is not an integer timestamp",
    )?))
}

fn record_maintenance_attempt() -> anyhow::Result<()> {
    crate::config::private_io::atomic_write_private(
        &maintenance_attempt_ts_path(),
        chrono::Utc::now().timestamp().to_string().as_bytes(),
    )
}

fn record_maintenance_success() -> anyhow::Result<()> {
    crate::config::private_io::atomic_write_private(
        &maintenance_ts_path(),
        chrono::Utc::now().timestamp().to_string().as_bytes(),
    )
}

/// The 12-hourly memory-maintenance job — Cognee memify (deterministic triplet
/// embeddings over the whole graph; no model gardening loop). Serializes
/// against ITSELF (restart storms must not stack passes); live turns keep
/// running — the memory layer's own locks (ladybug) arbitrate store access.
fn run_librarian_maintenance_job(turn_locks: TurnLocks) {
    static MAINTENANCE_LOCK: Mutex<()> = Mutex::const_new(());
    tokio::spawn(async move {
        let Ok(_pass) = MAINTENANCE_LOCK.try_lock() else {
            glog("maintenance: a pass is already running — skipping this fire");
            return;
        };
        if any_turn_running(&turn_locks) {
            glog("memory: maintenance skipped because a user or routine turn is active");
            return;
        }
        if let Err(error) = record_maintenance_attempt() {
            glog(&format!(
                "maintenance: could not record the attempt marker ({error:#}); refusing an unbounded retry loop"
            ));
            return;
        }
        let started = std::time::Instant::now();
        match tokio::task::spawn_blocking(|| {
            crate::runtime::company::global()?.purge_directory_items_due()
        })
        .await
        {
            Ok(Ok(purged)) => {
                for (kind, id) in purged {
                    glog(&format!(
                        "company: permanently purged expired {kind} `{id}` and owner-private state"
                    ));
                }
            }
            Ok(Err(error)) => glog(&format!(
                "company: expired private-state purge FAILED and will retry ({error:#})"
            )),
            Err(error) => glog(&format!(
                "company: expired private-state purge task failed ({error})"
            )),
        }
        glog("memory: 12h memify maintenance starting");
        match crate::runtime::memory_hooks::run_maintenance(None).await {
            Ok(receipts) => {
                for receipt in &receipts {
                    glog(&format!("  {receipt}"));
                }
                match record_maintenance_success() {
                    Ok(()) => glog(&format!(
                        "memory: memify maintenance done in {:.1}s",
                        started.elapsed().as_secs_f32(),
                    )),
                    Err(error) => glog(&format!(
                        "memory: memify completed but the durable success marker FAILED ({error:#}); the scheduler will retry"
                    )),
                }
            }
            Err(error) => glog(&format!("memory: memify maintenance FAILED: {error:#}")),
        }
        // Skill-card reconcile: card any skill that arrived outside
        // skill_install (manual copy, agent-written) and stale-mark removals,
        // so memory_recall keeps surfacing the real skill library.
        let workspace = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let receipt = crate::tools::skills::sync_skill_cards(&workspace).await;
        glog(&receipt);
    });
}

/// The 5-minute session-digest job — one project-state note per idle session
/// with new activity (see `runtime::session_digest`). `try_lock`, not `lock`:
/// a live turn means sessions are active, so this tick has nothing to do, and
/// a queued digest would grab the lock the moment the turn ends and delay the
/// user's next one.
fn run_session_digest_job(turn_locks: TurnLocks) {
    tokio::spawn(async move {
        // Only when the machine is quiet — digesting mid-turn would snapshot
        // a session that is actively changing.
        if any_turn_running(&turn_locks) {
            return;
        }
        let stored_any = match crate::runtime::session_digest::digest_idle_sessions_report().await {
            Ok(outcome) => {
                for receipt in &outcome.receipts {
                    glog(&format!("memory: {receipt}"));
                }
                outcome.stored_any
            }
            Err(error) => {
                glog(&format!("memory: session digest pass FAILED: {error:#}"));
                false
            }
        };
        // cognify runs reasoning-model calls and can take minutes;
        // cognify_backlog serializes against itself internally.
        if stored_any {
            match crate::librarian::memory::cognify_backlog().await {
                Ok(receipt) => glog(&format!("memory: digest index — {receipt}")),
                Err(error) => notice_cognify_failure("digest", &error),
            }
        }
    });
}

/// Check the cron store and run every due entry as a normal turn: the prompt
/// lands in the target session as a user message, so the agent wakes up to it.
/// Entries are advanced BEFORE the turn runs — a slow turn can't double-fire.
fn scheduled_agent_target(session_id: &str) -> Option<String> {
    let from_directory = crate::runtime::company::global_if_initialized()
        .and_then(|company| company.directory_snapshot().ok())
        .and_then(|snapshot| {
            snapshot.agents.into_iter().find_map(|agent| {
                (agent.profile.canonical_session_id.as_deref() == Some(session_id)
                    && agent.profile.internal_role != "phoenix")
                    .then_some(agent.profile.agent_id)
            })
        });
    from_directory.or_else(|| {
        session_id
            .strip_prefix("agent-")
            .filter(|agent| !agent.is_empty() && *agent != "phoenix")
            .map(str::to_string)
    })
}

fn scheduled_turn_id(entry: &crate::cron::CronEntry) -> String {
    // `claim_due` returns the clone from BEFORE it advances `next_run`, so this
    // identifies the scheduled occurrence rather than the mutable definition.
    // Retrying that claimed occurrence is idempotent; its next daily run is not.
    format!("routine:{}:{}", entry.id, entry.next_run.timestamp_millis())
}

fn settle_scheduled_turn(session_id: &str, saw_done: bool, failure: Option<String>) {
    let event = match failure {
        Some(message) => CliEvent::TerminalFailure { message },
        None if !saw_done => CliEvent::Done,
        None => return,
    };
    crate::notifications::publish_event(session_id, &event);
    crate::runtime::postbox::forward(session_id, event);
}

fn fire_due_crons(turn_locks: TurnLocks) {
    let now = chrono::Utc::now();
    let entries = match crate::cron::claim_due(now) {
        Ok(entries) => entries,
        Err(error) => {
            glog(&format!(
                "cron poll: failed to atomically claim due schedules: {error:#}"
            ));
            return;
        }
    };
    for entry in entries {
        let lock = turn_lock_for(&turn_locks, &entry.session_id);
        tokio::spawn(async move {
            let _turn = lock.lock().await;
            let started = std::time::Instant::now();
            let target_agent = scheduled_agent_target(&entry.session_id);
            let schedule = entry.describe_schedule();
            let scheduled_for = entry
                .next_run
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            let turn_id = scheduled_turn_id(&entry);
            let origin = crate::runtime::TurnOrigin::Routine {
                routine_id: entry.id.clone(),
                scheduled_for: scheduled_for.clone(),
                schedule: schedule.clone(),
            };
            let preview: String = entry.prompt.chars().take(80).collect();
            glog(&format!(
                "cron {} fired [{} -> {}] ({}) {preview}",
                entry.id,
                entry.session_id,
                target_agent.as_deref().unwrap_or("phoenix"),
                schedule
            ));
            let request = format!(
                "[cron {} | scheduled {} | occurrence {} | turn {}] {}",
                entry.id, schedule, scheduled_for, turn_id, entry.prompt
            );
            // A cron is a first-class turn in its OWNER'S endless conversation.
            // Announce it there before any model/tool event so an attached (or
            // later reopened) Canvas sees the schedule arrive naturally.
            // A routine starts a new replay window just like a foreground
            // composer turn. Without this, a reconnect received the previous
            // turn before the routine boundary and could attach new work to it.
            crate::runtime::postbox::begin_foreground_replay(&entry.session_id);
            crate::runtime::postbox::forward(
                &entry.session_id,
                CliEvent::WakeTurn {
                    // Match the canonical Session user message byte-for-byte.
                    // The Canvas presents this internal envelope as one compact
                    // “Scheduled work” row, and matching text lets its semantic
                    // deduper merge the live story with the later snapshot.
                    prompt: request.clone(),
                    turn_id: Some(turn_id.clone()),
                    origin: Some(origin),
                },
            );
            // Replay the target session's anchor (workspace + yolo from its
            // last client turn); daemon cwd + Workspace only for sessions
            // that never had one.
            let (wake_workspace, wake_mode) =
                crate::runtime::turn_anchor::wake_context(&entry.session_id);
            let (handle, mut event_rx) = super::spawn_event_loop(
                false,
                false,
                entry.session_id.clone(),
                request,
                wake_mode,
                crate::runtime::InteractionMode::Execute,
                false,
                wake_workspace,
                target_agent.clone(),
                None,
                None,
                None,
                None,
                None, // no attachments on an internal wake
                Some(turn_id),
            );
            let running_id = running_turns()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(
                    entry.session_id.clone(),
                    RunningTurn {
                        handle: handle.abort_handle(),
                        target_agent: target_agent
                            .as_deref()
                            .map(crate::runtime::postbox::base_agent)
                            .map(str::to_string),
                        target_group: None,
                        authored_prompt: None,
                        session_file_before: None,
                    },
                );
            let mut saw_done = false;
            let mut liveness = tokio::time::interval(std::time::Duration::from_millis(250));
            loop {
                let event = tokio::select! {
                    received = event_rx.recv() => match received {
                        Some(event) => event,
                        None => break,
                    },
                    _ = liveness.tick() => {
                        if !handle.is_finished() { continue; }
                        // The producer can finish with committed replies or
                        // cleanup events still buffered. Match the foreground
                        // consumer: drain them before settling this wake.
                        match drain_finished_turn_event(&mut event_rx) {
                            Some(event) => event,
                            None => break,
                        }
                    }
                };
                if let Some(brief) = event_brief(&event) {
                    glog(&format!("  {brief}"));
                }
                crate::notifications::publish_event(&entry.session_id, &event);
                // Unlike the old cron lane, this is the same durable event
                // stream a normal foreground turn uses. Tool calls, visible
                // commentary, questions and the final answer therefore remain
                // live and replayable in the owner's conversation.
                crate::runtime::postbox::forward(&entry.session_id, event.clone());
                if let CliEvent::FinalOutput(ref output) = event {
                    broadcast_session_answer(&entry.session_id, output);
                }
                if matches!(event, CliEvent::Done) {
                    saw_done = true;
                    break;
                }
            }
            let secs = started.elapsed().as_secs_f32();
            let result = handle.await;
            running_turns()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&entry.session_id, running_id);
            let failure = match result {
                Ok(Ok(_)) => {
                    glog(&format!(
                        "cron {} done [{}] in {secs:.1}s",
                        entry.id, entry.session_id
                    ));
                    None
                }
                Ok(Err(error)) => {
                    glog(&format!(
                        "cron {} FAILED [{}] in {secs:.1}s: {error:#}",
                        entry.id, entry.session_id
                    ));
                    Some(format!("Scheduled work failed: {}", error.root_cause()))
                }
                Err(join_error) => {
                    glog(&format!("cron {} internal error: {join_error:#}", entry.id));
                    Some(format!("Scheduled work was interrupted: {join_error}"))
                }
            };
            // A preflight failure (for example a dormant coworker) can end
            // before producing any events. Keep the cause in the owner's
            // journal and reconnect replay instead of silently emitting Done.
            settle_scheduled_turn(&entry.session_id, saw_done, failure);
        });
    }
}

/// Drain a user-authored steer that arrived at the edge of another live turn.
/// Detached coworker completion does not call this path: its return remains
/// durable for the next natural user turn and cannot create an unsolicited
/// second final answer.
fn fire_background_wake(session_id: String, turn_locks: TurnLocks) {
    fire_session_wake(
        session_id,
        turn_locks,
        "[user steer] A user-authored message arrived at the edge of the previous turn. Read the injected steer, act on it now, and produce one coherent answer."
            .to_string(),
        "steer wake",
        false,
        None,
    );
}

fn fire_terminal_wake(session_id: String, turn_locks: TurnLocks) {
    fire_session_wake(
        session_id, turn_locks,
        "[terminal completion] Your managed background terminal job has actually exited. Read the durable terminal result injected into this same conversation, continue the user's task from that evidence, and report the outcome. A start receipt or interim text is not completion. Do not rerun an already completed command or create another coworker."
            .to_string(),
        "terminal completion", true, None,
    );
}

/// The + Agent popup commits a tiny dormant record synchronously, then this
/// hidden Phoenix turn forms the identity. It uses a private system session so
/// setup instructions never appear among the user's canonical prompt rail.
/// Two bounded attempts cover transient provider/tool-call failures; durable
/// state makes daemon restart resume the same request instead of duplicating it.
fn fire_agent_provisioning_wake(role: String, turn_locks: TurnLocks) {
    tokio::spawn(async move {
        let session_id = format!("system-provision-{role}");
        let lock = turn_lock_for(&turn_locks, &session_id);
        let _turn = lock.lock().await;
        let _activity = crate::runtime::company_activity::begin(
            &format!("agent-{role}"),
            &role,
            "Phoenix is setting up this coworker",
            "phoenix",
            "configured",
        );
        crate::runtime::postbox::forward(
            &format!("agent-{role}"),
            CliEvent::GatewayNotice(
                "Setting up · Phoenix is forming this coworker’s role".to_string(),
            ),
        );
        for attempt in 1..=2 {
            crate::sub_agents::registry::refresh();
            if crate::sub_agents::registry::resolve_custom_talk_name(&role).is_some() {
                return;
            }
            let role_for_brief = role.clone();
            let brief = match tokio::task::spawn_blocking(move || {
                crate::tools::agent_forge::phoenix_provisioning_brief(&role_for_brief)
            })
            .await
            {
                Ok(Ok(brief)) => brief,
                Ok(Err(error)) => {
                    glog(&format!(
                        "agent provisioning `{role}` could not compile context: {error:#}"
                    ));
                    let _ = crate::tools::agent_forge::mark_agent_provisioning_failure(
                        &role,
                        &format!("context compilation failed: {error:#}"),
                    );
                    return;
                }
                Err(error) => {
                    glog(&format!(
                        "agent provisioning `{role}` context worker failed: {error}"
                    ));
                    return;
                }
            };
            glog(&format!(
                "agent provisioning `{role}`: Phoenix refinement attempt {attempt}/2"
            ));
            let (handle, mut events) = super::spawn_event_loop(
                false,
                false,
                session_id.clone(),
                brief,
                crate::tools::PermissionMode::FullAccess,
                crate::runtime::InteractionMode::Execute,
                false,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            );
            let mut provision_call = None;
            while let Some(event) = events.recv().await {
                if let CliEvent::ToolCallCompleted {
                    tool_name,
                    success,
                    output_summary,
                    ..
                } = event
                {
                    if tool_name == "agent_provision" {
                        provision_call = Some((success, output_summary));
                    }
                }
            }
            let turn_result = handle.await;
            crate::sub_agents::registry::refresh();
            if crate::sub_agents::registry::resolve_custom_talk_name(&role).is_some() {
                glog(&format!("agent provisioning `{role}`: ready"));
                crate::runtime::postbox::forward(
                    &format!("agent-{role}"),
                    CliEvent::GatewayNotice("Ready · coworker setup complete".to_string()),
                );
                if let Ok(message) = crate::tools::agent_forge::initial_agent_message(&role) {
                    crate::runtime::postbox::forward(
                        &format!("agent-{role}"),
                        CliEvent::FinalOutput(message),
                    );
                }
                return;
            }
            let detail = match (provision_call, turn_result) {
                (Some((false, output)), _) => format!("agent_provision failed: {output}"),
                (_, Ok(Err(error))) => format!("Phoenix refinement failed: {error:#}"),
                (_, Err(error)) => format!("Phoenix refinement stopped internally: {error}"),
                _ => "Phoenix returned without publishing the coworker".to_string(),
            };
            glog(&format!(
                "agent provisioning `{role}` attempt {attempt}/2 incomplete: {detail}"
            ));
            if attempt == 2 {
                let _ = crate::tools::agent_forge::mark_agent_provisioning_failure(&role, &detail);
                crate::runtime::postbox::forward(
                    &format!("agent-{role}"),
                    CliEvent::GatewayNotice(
                        "Coworker setup needs attention; retry is available from the agent menu"
                            .to_string(),
                    ),
                );
            }
        }
    });
}

/// Cross-session answer notification under the conversation's real owner.
fn broadcast_session_answer(session_id: &str, output: &str) {
    use crate::runtime::agent_conversation::CanonicalConversationOwner;

    let identity = crate::runtime::company::global()
        .and_then(|company| company.directory_snapshot())
        .ok()
        .and_then(|snapshot| {
            crate::runtime::agent_conversation::resolve_canonical_owner(&snapshot, session_id)
                .ok()
                .flatten()
                .map(|owner| match owner {
                    CanonicalConversationOwner::Agent(agent) => {
                        ("agent", agent.agent_id, agent.display_name)
                    }
                    CanonicalConversationOwner::Group { group_id } => {
                        let display_name = snapshot
                            .groups
                            .iter()
                            .find(|group| group.profile.group_id == group_id)
                            .map(|group| group.profile.name.clone())
                            .unwrap_or_else(|| group_id.clone());
                        ("group", group_id, display_name)
                    }
                })
        });
    let fallback = crate::notifications::session_display_name(session_id);
    let (owner_kind, owner_id, owner) = identity
        .as_ref()
        .map(|(kind, id, name)| (Some(*kind), Some(id.as_str()), name.as_str()))
        .unwrap_or((None, None, fallback.as_str()));
    let summary = output.chars().take(200).collect::<String>();
    crate::runtime::postbox::broadcast_answer(session_id, owner_kind, owner_id, owner, &summary);
}

// ── Group rooms while working: no queue ─────────────────────────────────
//
// "Everyone hears everything, only some act." A user message to a room that
// is working is written to the canonical room transcript at once (exactly one
// entry, keyed by its client turn id), steered into every member that is
// running in the room (actionable for the members who must act, FYI for the
// rest), and an idle member that must act gets its own turn started right
// away, concurrently with the running work. Idle bystanders are not woken.

/// What a room message did, for the sender's acknowledgement and tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct RoomDeliveryReport {
    turn_id: String,
    steered: Vec<(String, crate::runtime::postbox::RoomSteerKind)>,
    started: Vec<String>,
    duplicates: Vec<String>,
}

impl RoomDeliveryReport {
    fn summary(&self, context: &crate::runtime::group_conversation::GroupTurnContext) -> String {
        let name = |id: &str| {
            context
                .participants
                .iter()
                .find(|p| p.agent_id == id)
                .map(|p| p.display_name.clone())
                .unwrap_or_else(|| id.to_string())
        };
        let mut parts = Vec::new();
        let acting = self
            .steered
            .iter()
            .filter(|(_, kind)| kind.is_actionable())
            .map(|(id, _)| name(id))
            .collect::<Vec<_>>();
        if !acting.is_empty() {
            parts.push(format!("delivered mid-task to {}", acting.join(", ")));
        }
        if !self.started.is_empty() {
            parts.push(format!(
                "{} starting now",
                self.started.iter().map(|id| name(id)).collect::<Vec<_>>().join(", ")
            ));
        }
        let heard = self
            .steered
            .iter()
            .filter(|(_, kind)| !kind.is_actionable())
            .map(|(id, _)| name(id))
            .collect::<Vec<_>>();
        if !heard.is_empty() {
            parts.push(format!("{} heard it", heard.join(", ")));
        }
        if parts.is_empty() {
            "posted in the room".to_string()
        } else {
            format!("posted in the room · {}", parts.join(" · "))
        }
    }
}

/// Route one room message while the room is working. Synchronous and
/// side-effect ordered: (1) the canonical transcript entry, (2) per-lane
/// steers, each atomic with its liveness check, (3) the set of idle actors to
/// start. A lane that finished between the room check and its steer is
/// reported as an actor to start, never silently skipped.
fn route_room_message(
    session_id: &str,
    group_id: &str,
    turn_id: &str,
    user_request: &str,
    request_for_task: &str,
    intent: &crate::runtime::group_conversation::GroupActivationIntent,
) -> Result<(
    RoomDeliveryReport,
    crate::runtime::group_conversation::GroupTurnContext,
)> {
    use crate::runtime::group_conversation::{self as room, RoomDelivery};
    use crate::runtime::postbox::RoomSteerOutcome;

    let company = crate::runtime::company::global()?;
    let snapshot = company.directory_snapshot()?;
    let context = room::resolve_group_turn(&snapshot, group_id, user_request)?;
    anyhow::ensure!(
        context.canonical_session_id == session_id,
        "group `{group_id}` uses canonical session `{}` (received `{session_id}`)",
        context.canonical_session_id
    );
    // (1) The room transcript gets the message now, exactly once. A retry or
    // a race-fallback turn reuses this entry (keyed by the client turn id).
    let model = crate::session::SessionStore::read_one_from_disk(
        &crate::config::paths::phoenix_sessions_root(),
        session_id,
    )
    .ok()
    .flatten()
    .map(|session| session.model)
    .unwrap_or_else(|| "phoenix-scaffold-model".to_string());
    crate::runtime::runner::persist_group_user_boundary(
        &crate::config::phoenix_home(),
        &context,
        turn_id,
        request_for_task,
        &model,
    )?;
    // (2)+(3) Everyone running hears it; the actors act.
    let everyone = room::addresses_everyone(user_request);
    let plan = room::plan_room_delivery(&context, &intent.active_agent_ids, everyone, |lane| {
        crate::runtime::postbox::agent_turn_active(session_id, lane)
            || crate::runtime::postbox::agent_turn_starting(session_id, lane)
    });
    let mut report = RoomDeliveryReport {
        turn_id: turn_id.to_string(),
        ..RoomDeliveryReport::default()
    };
    for member in plan {
        match member.delivery {
            RoomDelivery::Start => report.started.push(member.agent_id),
            RoomDelivery::Steer(kind) => match crate::runtime::postbox::steer_room_user(
                session_id,
                &member.lane,
                turn_id,
                kind,
                request_for_task,
            ) {
                RoomSteerOutcome::Delivered => report.steered.push((member.agent_id, kind)),
                RoomSteerOutcome::Duplicate => report.duplicates.push(member.agent_id),
                // Finished between the check and the steer: an actor gets a
                // normal turn; a bystander reads the transcript next time.
                RoomSteerOutcome::NotRunning if kind.is_actionable() => {
                    report.started.push(member.agent_id)
                }
                RoomSteerOutcome::NotRunning => {}
            },
        }
    }
    // Keep roster order for the started subset.
    report.started = intent
        .active_agent_ids
        .iter()
        .filter(|id| report.started.contains(id))
        .cloned()
        .collect();
    Ok((report, context))
}

/// Start a room turn for `intent`'s members right now, beside whatever the
/// room is already running (an independent execution permit, not the ordered
/// session lock). Events reach every open view through the session postbox.
#[allow(clippy::too_many_arguments)]
fn spawn_room_turn(
    session_id: String,
    group_id: String,
    turn_id: String,
    request: String,
    intent: crate::runtime::group_conversation::GroupActivationIntent,
    workspace: Option<PathBuf>,
    permission_mode: Option<crate::tools::PermissionMode>,
    attachments: Option<Vec<String>>,
    finalize_receipt: bool,
    turn_locks: TurnLocks,
) {
    if intent.active_agent_ids.is_empty() {
        return;
    }
    tokio::spawn(async move {
        let lock = turn_lock_for(&turn_locks, &session_id);
        let _permit = lock.continuation().await;
        let (anchor_workspace, anchor_mode) = crate::runtime::turn_anchor::wake_context(&session_id);
        let workspace = workspace
            .or(anchor_workspace)
            .or_else(|| Some(crate::config::phoenix_workspace_root()));
        let permission_mode = permission_mode.unwrap_or(anchor_mode);
        let scope = crate::runtime::postbox::ExecutionScope::new(turn_id.clone(), turn_id.clone());
        crate::runtime::postbox::forward_owned(
            &session_id,
            Some(&scope),
            CliEvent::WakeTurn {
                prompt: request.clone(),
                turn_id: Some(turn_id.clone()),
                origin: None,
            },
        );
        let session_file_before = session_file_version(&session_id);
        let (handle, mut event_rx) = super::spawn_event_loop(
            false,
            false,
            session_id.clone(),
            request.clone(),
            permission_mode,
            crate::runtime::InteractionMode::Execute,
            false,
            workspace,
            None,
            Some(group_id.clone()),
            Some(intent),
            None,
            None,
            attachments,
            Some(turn_id.clone()),
        );
        let running_id = running_turns()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(
                session_id.clone(),
                RunningTurn {
                    handle: handle.abort_handle(),
                    target_agent: None,
                    target_group: Some(group_id.clone()),
                    authored_prompt: Some(request),
                    session_file_before,
                },
            );
        let mut liveness = tokio::time::interval(Duration::from_millis(250));
        while let Some(event) =
            super::turn_events::next_event(&mut event_rx, &handle, &mut liveness).await
        {
            crate::notifications::publish_event(&session_id, &event);
            if let CliEvent::FinalOutput(ref output) = event {
                broadcast_session_answer(&session_id, output);
            }
            let done = matches!(event, CliEvent::Done);
            crate::runtime::postbox::forward_owned(&session_id, Some(&scope), event);
            if done {
                break;
            }
        }
        let result = handle.await;
        if finalize_receipt {
            let succeeded = matches!(&result, Ok(Ok(report))
                if report.execution.outcome.completion == crate::runtime::OutcomeCompletion::Completed);
            if let Err(error) =
                super::turn_queue::finalize_immediate_group(&session_id, &turn_id, succeeded)
            {
                glog(&format!(
                    "room turn [{session_id}] {turn_id}: could not finalize idempotency receipt: {error:#}"
                ));
            }
        }
        running_turns()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&session_id, running_id);
        match result {
            Ok(Ok(_)) => glog(&format!("room turn done [{session_id}] {turn_id}")),
            Ok(Err(error)) => {
                glog(&format!("room turn FAILED [{session_id}] {turn_id}: {error:#}"));
                crate::runtime::postbox::forward(
                    &session_id,
                    CliEvent::GatewayNotice(format!("room turn failed: {error:#}")),
                );
            }
            Err(error) => glog(&format!("room turn stopped [{session_id}] {turn_id}: {error:#}")),
        }
    });
}

/// Race safety for one steered room delivery. The member's round-top drain
/// and this watcher race for the parked note under one postbox lock, so it
/// is injected exactly once. If the member's turn ended first, an actionable
/// message becomes a normal turn for that member; an FYI is simply retired
/// (the member reads the canonical transcript on its next turn).
#[allow(clippy::too_many_arguments)]
fn watch_room_steer(
    session_id: String,
    group_id: String,
    agent_id: String,
    lane: String,
    client_turn_id: String,
    intent: crate::runtime::group_conversation::GroupActivationIntent,
    workspace: Option<PathBuf>,
    permission_mode: Option<crate::tools::PermissionMode>,
    turn_locks: TurnLocks,
) {
    tokio::spawn(async move {
        let delivery_id = crate::runtime::postbox::room_delivery_id(&client_turn_id, &lane);
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        loop {
            tick.tick().await;
            match crate::runtime::postbox::settle_parked_room_steer(&session_id, &lane, &delivery_id) {
                crate::runtime::postbox::ParkedRoomSteer::Consumed => return,
                crate::runtime::postbox::ParkedRoomSteer::Live => continue,
                crate::runtime::postbox::ParkedRoomSteer::Orphaned(note) => {
                    let actionable = crate::runtime::postbox::room_steer_parts(&note.subject)
                        .is_some_and(|(kind, _)| kind.is_actionable());
                    if actionable {
                        glog(&format!(
                            "room [{session_id}] `{lane}` finished before reading the message; starting its turn"
                        ));
                        spawn_room_turn(
                            session_id.clone(),
                            group_id.clone(),
                            crate::runtime::group_conversation::room_fallback_turn_id(&client_turn_id, &lane),
                            note.body,
                            crate::runtime::group_conversation::narrow_activation(&intent, &[agent_id.clone()]),
                            workspace.clone(),
                            permission_mode,
                            None,
                            false,
                            turn_locks.clone(),
                        );
                    }
                    return;
                }
            }
        }
    });
}

/// Drain normal composer sends in FIFO order after the current turn releases
/// the canonical conversation lock. Claims are durable; a gateway restart
/// returns interrupted claims to queued state before launching these workers.
fn enqueue_ready_group_continuations(only_session: Option<&str>) -> Result<()> {
    let company = crate::runtime::company::global()?;
    enqueue_ready_group_continuations_from(&company, only_session)
}

fn enqueue_ready_group_continuations_from(company: &crate::runtime::company::CompanyStore, only_session: Option<&str>) -> Result<()> {
    let (ready_items, mut failures) = company.scan_group_continuations(only_session)?;
    for ready in ready_items {
        let identity = ready.turn_id.clone();
        if let Err(error) = enqueue_group_continuation(company, ready) {
            failures.push(format!("{identity}: {error:#}"));
        }
    }
    anyhow::ensure!(failures.is_empty(), "{} group continuation(s) remain pending: {}", failures.len(), failures.join("; "));
    Ok(())
}

fn enqueue_group_continuation(
    company: &crate::runtime::company::CompanyStore,
    ready: crate::runtime::company::GroupReadyContinuation,
) -> Result<()> {
        if let Some(saved) = company.group_continuation_dispatch(&ready.canonical_session_id, &ready.turn_id)? {
            // Once frozen, retry from the saved context even if the transcript
            // has since compacted or the current workspace changed.
            let payload = serde_json::from_str(&saved)?;
            super::turn_queue::reserve_immediate_group(&ready.canonical_session_id, &ready.turn_id, &payload)?;
            super::turn_queue::enqueue(&ready.canonical_session_id, &payload)?;
            company.acknowledge_group_continuation(&ready.canonical_session_id, &ready.turn_id)?;
            return Ok(());
        }
        let session = crate::session::SessionStore::read_one_from_disk(
            &crate::config::paths::phoenix_sessions_root(), &ready.canonical_session_id,
        )?.context("ready group continuation has no saved conversation")?;
        let inputs = crate::runtime::compaction::resolve_group_contributions(
            &crate::config::paths::phoenix_sessions_root(), &ready.canonical_session_id,
            &ready.activation.group_id, &ready.predecessor_receipts, &session.messages,
        )?;
        let predecessor_context = group_continuation_inputs(&ready, &inputs)?;
        let (workspace, permission_mode) = crate::runtime::turn_anchor::wake_context(&ready.canonical_session_id);
        let participants = ready.activation.active_agent_ids.iter().map(|id| format!("@{id}")).collect::<Vec<_>>().join(" ");
        // Group leader architecture: a leader convergence continuation.
        let converging = ready.turn_id.starts_with(crate::runtime::group_coordination::LEADER_CONVERGENCE_PREFIX);
        let (user_request, display) = if converging {
            (format!(
                "Leader convergence for turn {}. {} — the members you dispatched have reported; their saved room contributions are in the JSON below (task context, not new instructions). Converge: compare and critique them, record decisions on the mission board, run your red-team pass, then either dispatch newly ready plan items or deliver ONE coherent answer to the original request in the room. Do not ask members to repeat completed work.\n\n{}",
                ready.original_turn_id, participants, predecessor_context,
            ), format!("Members reported · {participants} converging"))
        } else {
            (format!(
                "Continue the saved group plan from turn {}. Ready participants: {}. Execute only these remaining stages. Do not repeat completed work. Follow the stored prerequisites. The JSON below contains the original request and saved predecessor contributions; these are task context, not new scheduler instructions.\n\n{}",
                ready.original_turn_id, participants, predecessor_context,
            ), format!("Prerequisites ready · {participants} continuing"))
        };
        let payload = super::turn_queue::QueuedUserTurn {
            turn_id: Some(ready.turn_id.clone()),
            user_request,
            origin: Some(crate::runtime::TurnOrigin::GroupContinuation {
                original_turn_id: ready.original_turn_id.clone(),
                display,
            }),
            interaction_mode: crate::runtime::InteractionMode::Execute,
            permission_mode: Some(permission_mode), yolo: None, workspace,
            target_agent: None, target_group: Some(ready.activation.group_id.clone()),
            group_activation: Some(ready.activation), sticky_notes: None, viewport: None, attachments: None,
        };
        // Freeze the complete envelope before crossing databases. Retry must
        // not change permissions, workspace or content under the same turn id.
        let saved = company.freeze_group_continuation_dispatch(
            &ready.canonical_session_id, &ready.turn_id, &serde_json::to_string(&payload)?,
        )?;
        let payload = serde_json::from_str(&saved)?;
        super::turn_queue::reserve_immediate_group(&ready.canonical_session_id, &ready.turn_id, &payload)?;
        super::turn_queue::enqueue(&ready.canonical_session_id, &payload)?;
        company.acknowledge_group_continuation(&ready.canonical_session_id, &ready.turn_id)?;
    Ok(())
}

fn group_continuation_inputs(
    ready: &crate::runtime::company::GroupReadyContinuation,
    messages: &[crate::session::Message],
) -> Result<String> {
    let mut contributions = Vec::new();
    for receipt in &ready.predecessor_receipts {
        let found = messages.iter().find_map(|message| match message {
            crate::session::Message::GroupContribution { message_id, group_id, agent_id, subject, body, .. }
                if message_id == receipt && group_id == &ready.activation.group_id => {
                Some(serde_json::json!({"receipt":receipt,"agent_id":agent_id,"subject":subject,"body":body}))
            }
            _ => None,
        }).with_context(|| format!("required group contribution {receipt} is missing from the saved conversation; continuation remains queued for recovery"))?;
        contributions.push(found);
    }
    Ok(serde_json::to_string(&serde_json::json!({
        "original_turn_id":ready.original_turn_id,
        "original_request":ready.original_request,
        "predecessor_contributions":contributions,
    }))?)
}

fn fire_queued_turn_wake(session_id: String, turn_locks: TurnLocks) {
    // Internal, durably addressed group continuations may progress beside
    // unrelated work. Ordinary authored prompts retain their FIFO admission.
    fire_queued_turn_lane(session_id.clone(), turn_locks.clone(), true);
    fire_queued_turn_lane(session_id, turn_locks, false);
}

fn queued_channel_completion(
    session_id: &str,
    queued: &super::turn_queue::QueuedUserTurn,
    workspace: Option<&Path>,
) -> Option<super::channel_receipts::Key> {
    if queued.target_group.is_some() {
        return None;
    }
    super::channel_receipts::Key::new(
        session_id, queued.turn_id.as_deref(), queued.target_agent.as_deref(), workspace,
        &queued.user_request,
    )
}

fn fail_queued_for_review(queue_id: &str, reason: String) -> anyhow::Error {
    match super::turn_queue::fail(queue_id, &reason) {
        Ok(()) => anyhow::anyhow!(reason),
        Err(error) => anyhow::anyhow!(
            "{reason} The review state could not be saved; the queue lane stopped: {error:#}"
        ),
    }
}

/// Called under the session execution permit. The closure owns both WakeTurn
/// publication and runner creation, so recovery cannot announce or repeat work.
fn start_queued_turn<T>(
    queue_id: &str,
    attempts: i64,
    key: Option<&super::channel_receipts::Key>,
    start: impl FnOnce() -> T,
) -> Result<Option<T>> {
    if let Some(key) = key {
        let reason = match super::channel_receipts::terminal_exists(key) {
            Ok(true) => {
                // Release the receipt lock before the queue transaction. Keep
                // the saved final and the immutable submission receipt intact.
                super::turn_queue::complete(queue_id)
                    .context("Saved terminal reply could not retire its queued turn; the queue lane stopped")?;
                return Ok(None);
            }
            Ok(false) if attempts == 1 => return Ok(Some(start())),
            Ok(false) => "Interrupted queued work has no retained terminal receipt. Review the saved conversation in Phoenix before continuing with a new turn; it was not automatically rerun.".to_string(),
            Err(error) => format!("Queued work could not verify its terminal receipt. Review it in Phoenix; no new execution was started: {error:#}"),
        };
        return Err(fail_queued_for_review(queue_id, reason));
    }
    Ok(Some(start()))
}

async fn finish_queued_turn(
    queue_id: &str,
    key: Option<&super::channel_receipts::Key>,
    completion: crate::runtime::OutcomeCompletion,
    markdown: &str,
) -> Result<()> {
    if let Some(key) = key.filter(|_| matches!(completion,
        crate::runtime::OutcomeCompletion::Completed | crate::runtime::OutcomeCompletion::Incomplete)) {
        if let Err(error) = super::channel_receipts::record(key, markdown).await {
            return Err(fail_queued_for_review(queue_id, format!(
                "Queued work returned, but its terminal reply could not be confirmed. Review the original conversation in Phoenix before continuing; the queued turn was retained: {error:#}"
            )));
        }
    }
    // Never retire first: a crash here must leave the final for the replay guard.
    super::turn_queue::complete(queue_id)
        .context("Queued work returned but its queue receipt could not settle; the queue lane stopped")
}

const CHANNEL_TERMINAL_REVIEW: &str = "Phoenix could not confirm a saved terminal result for this request. Review the original conversation before continuing; work may already have taken effect. It was not automatically rerun.";

/// A runner's Done precedes fallible trace/receipt persistence. Only channel
/// delivery waits for the gateway's confirmation; ordinary streams keep their
/// existing event lifecycle. FinalOutput remains a visible candidate meanwhile.
fn defer_channel_done(key: Option<&super::channel_receipts::Key>, event: &CliEvent) -> bool {
    key.is_some() && matches!(event, CliEvent::Done)
}

fn confirmed_channel_event(
    key: Option<&super::channel_receipts::Key>, confirmed: bool,
) -> Option<CliEvent> {
    key.map(|_| if confirmed { CliEvent::Done } else {
        CliEvent::TerminalFailure { message: CHANNEL_TERMINAL_REVIEW.into() }
    })
}

/// An error may occur before commit OR during cleanup after a valid commit.
/// Preserve both cases for review; never translate either into an absent
/// receipt, permission to execute again, or a clean durable acknowledgement.
async fn confirm_direct_channel_response(
    key: Option<&super::channel_receipts::Key>, response: WireResponse,
) -> WireResponse {
    if let (Some(key), WireResponse::Done(summary)) = (key, &response) {
        if matches!(summary.completion, TurnCompletion::Completed | TurnCompletion::Incomplete) {
            if let Err(error) = super::channel_receipts::record(key, &summary.final_markdown).await {
                glog(&format!("channel completion could not be confirmed: {error:#}"));
                return WireResponse::Error {
                    message: format!("{CHANNEL_TERMINAL_REVIEW} Details: {error:#}"),
                };
            }
        }
    }
    response
}

/// Remove queued (not yet running) answered-question continuations owned by
/// the stopped agent (or every one, for "stop all"). Group continuations keep
/// their own reservation lifecycle and are left to the group cancel path.
fn drop_queued_answer_continuations(session_id: &str, target: Option<&str>) -> usize {
    let queued = match super::turn_queue::list(session_id) {
        Ok(queued) => queued,
        Err(error) => {
            glog(&format!("session {session_id}: could not read queued answers on stop: {error:#}"));
            return 0;
        }
    };
    let mut dropped = 0;
    for row in queued {
        if row.state != "queued" || row.target_group.is_some() {
            continue;
        }
        if !matches!(row.origin, Some(crate::runtime::TurnOrigin::AskAnswer { .. })) {
            continue;
        }
        let owned = match (target, row.target_agent.as_deref()) {
            (None, _) => true,
            (Some(target), Some(owner)) => crate::runtime::postbox::base_agent(owner) == target,
            (Some(_), None) => false,
        };
        if !owned {
            continue;
        }
        match super::turn_queue::cancel(session_id, &row.queue_id) {
            Ok(_) => dropped += 1,
            Err(error) => glog(&format!(
                "session {session_id}: queued answer {} could not be dropped on stop: {error:#}",
                row.queue_id
            )),
        }
    }
    if dropped > 0 {
        glog(&format!("session {session_id}: stop dropped {dropped} queued answer continuation(s)"));
    }
    dropped
}

/// The caller has already passed the ordinary wire owner check. Removal and
/// any group settlement commit together before wake; a lost acknowledgement
/// must not leave other saved prompts behind a stopped queue lane.
async fn cancel_queued_turn_and_wake(
    write_half: &mut tokio::net::unix::OwnedWriteHalf,
    session_id: String,
    queue_id: String,
    wake: impl FnOnce(),
) -> Result<()> {
    super::turn_queue::cancel(&session_id, &queue_id)?;
    wake();
    send(
        write_half,
        &WireResponse::Done(TurnSummary {
            completion: TurnCompletion::Canceled,
            final_markdown: "queued message removed".to_string(),
            main_session_id: session_id,
            run_id: queue_id,
            trace_path: String::new(),
            route: "queue-cancel".to_string(),
            total_tokens: 0,
            orchestrator_tokens: None,
            coder_tokens: None,
            compression_saved_tokens: 0,
            compression_raw_tokens: 0,
            context_window: None,
            background_work_pending: false,
        }),
    )
    .await
}

fn fire_queued_turn_lane(session_id: String, turn_locks: TurnLocks, independent: bool) {
    tokio::spawn(async move {
        let lock = turn_lock_for(&turn_locks, &session_id);
        loop {
            let _turn = if independent { lock.continuation().await } else { lock.lock().await };
            if let Err(error) = enqueue_ready_group_continuations(Some(&session_id)) {
                crate::runtime::postbox::forward(&session_id, CliEvent::GatewayNotice(
                    format!("Ready group work remains saved but could not be queued: {error:#}"),
                ));
            }
            let next = if independent {
                super::turn_queue::claim_next_group_continuation(&session_id)
            } else {
                super::turn_queue::claim_next(&session_id)
            };
            let claimed = match next {
                Ok(Some(claimed)) => claimed,
                Ok(None) => return,
                Err(error) => {
                    crate::runtime::postbox::forward(
                        &session_id,
                        CliEvent::GatewayNotice(format!(
                            "queued prompt could not be claimed: {error:#}"
                        )),
                    );
                    return;
                }
            };
            let queued = claimed.payload;
            let activation_check = match (
                &queued.origin, &queued.group_activation, queued.target_group.as_deref(),
            ) {
                (Some(crate::runtime::TurnOrigin::AskAnswer { .. } | crate::runtime::TurnOrigin::GroupContinuation { .. }), Some(intent), Some(group_id))
                    if intent.group_id == group_id => {
                    // Internal answer continuations carry their stored graph;
                    // answer prose is data, not a new scheduling instruction.
                    crate::runtime::company::global()
                        .and_then(|company| company.directory_snapshot())
                        .and_then(|snapshot| crate::runtime::group_conversation::validate_group_activation(&snapshot, intent)
                            .map(|_| Some(intent.clone())))
                }
                _ => validated_group_activation_intent(
                    queued.target_group.as_deref(), &queued.user_request, queued.group_activation.as_ref(),
                ),
            };
            if let Err(error) = activation_check {
                let failure = format!(
                    "The group roster changed while this message was queued. Review its @mentions and send it again: {error:#}"
                );
                let _ = super::turn_queue::fail(&claimed.queue_id, &failure);
                if queued.target_group.is_some() {
                    if let Some(turn_id) = queued.turn_id.as_deref() {
                        let _ = super::turn_queue::release_immediate_group(&session_id, turn_id);
                    }
                }
                crate::runtime::postbox::forward(&session_id, CliEvent::GatewayNotice(failure));
                continue;
            }
            let authored_prompt = queued.user_request.clone();
            let visible_prompt = match queued.origin.as_ref() {
                Some(crate::runtime::TurnOrigin::AskAnswer { display, .. } | crate::runtime::TurnOrigin::GroupContinuation { display, .. }) => display.clone(),
                _ => authored_prompt.clone(),
            };
            // A queued composer send is a first-class authored turn. Publish
            // its boundary before any reasoning/tool/output event so an open
            // Canvas moves the already-visible queued bubble into canonical
            // order and starts the next turn's activity cursor immediately.
            // The queue id is stable on both sides and also makes a truncated
            // long prompt claimable without matching its presentation text.
            let execution_scope = crate::runtime::postbox::ExecutionScope::new(
                queued.turn_id.clone().unwrap_or_else(|| claimed.queue_id.clone()),
                queued.turn_id.clone().unwrap_or_else(|| claimed.queue_id.clone()),
            );
            let anchor = crate::runtime::turn_anchor::recall(&session_id);
            let workspace = queued
                .workspace
                .clone()
                .or_else(|| anchor.as_ref().map(|anchor| anchor.workspace.clone()))
                .or_else(|| Some(crate::config::phoenix_workspace_root()));
            let permission_mode = queued
                .permission_mode
                .or_else(|| {
                    queued.yolo.map(|enabled| {
                        if enabled {
                            crate::tools::PermissionMode::FullAccess
                        } else {
                            crate::tools::PermissionMode::Workspace
                        }
                    })
                })
                .or_else(|| anchor.as_ref().map(|anchor| anchor.permission_mode()))
                .unwrap_or(crate::tools::PermissionMode::Workspace);
            let task_turn_id = queued
                .turn_id
                .clone()
                .or_else(|| Some(claimed.queue_id.clone()));
            let channel_completion = queued_channel_completion(&session_id, &queued, workspace.as_deref());
            let immediate_group_receipt = queued
                .target_group
                .as_ref()
                .zip(queued.turn_id.clone())
                .map(|(_, turn_id)| turn_id);
            let started = start_queued_turn(
                &claimed.queue_id, claimed.attempts, channel_completion.as_ref(), || {
                    crate::runtime::postbox::begin_foreground_replay(&session_id);
                    crate::runtime::postbox::forward_owned(
                        &session_id,
                        Some(&execution_scope),
                        CliEvent::WakeTurn {
                            prompt: visible_prompt,
                            turn_id: task_turn_id.clone(),
                            origin: queued.origin.clone(),
                        },
                    );
                    if let Some(ref workspace) = workspace {
                        let _ = crate::runtime::turn_anchor::remember_mode(
                            &session_id, workspace, permission_mode,
                        );
                    }
                    crate::runtime::postbox::forward(
                        &session_id,
                        CliEvent::GatewayNotice(format!("queued prompt {} is starting", claimed.queue_id)),
                    );
                    let session_file_before = session_file_version(&session_id);
                    let (handle, event_rx) = super::spawn_event_loop(
                        false,
                        false,
                        session_id.clone(),
                        queued.user_request,
                        permission_mode,
                        crate::runtime::InteractionMode::Execute,
                        false,
                        workspace,
                        queued.target_agent.clone(),
                        queued.target_group.clone(),
                        queued.group_activation,
                        queued.sticky_notes,
                        queued.viewport,
                        queued.attachments,
                        task_turn_id,
                    );
                    (handle, event_rx, session_file_before)
                },
            );
            let (handle, mut event_rx, session_file_before) = match started {
                Ok(Some(started)) => started,
                Ok(None) => {
                    crate::runtime::postbox::forward(&session_id, CliEvent::GatewayNotice(
                        format!("queued prompt {} already has a saved terminal reply; execution was not repeated", claimed.queue_id),
                    ));
                    continue;
                }
                Err(error) => {
                    crate::runtime::postbox::forward(&session_id, CliEvent::GatewayNotice(format!("{error:#}")));
                    return;
                }
            };
            let running_id = running_turns()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(
                    session_id.clone(),
                    RunningTurn {
                        handle: handle.abort_handle(),
                        target_agent: queued
                            .target_agent
                            .as_deref()
                            .map(crate::runtime::postbox::base_agent)
                            .map(str::to_string),
                        target_group: queued.target_group,
                        authored_prompt: Some(authored_prompt),
                        session_file_before,
                    },
                );
            let mut liveness = tokio::time::interval(Duration::from_millis(250));
            while let Some(event) = super::turn_events::next_event(&mut event_rx, &handle, &mut liveness).await {
                if defer_channel_done(channel_completion.as_ref(), &event) {
                    break;
                }
                if let CliEvent::FinalOutput(ref output) = event {
                    broadcast_session_answer(&session_id, output);
                }
                let done = matches!(event, CliEvent::Done);
                crate::runtime::postbox::forward_owned(&session_id, Some(&execution_scope), event);
                if done {
                    break;
                }
            }
            let result = handle.await;
            if let Some(client_turn_id) = immediate_group_receipt.as_deref() {
                let succeeded = matches!(&result, Ok(Ok(report)) if report.execution.outcome.completion == crate::runtime::OutcomeCompletion::Completed);
                if let Err(error) = super::turn_queue::finalize_immediate_group(
                    &session_id,
                    client_turn_id,
                    succeeded,
                ) {
                    crate::runtime::postbox::forward(
                        &session_id,
                        CliEvent::GatewayNotice(format!(
                            "group turn receipt could not finalize: {error:#}"
                        )),
                    );
                }
            }
            running_turns()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&session_id, running_id);
            match result {
                Ok(Ok(report)) => {
                    if let Err(error) = finish_queued_turn(
                        &claimed.queue_id, channel_completion.as_ref(),
                        report.execution.outcome.completion, &report.execution.outcome.summary,
                    ).await {
                        if let Some(event) = confirmed_channel_event(channel_completion.as_ref(), false) {
                            crate::runtime::postbox::forward_owned(&session_id, Some(&execution_scope), event);
                        }
                        crate::runtime::postbox::forward(
                            &session_id,
                            CliEvent::GatewayNotice(format!("{error:#}")),
                        );
                        return;
                    }
                    if let Some(event) = confirmed_channel_event(channel_completion.as_ref(), matches!(
                        report.execution.outcome.completion,
                        crate::runtime::OutcomeCompletion::Completed | crate::runtime::OutcomeCompletion::Incomplete,
                    )) {
                        crate::runtime::postbox::forward_owned(&session_id, Some(&execution_scope), event);
                    }
                    crate::runtime::postbox::forward(
                        &session_id,
                        CliEvent::GatewayNotice(format!(
                            "queued prompt complete · {} tokens · route {}",
                            report.total_tokens(),
                            report.route_description()
                        )),
                    );
                }
                Ok(Err(error)) => {
                    let _ = super::turn_queue::fail(&claimed.queue_id, &format!("{error:#}"));
                    if let Some(event) = confirmed_channel_event(channel_completion.as_ref(), false) {
                        crate::runtime::postbox::forward_owned(&session_id, Some(&execution_scope), event);
                    }
                    crate::runtime::postbox::forward(
                        &session_id,
                        CliEvent::GatewayNotice(format!("queued prompt failed: {error:#}")),
                    );
                }
                Err(error) => {
                    let _ = super::turn_queue::fail(&claimed.queue_id, &format!("{error:#}"));
                    if let Some(event) = confirmed_channel_event(channel_completion.as_ref(), false) {
                        crate::runtime::postbox::forward_owned(&session_id, Some(&execution_scope), event);
                    }
                    crate::runtime::postbox::forward(
                        &session_id,
                        CliEvent::GatewayNotice(format!(
                            "queued prompt stopped internally: {error:#}"
                        )),
                    );
                }
            }
        }
    });
}

fn completed_jobs_fallback(jobs: &[crate::runtime::postbox::CompletedJob]) -> Option<String> {
    let bodies = jobs
        .iter()
        .filter(|job| !job.body.trim().is_empty())
        .map(|job| {
            format!(
                "### {} — {}\n\n{}",
                job.agent,
                if job.ok {
                    "completed"
                } else {
                    "partial/failed"
                },
                job.body.trim()
            )
        })
        .collect::<Vec<_>>();
    if bodies.is_empty() {
        return None;
    }
    Some(format!(
        "The model was unavailable, so Phoenix returned the recorded result directly.\n\n{}",
        bodies.join("\n\n---\n\n")
    ))
}

fn persist_wake_fallback(
    session_id: &str,
    text: &str,
    jobs: &[crate::runtime::postbox::CompletedJob],
) -> Result<()> {
    let mut store = crate::session::SessionStore::with_default_root()?;
    store.load_from_disk()?;
    let session = store.get_mut(session_id).context("wake owner session is unavailable")?;
    let terminal = jobs.iter().filter(|job| job.kind == crate::runtime::postbox::ReturnKind::Terminal)
        .cloned().collect::<Vec<_>>();
    for job in &terminal {
        let marker = format!("<!-- phoenix-background-return:{} -->", job.delivery_id);
        if !session.messages.iter().any(|message|
            matches!(message, crate::session::Message::ToolResult {output,..} if output.contains(&marker))) {
            session.push_message(crate::session::Message::ToolResult {
                tool_name:"terminal_job".into(),
                input:serde_json::json!({"job_id":job.delivery_id,"action":"status"}).to_string(),
                success:job.ok, output:format!("{}\n\n{marker}",job.body),
            });
        }
    }
    let duplicate = session.messages.last().is_some_and(|message| {
        matches!(message, crate::session::Message::Assistant { content } if content == text)
    });
    if !duplicate {
        session.push_message(crate::session::Message::Assistant {
            content: text.to_string(),
        });
    }
    // A fallback final is also an owner delivery. Save its receipt markers
    // before suppressing another completion wake, including provider failure.
    store.save_one(session_id)?;
    crate::runtime::postbox::acknowledge_ready(session_id, &terminal)
}

/// A user's popup answer arrived after the asking turn ended. Put the wake in
/// the same durable FIFO as composer sends before acknowledging it, so a
/// gateway crash cannot lose an approval the UI already closed.
#[derive(Debug)]
struct DetachedAskApprovalGrant {
    session_id: String,
    subject: String,
    grant_id: String,
}

fn grant_detached_ask_approval(
    record: &crate::runtime::asks::AskRecord,
    answer: &str,
) -> Result<Option<DetachedAskApprovalGrant>> {
    let Some(approval) = record.approval.as_ref() else {
        return Ok(None);
    };
    if approval.action == "permanent_agent"
        && approval.is_presented_in(&record.questions)
        && approval.confirmed_by(answer)
    {
        let grant_id =
            crate::tools::agent_forge::grant_permanent_hire(&record.session_id, &approval.subject)?;
        return Ok(Some(DetachedAskApprovalGrant {
            session_id: record.session_id.clone(),
            subject: approval.subject.clone(),
            grant_id,
        }));
    }
    Ok(None)
}

/// Keep a detached approval only if its corresponding continuation reached
/// the durable queue. Queue failure must not leave unrelated 24-hour authority
/// behind. The issuance token makes the rollback safe if a replacement grant
/// raced with this one.
fn retain_detached_ask_approval_if_queued<T>(
    grant: Option<&DetachedAskApprovalGrant>,
    queue_result: Result<T>,
) -> Result<T> {
    let Err(queue_error) = queue_result else {
        return queue_result;
    };
    let Some(grant) = grant else {
        return Err(queue_error);
    };
    match crate::tools::agent_forge::revoke_permanent_hire_grant(
        &grant.session_id,
        &grant.subject,
        &grant.grant_id,
    ) {
        Ok(true) => Err(queue_error),
        Ok(false) => Err(queue_error.context(
            "late-answer queue failed and its exact hiring grant was no longer available to roll back",
        )),
        Err(revoke_error) => Err(queue_error.context(format!(
            "late-answer queue failed and its hiring grant could not be rolled back: {revoke_error:#}"
        ))),
    }
}

/// `(group_id, target_agent_id)` of an outside-group call approval.
fn outside_call_scope(record: &crate::runtime::asks::AskRecord) -> Option<(String, String)> {
    let approval = record.approval.as_ref().filter(|approval| approval.action == "outside_group_call")?;
    let group_id = approval.details.get("group_id")?.trim();
    let agent_id = approval.details.get("target_agent_id")?.trim();
    (!group_id.is_empty() && !agent_id.is_empty()).then(|| (group_id.to_string(), agent_id.to_string()))
}

/// One decision answers every open card asking the same thing: the same
/// coworker, the same group, the same permission. Parallel callers used to
/// raise one card each and the user had to approve each separately.
fn settle_matching_outside_call_asks(record: Option<&crate::runtime::asks::AskRecord>, answer: &str) {
    let Some(record) = record else { return };
    let Some(scope) = outside_call_scope(record) else { return };
    let Ok(records) = crate::runtime::asks::conversation_records(&record.session_id) else { return };
    let mut seen = std::collections::HashSet::new();
    for sibling in records {
        if sibling.ask_id == record.ask_id
            || sibling.status != "pending"
            || sibling.resolved_at.is_some()
            || !seen.insert(sibling.ask_id.clone())
            || outside_call_scope(&sibling).as_ref() != Some(&scope)
        {
            continue;
        }
        match crate::runtime::asks::settle_superseded_approval(&sibling.ask_id, answer) {
            Ok(live) => glog(&format!(
                "ask {}: settled by matching decision on {} ({})",
                sibling.ask_id, record.ask_id, if live { "live" } else { "archived" }
            )),
            Err(error) => glog(&format!(
                "ask {}: matching decision could not be applied ({error:#})", sibling.ask_id
            )),
        }
    }
}

/// `(agent, exact action fingerprint)` of a single-call tool permission card.
fn tool_permission_scope(record: &crate::runtime::asks::AskRecord) -> Option<(String, String)> {
    let approval = record.approval.as_ref().filter(|approval| approval.action == "tool_permission")?;
    let fingerprint = approval.details.get("action_fingerprint")?.trim();
    let agent = record.agent_id.clone().unwrap_or_else(|| record.agent.clone());
    (!fingerprint.is_empty() && !agent.trim().is_empty()).then(|| (agent, fingerprint.to_string()))
}

/// Parallel instances of one coworker in a room can park on the very same
/// exact action (same tool, same input). One decision answers every LIVE card
/// for that exact action; a card whose turn ended keeps its own lifecycle
/// (an exact-action approval is never replayed into a stale continuation).
fn settle_matching_tool_permission_asks(record: Option<&crate::runtime::asks::AskRecord>, answer: &str) {
    let Some(record) = record else { return };
    let Some(scope) = tool_permission_scope(record) else { return };
    let Ok(records) = crate::runtime::asks::conversation_records(&record.session_id) else { return };
    for sibling in records {
        if sibling.ask_id == record.ask_id
            || sibling.status != "pending"
            || sibling.resolved_at.is_some()
            || tool_permission_scope(&sibling).as_ref() != Some(&scope)
        {
            continue;
        }
        if crate::runtime::asks::answer(&sibling.ask_id, answer.to_string()) {
            glog(&format!("ask {}: settled by matching decision on {}", sibling.ask_id, record.ask_id));
        }
    }
}

/// The turn that raised an outside-group card has ended, so nothing else will
/// save the grant. Save it here; the grant is idempotent per group+coworker.
fn grant_detached_outside_call(record: &crate::runtime::asks::AskRecord, answer: &str) -> Result<()> {
    let Some((group_id, agent_id)) = outside_call_scope(record) else { return Ok(()) };
    let approval = record.approval.as_ref().expect("scoped approval");
    if !(approval.is_presented_in(&record.questions) && approval.confirmed_by(answer)) {
        return Ok(());
    }
    let actor = record.agent_id.clone().filter(|id| !id.is_empty())
        .unwrap_or_else(|| if record.agent.is_empty() { "phoenix".into() } else { record.agent.clone() });
    crate::runtime::company::global()?.apply_directory_change(
        &actor,
        format!("outside-call-grant:{group_id}:{agent_id}"),
        crate::runtime::company_directory::DirectoryChange::OutsideCallGrantSet {
            group_id, agent_id, granted: true,
        },
    )?;
    Ok(())
}

fn resolved_answer_ack(ask_id: &str, answer: &str) -> Result<Option<WireResponse>> {
    let Some(record) = crate::runtime::asks::decision_record_for(ask_id)? else {
        return Ok(None);
    };
    if record.status == "pending" && record.resolved_at.is_none() {
        return Ok(None);
    }
    anyhow::ensure!(record.resolved_at.is_some(), "question resolution is incomplete");
    if outside_call_scope(&record).is_some() {
        // A duplicate card for a group decision that was already made (here
        // or on a sibling card). The decision stands; closing it is success.
        return Ok(Some(WireResponse::AskAnswered {
            disposition: "already_resolved".into(), continuation_turn_id: None,
        }));
    }
    anyhow::ensure!(
        matches!(record.status.as_str(), "answered" | "answered_late")
            && record.answer.as_deref() == Some(answer),
        "this question already has a different saved decision"
    );
    if record.status == "answered" {
        return Ok(Some(WireResponse::AskAnswered {
            disposition: "delivered".into(), continuation_turn_id: None,
        }));
    }
    // The immutable envelope and queue receipt must already exist. Never
    // reconstruct a continuation from an archived question or current access.
    let payload = super::turn_queue::frozen_answer_turn(&record.session_id, ask_id, answer, || {
        anyhow::bail!("saved question continuation is unavailable; review it in Phoenix")
    })?;
    let turn_id = payload.turn_id.context("saved answer has no turn identity")?;
    let queue_id = super::turn_queue::submitted_turn_receipt(&record.session_id, &turn_id)?
        .context("saved question submission is unavailable; review it in Phoenix")?;
    Ok(Some(WireResponse::AskAnswered {
        disposition: format!("late_answer_queued:{queue_id}"), continuation_turn_id: Some(turn_id),
    }))
}

fn queue_late_answer_wake(
    session_id: String,
    turn_locks: TurnLocks,
    ask_id: &str,
    answer: String,
    asking_agent_id: Option<&str>,
) -> Result<(String, String)> {
    let payload = super::turn_queue::frozen_answer_turn(&session_id, ask_id, &answer, || {
        build_late_answer_turn(&session_id, ask_id, &answer, asking_agent_id)
    })?;
    let continuation_turn_id = payload.turn_id.as_deref().context("answer turn has no identity")?;
    if let Some(queue_id) = super::turn_queue::retired_turn_receipt(&session_id, continuation_turn_id)? {
        // The queue may finish before ask archival after a crash/reconnect.
        // Never re-transfer an already completed ancestor or re-run this turn.
        return Ok((queue_id, continuation_turn_id.to_owned()));
    }
    let queue_id = super::turn_queue::enqueue(&session_id, &payload)?;
    if payload.target_group.is_some() {
        let _ = super::turn_queue::reserve_immediate_group(&session_id, continuation_turn_id, &payload)?;
        if let Some(original_turn_id) = crate::runtime::company::global()?
            .supersede_waiting_group_activation(&session_id, ask_id, continuation_turn_id)?
        {
            let _ = super::turn_queue::settle_immediate_group(&session_id, &original_turn_id);
        }
    }
    // Each late answer gets a whole turn of its own after any running one.
    // Slipping it into a running turn as a mid-task note was faster, but the
    // agent could finish that turn without acting on it (a Reddit "show me
    // first" answer was ignored on 2026-09-29).
    crate::runtime::postbox::forward(
        &session_id,
        CliEvent::GatewayNotice("popup answer queued — waking the agent with it".to_string()),
    );
    fire_queued_turn_wake(session_id, turn_locks);
    Ok((queue_id, continuation_turn_id.to_owned()))
}

fn build_late_answer_turn(
    session_id: &str,
    ask_id: &str,
    answer: &str,
    asking_agent_id: Option<&str>,
) -> Result<super::turn_queue::QueuedUserTurn> {
    let (workspace, permission_mode) = crate::runtime::turn_anchor::wake_context(&session_id);
    let (target_agent, target_group) =
        super::resolve_canonical_turn_route(&session_id, None, None)?;
    let continuation = format!(
        "[late ask answer] The user answered the saved question {ask_id}: \"{answer}\". \
         Continue the work this answer unblocks using the existing task and evidence. \
         Do not restart completed independent work or assume the original turn timed out."
    );
    let continuation_turn_id = {
        use sha2::{Digest, Sha256};
        let mut digest = Sha256::new();
        digest.update(b"phoenix-late-ask-continuation-v1\0");
        digest.update(session_id.as_bytes());
        digest.update([0]);
        digest.update(ask_id.as_bytes());
        format!("ask_answer_{:x}", digest.finalize())
    };
    let mut resumed_activation = None;
    // Group turns intentionally wake only explicit mentions. Preserve the
    // asking coworker's immutable identity so a late answer cannot wake zero
    // members or be guessed into Phoenix. Missing identity must not silently
    // redirect a private answer to whichever member is first in the roster.
    let user_request = if let Some(group_id) = target_group.as_deref() {
        let snapshot = crate::runtime::company::global()?.directory_snapshot()?;
        let context =
            crate::runtime::group_conversation::resolve_group_turn(&snapshot, group_id, "")?;
        let pending_turn = crate::runtime::company::global()?
            .group_turn_waiting_on_ask(&session_id, ask_id)?
            .filter(|turn| turn.group_id == group_id);
        let recorded_asker_id = pending_turn.as_ref().and_then(|turn| {
            turn.members
                .iter()
                .find(|member| {
                    member.state
                        == crate::runtime::group_conversation::GroupMemberActivationState::WaitingUser
                        && member.receipt_id.as_deref() == Some(ask_id)
                })
                .map(|member| member.participant.agent_id.as_str())
        });
        if let (Some(reported), Some(recorded)) = (asking_agent_id, recorded_asker_id) {
            anyhow::ensure!(reported == recorded, "group answer owner disagrees with its durable ask receipt");
        }
        let participant = recorded_asker_id
            .or(asking_agent_id)
            .and_then(|agent_id| {
                context
                    .participants
                    .iter()
                    .find(|participant| participant.agent_id == agent_id)
            })
            .context("late group answer has no active participant to wake")?;
        let frontier = pending_turn.as_ref().map(|turn| turn.answer_frontier(&participant.agent_id));
        let downstream = pending_turn
            .iter()
            .flat_map(|turn| turn.members.iter())
            .filter(|member| {
                member.state
                    == crate::runtime::group_conversation::GroupMemberActivationState::Queued
                    && frontier.as_ref().is_none_or(|ids| ids.contains(&member.participant.agent_id))
                    && member.participant.agent_id != participant.agent_id
                    && context
                        .participants
                        .iter()
                        .any(|candidate| candidate.agent_id == member.participant.agent_id)
            })
            .map(|member| format!("@{}", member.participant.agent_id))
            .collect::<Vec<_>>();
        if let Some(plan) = pending_turn.as_ref().and_then(|turn| turn.activation.as_ref()) {
            if let Some(edges) = &plan.execution_dependencies {
                let ids = frontier.as_ref().context("group answer has no execution frontier")?;
                anyhow::ensure!(ids.iter().all(|id| context.participants.iter().any(|member| &member.agent_id == id)), "a required group continuation member is no longer active");
                let mentions = ids.iter().map(|id| format!("@{id}")).collect::<Vec<_>>().join(" ");
                let mut intent = crate::runtime::group_conversation::preview_group_activation(&snapshot, group_id, &mentions)?.intent();
                intent.inherit_tool_constraints(plan);
                let dependencies = edges.iter().filter(|edge| ids.contains(&edge.prerequisite) && ids.contains(&edge.dependent)).cloned().collect::<Vec<_>>();
                intent.execution_mode = if dependencies.is_empty() { crate::runtime::group_conversation::GroupExecutionMode::Parallel } else { crate::runtime::group_conversation::GroupExecutionMode::Ordered };
                intent.execution_waves = crate::runtime::group_conversation::dependency_waves(&intent.active_agent_ids, &dependencies)?;
                intent.execution_dependencies = Some(dependencies);
                resumed_activation = Some(intent);
            }
        }
        if let Some(intent) = &resumed_activation {
            let participants = intent.active_agent_ids.iter().map(|id| format!("@{id}")).collect::<Vec<_>>().join(" ");
            format!("{continuation}\n\nResume participants: {participants}. Follow the stored task prerequisites. Continue your assigned stage using the saved predecessor results; do not restart completed work.")
        } else if downstream.is_empty() {
            format!("{continuation}\n\nResume owner: @{}", participant.agent_id)
        } else {
            // The answer must be processed by the asker before the dependency
            // frontier continues. All still-queued coworkers then share the
            // next wave and can run concurrently.
            format!(
                "{continuation}\n\nExecution resume: @{} first, then {}",
                participant.agent_id,
                downstream.join(" ")
            )
        }
    } else {
        continuation
    };
    let display = answer
        .lines()
        .filter_map(|line| line.trim().strip_prefix("A: "))
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let display = if display.is_empty() {
        answer.trim().to_string()
    } else {
        display
    };
    let group_activation = if resumed_activation.is_some() { resumed_activation } else {
        validated_group_activation_intent(target_group.as_deref(), &user_request, None)?
    };
    let payload = super::turn_queue::QueuedUserTurn {
        // A popup can be retried after a crash between the queue write and
        // the answer acknowledgement. Its successor identity must therefore
        // be deterministic, not another anonymous queue row.
        turn_id: Some(continuation_turn_id.clone()),
        user_request,
        origin: Some(crate::runtime::TurnOrigin::AskAnswer {
            ask_id: ask_id.to_string(),
            agent_id: asking_agent_id.map(str::to_string),
            display,
        }),
        interaction_mode: crate::runtime::InteractionMode::Execute,
        permission_mode: Some(permission_mode),
        yolo: None,
        workspace,
        target_agent,
        target_group,
        group_activation,
        sticky_notes: None,
        viewport: None,
        attachments: None,
    };
    Ok(payload)
}

/// Shared wake-turn core: take the session's own lock, optionally require a
/// ready postbox return, run the canonical conversation OWNER, and forward its
/// events to the session's subscribers.
fn fire_session_wake(
    session_id: String,
    turn_locks: TurnLocks,
    request: String,
    label: &'static str,
    require_terminal_ready: bool,
    notice: Option<String>,
) {
    tokio::spawn(async move {
        let (target_agent, target_group) = match super::resolve_canonical_turn_route(
            &session_id,
            None,
            None,
        ) {
            Ok(route) => route,
            Err(error) => {
                glog(&format!(
                    "{label} [{session_id}]: canonical owner resolution failed: {error:#}"
                ));
                crate::runtime::postbox::forward(
                    &session_id,
                    CliEvent::GatewayNotice(format!(
                        "This wake was stopped before execution because its conversation owner could not be verified: {error:#}"
                    )),
                );
                crate::runtime::postbox::forward(&session_id, CliEvent::Done);
                return;
            }
        };
        let owner_lane = target_agent
            .as_deref()
            .map(crate::runtime::postbox::base_agent)
            .unwrap_or("orchestrator")
            .to_string();
        // The SESSION'S OWN lock: the wake waits only for this session's
        // live turn (whose final drain absorbs the return), never for other
        // sessions' turns — a return must reach the user the moment its own
        // session is free.
        let lock = turn_lock_for(&turn_locks, &session_id);
        let _turn = lock.lock().await;
        // A safety-net wake drains the canonical owner's lane. Adopt only
        // genuinely orphaned steers into that lane; never rewrite ownership to
        // Phoenix merely because a producer omitted a target.
        let rehomed = crate::runtime::postbox::rehome_orphan_steers(&session_id, &owner_lane);
        if rehomed > 0 {
            glog(&format!(
                "{label} [{session_id}]: adopted {rehomed} orphaned steer(s) → {owner_lane}"
            ));
        }
        // The wake is a no-op when the live turn we waited on already drained
        // everything: a background return absorbed at its final round-top drain
        // (`has_ready`), OR a user steer delivered mid-turn that the running
        // orchestrator already picked up (`has_pending_steer`). The steer arm
        // is what closes the lost-message race — a user message delivered as an
        // orchestrator steer while a turn was live must still start a fresh turn
        // here if that turn ended before draining it, since it never became a
        // `ready` job. If it WAS drained, the steer lane is empty and we return.
        // A waiting coworker return alone never starts an owner turn in a
        // one-to-one chat: results are not fed back to the asker there.
        let returns_count = session_id.starts_with("group-") && crate::runtime::postbox::has_ready(&session_id);
        let pending_steer = crate::runtime::postbox::has_pending_steer(&session_id, &owner_lane);
        let can_wake = if require_terminal_ready {
            crate::runtime::postbox::has_terminal_ready(&session_id)
        } else { returns_count || pending_steer };
        if !can_wake {
            return;
        }
        // Race at turn end: a user message delivered into a turn that finished
        // before its next round-top drain becomes the request of a NORMAL new
        // turn (persisted as the user's own message, not a synthetic wake
        // prompt). `take_first_user_steer` removes it atomically, so the turn
        // that ended can never also have consumed it, and any further parked
        // messages are drained by this new turn's first round as mid-task
        // messages: exactly once, never lost.
        let mut request = request;
        if !require_terminal_ready {
            if let Some(note) =
                crate::runtime::postbox::take_first_user_steer(&session_id, &owner_lane)
            {
                crate::runtime::company::mirror_message_injected(
                    &session_id,
                    &note.message_id,
                    &owner_lane,
                );
                crate::runtime::postbox::forward(
                    &session_id,
                    CliEvent::SteerDelivered {
                        to: owner_lane.clone(),
                        subject: note.subject.clone(),
                    },
                );
                // Announce the adopted message as this turn's authored
                // boundary so live faces show the new turn starting from it.
                crate::runtime::postbox::forward(
                    &session_id,
                    CliEvent::WakeTurn {
                        prompt: note.body.clone(),
                        turn_id: Some(format!("steer_turn_{}", uuid::Uuid::new_v4().simple())),
                        origin: None,
                    },
                );
                request = note.body;
            }
        }
        // Snapshot before the turn drains the postbox. If provider integration
        // fails, these completed receipts are still enough to answer the user
        // deterministically and durably.
        let ready_snapshot = crate::runtime::postbox::ready_jobs(&session_id);
        let started = std::time::Instant::now();
        glog(&format!(
            "{label} [{session_id}]: session idle — starting owner `{owner_lane}` turn"
        ));
        if let Some(notice) = notice {
            crate::runtime::postbox::forward(&session_id, CliEvent::GatewayNotice(notice));
        }
        let saved_bytes_before = crate::tools::compress::saved_bytes_total();
        let raw_bytes_before = crate::tools::compress::raw_bytes_total();
        // Replay the session's own anchor (workspace + yolo) — a wake is a
        // continuation of the user's session, not a fresh daemon-cwd turn.
        let (wake_workspace, wake_mode) = crate::runtime::turn_anchor::wake_context(&session_id);
        let (handle, mut event_rx) = super::spawn_event_loop(
            false,
            false,
            session_id.clone(),
            request,
            wake_mode,
            crate::runtime::InteractionMode::Execute,
            false,
            wake_workspace,
            target_agent.clone(),
            target_group.clone(),
            None,
            None,
            None,
            None, // no attachments on a wake turn
            None,
        );
        let running_id = running_turns()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                session_id.clone(),
                RunningTurn {
                    handle: handle.abort_handle(),
                    target_agent: target_agent
                        .as_deref()
                        .map(crate::runtime::postbox::base_agent)
                        .map(str::to_string),
                    target_group: target_group.clone(),
                    authored_prompt: None,
                    session_file_before: None,
                },
            );
        let mut saw_final = false;
        let mut spawned_followup = false;
        while let Some(event) = event_rx.recv().await {
            // The wake turn runs while the session is idle, so — unlike a
            // background specialist — it IS the live turn the user is watching.
            // Forward the orchestrator's live work to the session's Subscribe
            // connections: Thinking (its chip lights up), the streaming ticker,
            // any delegations, FinalOutput (the TUI renders it as the answer),
            // and Done — the wake has no Turn socket, so no TurnFinished ever
            // reaches the TUI; Done is what tells it the stream ended and the
            // moods/spinner must clear (skipping it left "Phoenix thinking" lit
            // forever after a wake). Skip only the two background-lifecycle
            // events, which are owned by the postbox: job_started/job_finished
            // already notify subscribers and subscribe() replays them, so
            // forwarding them here would double-render the return block.
            match &event {
                CliEvent::BackgroundAgentSpawned { .. }
                | CliEvent::BackgroundAgentReturned { .. } => {
                    if matches!(&event, CliEvent::BackgroundAgentSpawned { .. }) {
                        spawned_followup = true;
                    }
                }
                // Delay Done until after a possible deterministic fallback, so
                // the final cannot arrive behind the UI's stream-closed event.
                CliEvent::Done => break,
                _ => {
                    if matches!(&event, CliEvent::FinalOutput(_)) {
                        saw_final = true;
                    }
                    crate::runtime::postbox::forward(&session_id, event.clone());
                }
            }
            if let Some(brief) = event_brief(&event) {
                glog(&format!("  {brief}"));
            }
        }
        let secs = started.elapsed().as_secs_f32();
        let handle_result = handle.await;
        running_turns()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&session_id, running_id);
        if !saw_final && !spawned_followup {
            if let Some(fallback) = completed_jobs_fallback(&ready_snapshot) {
                match persist_wake_fallback(&session_id, &fallback, &ready_snapshot) {
                    Ok(()) => {
                        crate::runtime::postbox::forward(&session_id, CliEvent::FinalOutput(fallback.clone()));
                        broadcast_session_answer(&session_id, &fallback);
                        glog(&format!("{label} [{session_id}]: provider produced no final; saved and delivered the recorded result directly"));
                    }
                    Err(error) => {
                        glog(&format!("{label} [{session_id}]: fallback delivery remains pending: {error:#}"));
                        crate::runtime::postbox::forward(&session_id, CliEvent::GatewayNotice(
                            "The terminal result is recorded, but saving its conversation delivery failed. It remains pending.".into()));
                    }
                }
            }
        }
        crate::runtime::postbox::forward(&session_id, CliEvent::Done);
        match handle_result {
            Ok(Ok(report)) => {
                // Surface the same closure line a normal turn shows (elapsed /
                // tokens / route / trace) over the standing Subscribe stream —
                // the wake has no Turn socket, so a GatewayNotice is the right
                // channel and keeps "wake is not a user turn" semantics.
                let summary = summarize(&report, saved_bytes_before, raw_bytes_before);
                let trace = std::path::Path::new(&summary.trace_path)
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| summary.trace_path.clone());
                // Sub-100-token savings are real but not news — "~1 tok
                // compressed" as a headline stat reads as a bug (2026-07-09).
                let compressed = if summary.compression_saved_tokens >= 100 {
                    format!(" · ~{} tok compressed", summary.compression_saved_tokens)
                } else {
                    String::new()
                };
                crate::runtime::postbox::forward(
                    &session_id,
                    CliEvent::GatewayNotice(format!(
                        "{secs:.1}s · {} tokens{compressed} · route {} · trace {trace}",
                        summary.total_tokens, summary.route,
                    )),
                );
                glog(&format!(
                    "{label} done [{session_id}] in {secs:.1}s ({} tokens, ~{} tok compressed, route {})",
                    summary.total_tokens, summary.compression_saved_tokens, summary.route,
                ));
            }
            Ok(Err(error)) => glog(&format!(
                "{label} FAILED [{session_id}] in {secs:.1}s: {error:#}"
            )),
            Err(join_error) => glog(&format!(
                "{label} internal error [{session_id}]: {join_error:#}"
            )),
        }
    });
}

async fn read_bounded_utf8_line<R>(
    reader: &mut R,
    max_bytes: usize,
) -> std::io::Result<Option<String>>
where
    R: AsyncBufRead + Unpin,
{
    let mut bytes = Vec::with_capacity(max_bytes.min(8 * 1024) + 1);
    let read = {
        // `lines()` grows a String until newline with no cap. Restrict the
        // underlying reader itself to max+1, so even a client that never sends
        // a newline cannot make us allocate beyond the protocol budget.
        let mut limited = (&mut *reader).take((max_bytes + 1) as u64);
        limited.read_until(b'\n', &mut bytes).await?
    };
    if read == 0 {
        return Ok(None);
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if bytes.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("request line exceeds {max_bytes} bytes"),
        ));
    }
    String::from_utf8(bytes).map(Some).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("request line is not valid UTF-8: {error}"),
        )
    })
}

fn vault_visible_scopes() -> Vec<crate::security::vault::CredentialScope> {
    use crate::security::vault::CredentialScope;

    let mut scopes = vec![CredentialScope::Company];
    if let Ok(company) = crate::runtime::company::global() {
        if let Ok(snapshot) = company.directory_snapshot() {
            scopes.extend(
                snapshot
                    .agents
                    .into_iter()
                    .map(|agent| CredentialScope::agent(agent.profile.agent_id)),
            );
            scopes.extend(
                snapshot
                    .groups
                    .into_iter()
                    .map(|group| CredentialScope::group(group.profile.group_id)),
            );
        }
    }
    scopes.sort_by(|left, right| format!("{left:?}").cmp(&format!("{right:?}")));
    scopes.dedup();
    scopes
}

fn execute_vault_command(command: &VaultCommand) -> Result<VaultReply> {
    use crate::security::vault::{NewPass, Vault};

    let vault = Vault::open_default();
    let result: Result<VaultReply> = match command {
        VaultCommand::Status => Ok(VaultReply::Status {
            status: vault.status().as_str().to_string(),
        }),
        VaultCommand::Initialize { master_password } => {
            let recovery_key = vault.initialize(master_password)?;
            Ok(VaultReply::RecoveryKey {
                recovery_key: recovery_key.to_string(),
            })
        }
        VaultCommand::UnlockWithPassword { master_password } => {
            vault.unlock_with_password(master_password)?;
            Ok(VaultReply::Unlocked)
        }
        VaultCommand::UnlockWithRecoveryKey { recovery_key } => {
            vault.unlock_with_recovery_key(recovery_key)?;
            Ok(VaultReply::Unlocked)
        }
        VaultCommand::Lock => {
            vault.lock();
            Ok(VaultReply::Locked)
        }
        VaultCommand::ChangeMasterPassword {
            current_password,
            new_password,
        } => {
            vault.change_master_password(current_password, new_password)?;
            Ok(VaultReply::PasswordChanged)
        }
        VaultCommand::RotateRecoveryKey => {
            let recovery_key = vault.rotate_recovery_key()?;
            Ok(VaultReply::RecoveryKey {
                recovery_key: recovery_key.to_string(),
            })
        }
        VaultCommand::List => Ok(VaultReply::Credentials {
            credentials: vault.list(&vault_visible_scopes())?,
        }),
        VaultCommand::Store {
            scope,
            site,
            label,
            username,
            kind,
            metadata_json,
            secret,
            fields,
        } => {
            anyhow::ensure!(
                vault_visible_scopes().contains(scope),
                "credential scope does not name an existing coworker or group"
            );
            Ok(VaultReply::Stored {
                credential: vault.put_pass(NewPass {
                    scope: Some(scope.clone()),
                    site,
                    label,
                    username: username.clone(),
                    kind,
                    metadata_json,
                    secret,
                    fields: fields.clone(),
                })?,
            })
        }
        VaultCommand::Update {
            credential_id,
            scope,
            site,
            label,
            username,
            kind,
            metadata_json,
            replacement_secret,
            replacement_fields,
        } => {
            let scopes = vault_visible_scopes();
            anyhow::ensure!(
                scopes.contains(scope),
                "credential scope does not name an existing coworker or group"
            );
            Ok(VaultReply::Stored {
                credential: vault.update_pass(
                    credential_id,
                    &scopes,
                    NewPass {
                        scope: Some(scope.clone()),
                        site,
                        label,
                        username: username.clone(),
                        kind,
                        metadata_json,
                        secret: replacement_secret.as_deref().unwrap_or(""),
                        fields: replacement_fields.clone().unwrap_or_default(),
                    },
                    replacement_secret.is_some(),
                )?,
            })
        }
        VaultCommand::FulfillRequest {
            ask_id,
            kind,
            site,
            label,
            username,
            metadata_json,
            secret,
            fields,
        } => fulfill_pass_request(&vault, ask_id, kind, site.as_deref(), label.as_deref(), username.clone(), metadata_json, secret, fields),
        VaultCommand::Reveal {
            credential_id,
            master_password,
        } => {
            // Unlock once per gateway lifetime; the per-reveal re-check is an
            // opt-in setting.
            let always_ask = crate::settings::effective_bool(
                "security.reveal_requires_password",
                &crate::settings::SettingsScope::Global,
            )
            .unwrap_or(false);
            let status = vault.status();
            if (always_ask && vault.has_master_password()) || !status.can_reveal() {
                vault.unlock_with_password(
                    master_password
                        .as_deref()
                        .context("Passes is locked; enter your master password to reveal")?,
                )?;
            }
            let credential = vault.reveal(credential_id, &vault_visible_scopes())?;
            Ok(VaultReply::Revealed {
                credential: credential.metadata.clone(),
                secret: credential.secret().to_string(),
                fields: credential.fields_map(),
            })
        }
        VaultCommand::Delete { credential_id } => Ok(VaultReply::Deleted {
            deleted: vault.delete(credential_id, &vault_visible_scopes())?,
        }),
    };
    result
}

/// Seal the user's answer to an `ask_for_pass` card. The owner/scope come
/// from the runtime-bound ask record, never from the client.
#[allow(clippy::too_many_arguments)]
fn fulfill_pass_request(
    vault: &crate::security::vault::Vault,
    ask_id: &str,
    kind: &str,
    site: Option<&str>,
    label: Option<&str>,
    username: Option<String>,
    metadata_json: &str,
    secret: &str,
    fields: &std::collections::BTreeMap<String, String>,
) -> Result<VaultReply> {
    let record = crate::runtime::asks::decision_record_for(ask_id)?
        .context("this pass request is no longer available")?;
    anyhow::ensure!(
        record.status == "pending" && record.resolved_at.is_none(),
        "this pass request was already answered"
    );
    let approval = record
        .approval
        .as_ref()
        .filter(|approval| approval.action == crate::tools::passes::PASS_REQUEST_ACTION)
        .context("this card is not a pass request")?;
    let details = &approval.details;
    let requested_kind = details.get("kind").map(String::as_str).unwrap_or("secret");
    anyhow::ensure!(
        kind == requested_kind,
        "this request asked for a {requested_kind}, not a {kind}"
    );
    let scope: crate::security::vault::CredentialScope = serde_json::from_str(
        details
            .get("credential_scope")
            .context("pass request has no bound owner")?,
    )
    .context("pass request owner is invalid")?;
    // The agent named the site; a login form may refine it (e.g. the user
    // types accounts.google.com for a gmail.com request).
    let site = site
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or(details.get("site").map(String::as_str))
        .unwrap_or(crate::security::vault::UNBOUND_SITE)
        .to_string();
    let title = label
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or(details.get("title").map(String::as_str))
        .unwrap_or("Saved pass")
        .to_string();
    let mut public: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(if metadata_json.trim().is_empty() { "{}" } else { metadata_json })
            .context("metadata_json must be a JSON object")?;
    public.insert("requested_by".into(), serde_json::json!(details.get("agent_id")));
    public.insert("request_id".into(), serde_json::json!(ask_id));
    if kind == "verification_code" {
        public.insert("one_time".into(), serde_json::json!(true));
    }
    let credential = vault.put_pass(crate::security::vault::NewPass {
        scope: Some(scope),
        site: &site,
        label: &title,
        username,
        kind: crate::tools::passes::stored_kind(kind),
        metadata_json: &serde_json::Value::Object(public).to_string(),
        secret,
        fields: fields.clone(),
    })?;
    glog(&format!(
        "ask {ask_id}: pass request saved as {} ({})",
        credential.credential_id, credential.kind
    ));
    Ok(VaultReply::PassRequestFulfilled {
        answer: crate::tools::passes::fulfilled_answer(&credential),
        credential,
    })
}

async fn handle_connection(stream: UnixStream, turn_locks: TurnLocks) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let line = match read_bounded_utf8_line(&mut reader, MAX_WIRE_REQUEST_BYTES).await {
        Ok(Some(line)) => Zeroizing::new(line),
        Ok(None) => return Ok(()), // liveness probe connected and left
        Err(error) => {
            glog(&format!("bad request rejected before JSON parse: {error}"));
            send(
                &mut write_half,
                &WireResponse::Error {
                    message: format!("bad request: {error}"),
                },
            )
            .await?;
            return Ok(());
        }
    };
    let request: WireRequest = match serde_json::from_str(line.as_str()) {
        Ok(req) => req,
        Err(error) => {
            glog(&format!("bad request: {error}"));
            send(
                &mut write_half,
                &WireResponse::Error {
                    message: format!("bad request: {error}"),
                },
            )
            .await?;
            return Ok(());
        }
    };
    if let Err(error) = validate_wire_request_session_ids(&request) {
        glog(&format!("bad request: {error:#}"));
        send(
            &mut write_half,
            &WireResponse::Error {
                message: format!("bad request: {error:#}"),
            },
        )
        .await?;
        return Ok(());
    }
    if let Err(error) = authorize_wire_request_owner(&request) {
        glog(&format!("conversation owner rejected: {error:#}"));
        send(
            &mut write_half,
            &WireResponse::Error {
                message: format!("conversation owner rejected: {error:#}"),
            },
        )
        .await?;
        return Ok(());
    }

    match request {
        WireRequest::ProtocolInfo => {
            send(
                &mut write_half,
                &WireResponse::ProtocolInfo {
                    protocol: GATEWAY_WIRE_PROTOCOL.to_string(),
                    package_version: env!("CARGO_PKG_VERSION").to_string(),
                    binary_id: Some(GATEWAY_BUILD_ID.to_string()),
                },
            )
            .await
        }
        WireRequest::Ping => send(&mut write_half, &WireResponse::Pong).await,
        WireRequest::CompanySnapshot { session_id, .. } => {
            let response = match crate::runtime::company::global()
                .and_then(|store| store.snapshot(session_id.as_deref()))
            {
                Ok(snapshot) => WireResponse::CompanySnapshot(snapshot),
                Err(error) => WireResponse::Error {
                    message: format!("company snapshot unavailable: {error:#}"),
                },
            };
            send(&mut write_half, &response).await
        }
        WireRequest::Vault(command) => {
            // Password KDF and encrypted-store I/O are intentionally blocking;
            // keep them off Tokio's gateway threads so live turns and browser
            // frames do not freeze while Settings unlocks the vault.
            let response =
                match tokio::task::spawn_blocking(move || execute_vault_command(&command)).await {
                    Ok(Ok(reply)) => WireResponse::Vault(reply),
                    Ok(Err(error)) => WireResponse::Error {
                        message: format!("credential vault operation failed: {error:#}"),
                    },
                    Err(error) => WireResponse::Error {
                        message: format!("credential vault worker failed: {error}"),
                    },
                };
            send(&mut write_half, &response).await
        }
        WireRequest::Onboarding(command) => {
            // Cookie inspection uses a blocking worker while provider
            // verification performs one bounded live request. Both paths keep
            // unrelated conversation turns responsive.
            let response = match crate::onboarding::execute_async(command).await {
                Ok(reply) => WireResponse::Onboarding(reply),
                Err(error) => WireResponse::Error {
                    message: format!("onboarding operation failed: {error:#}"),
                },
            };
            send(&mut write_half, &response).await
        }
        WireRequest::CompanyDirectory(command) => {
            // Directory snapshots combine SQLite projections with a bounded
            // set of canonical session files. Keep that blocking I/O off the
            // gateway's async reactor so active turns and browser frames do
            // not hitch when the sidebar refreshes.
            let response = match tokio::task::spawn_blocking(move || {
                crate::runtime::company_control::execute(command)
            })
            .await
            {
                Ok(Ok(view)) => {
                    if let Some(
                        crate::runtime::company_control::CompanyDirectoryMutation::AgentRequested {
                            agent_id,
                        },
                    ) = view.mutation.as_ref()
                    {
                        crate::runtime::postbox::forward(
                            &format!("agent-{agent_id}"),
                            CliEvent::GatewayNotice("Created · coworker record saved".to_string()),
                        );
                        fire_agent_provisioning_wake(agent_id.clone(), Arc::clone(&turn_locks));
                    }
                    WireResponse::CompanyDirectory(view)
                }
                Ok(Err(error)) => WireResponse::Error {
                    message: format!("company directory operation failed: {error:#}"),
                },
                Err(error) => WireResponse::Error {
                    message: format!("company directory worker failed: {error}"),
                },
            };
            send(&mut write_half, &response).await
        }
        WireRequest::GroupActivationPreview {
            group_id,
            user_request,
        } => {
            let response = match tokio::task::spawn_blocking(move || {
                let snapshot = crate::runtime::company::global()?.directory_snapshot()?;
                crate::runtime::group_conversation::preview_group_activation(
                    &snapshot,
                    &group_id,
                    &user_request,
                )
            })
            .await
            {
                Ok(Ok(preview)) => WireResponse::GroupActivationPreview(preview),
                Ok(Err(error)) => WireResponse::Error {
                    message: format!("group activation preview failed: {error:#}"),
                },
                Err(error) => WireResponse::Error {
                    message: format!("group activation preview worker failed: {error}"),
                },
            };
            send(&mut write_half, &response).await
        }
        WireRequest::TeachWorkflow(command) => {
            // Browser/CDP and private durable writes are synchronous. Keep the
            // complete record-then-commit action off Tokio's async workers.
            let outcome = tokio::task::spawn_blocking(move || {
                crate::runtime::workflow_teaching::handle_command(command)
            })
            .await;
            let response = match outcome {
                Ok(Ok(reply)) => WireResponse::TeachWorkflow(reply),
                Ok(Err(error)) => WireResponse::Error {
                    message: format!("{error:#}"),
                },
                Err(join) => WireResponse::Error {
                    message: join.to_string(),
                },
            };
            send(&mut write_half, &response).await?;
            Ok(())
        }
        WireRequest::Settings(command) => {
            // Settings performs bounded JSON reads, migration, validation and
            // an fsync-backed replacement. Keep those durable operations off
            // Tokio's reactor just like the other desktop control planes.
            let outcome =
                tokio::task::spawn_blocking(move || crate::settings::execute(command)).await;
            let response = match outcome {
                Ok(Ok(reply)) => WireResponse::Settings(reply),
                Ok(Err(error)) => WireResponse::Error {
                    message: format!("settings operation failed: {error:#}"),
                },
                Err(join) => WireResponse::Error {
                    message: format!("settings worker failed: {join}"),
                },
            };
            send(&mut write_half, &response).await
        }
        WireRequest::Subscribe { session_id, .. } => {
            // Register with the postbox and forward its events down this
            // socket until the client hangs up. Runs outside the turn lock —
            // background events must flow while a turn runs.
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<CliEvent>();
            crate::runtime::postbox::subscribe(&session_id, tx);
            glog(&format!("session {session_id}: event subscriber attached"));
            // Registration barrier. `subscribe_events` waits for this before
            // returning, so a one-shot caller can safely submit its Turn on a
            // second socket without losing a fast background spawn between
            // the two connections. Replayed lifecycle events remain queued
            // behind this acknowledgement.
            send(&mut write_half, &WireResponse::Pong).await?;
            let mut disconnect_probe = [0_u8; 1];
            loop {
                tokio::select! {
                    event = rx.recv() => match event {
                        Some(event) => {
                            if send(&mut write_half, &WireResponse::Event(event)).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    },
                    // Subscribe is server-to-client only. EOF (or unexpected
                    // client bytes) retires this connection immediately,
                    // rather than retaining one idle handler per completed
                    // zero-background benchmark case until a future event.
                    _ = reader.read(&mut disconnect_probe) => break,
                }
            }
            Ok(())
        }
        WireRequest::SubscribeJournal { session_id, .. } => {
            // Same subscription, reduced through the story lane — the
            // reducer's tallies are per-connection state, so every journal
            // subscriber gets receipts flushed at its own stream's beats.
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let replay = crate::runtime::postbox::subscribe_journal(&session_id, tx);
            glog(&format!(
                "session {session_id}: journal subscriber attached"
            ));
            let mut reducer = crate::runtime::story::OwnedStoryReducer::default();
            for event in replay {
                for row in reducer.push(&event) {
                    send(&mut write_half, &owned_story_frame(row, &event, true)?).await?;
                }
            }
            // Registration barrier: the desktop takes a second authoritative
            // session snapshot only after this Pong. Any turn event occurring
            // after registration is queued behind the barrier, so completion
            // cannot fall into the old snapshot/subscription gap.
            send(&mut write_half, &WireResponse::Pong).await?;
            let mut disconnect_probe = [0_u8; 1];
            'stream: loop {
                // Journal subscriptions are server-to-client only. Retire
                // idle connections on EOF instead of waiting for new work.
                let event = tokio::select! {
                    event = rx.recv() => match event {
                        Some(event) => event,
                        None => break,
                    },
                    _ = reader.read(&mut disconnect_probe) => break,
                };
                for row in reducer.push(&event) {
                    if send(&mut write_half, &owned_story_frame(row, &event, false)?)
                        .await
                        .is_err()
                    {
                        break 'stream; // client gone; postbox prunes the sender
                    }
                }
            }
            Ok(())
        }
        WireRequest::BrowserClick { instance, x, y } => {
            // CDP calls are sync — keep them off the async executor.
            let outcome = tokio::task::spawn_blocking(move || {
                crate::tools::browser_native::click_instance(&instance, x, y)
            })
            .await;
            let response = match outcome {
                Ok(Ok(())) => WireResponse::Pong,
                Ok(Err(message)) => WireResponse::Error { message },
                Err(join) => WireResponse::Error {
                    message: join.to_string(),
                },
            };
            send(&mut write_half, &response).await?;
            Ok(())
        }
        WireRequest::BrowserInteract {
            instance,
            browser_action,
        } => {
            let outcome = tokio::task::spawn_blocking(move || {
                crate::tools::browser_native::user_interact(&instance, &browser_action)
            })
            .await;
            let response = match outcome {
                Ok(Ok(receipt)) => WireResponse::BrowserInteraction(receipt),
                Ok(Err(message)) => WireResponse::Error { message },
                Err(join) => WireResponse::Error {
                    message: join.to_string(),
                },
            };
            send(&mut write_half, &response).await?;
            Ok(())
        }
        WireRequest::BrowserSurface { instance, action } => {
            // Launching or reconciling a managed Chromium process is blocking
            // CDP/process work. Keep it off Tokio so opening the native window
            // cannot stall chat events or other coworkers' browser lanes.
            let outcome = tokio::task::spawn_blocking(move || {
                crate::tools::browser_native::browser_surface(&instance, action)
            })
            .await;
            let response = match outcome {
                Ok(Ok(reply)) => WireResponse::BrowserSurface(reply),
                Ok(Err(message)) => WireResponse::Error { message },
                Err(join) => WireResponse::Error {
                    message: join.to_string(),
                },
            };
            send(&mut write_half, &response).await?;
            Ok(())
        }
        WireRequest::DesktopWorkspaces => {
            // A compositor startup may briefly own the registry lock; never
            // make a viewer request hold up chat on Tokio's reactor.
            let response=match tokio::task::spawn_blocking(
                crate::tools::isolated_desktop::existing_desktops).await {
                Ok(views)=>WireResponse::DesktopWorkspaces(views),
                Err(error)=>WireResponse::Error{message:error.to_string()},
            };
            send(&mut write_half,&response).await?;
            Ok(())
        }
        WireRequest::DesktopObservation{scope_key,after_ms} => {
            let response=match tokio::task::spawn_blocking(move ||
                crate::tools::isolated_desktop::latest_observation(&scope_key,after_ms)).await {
                Ok(Ok(observation))=>WireResponse::DesktopObservation(observation),
                Ok(Err(error))=>WireResponse::Error{message:error.to_string()},
                Err(error)=>WireResponse::Error{message:error.to_string()},
            };
            send(&mut write_half,&response).await?;
            Ok(())
        }
        WireRequest::SubscribeBrowser => {
            // Fan the live screencast out to this client until it hangs up.
            // Drain to the newest frame before each write and cap delivery at
            // ~15fps. A socket can accept data faster than WebKit can decode
            // it, so broadcast lag alone is not sufficient to prevent a
            // seconds-long queue inside the localhost relay. The WebView now
            // has its own one-in-flight decoder, so deliver at up to 30fps for
            // low pointer/scroll latency while still dropping stale frames.
            let mut rx = crate::tools::browser_native::frames_subscribe();
            glog("browser view subscriber attached");
            loop {
                match rx.recv().await {
                    Ok(mut frame) => {
                        while let Ok(newest) = rx.try_recv() {
                            frame = newest;
                        }
                        if send(&mut write_half, &WireResponse::BrowserFrame(frame))
                            .await
                            .is_err()
                        {
                            break; // client gone
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(33)).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            Ok(())
        }
        WireRequest::DigestSession { session_id, .. } => {
            // Ack immediately — the client is quitting and must not wait on a
            // model call. The digest runs detached on the daemon.
            let ack = send(&mut write_half, &WireResponse::Pong).await;
            tokio::spawn(async move {
                match crate::runtime::session_digest::digest_session_now(&session_id).await {
                    Ok(Some(receipt)) => glog(&format!("memory: quit-digest — {receipt}")),
                    Ok(None) => {}
                    Err(error) => glog(&format!(
                        "memory: quit-digest for {session_id} FAILED: {error:#}"
                    )),
                }
                // Index unconditionally, not only when THIS digest wrote a
                // note: a session boundary is exactly when pending saves
                // (earlier digests whose cognify failed, lib-beat notes)
                // must become searchable — the next session's first recall
                // is minutes away. Near-free when the backlog is empty.
                match crate::librarian::memory::cognify_backlog().await {
                    Ok(receipt) => glog(&format!("memory: quit-digest index — {receipt}")),
                    Err(error) => notice_cognify_failure("quit-digest", &error),
                }
            });
            ack
        }
        WireRequest::IndexMemory => {
            // Ack immediately — indexing runs reasoning-model calls and can
            // take minutes; the client never waits on it. cognify_backlog
            // serializes against itself and is near-free when nothing is
            // pending.
            let ack = send(&mut write_half, &WireResponse::Pong).await;
            tokio::spawn(async {
                match crate::librarian::memory::cognify_backlog().await {
                    Ok(receipt) => glog(&format!("memory: fresh-start index — {receipt}")),
                    Err(error) => notice_cognify_failure("fresh-start", &error),
                }
            });
            ack
        }
        WireRequest::AnswerAsk { ask_id, answer, .. } => {
            match resolved_answer_ack(&ask_id, &answer) {
                Ok(Some(receipt)) => return send(&mut write_half, &receipt).await,
                Ok(None) => {}
                Err(error) => return send(&mut write_half, &WireResponse::Error {
                    message: error.to_string(),
                }).await,
            }
            let prior_record = crate::runtime::asks::decision_record_for(&ask_id).ok().flatten();
            let delivered = crate::runtime::asks::answer(&ask_id, answer.clone());
            if delivered {
                glog(&format!("ask {ask_id}: user answer delivered"));
                settle_matching_outside_call_asks(prior_record.as_ref(), &answer);
                settle_matching_tool_permission_asks(prior_record.as_ref(), &answer);
                send(
                    &mut write_half,
                    &WireResponse::AskAnswered {
                        disposition: "delivered".to_string(),
                        continuation_turn_id: None,
                    },
                )
                .await
            } else if let Some((session_id, asking_agent_id, ask_record)) =
                crate::runtime::asks::record_for(&ask_id)
                    .map(|record| {
                        (
                            record.session_id.clone(),
                            record.agent_id.clone(),
                            Some(record),
                        )
                    })
                    .or_else(|| {
                        crate::runtime::asks::session_for(&ask_id)
                            .map(|session_id| (session_id, None, None))
                    })
            {
                // The asking turn is gone (popup outlived a timeout, a
                // watcher halt, an Esc) — the user's answer must not vanish
                // while the TUI claims "continues": wake the session WITH it.
                glog(&format!(
                    "ask {ask_id}: turn already ended — waking [{session_id}] with the late answer"
                ));
                crate::runtime::journal::record(
                    &session_id,
                    "ask",
                    "user",
                    &format!(
                        "late answer to {ask_id}: \"{}\"",
                        answer.chars().take(120).collect::<String>()
                    ),
                );
                if let Some(record) = ask_record.as_ref().filter(|record| outside_call_scope(record).is_some()) {
                    // A group-boundary decision outlived the turn that asked.
                    // Save the decision itself; waking the asker is a courtesy
                    // and its absence (the caller left the group, the turn was
                    // superseded) must not surface as an error on the card.
                    if let Err(error) = grant_detached_outside_call(record, &answer) {
                        return send(&mut write_half, &WireResponse::Error {
                            message: format!("the group permission was not saved; retry: {error:#}"),
                        }).await;
                    }
                    let (disposition, continuation_turn_id) = match queue_late_answer_wake(
                        session_id,
                        Arc::clone(&turn_locks),
                        &ask_id,
                        answer.clone(),
                        asking_agent_id.as_deref(),
                    ) {
                        Ok((queue_id, turn_id)) => (format!("late_answer_queued:{queue_id}"), Some(turn_id)),
                        Err(error) => {
                            glog(&format!("ask {ask_id}: group decision saved; no turn to wake ({error:#})"));
                            ("resolved".to_string(), None)
                        }
                    };
                    crate::runtime::asks::archive_late_answer(&ask_id, &answer);
                    settle_matching_outside_call_asks(Some(record), &answer);
                    return send(&mut write_half, &WireResponse::AskAnswered {
                        disposition, continuation_turn_id,
                    }).await;
                }
                let detached_approval = if let Some(record) = ask_record.as_ref() {
                    match grant_detached_ask_approval(record, &answer) {
                        Ok(grant) => grant,
                        Err(error) => {
                            glog(&format!(
                                "ask {ask_id}: answer received but approval receipt could not be saved ({error:#})"
                            ));
                            return send(
                                &mut write_half,
                                &WireResponse::Error {
                                    message: format!(
                                        "your answer was received, but its approval receipt was not saved; keep the card open and retry: {error:#}"
                                    ),
                                },
                            )
                            .await;
                        }
                    }
                } else {
                    None
                };
                let queue_result = queue_late_answer_wake(
                    session_id,
                    Arc::clone(&turn_locks),
                    &ask_id,
                    answer.clone(),
                    asking_agent_id.as_deref(),
                );
                match retain_detached_ask_approval_if_queued(
                    detached_approval.as_ref(),
                    queue_result,
                ) {
                    Ok((queue_id, continuation_turn_id)) => {
                        crate::runtime::asks::archive_late_answer(&ask_id, &answer);
                        send(
                            &mut write_half,
                            &WireResponse::AskAnswered {
                                disposition: format!("late_answer_queued:{queue_id}"),
                                continuation_turn_id: Some(continuation_turn_id),
                            },
                        )
                        .await
                    }
                    Err(error) => {
                        glog(&format!(
                            "ask {ask_id}: late answer could not be durably queued ({error:#})"
                        ));
                        send(
                            &mut write_half,
                            &WireResponse::Error {
                                message: format!(
                                    "your answer was not queued; keep this approval open and retry: {error:#}"
                                ),
                            },
                        )
                        .await
                    }
                }
            } else {
                send(
                    &mut write_half,
                    &WireResponse::Error {
                        message: format!(
                            "ask {ask_id} is no longer pending (timed out or already answered)"
                        ),
                    },
                )
                .await
            }
        }
        WireRequest::DismissAsk { ask_id, .. } => {
            // A reconnect can close an already resolved card. Do not let a
            // stale pending copy replace its committed decision with dismissal.
            if crate::runtime::asks::decision_record_for(&ask_id)?
                .is_some_and(|record| record.status != "pending" || record.resolved_at.is_some())
            {
                return send(&mut write_half, &WireResponse::AskAnswered {
                    disposition: "stale_dismissed".into(), continuation_turn_id: None,
                }).await;
            }
            let delivered = crate::runtime::asks::dismiss(&ask_id);
            glog(&format!(
                "ask {ask_id}: {} without a wake",
                if delivered {
                    "dismissed"
                } else {
                    "stale card dismissed"
                }
            ));
            send(
                &mut write_half,
                &WireResponse::AskAnswered {
                    disposition: if delivered {
                        "dismissed".to_string()
                    } else {
                        "stale_dismissed".to_string()
                    },
                    continuation_turn_id: None,
                },
            )
            .await
        }
        WireRequest::QueuedTurns { session_id, .. } => {
            let queued = super::turn_queue::list(&session_id)?;
            send(&mut write_half, &WireResponse::QueuedTurns(queued)).await
        }
        WireRequest::TodoList { session_id, .. } => {
            match crate::tools::todo_snapshot(&session_id) {
                Ok(todos) => send(&mut write_half, &WireResponse::TodoList(todos)).await,
                Err(error) => {
                    send(
                        &mut write_half,
                        &WireResponse::Error {
                            message: format!(
                                "could not read this conversation's task list: {error:#}"
                            ),
                        },
                    )
                    .await
                }
            }
        }
        WireRequest::ConversationAsks { session_id, .. } => {
            let records = tokio::task::spawn_blocking(move || {
                crate::runtime::asks::conversation_records(&session_id)
            })
            .await;
            match records {
                Ok(Ok(records)) => {
                    send(&mut write_half, &WireResponse::ConversationAsks(records)).await
                }
                Ok(Err(error)) => {
                    send(
                        &mut write_half,
                        &WireResponse::Error {
                            message: format!(
                                "could not read this conversation's questions: {error:#}"
                            ),
                        },
                    )
                    .await
                }
                Err(error) => {
                    send(
                        &mut write_half,
                        &WireResponse::Error {
                            message: format!("question history worker failed: {error}"),
                        },
                    )
                    .await
                }
            }
        }
        WireRequest::CancelQueuedTurn {
            session_id,
            queue_id,
            ..
        } => {
            let wake_session = session_id.clone();
            cancel_queued_turn_and_wake(
                &mut write_half, session_id, queue_id,
                || fire_queued_turn_wake(wake_session, Arc::clone(&turn_locks)),
            )
            .await
        }
        WireRequest::SteerQueuedTurn {
            session_id,
            queue_id,
            ..
        } => {
            let summary = super::turn_queue::list(&session_id)?
                .into_iter()
                .find(|queued| queued.queue_id == queue_id)
                .with_context(|| format!("queued prompt `{queue_id}` does not exist"))?;
            anyhow::ensure!(
                summary.target_group.is_none(),
                "a group prompt cannot be converted to steer; it remains the next ordered group turn"
            );
            let payload = super::turn_queue::take_queued(&session_id, &queue_id)?;
            let target = payload
                .target_agent
                .as_deref()
                .map(crate::runtime::postbox::base_agent)
                .unwrap_or("orchestrator");
            crate::runtime::postbox::steer(
                &session_id,
                target,
                crate::runtime::postbox::SteerNote {
                    message_id: String::new(),
                    from: "user".to_string(),
                    subject: "queued prompt steered now".to_string(),
                    body: payload.user_request,
                },
            );
            crate::runtime::postbox::wake(&session_id);
            send(
                &mut write_half,
                &WireResponse::Done(TurnSummary {
                    completion: TurnCompletion::Steered,
                    final_markdown: String::new(), // receipt, not an answer: see steered_turn_summary
                    main_session_id: session_id,
                    run_id: queue_id,
                    trace_path: String::new(),
                    route: "queue-steer".to_string(),
                    total_tokens: 0,
                    orchestrator_tokens: None,
                    coder_tokens: None,
                    compression_saved_tokens: 0,
                    compression_raw_tokens: 0,
                    context_window: None,
                    background_work_pending: true,
                }),
            )
            .await
        }
        WireRequest::Cancel {
            session_id,
            target_agent,
            ..
        } => {
            let target = target_agent
                .as_deref()
                .map(crate::runtime::postbox::base_agent)
                .map(str::to_string);
            let background_aborted = target.as_deref().is_some_and(|agent| {
                crate::runtime::postbox::cancel_background_agent(&session_id, agent)
            });
            let terminal_cancelled=crate::tools::terminal_jobs::cancel_session(&session_id,target.as_deref())>0;
            // An answered question can be waiting behind the run being stopped.
            // Drop it BEFORE the run releases its lane, or it starts seconds
            // later and the stopped agent "starts working again". Done here so
            // every Stop (desktop, Esc, TUI) gets it, not only one client.
            let answers_dropped = drop_queued_answer_continuations(&session_id, target.as_deref());
            let foreground_aborted = {
                let registry = running_turns().lock().unwrap_or_else(|p| p.into_inner());
                registry.cancel(&session_id, target.as_deref())
            };
            let aborted = background_aborted || foreground_aborted || answers_dropped > 0;
            if target.is_none() {
                // "Stop all" also drops handoffs still waiting to be delivered,
                // so the next turn cannot quietly restart the stopped work.
                if let Ok(company) = crate::runtime::company::global() {
                    if let Ok(dropped) = company.cancel_all_pending_messages(&session_id) {
                        if dropped > 0 { glog(&format!("session {session_id}: dropped {dropped} undelivered handoffs")); }
                    }
                }
            }
            // Execution-owned provider senders drop with the aborted future;
            // there is no session-wide stream slot to clear.
            if aborted || terminal_cancelled {
                let scope = target.as_deref().unwrap_or("all agents");
                glog(&format!("session {session_id}: {scope} cancelled by user"));
                match super::turn_queue::retire_stopped(&session_id, target.as_deref()) {
                    Ok(0) => {}
                    Ok(count) => glog(&format!("session {session_id}: closed {count} stopped queued turn(s)")),
                    Err(error) => glog(&format!("session {session_id}: stopped queued turn could not be closed: {error:#}")),
                }
                send(
                    &mut write_half,
                    &WireResponse::Done(TurnSummary {
                        completion: TurnCompletion::Canceled,
                        final_markdown: format!("{scope} stopped"),
                        main_session_id: session_id,
                        run_id: "user-cancel".to_string(),
                        trace_path: String::new(),
                        route: "cancel".to_string(),
                        total_tokens: 0,
                        orchestrator_tokens: None,
                        coder_tokens: None,
                        compression_saved_tokens: 0,
                        compression_raw_tokens: 0,
                        context_window: None,
                        background_work_pending: false,
                    }),
                )
                .await
            } else {
                let message = match target {
                    Some(agent) => format!("{agent} is not running"),
                    None => format!("no running turn for session {session_id}"),
                };
                send(&mut write_half, &WireResponse::Error { message }).await
            }
        }
        WireRequest::DeleteTranscriptTurn {
            session_id,
            turns_from_end,
            expected_prompt,
            scope,
            ask_ids,
            ..
        } => {
            // A live turn owns an in-memory Session clone. Letting it survive
            // this mutation would allow its next save to resurrect the rows the
            // user just permanently deleted. Freeze admission first, then
            // abort and await every admitted execution before mutating files.
            let session_lock = turn_lock_for(&turn_locks, &session_id);
            let frozen = session_lock.freeze();
            let active_turns = running_turns()
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .abort_for_deletion(&session_id);
            crate::runtime::postbox::discard_session_work(&session_id);
            let _turn = frozen.wait_idle().await;
            let active_prompt_not_persisted = turns_from_end == 0
                // The never-persisted shortcut is only provable with one
                // execution. Multiple snapshots must use canonical deletion.
                && active_turns.len() == 1
                && active_turns.first().is_some_and(|(prompt, before)| {
                    prompt.as_deref().is_some_and(|prompt| {
                        transcript_prompt_identity(prompt)
                            == transcript_prompt_identity(&expected_prompt)
                    }) && session_file_version(&session_id) == before.clone()
                });
            let mutation_session = session_id.clone();
            let result = tokio::task::spawn_blocking(move || {
                delete_transcript_turn_blocking(
                    &mutation_session,
                    turns_from_end,
                    &expected_prompt,
                    scope,
                    active_prompt_not_persisted,
                    ask_ids,
                )
            })
            .await;
            let response = match result {
                Ok(Ok((removed_messages, deleted_prompt))) => {
                    crate::runtime::postbox::begin_foreground_replay(&session_id);
                    WireResponse::TranscriptDeleted {
                        removed_messages,
                        deleted_prompt,
                    }
                }
                Ok(Err(error)) => WireResponse::Error {
                    message: format!("could not delete that turn: {error:#}"),
                },
                Err(error) => WireResponse::Error {
                    message: format!("transcript deletion worker failed: {error}"),
                },
            };
            send(&mut write_half, &response).await
        }
        WireRequest::Turn {
            session_id,
            turn_id,
            user_request,
            interaction_mode,
            permission_mode,
            yolo,
            workspace,
            journal,
            target_agent,
            target_group,
            group_activation,
            delivery,
            sticky_notes,
            viewport,
            attachments,
        } => {
            // The product now has one default execution behavior. Keep the
            // legacy wire field readable, but do not let it select behavior.
            let interaction_mode = crate::runtime::InteractionMode::Execute;
            if let Some(turn_id) = turn_id.as_deref() {
                if let Err(error) = super::turn_queue::validate_client_turn_id(turn_id) {
                    return send(
                        &mut write_half,
                        &WireResponse::Error {
                            message: error.to_string(),
                        },
                    )
                    .await;
                }
            }
            if let Err(error) = validate_turn_delivery(target_group.as_deref(), delivery) {
                return send(
                    &mut write_half,
                    &WireResponse::Error {
                        message: error.to_string(),
                    },
                )
                .await;
            }
            // Normal Send in a one-to-one conversation is delivered INTO a
            // running turn (it never waits as a queued turn); only group rooms
            // keep their ordered FIFO. See `effective_turn_delivery`.
            let delivery = effective_turn_delivery(target_group.as_deref(), delivery);
            let group_activation = match validated_group_activation_intent(
                target_group.as_deref(),
                &user_request,
                group_activation.as_ref(),
            ) {
                Ok(intent) => intent,
                Err(error) => {
                    return send(
                        &mut write_half,
                        &WireResponse::Error {
                            message: format!(
                                "The group changed before this message could be sent. Refresh the activation preview and try again: {error:#}"
                            ),
                        },
                    )
                    .await;
                }
            };
            let authored_turn = super::turn_queue::QueuedUserTurn {
                turn_id: turn_id.clone(),
                user_request: user_request.clone(),
                origin: None,
                interaction_mode,
                permission_mode,
                yolo,
                workspace: workspace.clone(),
                target_agent: target_agent.clone(),
                target_group: target_group.clone(),
                group_activation: group_activation.clone(),
                sticky_notes: sticky_notes.clone(),
                viewport: viewport.clone(),
                attachments: attachments.clone(),
            };
            // Reserve group-authored turns before checking the session lock.
            // A reconnect can race the original live socket; reserving here
            // prevents that retry from being mistaken for a new queued turn.
            if target_group.is_some() {
                if let Some(client_turn_id) = turn_id.as_deref() {
                    match super::turn_queue::reserve_immediate_group(
                        &session_id,
                        client_turn_id,
                        &authored_turn,
                    ) {
                        Ok(
                            super::turn_queue::ImmediateGroupReservation::New
                            | super::turn_queue::ImmediateGroupReservation::Recovered,
                        ) => {}
                        Ok(
                            super::turn_queue::ImmediateGroupReservation::ExistingInFlight
                            | super::turn_queue::ImmediateGroupReservation::ExistingSettled,
                        ) => {
                            return send(
                                &mut write_half,
                                &WireResponse::Done(TurnSummary {
                                    // Already in the room (delivered or running):
                                    // a retry is acknowledged, never re-run.
                                    completion: TurnCompletion::Steered,
                                    final_markdown: String::new(), // receipt, not an answer: see steered_turn_summary
                                    main_session_id: session_id,
                                    run_id: client_turn_id.to_string(),
                                    trace_path: String::new(),
                                    route: "turn-idempotent".to_string(),
                                    total_tokens: 0,
                                    orchestrator_tokens: None,
                                    coder_tokens: None,
                                    compression_saved_tokens: 0,
                                    compression_raw_tokens: 0,
                                    context_window: None,
                                    background_work_pending: true,
                                }),
                            )
                            .await;
                        }
                        Err(error) => {
                            return send(
                                &mut write_half,
                                &WireResponse::Error {
                                    message: error.to_string(),
                                },
                            )
                            .await;
                        }
                    }
                }
            }
            // Busy-target shortcut: a user turn addressed to a specialist that is
            // mid-mission in the BACKGROUND would otherwise enter the mesh,
            // grab that agent's lane lock — which the running background job
            // already holds for its whole turn — and BLOCK until the job
            // finishes. The message never reaches the agent mid-flight: it
            // lands only after the work is done, so the agent ignores every
            // steer (a retired browser worker once ignored repeated steers;
            // the text echoed locally then the turn hung on the lock). The
            // one-of-each contract already routes new work for a busy agent
            // into its running job via the postbox steer lane (postbox.rs);
            // this extends that same path to a direct user turn. The note
            // drains at the agent's next round top (turn_loop::take_steer),
            // so it lands in the very next model call — no lock, no wait.
            if delivery == TurnDelivery::Steer {
                if let Some(target) = &target_agent {
                    let base = crate::runtime::postbox::base_agent(target);
                    if base != "orchestrator"
                        && crate::runtime::postbox::agent_turn_active(&session_id, base)
                    {
                        glog(&format!(
                            "turn [{session_id}] → talk into busy `{base}`: {}",
                            user_request.chars().take(80).collect::<String>()
                        ));
                        let steer_body = super::request_with_composer_attachments(
                            &user_request,
                            attachments.as_ref(),
                        );
                        crate::runtime::postbox::steer_user(
                            &session_id,
                            base,
                            turn_id.as_deref(),
                            &steer_body,
                        );
                        let persona = crate::runtime::delegation::agent_display_name(base);
                        glog(&format!("turn [{session_id}]: message delivered to {persona}"));
                        let _ = send(
                            &mut write_half,
                            &WireResponse::Done(TurnSummary {
                                completion: TurnCompletion::Steered,
                                final_markdown: String::new(), // receipt, not an answer: see steered_turn_summary
                                main_session_id: session_id.clone(),
                                run_id: String::new(),
                                trace_path: String::new(),
                                route: "talk".to_string(),
                                total_tokens: 0,
                                orchestrator_tokens: None,
                                coder_tokens: None,
                                compression_saved_tokens: 0,
                                compression_raw_tokens: 0,
                                context_window: None,
                                background_work_pending:
                                    crate::runtime::postbox::has_background_work(&session_id),
                            }),
                        )
                        .await;
                        return Ok(());
                    }
                }
            }
            // The session's OWN lock — other sessions' turns never delay this
            // one. A same-session collision USED to queue: send a "queued"
            // notice, then block on `session_lock.lock().await` until the live
            // turn finished — 10+ minutes for a goal-wake / background loop, and
            // if that turn ended before this queued one ran the message could be
            // dropped entirely (the user's "my messages don't reach the agent").
            // Now the message is INJECTED into the running turn instead: deliver
            // it as a postbox steer to the base agent the turn targets. The
            // orchestrator (and every specialist) drains its steer lane at each
            // round top (turn_loop::take_steer), so the note lands in the very
            // next model call — no lock, no wait, no queue. We ack and return
            // WITHOUT taking the lock: this turn never runs as its own turn, so
            // the message is delivered exactly once. The safety net for the race
            // where the live turn ends before its next drain is the wake ping
            // below (idle-wake starts a fresh orchestrator turn that drains the
            // pending steer at its round top) — the ONLY other delivery path,
            // never combined with block-and-run, so the message can't double.
            // A reconnect retry of a message that was already delivered into a
            // running turn (or parked for the race-safety wake) must not run a
            // second time as its own turn once the lock frees.
            if let Some(client_turn_id) = turn_id.as_deref() {
                if target_group.is_none()
                    && crate::runtime::postbox::user_steer_seen(&session_id, client_turn_id)
                {
                    return send(
                        &mut write_half,
                        &WireResponse::Done(steered_turn_summary(
                            &session_id,
                            "message already delivered".to_string(),
                        )),
                    )
                    .await;
                }
            }
            let session_lock = turn_lock_for(&turn_locks, &session_id);
            let _turn = match session_lock.try_lock() {
                Ok(guard) => guard,
                Err(_) => {
                    // A group room that is working never queues: the message
                    // lands in the room transcript now, every running member
                    // hears it, and the addressed members act on it.
                    if let Some(group_id) = target_group.as_deref() {
                        let client_turn_id = turn_id
                            .clone()
                            .unwrap_or_else(|| format!("room_{}", uuid::Uuid::new_v4().simple()));
                        let request_for_task = super::request_with_composer_attachments(
                            &user_request,
                            attachments.as_ref(),
                        );
                        let routed = group_activation
                            .clone()
                            .context("group activation is missing")
                            .and_then(|intent| {
                                route_room_message(
                                    &session_id,
                                    group_id,
                                    &client_turn_id,
                                    &user_request,
                                    &request_for_task,
                                    &intent,
                                )
                                .map(|(report, context)| (report, context, intent))
                            });
                        let (report, context, intent) = match routed {
                            Ok(routed) => routed,
                            Err(error) => {
                                if let Some(id) = turn_id.as_deref() {
                                    let _ = super::turn_queue::release_immediate_group(&session_id, id);
                                }
                                return send(
                                    &mut write_half,
                                    &WireResponse::Error {
                                        message: format!("The room could not take this message: {error:#}"),
                                    },
                                )
                                .await;
                            }
                        };
                        glog(&format!(
                            "room [{session_id}] {client_turn_id}: steered {:?} · started {:?}",
                            report.steered, report.started
                        ));
                        let permission_mode = permission_mode.or_else(|| {
                            yolo.map(|enabled| {
                                if enabled {
                                    crate::tools::PermissionMode::FullAccess
                                } else {
                                    crate::tools::PermissionMode::Workspace
                                }
                            })
                        });
                        for (agent_id, _) in &report.steered {
                            let Some(lane) = context
                                .participants
                                .iter()
                                .find(|p| &p.agent_id == agent_id)
                                .and_then(crate::runtime::group_conversation::participant_lane)
                            else {
                                continue;
                            };
                            watch_room_steer(
                                session_id.clone(),
                                group_id.to_string(),
                                agent_id.clone(),
                                lane,
                                client_turn_id.clone(),
                                intent.clone(),
                                workspace.clone(),
                                permission_mode,
                                Arc::clone(&turn_locks),
                            );
                        }
                        if report.started.is_empty() {
                            // Steer-only: the message is fully delivered; a
                            // reconnect retry of it is answered as settled.
                            if let Some(id) = turn_id.as_deref() {
                                let _ = super::turn_queue::settle_immediate_group(&session_id, id);
                            }
                        } else {
                            spawn_room_turn(
                                session_id.clone(),
                                group_id.to_string(),
                                client_turn_id.clone(),
                                request_for_task.clone(),
                                crate::runtime::group_conversation::narrow_activation(
                                    &intent,
                                    &report.started,
                                ),
                                workspace.clone(),
                                permission_mode,
                                attachments.clone(),
                                turn_id.is_some(),
                                Arc::clone(&turn_locks),
                            );
                        }
                        return send(
                            &mut write_half,
                            &WireResponse::Done(steered_turn_summary(
                                &session_id,
                                report.summary(&context),
                            )),
                        )
                        .await;
                    }
                    if delivery == TurnDelivery::Queue {
                        let queue_id = match super::turn_queue::enqueue(&session_id, &authored_turn)
                        {
                            Ok(queue_id) => queue_id,
                            Err(error) => {
                                if authored_turn.target_group.is_some() {
                                    if let Some(turn_id) = authored_turn.turn_id.as_deref() {
                                        let _ = super::turn_queue::release_immediate_group(
                                            &session_id,
                                            turn_id,
                                        );
                                    }
                                }
                                return Err(error);
                            }
                        };
                        fire_queued_turn_wake(session_id.clone(), Arc::clone(&turn_locks));
                        let destination = target_group
                            .as_deref()
                            .map(|group| format!("group {group}"))
                            .or_else(|| target_agent.clone())
                            .unwrap_or_else(|| "Phoenix".to_string());
                        let _ = send(
                            &mut write_half,
                            &WireResponse::Story(crate::runtime::story::StoryEvent::Narration {
                                agent: "system".to_string(),
                                text: format!(
                                    "Queued for {destination} after the current turn ({queue_id})."
                                ),
                            }),
                        )
                        .await;
                        let _ = send(
                            &mut write_half,
                            &WireResponse::Done(TurnSummary {
                                completion: TurnCompletion::Queued,
                                final_markdown: "message queued".to_string(),
                                main_session_id: session_id.clone(),
                                run_id: queue_id,
                                trace_path: String::new(),
                                route: "turn-queue".to_string(),
                                total_tokens: 0,
                                orchestrator_tokens: None,
                                coder_tokens: None,
                                compression_saved_tokens: 0,
                                compression_raw_tokens: 0,
                                context_window: None,
                                background_work_pending: true,
                            }),
                        )
                        .await;
                        return Ok(());
                    }
                    // The base role this turn addresses: the orchestrator when
                    // no specialist target is set (or the target resolves to
                    // orchestrator), else the specialist's base. The busy-
                    // specialist steer above (active bg job) already handled the
                    // "specialist mid-mission" case; this covers a message to an
                    // idle specialist, or the orchestrator, while a turn is live.
                    let base = target_agent
                        .as_deref()
                        .map(crate::runtime::postbox::base_agent)
                        .unwrap_or("orchestrator");
                    glog(&format!(
                        "session {session_id}: turn live → talk into `{base}` (no queue): {}",
                        user_request.chars().take(80).collect::<String>()
                    ));
                    let steer_body = super::request_with_composer_attachments(
                        &user_request,
                        attachments.as_ref(),
                    );
                    let fresh = crate::runtime::postbox::steer_user(
                        &session_id,
                        base,
                        turn_id.as_deref(),
                        &steer_body,
                    );
                    if !fresh {
                        return send(
                            &mut write_half,
                            &WireResponse::Done(steered_turn_summary(
                                &session_id,
                                "message already delivered".to_string(),
                            )),
                        )
                        .await;
                    }
                    // Safety net: if the live turn ENDS before its next round-top
                    // drain, the steer would sit unread. Ping the idle-wake loop
                    // (the same channel `job_finished` uses); once this session's
                    // lock frees, `fire_background_wake` fires and — because
                    // `has_pending_steer` now counts as a wake reason — starts a
                    // fresh orchestrator turn that drains the pending steer. If
                    // the live turn drained it first, the steer lane is empty and
                    // the wake is a no-op: exactly-once, never dropped, never
                    // double-processed.
                    //
                    // We ping for BOTH targets. The orchestrator case is the one
                    // `has_pending_steer("orchestrator")` gates in directly. A
                    // SPECIALIST target here is guaranteed IDLE — the busy arm
                    // above returned when it had a live job — so its steer would
                    // sit in a lane no turn drains and no wake runs (the wake only
                    // runs the orchestrator). Before this fix that message was
                    // LOST whenever the lock holder was a *different* turn (a goal
                    // wake, another specialist). The wake now re-homes an orphaned
                    // specialist steer to the orchestrator once it holds the lock
                    // (`rehome_orphan_steers_to_orchestrator`), so pinging here is
                    // what lets the coordinator adopt and route it.
                    crate::runtime::postbox::wake(&session_id);
                    let persona = crate::runtime::delegation::agent_display_name(base);
                    glog(&format!("turn [{session_id}]: message delivered to {persona}"));
                    let _ = send(
                        &mut write_half,
                        &WireResponse::Done(TurnSummary {
                            completion: TurnCompletion::Steered,
                            final_markdown: String::new(), // receipt, not an answer: see steered_turn_summary
                            main_session_id: session_id.clone(),
                            run_id: String::new(),
                            trace_path: String::new(),
                            route: "talk".to_string(),
                            total_tokens: 0,
                            orchestrator_tokens: None,
                            coder_tokens: None,
                            compression_saved_tokens: 0,
                            compression_raw_tokens: 0,
                            context_window: None,
                            background_work_pending: crate::runtime::postbox::has_background_work(
                                &session_id,
                            ),
                        }),
                    )
                    .await;
                    return Ok(());
                }
            };

            // A client that doesn't know the session's workspace/mode (the
            // canvas and other WS faces) sends None — fill the gaps from the
            // session's anchor, the same replay every internal wake does. A
            // TUI-started session keeps the cwd it was started from no matter
            // which face submits the turn.
            let anchor = crate::runtime::turn_anchor::recall(&session_id);
            let workspace = workspace
                .or_else(|| anchor.as_ref().map(|a| a.workspace.clone()))
                .or_else(|| Some(crate::config::phoenix_workspace_root()));
            let permission_mode = permission_mode
                .or_else(|| {
                    yolo.map(|enabled| {
                        if enabled {
                            crate::tools::PermissionMode::FullAccess
                        } else {
                            crate::tools::PermissionMode::Workspace
                        }
                    })
                })
                .or_else(|| anchor.as_ref().map(|anchor| anchor.permission_mode()))
                .unwrap_or_else(|| {
                    let scope = target_group
                        .as_deref()
                        .map(|id| crate::settings::SettingsScope::Group { id: id.to_string() })
                        .or_else(|| {
                            target_agent.as_deref().map(|id| {
                                crate::settings::SettingsScope::Agent { id: id.to_string() }
                            })
                        })
                        .unwrap_or(crate::settings::SettingsScope::Global);
                    match crate::settings::effective_string("composer.default_permission", &scope)
                        .as_deref()
                    {
                        Some("talk") => crate::tools::PermissionMode::Talk,
                        Some("full_access") => crate::tools::PermissionMode::FullAccess,
                        _ => crate::tools::PermissionMode::Workspace,
                    }
                });
            // Remember this client turn's anchor so INTERNAL wake turns
            // (background returns, goal heartbeats, cron) replay the same
            // workspace + mode instead of reverting to daemon-cwd Workspace
            // (the main-60e78831 failure: a YOLO session's wake got
            // `path escapes workspace` on its own files).
            if let Some(ref ws) = workspace {
                if let Err(error) =
                    crate::runtime::turn_anchor::remember_mode(&session_id, ws, permission_mode)
                {
                    let warning = format!(
                        "session workspace/mode could not be persisted ({error:#}); this turn can run, but a later background wake may require the client to resend its workspace"
                    );
                    glog(&format!("turn [{session_id}]: WARNING {warning}"));
                    let _ = send(
                        &mut write_half,
                        &WireResponse::Story(crate::runtime::story::StoryEvent::Narration {
                            agent: "system".to_string(),
                            text: format!("⚠ {warning}"),
                        }),
                    )
                    .await;
                }
            }
            let preview: String = user_request.chars().take(80).collect();
            let started = std::time::Instant::now();
            let saved_bytes_before = crate::tools::compress::saved_bytes_total();
            let raw_bytes_before = crate::tools::compress::raw_bytes_total();
            glog(&format!("turn start [{session_id}] {preview}"));
            crate::runtime::postbox::begin_foreground_replay(&session_id);
            let execution_scope = turn_id.as_ref().map(|id|
                crate::runtime::postbox::ExecutionScope::new(id.clone(), id.clone()));
            if let Some(scope) = &execution_scope {
                crate::runtime::postbox::forward_owned(&session_id, Some(scope), CliEvent::WakeTurn {
                    prompt: user_request.clone(), turn_id: Some(scope.turn_id.clone()), origin: None,
                });
            }
            let channel_completion = target_group.is_none().then(|| super::channel_receipts::Key::new(
                &session_id, turn_id.as_deref(), target_agent.as_deref(), workspace.as_deref(), &user_request,
            )).flatten();
            let authored_prompt = user_request.clone();
            let session_file_before = session_file_version(&session_id);
            let iris_cancel_workspace = workspace.clone();
            // `interactive: false` — the daemon has no terminal to prompt on,
            // so ask_user resolves to honest feedback instead of blocking.
            let (handle, mut event_rx) = super::spawn_event_loop(
                false,
                false,
                session_id.clone(),
                user_request,
                permission_mode,
                interaction_mode,
                false,
                workspace,
                target_agent.clone(),
                target_group.clone(),
                group_activation,
                sticky_notes.clone(),
                viewport.clone(),
                attachments.clone(),
                turn_id.clone(),
            );
            let running_id = running_turns()
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(
                    session_id.clone(),
                    RunningTurn {
                        handle: handle.abort_handle(),
                        target_agent: target_agent
                            .as_deref()
                            .map(crate::runtime::postbox::base_agent)
                            .map(str::to_string),
                        target_group: target_group.clone(),
                        authored_prompt: Some(authored_prompt),
                        session_file_before,
                    },
                );
            let mut client_gone = false;
            let mut reducer = crate::runtime::story::StoryReducer::new();

            // If sticky notes were provided, emit a Context story row so the
            // frontend can show what context the agent is working with.
            if let Some(ref notes) = sticky_notes {
                let non_empty: Vec<_> =
                    notes.iter().filter(|n| !n.text.trim().is_empty()).collect();
                if !non_empty.is_empty() {
                    let items = non_empty
                        .iter()
                        .map(|n| crate::runtime::story::ContextItemSummary {
                            kind: "sticky_note".to_string(),
                            label: format!("Note {}", n.id),
                            preview: n.text.chars().take(120).collect(),
                        })
                        .collect::<Vec<_>>();
                    let row = crate::runtime::story::StoryEvent::Context {
                        agent: target_agent
                            .clone()
                            .unwrap_or_else(|| "orchestrator".to_string()),
                        items,
                    };
                    if journal {
                        let _ = send(&mut write_half, &WireResponse::Story(row)).await;
                    }
                }
            }
            // The event channel alone cannot signal turn death: a global
            // sender clone (the live thinking-ticker slot) can keep it open
            // after the turn task is ABORTED, which would park this loop
            // forever on a dead turn. Poll task liveness alongside events.
            let mut group_members = UnfinishedGroupMembers::default();
            let mut liveness = tokio::time::interval(std::time::Duration::from_millis(250));
            loop {
                let event = tokio::select! {
                    received = event_rx.recv() => match received {
                        Some(event) => event,
                        None => break,
                    },
                    _ = liveness.tick() => {
                        if !handle.is_finished() {
                            continue;
                        }
                        // Turn completion drops volume-worker guards before
                        // the JoinHandle becomes finished. Drain every event
                        // already queued by that cleanup instead of breaking
                        // first and leaving an Environment worker chip live
                        // until a reconnect.
                        match drain_finished_turn_event(&mut event_rx) {
                            Some(event) => event,
                            None => break,
                        }
                    }
                };
                if defer_channel_done(channel_completion.as_ref(), &event) {
                    break;
                }
                if let Some(brief) = event_brief(&event) {
                    glog(&format!("  {brief}"));
                }
                crate::notifications::publish_event(&session_id, &event);
                // A desktop may leave this conversation while the turn keeps
                // running detached. Mirror every foreground event into the
                // session postbox so a newly attached Journal subscriber can
                // resume live updates instead of waiting for another reopen.
                group_members.observe(&event);
                let journal_event = crate::runtime::postbox::forward_owned(&session_id, execution_scope.as_ref(), event.clone());
                let done = matches!(event, CliEvent::Done);
                // When FinalOutput arrives, broadcast a cross-session answer
                // notification to all session boxes under the actual editable
                // coworker/group name, not a hard-coded Phoenix identity.
                if let CliEvent::FinalOutput(ref text) = event {
                    broadcast_session_answer(&session_id, text);
                }
                // Journal mode reduces the stream daemon-side; raw mode passes
                // events through untouched (the TUI's lane).
                let mut frames: Vec<serde_json::Value> = Vec::new();
                if journal {
                    for row in reducer.push(&event) {
                        frames.push(owned_story_frame(row, &journal_event, false)?);
                    }
                    if matches!(event, CliEvent::AskUser { .. } | CliEvent::Done) {
                        frames.push(serde_json::to_value(WireResponse::Event(event))?);
                    }
                } else {
                    frames.push(serde_json::to_value(WireResponse::Event(event))?);
                }
                // A vanished client must not kill the daemon mid-turn; the turn
                // finishes (and persists its session) detached.
                for frame in frames {
                    if !client_gone && send(&mut write_half, &frame).await.is_err() {
                        client_gone = true;
                        glog(&format!(
                            "session {session_id}: client disconnected mid-turn; finishing detached"
                        ));
                        eprintln!("gateway: client disconnected mid-turn; finishing turn detached");
                    }
                }
                if done {
                    break;
                }
            }
            let result = handle.await;
            let cancelled = result.as_ref().err().is_some_and(|error| error.is_cancelled());
            let iris_cancellation_error = if cancelled
                && target_agent
                    .as_deref()
                    .map(crate::runtime::postbox::base_agent)
                    == Some("frontend")
            {
                iris_cancel_workspace.as_deref().and_then(|workspace| {
                    match crate::runtime::iris_design::finalize_cancelled_turn(
                        &crate::config::phoenix_home(),
                        &session_id,
                        workspace,
                    ) {
                        Ok(true) => {
                            glog(&format!(
                                "session {session_id}: finalized cancelled managed Iris design"
                            ));
                            None
                        }
                        Ok(false) => None,
                        Err(error) => Some(error),
                    }
                })
            } else {
                None
            };
            // JoinHandle completion is the boundary: no foreground producer
            // can now append another handoff for this stopped group turn.
            // Do not apply this to private detached work or other room turns.
            let cancellation_error = if cancelled && target_group.is_some() {
                turn_id.as_deref().and_then(|id| crate::runtime::company::global()
                    .and_then(|company| company.cancel_pending_group_turn_messages(&session_id, id)).err())
            } else { None };
            let response = match result {
                Ok(Ok(report)) => {
                    WireResponse::Done(summarize(&report, saved_bytes_before, raw_bytes_before))
                }
                Ok(Err(error)) => WireResponse::Error {
                    message: format!("{error:#}"),
                },
                Err(join_error) if join_error.is_cancelled() => WireResponse::Error {
                    message: "turn stopped by user".to_string(),
                },
                Err(join_error) => WireResponse::Error {
                    message: format!("internal: {join_error:#}"),
                },
            };
            let response = if let Some(error) = iris_cancellation_error {
                WireResponse::Error {
                    message: format!(
                        "Work stopped, but its Iris Design state could not be finalized: {error:#}"
                    ),
                }
            } else if let Some(error) = cancellation_error {
                WireResponse::Error { message: format!("Work stopped, but its queued handoffs could not be retired: {error:#}") }
            } else { response };
            let response = confirm_direct_channel_response(channel_completion.as_ref(), response).await;
            if matches!(&response, WireResponse::Error { .. }) {
                // Aborting the runner drops its futures before they can publish
                // member terminal states. Settle only the queued/working members
                // observed in this execution, before the wire terminal receipt.
                // Forward through the postbox too so reopened conversations agree.
                for event in group_members.finish(cancelled) {
                    let journal_event = crate::runtime::postbox::forward_owned(
                        &session_id, execution_scope.as_ref(), event.clone());
                    let frames = if journal {
                        reducer.push(&event).into_iter()
                            .map(|row| owned_story_frame(row, &journal_event, false))
                            .collect::<Result<Vec<_>>>()?
                    } else {
                        vec![serde_json::to_value(WireResponse::Event(event))?]
                    };
                    for frame in frames {
                        if !client_gone && send(&mut write_half, &frame).await.is_err() {
                            client_gone = true;
                        }
                    }
                }
            }
            if target_group.is_some() {
                if let Some(client_turn_id) = turn_id.as_deref() {
                    let succeeded = matches!(&response, WireResponse::Done(summary) if summary.completion == TurnCompletion::Completed);
                    if let Err(error) = super::turn_queue::finalize_immediate_group(
                        &session_id,
                        client_turn_id,
                        succeeded,
                    ) {
                        glog(&format!(
                            "turn [{session_id}]: could not finalize idempotency receipt: {error:#}"
                        ));
                    }
                }
            }
            if let Some(event) = confirmed_channel_event(channel_completion.as_ref(), matches!(
                &response, WireResponse::Done(summary)
                    if matches!(summary.completion, TurnCompletion::Completed | TurnCompletion::Incomplete),
            )) {
                crate::notifications::publish_event(&session_id, &event);
                let entry = crate::runtime::postbox::forward_owned(&session_id, execution_scope.as_ref(), event.clone());
                let mut frames = if journal {
                    reducer.push(&event).into_iter().map(|row| owned_story_frame(row, &entry, false))
                        .collect::<Result<Vec<_>>>()?
                } else { Vec::new() };
                frames.push(serde_json::to_value(WireResponse::Event(event))?);
                for frame in frames {
                    if !client_gone && send(&mut write_half, &frame).await.is_err() { client_gone = true; }
                }
            }
            running_turns()
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&session_id, running_id);
            let secs = started.elapsed().as_secs_f32();
            match &response {
                WireResponse::Done(summary) => glog(&format!(
                    "turn done [{session_id}] in {secs:.1}s ({} tokens, ~{} tok compressed, route {})",
                    summary.total_tokens, summary.compression_saved_tokens, summary.route
                )),
                WireResponse::Error { message } => glog(&format!(
                    "turn FAILED [{session_id}] in {secs:.1}s: {message}"
                )),
                _ => {}
            }
            if !client_gone {
                let _ = send(&mut write_half, &response).await;
            }
            Ok(())
        }
    }
}

pub(crate) fn summarize(
    report: &super::TurnReport,
    saved_bytes_before: u64,
    raw_bytes_before: u64,
) -> TurnSummary {
    let saved_bytes =
        crate::tools::compress::saved_bytes_total().saturating_sub(saved_bytes_before);
    let raw_bytes = crate::tools::compress::raw_bytes_total().saturating_sub(raw_bytes_before);
    // The proof line the user can grep for: what entered, what was shaved,
    // the ratio — every turn, in the file they actually read.
    if raw_bytes > 0 {
        glog(&format!(
            "compression [{}]: {:.0}KB tool output in, ~{:.0}KB ({:.0}%) shaved before model context",
            report.execution.main_session_id,
            raw_bytes as f64 / 1024.0,
            saved_bytes as f64 / 1024.0,
            100.0 * saved_bytes as f64 / raw_bytes as f64,
        ));
    }
    // Context window usage: (used, limit). `used` is the PEAK single-call input
    // (max over the turn's rounds) — NOT `orchestrator_tokens.0`, which is the
    // SUM across rounds and reads as >100% of the window on any multi-round turn
    // (that was the "259% of 1M" bug — a burn number shown as window fullness).
    // `limit` is the model's context window from the provider catalog, falling
    // back to configured max_tokens. None if either piece is missing.
    let context_window = report.orchestrator_peak_input.and_then(|used| {
        let limit = report
            .trace_diagnostics
            .as_ref()
            .and_then(|d| {
                let provider = d.provider_id.as_deref()?;
                let model = d
                    .response_model
                    .as_deref()
                    .or(d.orchestrator_model.as_deref())?;
                crate::providers::providers_data::context_window_for(provider, model)
            })
            .or_else(|| {
                // Fallback: the configured max_tokens (if set) is a
                // reasonable proxy for the context limit.
                report
                    .trace_diagnostics
                    .as_ref()
                    .and_then(|d| d.configured_max_tokens.map(|m| m as u64))
            });
        limit
            .filter(|l| used <= *l as u32)
            .map(|l| (used, l as u32))
    });
    TurnSummary {
        completion: report.execution.outcome.completion.into(),
        final_markdown: report.execution.outcome.summary.clone(),
        main_session_id: report.execution.main_session_id.clone(),
        run_id: report.run_id.clone(),
        trace_path: report.trace_path.display().to_string(),
        route: report.route_description(),
        total_tokens: report.total_tokens(),
        orchestrator_tokens: report.orchestrator_tokens,
        coder_tokens: report.coder_tokens,
        compression_saved_tokens: saved_bytes / 4,
        compression_raw_tokens: raw_bytes / 4,
        context_window,
        background_work_pending: crate::runtime::postbox::has_background_work(
            &report.execution.main_session_id,
        ),
    }
}

fn owned_story_frame(
    row: crate::runtime::story::StoryEvent,
    entry: &crate::runtime::postbox::JournalEvent,
    replay: bool,
) -> Result<serde_json::Value> {
    let mut value = serde_json::to_value(row)?;
    if let Some(scope) = &entry.scope {
        value["execution"] = serde_json::to_value(scope)?;
        value["event_sequence"] = entry.sequence.into();
    }
    let key = if replay { "StoryReplay" } else { "Story" };
    Ok(serde_json::json!({key: value}))
}

#[test]
fn owned_story_wire_preserves_legacy_kind_and_execution_identity() {
    use crate::runtime::postbox::{ExecutionScope, JournalEvent};
    use crate::runtime::story::StoryEvent;
    let scope = ExecutionScope::new("turn-wire".into(), "task-wire".into());
    let entry = JournalEvent { scope: Some(scope.clone()), sequence: 42, event: CliEvent::Done };
    let frame = owned_story_frame(StoryEvent::Narration { agent: "coder".into(), text: "Visible update".into() }, &entry, false).unwrap();
    assert_eq!(frame["Story"]["kind"], "narration");
    assert_eq!(frame["Story"]["execution"]["attempt_id"], scope.attempt_id);
    assert_eq!(frame["Story"]["event_sequence"], 42);
    assert!(matches!(serde_json::from_value::<WireResponse>(frame).unwrap(), WireResponse::Story(StoryEvent::Narration { .. })));
    let frame = owned_story_frame(StoryEvent::Narration { agent: "coder".into(), text: "Visible update".into() }, &entry, true).unwrap();
    assert_eq!(frame["StoryReplay"]["execution"]["turn_id"], "turn-wire");
}

async fn send<T: serde::Serialize>(
    write_half: &mut tokio::net::unix::OwnedWriteHalf,
    response: &T,
) -> Result<()> {
    // This buffer can contain a one-time recovery key or a revealed password.
    // Keep it in a wiping wrapper for the complete socket write.
    let mut payload = Zeroizing::new(serde_json::to_string(response)?);
    payload.push('\n');
    write_half.write_all(payload.as_bytes()).await?;
    Ok(())
}

// ── Client ────────────────────────────────────────────────────────────

/// Outcome of a turn submitted over the gateway socket.
pub enum RemoteOutcome {
    Summary(TurnSummary),
    Error(String),
    /// The stream ended without Done/Error (daemon died mid-turn).
    Lost,
}

/// The longest a one-shot invocation will stay attached after its foreground
/// turn while detached same-session work finishes and is integrated. The
/// detached job remains owned by the gateway if this client-side bound fires.
pub const ONE_SHOT_BACKGROUND_SETTLEMENT_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(15 * 60);

/// Ordered state reconstructed from the pre-submit postbox subscription.
///
/// `running == 0` is not terminal: a returned result remains unintegrated
/// until `BackgroundResultsAbsorbed`, and even after that barrier the client
/// must see the integrating wake's own FinalOutput + Done. Clearing the
/// candidate final on every lifecycle transition closes the race where a
/// second result lands during the first wake's last provider call.
#[derive(Debug, Default)]
struct BackgroundSettlementTracker {
    running: usize,
    unabsorbed: usize,
    saw_absorption: bool,
    candidate_final: Option<String>,
}

impl BackgroundSettlementTracker {
    fn observe(&mut self, event: &CliEvent) -> Option<String> {
        match event {
            CliEvent::BackgroundAgentSpawned { .. } => {
                self.running = self.running.saturating_add(1);
                self.candidate_final = None;
            }
            CliEvent::BackgroundAgentReturned { .. } => {
                self.running = self.running.saturating_sub(1);
                self.unabsorbed = self.unabsorbed.saturating_add(1);
                self.candidate_final = None;
            }
            CliEvent::BackgroundResultsAbsorbed { count } => {
                self.unabsorbed = self.unabsorbed.saturating_sub(*count);
                self.saw_absorption = true;
                self.candidate_final = None;
            }
            CliEvent::FinalOutput(markdown) => {
                self.candidate_final = Some(markdown.clone());
            }
            CliEvent::Done if self.saw_absorption && self.running == 0 && self.unabsorbed == 0 => {
                return self.candidate_final.take();
            }
            _ => {}
        }
        None
    }
}

/// Filter a standing session subscription into the one terminal integration
/// stream a one-shot caller expects. Intermediate wake `Done` events are
/// withheld; all working events (including AskUser) continue to the caller.
/// A zero-background summary completes synchronously without waiting for the
/// subscription, preserving the fast path for ordinary one-shot turns.
pub fn spawn_background_settlement(
    mut events: tokio::sync::mpsc::Receiver<CliEvent>,
    mut summary: TurnSummary,
    timeout: std::time::Duration,
) -> (
    tokio::sync::mpsc::Receiver<CliEvent>,
    tokio::task::JoinHandle<RemoteOutcome>,
) {
    let (tx, rx) = tokio::sync::mpsc::channel::<CliEvent>(256);
    let handle = tokio::spawn(async move {
        if !summary.background_work_pending {
            return RemoteOutcome::Summary(summary);
        }

        let settlement = async {
            let mut tracker = BackgroundSettlementTracker::default();
            while let Some(event) = events.recv().await {
                let integrated_final = tracker.observe(&event);
                let terminal_done = integrated_final.is_some();

                // The absorbed barrier is lifecycle plumbing. Canvas/TUI and
                // the classic renderer intentionally receive no extra row.
                // Non-terminal Done belongs to an intermediate integration
                // cycle and must not close the one-shot renderer.
                if !matches!(&event, CliEvent::BackgroundResultsAbsorbed { .. })
                    && (!matches!(&event, CliEvent::Done) || terminal_done)
                {
                    let _ = tx.send(event).await;
                }

                if let Some(final_markdown) = integrated_final {
                    summary.final_markdown = final_markdown;
                    summary.background_work_pending = false;
                    return RemoteOutcome::Summary(summary);
                }
            }
            RemoteOutcome::Lost
        };

        match tokio::time::timeout(timeout, settlement).await {
            Ok(outcome) => outcome,
            Err(_) => RemoteOutcome::Error(format!(
                "background work did not reach an integrated terminal result within {}s; it remains owned by the gateway",
                timeout.as_secs()
            )),
        }
    });
    (rx, handle)
}

/// Connect and submit one turn. Returns the event receiver to render plus a
/// join handle resolving to the turn outcome. `Err` here means "could not
/// connect" — the gateway is not running.
/// Ask the gateway to stop the running turn for a session (the TUI's Esc).
/// Fire-and-forget from the caller's perspective: the turn connection itself
/// reports the stop.
pub async fn cancel_turn(session_id: String) -> Result<()> {
    let stream = UnixStream::connect(socket_path()).await?;
    let (read_half, mut write_half) = stream.into_split();
    let mut payload = serde_json::to_string(&WireRequest::Cancel {
        session_id,
        target_agent: None,
        owner: None,
    })?;
    payload.push('\n');
    write_half.write_all(payload.as_bytes()).await?;
    let mut lines = BufReader::new(read_half).lines();
    let _ = lines.next_line().await; // ack — content irrelevant
    Ok(())
}

/// Deliver the user's answer to a pending `ask_user` popup.
pub async fn answer_ask(ask_id: String, answer: String) -> Result<()> {
    let stream = UnixStream::connect(socket_path()).await?;
    let (read_half, mut write_half) = stream.into_split();
    let mut payload = serde_json::to_string(&WireRequest::AnswerAsk {
        ask_id,
        answer,
        session_id: None,
        owner: None,
    })?;
    payload.push('\n');
    write_half.write_all(payload.as_bytes()).await?;
    let mut lines = BufReader::new(read_half).lines();
    let _ = lines.next_line().await; // ack — content irrelevant
    Ok(())
}

/// Ask the gateway to digest a session into memory right now (`/quit` path).
/// Best-effort and fast: the daemon acks before running the digest, so the
/// quitting client never waits on a model call. No daemon = nothing to do
/// (the timer pass will cover it whenever the daemon is next up).
pub async fn request_session_digest(session_id: String) -> Result<()> {
    let stream = UnixStream::connect(socket_path()).await?;
    let (read_half, mut write_half) = stream.into_split();
    let mut payload = serde_json::to_string(&WireRequest::DigestSession {
        session_id,
        owner: None,
    })?;
    payload.push('\n');
    write_half.write_all(payload.as_bytes()).await?;
    let mut lines = BufReader::new(read_half).lines();
    let _ = lines.next_line().await; // ack — content irrelevant
    Ok(())
}

/// Ask the gateway to index the pending memory backlog right now (the
/// `/new`/`/clear` fresh-start path — no session digest attached). Best-effort
/// and fast: the daemon acks before indexing, so the client never waits on a
/// model call. No daemon = nothing to do (the startup catch-up covers the
/// backlog whenever the daemon is next up).
pub async fn request_memory_index() -> Result<()> {
    let stream = UnixStream::connect(socket_path()).await?;
    let (read_half, mut write_half) = stream.into_split();
    let mut payload = serde_json::to_string(&WireRequest::IndexMemory)?;
    payload.push('\n');
    write_half.write_all(payload.as_bytes()).await?;
    let mut lines = BufReader::new(read_half).lines();
    let _ = lines.next_line().await; // ack — content irrelevant
    Ok(())
}

/// Execute one local Settings vault operation in the long-lived gateway.
/// The serialized command and response line are wiped after use. Callers that
/// receive a secret-bearing `VaultReply` inherit its zeroizing Drop behavior.
pub async fn request_vault(command: VaultCommand) -> Result<VaultReply> {
    let stream = UnixStream::connect(socket_path()).await?;
    let (read_half, mut write_half) = stream.into_split();
    let mut payload = Zeroizing::new(serde_json::to_string(&WireRequest::Vault(command))?);
    payload.push('\n');
    write_half.write_all(payload.as_bytes()).await?;
    let line = BufReader::new(read_half)
        .lines()
        .next_line()
        .await?
        .context("gateway closed before returning the vault response")?;
    let line = Zeroizing::new(line);
    match serde_json::from_str::<WireResponse>(line.as_str())? {
        WireResponse::Vault(reply) => Ok(reply),
        WireResponse::Error { message } => anyhow::bail!(message),
        _ => anyhow::bail!("gateway returned an unexpected vault response"),
    }
}

/// Execute one non-secret onboarding operation in the long-lived gateway.
/// The desktop can reconnect after any step and recover the authoritative
/// snapshot instead of maintaining a fragile client-only wizard index.
pub async fn request_onboarding(
    command: crate::onboarding::OnboardingCommand,
) -> Result<crate::onboarding::OnboardingReply> {
    let stream = UnixStream::connect(socket_path()).await?;
    let (read_half, mut write_half) = stream.into_split();
    let mut payload = serde_json::to_string(&WireRequest::Onboarding(command))?;
    payload.push('\n');
    write_half.write_all(payload.as_bytes()).await?;
    let line = BufReader::new(read_half)
        .lines()
        .next_line()
        .await?
        .context("gateway closed before returning the onboarding response")?;
    match serde_json::from_str::<WireResponse>(&line)? {
        WireResponse::Onboarding(reply) => Ok(reply),
        WireResponse::Error { message } => anyhow::bail!(message),
        _ => anyhow::bail!("gateway returned an unexpected onboarding response"),
    }
}

/// Read or mutate the authoritative company directory through the daemon.
/// Every successful mutation returns a complete fresh view for reconnect-safe
/// desktop rendering.
pub async fn request_company_directory(
    command: crate::runtime::company_control::CompanyDirectoryCommand,
) -> Result<crate::runtime::company_control::CompanyDirectoryView> {
    let stream = UnixStream::connect(socket_path()).await?;
    let (read_half, mut write_half) = stream.into_split();
    let mut payload = serde_json::to_string(&WireRequest::CompanyDirectory(command))?;
    payload.push('\n');
    write_half.write_all(payload.as_bytes()).await?;
    let line = BufReader::new(read_half)
        .lines()
        .next_line()
        .await?
        .context("gateway closed before returning the company directory response")?;
    match serde_json::from_str::<WireResponse>(&line)? {
        WireResponse::CompanyDirectory(view) => Ok(view),
        WireResponse::Error { message } => anyhow::bail!(message),
        _ => anyhow::bail!("gateway returned an unexpected company directory response"),
    }
}

/// Execute one embedded-browser teaching operation. The request buffer is
/// zeroized because an in-flight Type action may contain a password even
/// though the durable workflow recorder replaces it with a vault placeholder.
pub async fn request_teach_workflow(
    command: crate::runtime::workflow_teaching::TeachWorkflowCommand,
) -> Result<crate::runtime::workflow_teaching::TeachWorkflowReply> {
    let stream = UnixStream::connect(socket_path()).await?;
    let (read_half, mut write_half) = stream.into_split();
    let mut payload = Zeroizing::new(serde_json::to_string(&WireRequest::TeachWorkflow(command))?);
    payload.push('\n');
    write_half.write_all(payload.as_bytes()).await?;
    let line = BufReader::new(read_half)
        .lines()
        .next_line()
        .await?
        .context("gateway closed before returning the taught workflow response")?;
    let line = Zeroizing::new(line);
    match serde_json::from_str::<WireResponse>(line.as_str())? {
        WireResponse::TeachWorkflow(reply) => Ok(reply),
        WireResponse::Error { message } => anyhow::bail!(message),
        _ => anyhow::bail!("gateway returned an unexpected taught workflow response"),
    }
}

/// Read or mutate typed product settings through the long-lived gateway.
/// Every successful mutation returns a fresh authoritative snapshot, so the
/// desktop can save instantly without maintaining a second local truth.
pub async fn request_settings(
    command: crate::settings::SettingsCommand,
) -> Result<crate::settings::SettingsReply> {
    let stream = UnixStream::connect(socket_path()).await?;
    let (read_half, mut write_half) = stream.into_split();
    let mut payload = serde_json::to_string(&WireRequest::Settings(command))?;
    payload.push('\n');
    write_half.write_all(payload.as_bytes()).await?;
    let line = BufReader::new(read_half)
        .lines()
        .next_line()
        .await?
        .context("gateway closed before returning the settings response")?;
    match serde_json::from_str::<WireResponse>(&line)? {
        WireResponse::Settings(reply) => Ok(reply),
        WireResponse::Error { message } => anyhow::bail!(message),
        _ => anyhow::bail!("gateway returned an unexpected settings response"),
    }
}

fn drain_finished_turn_event(events: &mut tokio::sync::mpsc::Receiver<CliEvent>) -> Option<CliEvent> {
    // Only called once the producer is finished. Sender clones may remain
    // alive; waiting for channel closure here would hang forever.
    events.try_recv().ok()
}

#[derive(Default)]
struct UnfinishedGroupMembers {
    members: std::collections::BTreeMap<(String, String, String), CliEvent>,
}

impl UnfinishedGroupMembers {
    fn observe(&mut self, event: &CliEvent) {
        if let CliEvent::GroupMemberStatus { turn_id, group_id, agent_id, state, .. } = event {
            let key = (turn_id.clone(), group_id.clone(), agent_id.clone());
            if matches!(state.as_str(), "queued" | "working") {
                self.members.insert(key, event.clone());
            } else {
                self.members.remove(&key);
            }
        }
    }

    fn finish(self, cancelled: bool) -> Vec<CliEvent> {
        self.members.into_values().map(|mut event| {
            if let CliEvent::GroupMemberStatus { state, detail, .. } = &mut event {
                *state = if cancelled { "cancelled" } else { "failed" }.into();
                *detail = if cancelled { "Run stopped before this task finished" }
                    else { "Run ended before this task finished" }.into();
            }
            event
        }).collect()
    }
}

#[test]
fn group_terminal_cleanup_preserves_finished_and_waiting_members() {
    for cancelled in [true, false] {
        let mut tracker = UnfinishedGroupMembers::default();
        let event = |agent: &str, state: &str| CliEvent::GroupMemberStatus {
            turn_id: "turn-a".into(), group_id: "room".into(), agent_id: agent.into(),
            agent_name: format!("Name {agent}"), state: state.into(), detail: "original".into(),
        };
        for (agent, state) in [("reviewer", "queued"), ("builder", "working"),
            ("inspector", "working"), ("inspector", "done"),
            ("question", "working"), ("question", "waiting_user"), ("failed", "failed")] {
            tracker.observe(&event(agent, state));
        }
        tracker.observe(&CliEvent::Thinking);
        let terminal = tracker.finish(cancelled);
        assert_eq!(terminal.len(), 2);
        for event in terminal {
            let CliEvent::GroupMemberStatus { turn_id, group_id, agent_id, agent_name, state, detail } = event else { panic!("member event expected") };
            assert!(matches!(agent_id.as_str(), "builder" | "reviewer"));
            assert_eq!(turn_id, "turn-a"); assert_eq!(group_id, "room");
            assert_eq!(agent_name, format!("Name {agent_id}"));
            assert_eq!(state, if cancelled { "cancelled" } else { "failed" });
            assert!(detail.contains("before this task finished"));
        }
    }
}

#[tokio::test]
async fn group_terminal_cleanup_reaches_live_and_reopened_journals() {
    use crate::runtime::postbox::{self, ExecutionScope};
    let sid = format!("group-terminal-test-{}", uuid::Uuid::new_v4());
    let scope = ExecutionScope::new("turn-cancelled".into(), "task-cancelled".into());
    postbox::begin_foreground_replay(&sid);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    assert!(postbox::subscribe_journal(&sid, tx).is_empty());
    let mut tracker = UnfinishedGroupMembers::default();
    tracker.observe(&CliEvent::GroupMemberStatus {
        turn_id: scope.turn_id.clone(), group_id: "room".into(), agent_id: "coder".into(),
        agent_name: "Leo".into(), state: "working".into(), detail: "Editing".into(),
    });
    for event in tracker.finish(true) { postbox::forward_owned(&sid, Some(&scope), event); }
    let live = rx.try_recv().unwrap();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let replay = postbox::subscribe_journal(&sid, tx);
    assert_eq!(replay.len(), 1);
    for entry in [&live, &replay[0]] {
        assert_eq!(entry.scope.as_ref(), Some(&scope));
        let mut reducer = crate::runtime::story::StoryReducer::default();
        let rows = reducer.push(&entry.event);
        assert!(matches!(rows.as_slice(), [crate::runtime::story::StoryEvent::GroupMemberStatus { state, agent_id, .. }]
            if state == "cancelled" && agent_id == "coder"));
    }
}

#[tokio::test]
async fn finished_turn_drain_preserves_buffered_reply_despite_retained_sender() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let retained_sender = tx.clone();
    let producer = tokio::spawn(async move {
        tx.send(CliEvent::FinalOutput("saved reply".into())).await.unwrap();
        tx.send(CliEvent::Done).await.unwrap();
    });
    producer.await.unwrap();
    assert!(matches!(drain_finished_turn_event(&mut rx), Some(CliEvent::FinalOutput(body)) if body == "saved reply"));
    assert!(matches!(drain_finished_turn_event(&mut rx), Some(CliEvent::Done)));
    assert!(drain_finished_turn_event(&mut rx).is_none());
    assert!(!retained_sender.is_closed());
}

/// Open the long-lived background-event stream for a session. Returns a
/// receiver of `CliEvent`s (spawns, returns, background working steps) that
/// stays live across turns; it closes when the gateway goes away. The caller
/// re-subscribes when switching sessions.
pub async fn subscribe_events(session_id: String) -> Result<tokio::sync::mpsc::Receiver<CliEvent>> {
    let stream = UnixStream::connect(socket_path()).await?;
    let (read_half, mut write_half) = stream.into_split();
    let mut payload = serde_json::to_string(&WireRequest::Subscribe {
        session_id,
        owner: None,
    })?;
    payload.push('\n');
    write_half.write_all(payload.as_bytes()).await?;
    let mut lines = BufReader::new(read_half).lines();
    let acknowledgement = tokio::time::timeout(Duration::from_secs(3), lines.next_line())
        .await
        .map_err(|_| anyhow::anyhow!("gateway timed out acknowledging the event subscription"))??;
    match acknowledgement {
        Some(line) => match serde_json::from_str::<WireResponse>(&line) {
            Ok(WireResponse::Pong) => {}
            Ok(WireResponse::Error { message }) => anyhow::bail!(message),
            Ok(_) => anyhow::bail!("gateway returned an unexpected subscription acknowledgement"),
            Err(error) => anyhow::bail!("invalid subscription acknowledgement: {error}"),
        },
        None => anyhow::bail!("gateway closed before acknowledging the event subscription"),
    }
    let (tx, rx) = tokio::sync::mpsc::channel::<CliEvent>(256);
    tokio::spawn(async move {
        let _write_half = write_half; // keep our end open for the stream's life
        loop {
            let line = tokio::select! {
                _ = tx.closed() => break,
                line = lines.next_line() => line,
            };
            let Ok(Some(line)) = line else {
                break;
            };
            if let Ok(WireResponse::Event(event)) = serde_json::from_str::<WireResponse>(&line) {
                if tx.send(event).await.is_err() {
                    break;
                }
            }
        }
    });
    Ok(rx)
}

pub async fn submit_turn(
    session_id: String,
    user_request: String,
    yolo: bool,
) -> Result<(
    tokio::sync::mpsc::Receiver<CliEvent>,
    tokio::task::JoinHandle<RemoteOutcome>,
)> {
    let stream = UnixStream::connect(socket_path()).await?;
    let (read_half, mut write_half) = stream.into_split();
    // A resumed specialist session is still owned by that specialist.  The
    // desktop supplies this field explicitly, but the CLI historically left
    // it empty, which made the gateway enter the same canonical transcript as
    // Phoenix and then delegate the work back to its actual owner.  Besides
    // showing the wrong identity, that created an unnecessary foreground turn
    // and a duplicate background handoff.  Infer the route from the durable
    // session kind for every built-in and custom coworker.
    let target_agent = target_agent_for_session_id(&session_id)?;
    let request = WireRequest::Turn {
        session_id,
        turn_id: Some(format!("turn_{}", uuid::Uuid::new_v4().simple())),
        user_request,
        interaction_mode: crate::runtime::InteractionMode::Execute,
        permission_mode: None,
        yolo: Some(yolo),
        workspace: std::env::current_dir().ok(),
        journal: false, // the TUI renders the raw lane itself
        target_agent,
        target_group: None,
        group_activation: None,
        delivery: TurnDelivery::Queue,
        sticky_notes: None,
        viewport: None,
        attachments: None,
    };
    let mut payload = serde_json::to_string(&request)?;
    payload.push('\n');
    write_half.write_all(payload.as_bytes()).await?;

    let (tx, rx) = tokio::sync::mpsc::channel::<CliEvent>(256);
    let reader = tokio::spawn(async move {
        let _write_half = write_half; // keep our end open for the whole turn
        let mut lines = BufReader::new(read_half).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            match serde_json::from_str::<WireResponse>(&line) {
                Ok(WireResponse::Event(event)) => {
                    let done = matches!(event, CliEvent::Done);
                    if tx.send(event).await.is_err() {
                        break;
                    }
                    if done {
                        continue; // Done event rendered; summary line still coming
                    }
                }
                Ok(WireResponse::Done(summary)) => return RemoteOutcome::Summary(summary),
                Ok(WireResponse::Error { message }) => return RemoteOutcome::Error(message),
                // Story rows / browser frames only flow on their own
                // subscription connections, never on a Turn connection —
                // tolerated like any stray frame.
                Ok(WireResponse::ProtocolInfo { .. })
                | Ok(WireResponse::Pong)
                | Ok(WireResponse::Story(_))
                | Ok(WireResponse::StoryReplay(_))
                | Ok(WireResponse::BrowserFrame(_))
                | Ok(WireResponse::CompanySnapshot(_))
                | Ok(WireResponse::Vault(_))
                | Ok(WireResponse::Onboarding(_))
                | Ok(WireResponse::CompanyDirectory(_))
                | Ok(WireResponse::TeachWorkflow(_))
                | Ok(WireResponse::Settings(_))
                | Ok(WireResponse::GroupActivationPreview(_))
                | Ok(WireResponse::QueuedTurns(_))
                | Ok(WireResponse::TodoList(_))
                | Ok(WireResponse::ConversationAsks(_))
                | Ok(WireResponse::AskAnswered { .. })
                | Ok(WireResponse::TranscriptDeleted { .. })
                | Ok(WireResponse::BrowserInteraction(_))
                | Ok(WireResponse::BrowserSurface(_))
                | Ok(WireResponse::DesktopWorkspaces(_))
                | Ok(WireResponse::DesktopObservation(_))
                | Err(_) => {}
            }
        }
        RemoteOutcome::Lost
    });
    Ok((rx, reader))
}

fn target_agent_for_session_kind(kind: &crate::session::SessionKind) -> Option<String> {
    match kind {
        crate::session::SessionKind::Main => None,
        crate::session::SessionKind::SubAgent(agent) => {
            Some(crate::runtime::delegation::specialist_label(*agent).to_string())
        }
    }
}

fn target_agent_for_session_id(session_id: &str) -> Result<Option<String>> {
    Ok(crate::session::SessionStore::read_one_from_disk(
        &crate::config::phoenix_home().join("sessions"),
        session_id,
    )?
    .and_then(|session| target_agent_for_session_kind(&session.kind)))
}

#[cfg(test)]
mod channel_confirmation_tests {
    use super::*;
    use crate::cli::channel_receipts::{self as receipts, Key};
    use crate::runtime::postbox::{ExecutionScope, JournalEvent};
    use crate::runtime::story::{OwnedStoryReducer, StoryEvent};

    fn summary(completion: TurnCompletion, markdown: &str) -> WireResponse {
        WireResponse::Done(TurnSummary {
            completion, final_markdown: markdown.into(), main_session_id: "terminal-test".into(),
            run_id: "existing-run".into(), trace_path: String::new(), route: "test".into(),
            total_tokens: 0, orchestrator_tokens: None, coder_tokens: None,
            compression_saved_tokens: 0, compression_raw_tokens: 0,
            context_window: None, background_work_pending: false,
        })
    }

    fn key(home: &Path) -> Key {
        Key::new("terminal-test", Some(&format!("channel_{}", "e".repeat(64))),
            Some("avery"), Some(home), "Keep the exact original request").unwrap()
    }

    fn target(home: &Path) -> PathBuf {
        home.join("channels/completions").join(format!("{}.json", crate::channels::digest("terminal-test")))
    }

    fn preview(reducer: &mut OwnedStoryReducer, key: &Key, scope: &ExecutionScope) -> Vec<StoryEvent> {
        let mut rows = Vec::new();
        for (index, event) in [CliEvent::FinalOutput("Candidate".into()), CliEvent::Done].into_iter().enumerate() {
            if !defer_channel_done(Some(key), &event) {
                rows.extend(reducer.push(&JournalEvent { scope: Some(scope.clone()), sequence: index as u64 + 1, event }));
            }
        }
        assert!(matches!(&rows[..], [StoryEvent::Answer { .. }]), "no early terminal may escape before persistence");
        rows
    }

    fn settle(reducer: &mut OwnedStoryReducer, key: &Key, scope: ExecutionScope, response: &WireResponse) -> Vec<StoryEvent> {
        let confirmed = matches!(response, WireResponse::Done(summary)
            if matches!(summary.completion, TurnCompletion::Completed | TurnCompletion::Incomplete));
        reducer.push(&JournalEvent { scope: Some(scope), sequence: 3,
            event: confirmed_channel_event(Some(key), confirmed).unwrap() })
    }

    #[tokio::test]
    async fn successful_direct_confirmation_preserves_completed_and_incomplete_outcomes() {
        for completion in [TurnCompletion::Completed, TurnCompletion::Incomplete] {
            let home = tempfile::tempdir().unwrap();
            let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
            let key = key(home.path());
            let scope = ExecutionScope::new("turn".into(), "turn".into());
            let mut reducer = OwnedStoryReducer::default();
            preview(&mut reducer, &key, &scope);
            assert!(!receipts::terminal_exists(&key).unwrap());
            let response = confirm_direct_channel_response(Some(&key), summary(completion, "Exact final")).await;
            assert!(matches!(&response, WireResponse::Done(value) if value.completion == completion));
            assert_eq!(receipts::find(&key).unwrap().as_deref(), Some("Exact final"));
            assert!(matches!(&settle(&mut reducer, &key, scope, &response)[..], [StoryEvent::ExecutionEnded { error: None }]));
        }
    }

    #[tokio::test]
    async fn failed_direct_confirmation_never_publishes_a_clean_terminal_or_replaces_a_receipt() {
        for mode in ["precommit", "conflict", "postcommit-cleanup"] {
            let home = tempfile::tempdir().unwrap();
            let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
            let key = key(home.path());
            let scope = ExecutionScope::new("turn".into(), "turn".into());
            let mut reducer = OwnedStoryReducer::default();
            preview(&mut reducer, &key, &scope);
            let target = target(home.path());
            let old = match mode {
                "precommit" => { std::fs::create_dir_all(&target).unwrap(); None },
                "conflict" => { receipts::save(&key, "Previously committed final").unwrap(); Some(std::fs::read(&target).unwrap()) },
                _ => {
                    // A directory with an obsolete image filename is not a
                    // removable private file. Actual cleanup fails AFTER JSON
                    // commit; no instruction or return value is intercepted.
                    std::fs::create_dir_all(target.with_extension("").join("images/image-obsolete.png")).unwrap();
                    None
                },
            };
            let response = confirm_direct_channel_response(Some(&key), summary(TurnCompletion::Completed, "Candidate")).await;
            assert!(matches!(&response, WireResponse::Error { message } if message.contains("work may already have taken effect")), "{mode}");
            let rows = settle(&mut reducer, &key, scope, &response);
            assert!(matches!(&rows[..], [StoryEvent::Failure { .. }, StoryEvent::ExecutionEnded { error: Some(_) }]), "{mode}");
            assert!(serde_json::to_value(&response).unwrap().get("Done").is_none(), "{mode}");
            if let Some(old) = old { assert_eq!(std::fs::read(&target).unwrap(), old); }
            if mode == "postcommit-cleanup" {
                assert!(receipts::terminal_exists(&key).unwrap(), "an error must not be interpreted as an absent receipt");
                assert_eq!(receipts::find(&key).unwrap().as_deref(), Some("Candidate"));
            }
        }
    }

    #[tokio::test]
    async fn runner_failure_and_nonterminal_outcomes_cannot_confirm_a_preview() {
        let home = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let key = key(home.path());
        for response in [WireResponse::Error { message: "trace write failed after output".into() },
            summary(TurnCompletion::Canceled, "Candidate"), summary(TurnCompletion::Queued, "Accepted only")] {
            let scope = ExecutionScope::new("turn".into(), "turn".into());
            let mut reducer = OwnedStoryReducer::default();
            preview(&mut reducer, &key, &scope);
            let response = confirm_direct_channel_response(Some(&key), response).await;
            assert!(!receipts::terminal_exists(&key).unwrap());
            let rows = settle(&mut reducer, &key, scope, &response);
            assert!(matches!(rows.last(), Some(StoryEvent::ExecutionEnded { error: Some(_) })));
        }
        assert!(!defer_channel_done(None, &CliEvent::Done));
        assert!(confirmed_channel_event(None, true).is_none());
        assert!(confirmed_channel_event(None, false).is_none());
        assert!(!defer_channel_done(Some(&key), &CliEvent::FinalOutput("Candidate".into())));
        assert!(matches!(confirm_direct_channel_response(None, summary(TurnCompletion::Completed, "Ordinary result")).await,
            WireResponse::Done(_)), "ordinary non-channel streams retain their prior boundary");
    }
}

#[cfg(test)]
mod queued_replay_tests {
    use super::{finish_queued_turn, queued_channel_completion, start_queued_turn};
    use crate::cli::{channel_receipts as receipts, turn_queue as queue};
    use crate::runtime::OutcomeCompletion;
    use rusqlite::{params, Connection, OptionalExtension};
    use sha2::{Digest, Sha256};
    use std::cell::Cell;
    use std::path::{Path, PathBuf};

    fn payload(workspace: &Path, turn: &str) -> queue::QueuedUserTurn {
        serde_json::from_value(serde_json::json!({
            "turn_id":turn, "user_request":"Use the exact saved inputs; keep the user's 0.1 mm setting",
            "workspace":workspace, "target_agent":"avery", "permission_mode":"workspace",
            "interaction_mode":"execute"
        })).unwrap()
    }

    fn receipt_path(home: &Path, session: &str) -> PathBuf {
        home.join("channels/completions").join(format!("{}.json", crate::channels::digest(session)))
    }

    fn database(home: &Path) -> Connection {
        Connection::open(home.join("company/turn_queue.sqlite")).unwrap()
    }

    fn row(home: &Path, queue_id: &str) -> (String, i64, String) {
        database(home).query_row(
            "SELECT state,attempts,payload_json FROM queued_user_turns WHERE queue_id=?1", [queue_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).unwrap()
    }

    fn submission(home: &Path, session: &str, turn: &str) -> (String, String) {
        database(home).query_row(
            "SELECT queue_id,payload_hash FROM queued_turn_receipts WHERE session_id=?1 AND turn_id=?2",
            params![session, turn], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap()
    }

    fn frozen(home: &Path, session: &str) -> Option<String> {
        database(home).query_row(
            "SELECT payload_json FROM frozen_answer_turns WHERE session_id=?1", [session], |r| r.get(0),
        ).optional().unwrap()
    }

    #[tokio::test]
    async fn saved_terminal_prevents_new_execution_and_preserves_exact_records() {
        for completion in [OutcomeCompletion::Completed, OutcomeCompletion::Incomplete] {
            for prefix in ["channel", "ask_answer"] {
                let home = tempfile::tempdir().unwrap();
                let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
                let gate = std::sync::Arc::new(crate::cli::session_gate::SessionGate::default());
                let _permit = gate.lock().await;
                let session = "queued-replay";
                let turn = format!("{prefix}_{}", crate::channels::digest(&format!("{completion:?}")));
                let mut queued = payload(home.path(), &turn);
                if prefix == "ask_answer" {
                    queued.origin = Some(crate::runtime::TurnOrigin::AskAnswer {
                        ask_id:"saved-ask".into(), agent_id:Some("avery".into()), display:"0.1 mm".into(),
                    });
                    queued = queue::frozen_answer_turn(session, "saved-ask", "0.1 mm", || Ok(queued.clone())).unwrap();
                }
                let queue_id = queue::enqueue(session, &queued).unwrap();
                let first = queue::claim_next(session).unwrap().unwrap();
                let key = queued_channel_completion(session, &first.payload, first.payload.workspace.as_deref()).unwrap();
                let calls = Cell::new(0);
                assert!(start_queued_turn(&queue_id, first.attempts, Some(&key), || calls.set(calls.get() + 1)).unwrap().is_some());
                assert_eq!(calls.get(), 1);
                let original_payload = row(home.path(), &queue_id).2;
                let original_submission = submission(home.path(), session, &turn);
                let original_frozen = frozen(home.path(), session);
                image::RgbImage::new(8, 8).save(home.path().join("result.png")).unwrap();
                let original_image = std::fs::read(home.path().join("result.png")).unwrap();
                let markdown = format!("{completion:?} result. ![Result](result.png)");

                // Real receipt I/O succeeds, then the real queue DELETE fails.
                // This leaves the same durable boundary as death before retirement.
                database(home.path()).execute_batch("CREATE TRIGGER hold_queue_retirement BEFORE DELETE ON queued_user_turns
                    BEGIN SELECT RAISE(ABORT,'retirement unavailable'); END;").unwrap();
                let error = finish_queued_turn(&queue_id, Some(&key), completion, &markdown).await.unwrap_err();
                assert!(error.to_string().contains("could not settle"));
                assert_eq!(row(home.path(), &queue_id), ("running".into(), 1, original_payload.clone()));
                let target = receipt_path(home.path(), session);
                let receipt_bytes = std::fs::read(&target).unwrap();
                let value: serde_json::Value = serde_json::from_slice(&receipt_bytes).unwrap();
                let image = value["receipts"][0]["output"].as_array().unwrap().iter()
                    .find(|item| item["kind"] == "image").unwrap();
                let image_path = PathBuf::from(image["path"].as_str().unwrap());
                let frozen_image = std::fs::read(&image_path).unwrap();
                assert_eq!(frozen_image, original_image);
                std::fs::write(home.path().join("result.png"), b"source replaced after execution").unwrap();
                assert_eq!(queue::recover_interrupted().unwrap(), 1);
                let replay = queue::claim_next(session).unwrap().unwrap();
                assert_eq!(replay.queue_id, queue_id);
                assert_eq!(replay.attempts, 2);
                assert_eq!(row(home.path(), &queue_id).2, original_payload);
                let replay_key = queued_channel_completion(session, &replay.payload, replay.payload.workspace.as_deref()).unwrap();
                assert_eq!(replay_key, key);

                // Failed retirement during the guard also never invokes execution.
                assert!(start_queued_turn(&queue_id, replay.attempts, Some(&replay_key), || calls.set(calls.get() + 1)).is_err());
                assert_eq!(calls.get(), 1);
                assert_eq!(row(home.path(), &queue_id).0, "running");
                database(home.path()).execute_batch("DROP TRIGGER hold_queue_retirement;").unwrap();
                assert!(start_queued_turn(&queue_id, replay.attempts, Some(&replay_key), || calls.set(calls.get() + 1)).unwrap().is_none());
                assert_eq!(calls.get(), 1, "zero new execution invocations for a saved terminal reply");
                assert!(queue::list(session).unwrap().is_empty());
                assert_eq!(queue::retired_turn_receipt(session, &turn).unwrap(), Some(queue_id.clone()));
                assert_eq!(queue::enqueue(session, &queued).unwrap(), queue_id);
                assert!(queue::claim_next(session).unwrap().is_none());
                assert_eq!(submission(home.path(), session, &turn), original_submission);
                assert_eq!(frozen(home.path(), session), original_frozen);
                assert_eq!(std::fs::read(&target).unwrap(), receipt_bytes);
                assert_eq!(Sha256::digest(std::fs::read(&target).unwrap()), Sha256::digest(&receipt_bytes));
                assert_eq!(std::fs::read(&image_path).unwrap(), frozen_image);
                assert_eq!(Sha256::digest(std::fs::read(&image_path).unwrap()), Sha256::digest(&original_image));
                assert_eq!(receipts::find(&key).unwrap().as_deref(), Some(markdown.as_str()));
            }
        }
    }

    #[tokio::test]
    async fn fresh_miss_runs_once_but_interrupted_miss_requires_review() {
        let home = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let gate = std::sync::Arc::new(crate::cli::session_gate::SessionGate::default());
        let _permit = gate.lock().await;
        let session = "queued-miss";
        let turn = format!("channel_{}", "a".repeat(64));
        let queued = payload(home.path(), &turn);
        let id = queue::enqueue(session, &queued).unwrap();
        let first = queue::claim_next(session).unwrap().unwrap();
        let key = queued_channel_completion(session, &first.payload, first.payload.workspace.as_deref()).unwrap();
        let original = row(home.path(), &id).2;
        let proof = submission(home.path(), session, &turn);
        let calls = Cell::new(0);
        assert!(start_queued_turn(&id, first.attempts, Some(&key), || calls.set(calls.get() + 1)).unwrap().is_some());
        assert_eq!(queue::recover_interrupted().unwrap(), 1);
        let replay = queue::claim_next(session).unwrap().unwrap();
        let error = start_queued_turn(&id, replay.attempts, Some(&key), || calls.set(calls.get() + 1)).unwrap_err();
        assert!(error.to_string().contains("no retained terminal receipt"));
        assert_eq!(calls.get(), 1);
        assert_eq!(row(home.path(), &id), ("failed".into(), 2, original));
        assert_eq!(submission(home.path(), session, &turn), proof);
        assert!(queue::list(session).unwrap()[0].failure.as_deref().unwrap().contains("Review"));
        assert_eq!(queue::recover_interrupted().unwrap(), 0);
        assert!(queue::claim_next(session).unwrap().is_none());
        assert!(!receipts::terminal_exists(&key).unwrap());
    }

    #[tokio::test]
    async fn corrupt_or_mismatched_receipt_fails_closed_even_on_first_claim() {
        for attempts in [1, 2] {
            for corrupt in [true, false] {
                let home = tempfile::tempdir().unwrap();
                let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
                let gate = std::sync::Arc::new(crate::cli::session_gate::SessionGate::default());
                let _permit = gate.lock().await;
                let session = "queued-invalid";
                let turn = format!("channel_{}", "b".repeat(64));
                let queued = payload(home.path(), &turn);
                let id = queue::enqueue(session, &queued).unwrap();
                let mut claimed = queue::claim_next(session).unwrap().unwrap();
                if attempts == 2 {
                    queue::recover_interrupted().unwrap();
                    claimed = queue::claim_next(session).unwrap().unwrap();
                }
                let key = queued_channel_completion(session, &claimed.payload, claimed.payload.workspace.as_deref()).unwrap();
                receipts::save(&key, "Existing terminal").unwrap();
                let target = receipt_path(home.path(), session);
                let bytes = if corrupt { b"{incomplete json".to_vec() } else {
                    let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&target).unwrap()).unwrap();
                    value["receipts"][0]["key"]["request"] = serde_json::json!(crate::channels::digest("another request"));
                    serde_json::to_vec(&value).unwrap()
                };
                crate::config::private_io::atomic_write_private(&target, &bytes).unwrap();
                let calls = Cell::new(0);
                assert!(start_queued_turn(&id, claimed.attempts, Some(&key), || calls.set(calls.get() + 1)).is_err());
                assert_eq!(calls.get(), 0);
                assert_eq!(row(home.path(), &id).0, "failed");
                assert_eq!(std::fs::read(&target).unwrap(), bytes);
                assert_eq!(queue::enqueue(session, &queued).unwrap(), id);
                assert!(queue::claim_next(session).unwrap().is_none());
            }
        }
    }

    #[tokio::test]
    async fn final_save_io_error_or_conflict_retains_failed_queue() {
        for conflict in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
            let gate = std::sync::Arc::new(crate::cli::session_gate::SessionGate::default());
            let _permit = gate.lock().await;
            let session = "queued-save-error";
            let turn = format!("ask_answer_{}", "c".repeat(64));
            let queued = payload(home.path(), &turn);
            let id = queue::enqueue(session, &queued).unwrap();
            let claimed = queue::claim_next(session).unwrap().unwrap();
            let key = queued_channel_completion(session, &claimed.payload, claimed.payload.workspace.as_deref()).unwrap();
            let original = row(home.path(), &id).2;
            let proof = submission(home.path(), session, &turn);
            let calls = Cell::new(0);
            assert!(start_queued_turn(&id, claimed.attempts, Some(&key), || calls.set(calls.get() + 1)).unwrap().is_some());
            let target = receipt_path(home.path(), session);
            let previous = if conflict {
                receipts::save(&key, "First committed final").unwrap();
                Some(std::fs::read(&target).unwrap())
            } else {
                std::fs::create_dir(&target).unwrap();
                None
            };
            let error = finish_queued_turn(&id, Some(&key), OutcomeCompletion::Completed, "Different final").await.unwrap_err();
            assert!(error.to_string().contains("terminal reply could not be confirmed"));
            assert_eq!(calls.get(), 1);
            assert_eq!(row(home.path(), &id), ("failed".into(), 1, original));
            assert_eq!(submission(home.path(), session, &turn), proof);
            assert_eq!(queue::recover_interrupted().unwrap(), 0);
            assert!(queue::claim_next(session).unwrap().is_none());
            if let Some(bytes) = previous { assert_eq!(std::fs::read(&target).unwrap(), bytes); }
            else { assert!(target.is_dir()); }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn post_commit_image_cleanup_error_preserves_final_and_reviewable_queue() {
        let home = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let gate = std::sync::Arc::new(crate::cli::session_gate::SessionGate::default());
        let _permit = gate.lock().await;
        let session = "queued-cleanup-error";
        let turn = format!("channel_{}", "d".repeat(64));
        let queued = payload(home.path(), &turn);
        let id = queue::enqueue(session, &queued).unwrap();
        let claimed = queue::claim_next(session).unwrap().unwrap();
        let key = queued_channel_completion(session, &claimed.payload, claimed.payload.workspace.as_deref()).unwrap();
        let images = receipt_path(home.path(), session).with_extension("").join("images");
        std::fs::create_dir_all(&images).unwrap();
        let sentinel = home.path().join("retained.bin");
        std::fs::write(&sentinel, b"retain these bytes").unwrap();
        std::os::unix::fs::symlink(&sentinel, images.join("image-stale.png")).unwrap();
        assert!(finish_queued_turn(&id, Some(&key), OutcomeCompletion::Incomplete, "Work remains incomplete").await.is_err());
        assert_eq!(row(home.path(), &id).0, "failed");
        assert!(receipts::terminal_exists(&key).unwrap());
        assert_eq!(receipts::find(&key).unwrap().as_deref(), Some("Work remains incomplete"));
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"retain these bytes");
        assert_eq!(queue::recover_interrupted().unwrap(), 0);
    }

    #[tokio::test]
    async fn review_write_failure_never_releases_execution() {
        let home = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let gate = std::sync::Arc::new(crate::cli::session_gate::SessionGate::default());
        let _permit = gate.lock().await;
        let session = "queued-review-error";
        let queued = payload(home.path(), &format!("channel_{}", "e".repeat(64)));
        let id = queue::enqueue(session, &queued).unwrap();
        queue::claim_next(session).unwrap().unwrap();
        queue::recover_interrupted().unwrap();
        let claimed = queue::claim_next(session).unwrap().unwrap();
        let key = queued_channel_completion(session, &claimed.payload, claimed.payload.workspace.as_deref()).unwrap();
        let before = row(home.path(), &id);
        database(home.path()).execute_batch("CREATE TRIGGER hold_queue_review BEFORE UPDATE OF state ON queued_user_turns
            WHEN NEW.state='failed' BEGIN SELECT RAISE(ABORT,'review unavailable'); END;").unwrap();
        let calls = Cell::new(0);
        let error = start_queued_turn(&id, claimed.attempts, Some(&key), || calls.set(calls.get() + 1)).unwrap_err();
        assert!(error.to_string().contains("review state could not be saved"));
        assert_eq!(calls.get(), 0);
        assert_eq!(row(home.path(), &id), before);
        database(home.path()).execute_batch("DROP TRIGGER hold_queue_review;").unwrap();
        assert!(start_queued_turn(&id, claimed.attempts, Some(&key), || calls.set(calls.get() + 1)).is_err());
        assert_eq!(calls.get(), 0);
        assert_eq!(row(home.path(), &id).0, "failed");
    }

    #[tokio::test]
    async fn group_and_nonreceipt_claims_keep_existing_execution_semantics() {
        for group in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
            let gate = std::sync::Arc::new(crate::cli::session_gate::SessionGate::default());
            let _permit = gate.lock().await;
            let session = "queued-unaffected";
            let turn = if group { format!("ask_answer_{}", "f".repeat(64)) } else { "ordinary-turn".into() };
            let mut queued = payload(home.path(), &turn);
            if group { queued.target_group = Some("build".into()); }
            let id = queue::enqueue(session, &queued).unwrap();
            queue::claim_next(session).unwrap().unwrap();
            queue::recover_interrupted().unwrap();
            let claimed = queue::claim_next(session).unwrap().unwrap();
            let key = queued_channel_completion(session, &claimed.payload, claimed.payload.workspace.as_deref());
            assert!(key.is_none());
            let target = receipt_path(home.path(), session);
            crate::config::private_io::atomic_write_private(&target, b"unrelated damaged cache").unwrap();
            let calls = Cell::new(0);
            assert!(start_queued_turn(&id, claimed.attempts, key.as_ref(), || calls.set(calls.get() + 1)).unwrap().is_some());
            assert_eq!(calls.get(), 1);
            finish_queued_turn(&id, key.as_ref(), OutcomeCompletion::Unknown, "Existing result").await.unwrap();
            assert!(queue::list(session).unwrap().is_empty());
            assert_eq!(std::fs::read(&target).unwrap(), b"unrelated damaged cache");
        }
    }

    mod cancel_wake {
        use super::*;
        use crate::cli::daemon::{cancel_queued_turn_and_wake, TurnCompletion, WireResponse};
        use tokio::io::{AsyncBufReadExt, BufReader};
        use tokio::net::UnixStream;

        #[tokio::test]
        async fn committed_removal_wakes_successor_before_even_a_lost_acknowledgement() {
            for group in [false, true] {
                for failed in [false, true] {
                    for lost_ack in [false, true] {
                        let home = tempfile::tempdir().unwrap();
                        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
                        let session = "cancel-successor";
                        let turn = "turn_cancel_original";
                        let mut original = payload(home.path(), turn);
                        if group {
                            original.target_agent = None;
                            original.target_group = Some("build".into());
                            queue::reserve_immediate_group(session, turn, &original).unwrap();
                        }
                        let id = queue::enqueue(session, &original).unwrap();
                        if failed {
                            assert_eq!(queue::claim_next(session).unwrap().unwrap().queue_id, id);
                            queue::fail(&id, "Review interrupted work before continuing").unwrap();
                        }
                        let proof = submission(home.path(), session, turn);
                        let next = queue::enqueue(session, &payload(home.path(), "turn_retained_successor")).unwrap();
                        let next_payload = row(home.path(), &next).2;
                        let (peer, server) = UnixStream::pair().unwrap();
                        let mut peer = Some(peer);
                        if lost_ack { drop(peer.take()); }
                        let (_read, mut write) = server.into_split();
                        let wakes = Cell::new(0);
                        let result = cancel_queued_turn_and_wake(&mut write, session.into(), id.clone(), || {
                            wakes.set(wakes.get() + 1);
                            assert!(queue::list(session).unwrap().iter().all(|entry| entry.queue_id != id));
                            assert_eq!(submission(home.path(), session, turn), proof);
                            if group {
                                assert_eq!(queue::reserve_immediate_group(session, turn, &original).unwrap(),
                                    queue::ImmediateGroupReservation::ExistingSettled,
                                    "group cancellation must settle before the wake");
                            }
                            let claimed = queue::claim_next(session).unwrap().unwrap();
                            assert_eq!(claimed.queue_id, next);
                            assert_eq!(claimed.attempts, 1);
                        }).await;
                        assert_eq!(result.is_err(), lost_ack);
                        assert_eq!(wakes.get(), 1);
                        assert_eq!(row(home.path(), &next), ("running".into(), 1, next_payload));
                        assert_eq!(submission(home.path(), session, turn), proof);
                        if let Some(peer) = peer {
                            let mut response = String::new();
                            tokio::time::timeout(std::time::Duration::from_secs(2),
                                BufReader::new(peer).read_line(&mut response)).await.unwrap().unwrap();
                            assert!(matches!(serde_json::from_str::<WireResponse>(&response).unwrap(),
                                WireResponse::Done(summary) if summary.completion == TurnCompletion::Canceled
                                    && summary.main_session_id == session && summary.run_id == id));
                        }
                        // A repeated click cannot create a second wake. The first
                        // successful removal already woke work before its reply.
                        assert!(cancel_queued_turn_and_wake(&mut write, session.into(), id, || {
                            wakes.set(wakes.get() + 1);
                        }).await.is_err());
                        assert_eq!(wakes.get(), 1);
                        assert_eq!(row(home.path(), &next).1, 1);
                    }
                }
            }
        }

        #[tokio::test]
        async fn rejected_or_failed_cancellation_never_wakes_or_changes_ownership() {
            for failure in ["wrong-session", "running", "missing", "invalid-id", "delete-error", "invalid-payload"] {
                let home = tempfile::tempdir().unwrap();
                let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
                let session = "cancel-owner";
                let turn = "turn_cancel_rejected";
                let id = queue::enqueue(session, &payload(home.path(), turn)).unwrap();
                if failure == "running" { queue::claim_next(session).unwrap().unwrap(); }
                if failure == "delete-error" {
                    database(home.path()).execute_batch("CREATE TRIGGER reject_cancel BEFORE DELETE ON queued_user_turns
                        BEGIN SELECT RAISE(ABORT,'cancel unavailable'); END;").unwrap();
                }
                if failure == "invalid-payload" {
                    database(home.path()).execute("UPDATE queued_user_turns SET payload_json='unreadable envelope' WHERE queue_id=?1", [&id]).unwrap();
                }
                let before = row(home.path(), &id);
                let proof = submission(home.path(), session, turn);
                let (peer, server) = UnixStream::pair().unwrap();
                let (_read, mut write) = server.into_split();
                let requested_session = if failure == "wrong-session" { "other-session" } else { session };
                let requested_id = match failure {
                    "missing" => "queued_missing".into(),
                    "invalid-id" => "../invalid".into(),
                    _ => id.clone(),
                };
                let wakes = Cell::new(0);
                assert!(cancel_queued_turn_and_wake(&mut write, requested_session.into(), requested_id, || {
                    wakes.set(wakes.get() + 1);
                }).await.is_err(), "{failure}");
                assert_eq!(wakes.get(), 0, "{failure}");
                assert_eq!(row(home.path(), &id), before, "{failure}");
                assert_eq!(submission(home.path(), session, turn), proof, "{failure}");
                drop(peer);
            }
        }

        #[tokio::test]
        async fn group_settlement_error_rolls_back_removal_and_suppresses_wake() {
            for missing_reservation in [false, true] {
                let home = tempfile::tempdir().unwrap();
                let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
                let session = "cancel-group";
                let turn = "turn_cancel_group";
                let mut original = payload(home.path(), turn);
                original.target_agent = None;
                original.target_group = Some("build".into());
                if !missing_reservation { queue::reserve_immediate_group(session, turn, &original).unwrap(); }
                let id = queue::enqueue(session, &original).unwrap();
                let before = row(home.path(), &id);
                let proof = submission(home.path(), session, turn);
                if !missing_reservation {
                    database(home.path()).execute_batch("CREATE TRIGGER reject_group_settlement BEFORE UPDATE ON immediate_group_turns
                        BEGIN SELECT RAISE(ABORT,'settlement unavailable'); END;").unwrap();
                }
                let (peer, server) = UnixStream::pair().unwrap();
                let (_read, mut write) = server.into_split();
                let wakes = Cell::new(0);
                assert!(cancel_queued_turn_and_wake(&mut write, session.into(), id.clone(), || {
                    wakes.set(wakes.get() + 1);
                }).await.is_err());
                assert_eq!(wakes.get(), 0);
                assert_eq!(row(home.path(), &id), before, "failed settlement must restore the exact removed row");
                assert_eq!(submission(home.path(), session, turn), proof);
                if missing_reservation {
                    queue::reserve_immediate_group(session, turn, &original).unwrap();
                } else {
                    database(home.path()).execute_batch("DROP TRIGGER reject_group_settlement;").unwrap();
                    assert_eq!(queue::reserve_immediate_group(session, turn, &original).unwrap(),
                        queue::ImmediateGroupReservation::ExistingInFlight);
                }
                cancel_queued_turn_and_wake(&mut write, session.into(), id, || {
                    assert_eq!(queue::reserve_immediate_group(session, turn, &original).unwrap(),
                        queue::ImmediateGroupReservation::ExistingSettled);
                    wakes.set(wakes.get() + 1);
                }).await.unwrap();
                assert_eq!(wakes.get(), 1);
                assert!(queue::list(session).unwrap().is_empty());
                drop(peer);
            }
        }
    }
}

#[cfg(test)]
mod ensure_gateway_tests {
    #[tokio::test]
    async fn idle_journal_socket_disconnect_retires_handler_without_an_event() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let home = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        for attempt in 0..8 {
            let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
            let mut handler = tokio::spawn(super::handle_connection(server, Default::default()));
            let request = super::WireRequest::SubscribeJournal {
                session_id: format!("test-idle-journal-{attempt}"), owner: None,
            };
            let mut bytes = serde_json::to_vec(&request).unwrap();
            bytes.push(b'\n');
            client.write_all(&bytes).await.unwrap();
            let mut client = tokio::io::BufReader::new(client);
            let mut barrier = String::new();
            tokio::time::timeout(std::time::Duration::from_secs(2), client.read_line(&mut barrier))
                .await.unwrap().unwrap();
            assert!(matches!(serde_json::from_str::<super::WireResponse>(&barrier).unwrap(), super::WireResponse::Pong));
            assert!(!handler.is_finished());
            drop(client);
            let result = tokio::time::timeout(std::time::Duration::from_secs(2), &mut handler).await;
            if result.is_err() { handler.abort(); }
            result.expect("closed idle journal must release its handler without a future event")
                .unwrap().unwrap();
        }
    }

    #[tokio::test]
    async fn running_execution_registry_preserves_peers_and_scopes_cancellation() {
        fn turn(handle: &tokio::task::JoinHandle<()>, agent: &str) -> super::RunningTurn {
            super::RunningTurn {
                handle: handle.abort_handle(), target_agent: Some(agent.to_string()),
                target_group: Some("build".to_string()), authored_prompt: Some(format!("work for {agent}")),
                session_file_before: None,
            }
        }
        async fn aborted(handle: tokio::task::JoinHandle<()>) {
            assert!(tokio::time::timeout(std::time::Duration::from_secs(2), handle)
                .await.expect("cancellation must settle").unwrap_err().is_cancelled());
        }
        let theo = tokio::spawn(std::future::pending::<()>());
        let iris = tokio::spawn(std::future::pending::<()>());
        let leo = tokio::spawn(std::future::pending::<()>());
        let other_room = tokio::spawn(std::future::pending::<()>());

        // Before: the second registration erases the first Stop handle; an
        // earlier execution's cleanup then removes its still-running peer.
        let mut old = std::collections::HashMap::new();
        old.insert("room", theo.abort_handle());
        old.insert("room", iris.abort_handle());
        assert_eq!(old.len(), 1);
        assert_eq!(old.remove("room").unwrap().id(), iris.id());
        assert!(!iris.is_finished());

        let mut registry = super::RunningTurns::default();
        let theo_id = registry.insert("room".to_string(), turn(&theo, "theo"));
        let iris_id = registry.insert("room".to_string(), turn(&iris, "iris"));
        let leo_id = registry.insert("room".to_string(), turn(&leo, "leo"));
        let other_id = registry.insert("other".to_string(), turn(&other_room, "iris"));
        assert_eq!(registry.sessions["room"].len(), 3);
        assert!(!registry.cancel("room", Some("missing")));
        assert!(registry.cancel("room", Some("iris")));
        aborted(iris).await;
        assert!(!theo.is_finished() && !leo.is_finished() && !other_room.is_finished());
        registry.remove("room", iris_id);
        registry.remove("room", iris_id); // stale duplicate cleanup
        registry.remove("room", other_id); // wrong conversation
        assert_eq!(registry.sessions["room"].len(), 2);

        assert!(registry.cancel("room", None));
        aborted(theo).await;
        aborted(leo).await;
        assert!(!other_room.is_finished());
        assert!(!registry.cancel("room", None), "settled handles are not live work");
        registry.remove("room", theo_id);
        registry.remove("room", leo_id);
        assert!(!registry.sessions.contains_key("room"));
        assert_eq!(registry.abort_for_deletion("other"), vec![(Some("work for iris".to_string()), None)]);
        aborted(other_room).await;
        registry.remove("other", other_id);
        assert!(registry.sessions.is_empty());
    }

    use super::{
        authorize_ask_request_owner_against_snapshot,
        authorize_wire_request_owner_against_snapshot, claim_gateway_listener_locked,
        completed_jobs_fallback, ensure_gateway_running, gateway_probe_at,
        gateway_probe_confirmed_at, maintenance_attempt_ts_path, maintenance_elapsed_since_attempt,
        maintenance_elapsed_since_success, maintenance_ts_path, open_private_append_file,
        pid_is_verified_gateway_at, process_instance_exited_at, process_start_time_at,
        process_uses_selected_executable_at, publish_gateway_pid, read_bounded_utf8_line,
        record_maintenance_attempt, record_maintenance_success,
        retain_detached_ask_approval_if_queued, rotate_log_if_oversized, rotated_log_path,
        scheduled_agent_target, scheduled_turn_id, send, socket_path, spawn_background_settlement,
        subscribe_events, target_agent_for_session_kind, validate_turn_delivery, effective_turn_delivery,
        validate_wire_request_session_ids, BackgroundSettlementTracker, DetachedAskApprovalGrant,
        GatewayLifecycleLock, GatewayProbe, RemoteOutcome, TranscriptDeleteScope, TurnDelivery,
        TurnCompletion, TurnSummary, VaultCommand, VaultReply, WireRequest, WireResponse, GATEWAY_WIRE_PROTOCOL,
        PROTOCOL_PROBE_ATTEMPTS,
    };

    #[test]
    fn request_completion_is_explicit_and_legacy_summaries_are_unknown() {
        let summary=settlement_summary(true,"Current answer");
        let mut value=serde_json::to_value(&summary).unwrap();
        assert_eq!(value["completion"],"completed");
        assert_eq!(value["background_work_pending"],true);
        value.as_object_mut().unwrap().remove("completion");
        assert_eq!(serde_json::from_value::<TurnSummary>(value).unwrap().completion,TurnCompletion::Unknown);
        let incomplete = TurnCompletion::from(crate::runtime::OutcomeCompletion::Incomplete);
        assert_eq!(serde_json::to_value(incomplete).unwrap(), "incomplete");
        assert_ne!(incomplete, TurnCompletion::Completed);
        assert_eq!(TurnCompletion::from(crate::runtime::OutcomeCompletion::Unknown), TurnCompletion::Unknown);
    }

    #[test]
    fn broken_group_continuation_does_not_starve_an_independent_saved_dispatch() {
        let home = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let path = home.path().join("company-test.sqlite");
        let company = crate::runtime::company::CompanyStore::open(&path).unwrap();
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute("INSERT INTO company_group_continuations(canonical_session_id,turn_id,payload_json) VALUES(?1,?2,?3)", rusqlite::params!["continuation-isolation", "malformed-payload", "not json"]).unwrap();
        for id in ["broken", "healthy-task"] {
            let ready = serde_json::json!({
                "canonical_session_id":"continuation-isolation", "original_turn_id":"original", "turn_id":id,
                "activation":{"group_id":"build","roster_fingerprint":"fingerprint","selection":"explicit","active_agent_ids":["iris"],"execution_mode":"parallel","execution_waves":[["iris"]],"execution_dependencies":[]},
                "predecessor_receipts":[], "original_request":"Build the requested artifact"
            });
            connection.execute("INSERT INTO company_group_continuations(canonical_session_id,turn_id,payload_json) VALUES(?1,?2,?3)", rusqlite::params!["continuation-isolation", id, ready.to_string()]).unwrap();
        }
        company.freeze_group_continuation_dispatch("continuation-isolation", "broken", "invalid envelope").unwrap();
        let payload = serde_json::json!({
            "turn_id":"healthy-task","user_request":"Continue the saved task","origin":{"kind":"group_continuation","original_turn_id":"original","display":"Ready work"},
            "interaction_mode":"execute","permission_mode":"workspace","yolo":null,"workspace":home.path(),
            "target_agent":null,"target_group":"build","group_activation":null,"sticky_notes":null,"viewport":null,"attachments":null
        });
        company.freeze_group_continuation_dispatch("continuation-isolation", "healthy-task", &payload.to_string()).unwrap();
        let error = super::enqueue_ready_group_continuations_from(&company, Some("continuation-isolation")).unwrap_err();
        assert!(error.to_string().contains("broken"));
        assert!(error.to_string().contains("malformed-payload"));
        assert!(!error.to_string().contains("healthy-task:"), "{error:#}");
        let queued = crate::cli::turn_queue::claim_next("continuation-isolation").unwrap().unwrap();
        assert_eq!(queued.payload.turn_id.as_deref(), Some("healthy-task"));
        assert_eq!(crate::cli::turn_queue::reserve_immediate_group("continuation-isolation", "healthy-task", &queued.payload).unwrap(),
            crate::cli::turn_queue::ImmediateGroupReservation::ExistingInFlight,
            "ready continuation must own the receipt that completion will settle");
        assert!(crate::cli::turn_queue::claim_next("continuation-isolation").unwrap().is_none());
        let (pending, failures) = company.scan_group_continuations(Some("continuation-isolation")).unwrap();
        assert_eq!(failures.len(), 1);
        assert!(company.scan_group_continuations(Some("unrelated-room")).unwrap().1.is_empty());
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].turn_id, "broken");
    }

    #[test]
    fn group_continuation_context_uses_exact_saved_inputs_not_unrelated_chat() {
        use crate::runtime::group_conversation::{GroupActivationIntent, GroupActivationSelection, GroupExecutionMode};
        let ready = crate::runtime::company::GroupReadyContinuation {
            canonical_session_id: "group-context".into(), original_turn_id: "original".into(), turn_id: "ready".into(),
            original_request: Some("Build the agreed report".into()), predecessor_receipts: vec!["required".into()],
            activation: GroupActivationIntent {
                tool_constraints: Default::default(),
                inspection_participants: Default::default(),
                group_id: "build".into(), roster_fingerprint: "fingerprint".into(), selection: GroupActivationSelection::Explicit,
                active_agent_ids: vec!["integrator".into()], execution_mode: GroupExecutionMode::Parallel,
                execution_waves: vec![vec!["integrator".into()]], execution_dependencies: Some(vec![]),
            },
        };
        let contribution = |id: &str, group: &str, body: &str| crate::session::Message::GroupContribution {
            turn_id: "branch".into(), message_id: id.into(), group_id: group.into(), agent_id: "iris".into(),
            internal_role: "frontend".into(), display_name: "Iris".into(), role_title: "Design".into(), color: "".into(),
            icon_seed: "".into(), avatar: None, subject: "Design brief".into(), body: body.into(), reply_to: None, causation_id: None,
        };
        let messages = vec![contribution("required", "other-group", "WRONG GROUP"), contribution("unrelated", "build", "UNRELATED"), contribution("required", "build", "Exact agreed dimensions: 42 × 18")];
        let inputs = super::group_continuation_inputs(&ready, &messages).unwrap();
        assert!(inputs.contains("Exact agreed dimensions: 42 × 18"));
        assert!(inputs.contains("Build the agreed report"));
        assert!(!inputs.contains("WRONG GROUP") && !inputs.contains("UNRELATED"));
        assert!(super::group_continuation_inputs(&ready, &messages[..2]).is_err());
    }

    #[test]
    fn late_answer_queue_failure_rolls_back_its_hiring_grant() {
        let home = tempfile::tempdir().expect("temp home");
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let session_id = "late-answer-hire-rollback";
        let subject = "school_coach";
        let grant_id =
            crate::tools::agent_forge::grant_permanent_hire(session_id, subject).unwrap();
        let grant = DetachedAskApprovalGrant {
            session_id: session_id.to_string(),
            subject: subject.to_string(),
            grant_id,
        };

        let queue_result: anyhow::Result<String> =
            Err(anyhow::anyhow!("forced durable queue failure"));
        let error = retain_detached_ask_approval_if_queued(Some(&grant), queue_result)
            .expect_err("the queue failure must be preserved");
        assert!(error.to_string().contains("forced durable queue failure"));
        assert!(!crate::tools::agent_forge::permanent_hire_is_granted(
            Some(session_id),
            subject,
        ));
    }

    #[test]
    fn cli_resume_routes_every_specialist_session_to_its_owner() {
        use crate::session::{SessionKind, SubAgentType};

        assert_eq!(target_agent_for_session_kind(&SessionKind::Main), None);
        for (agent, expected) in [
            (SubAgentType::Coder, "coder"),
            (SubAgentType::Researcher, "researcher"),
            (SubAgentType::Browser, "browser"),
            (SubAgentType::Frontend, "frontend"),
            (SubAgentType::Database, "database"),
            (SubAgentType::Hacker, "hacker"),
            (SubAgentType::Presentation, "presentation"),
            (SubAgentType::Finance, "finance"),
            (SubAgentType::ComputerUse, "computer_use"),
            (SubAgentType::Critic, "critic"),
            (SubAgentType::Tester, "tester"),
            (SubAgentType::Planner, "planner"),
            (SubAgentType::Scribe, "scribe"),
            (SubAgentType::Sales, "sales"),
            (SubAgentType::Marketing, "marketing"),
            (SubAgentType::PersonalLogistics, "personal_logistics"),
            (SubAgentType::custom("school_coach"), "school_coach"),
        ] {
            assert_eq!(
                target_agent_for_session_kind(&SessionKind::SubAgent(agent)).as_deref(),
                Some(expected)
            );
        }
    }

    fn settlement_summary(pending: bool, markdown: &str) -> TurnSummary {
        TurnSummary {
            completion: TurnCompletion::Completed,
            final_markdown: markdown.to_string(),
            main_session_id: "settlement-test".to_string(),
            run_id: "initial-run".to_string(),
            trace_path: "trace.json".to_string(),
            route: "orchestrator".to_string(),
            total_tokens: 10,
            orchestrator_tokens: None,
            coder_tokens: None,
            compression_saved_tokens: 0,
            compression_raw_tokens: 0,
            context_window: None,
            background_work_pending: pending,
        }
    }

    #[test]
    fn legacy_turn_summary_defaults_to_no_background_wait() {
        let summary: TurnSummary = serde_json::from_value(serde_json::json!({
            "final_markdown": "done",
            "main_session_id": "legacy",
            "run_id": "run",
            "trace_path": "trace.json",
            "route": "direct",
            "total_tokens": 1,
            "orchestrator_tokens": null,
            "coder_tokens": null
        }))
        .expect("older summaries remain readable after the wire bump");
        assert!(!summary.background_work_pending);
    }

    #[test]
    fn vault_wire_commands_are_redacted_and_operate_in_gateway_memory() {
        let home = tempfile::tempdir().expect("temp home");
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let password = "gateway master password";

        let initialize = VaultCommand::Initialize {
            master_password: password.to_string(),
        };
        let debug = format!("{initialize:?}");
        assert!(!debug.contains(password));
        let recovery = super::execute_vault_command(&initialize).expect("initialize vault");
        assert!(
            matches!(&recovery, VaultReply::RecoveryKey { recovery_key } if recovery_key.starts_with("PHX1-"))
        );
        assert!(!format!("{recovery:?}").contains("PHX1-"));

        let store = VaultCommand::Store {
            scope: crate::security::vault::CredentialScope::Company,
            site: "example.com".to_string(),
            label: "Example login".to_string(),
            username: Some("owner@example.com".to_string()),
            kind: "password".to_string(),
            metadata_json: "{}".to_string(),
            secret: "never-print-this-secret".to_string(),
            fields: Default::default(),
        };
        assert!(!format!("{store:?}").contains("never-print-this-secret"));
        let stored = super::execute_vault_command(&store).expect("store credential");
        let credential_id = match &stored {
            VaultReply::Stored { credential } => credential.credential_id.clone(),
            other => panic!("unexpected store reply: {other:?}"),
        };

        let updated = super::execute_vault_command(&VaultCommand::Update {
            credential_id: credential_id.clone(),
            scope: crate::security::vault::CredentialScope::Company,
            site: "login.example.com".to_string(),
            label: "Updated login".to_string(),
            username: Some("owner@example.com".to_string()),
            kind: "password".to_string(),
            metadata_json: r#"{"note":"edited in settings"}"#.to_string(),
            replacement_secret: None,
            replacement_fields: None,
        })
        .expect("update credential");
        assert!(
            matches!(&updated, VaultReply::Stored { credential } if credential.label == "Updated login" && credential.site == "login.example.com")
        );

        let listed = super::execute_vault_command(&VaultCommand::List).expect("list credentials");
        assert!(
            matches!(&listed, VaultReply::Credentials { credentials } if credentials.len() == 1 && credentials[0].credential_id == credential_id)
        );
        // Locked: a reveal needs the master password once, then stays unlocked.
        super::execute_vault_command(&VaultCommand::Lock).expect("lock vault");
        assert!(super::execute_vault_command(&VaultCommand::Reveal {
            credential_id: credential_id.clone(),
            master_password: None,
        })
        .is_err());
        assert!(super::execute_vault_command(&VaultCommand::Reveal {
            credential_id: credential_id.clone(),
            master_password: Some("wrong password".to_string()),
        })
        .is_err());
        let revealed = super::execute_vault_command(&VaultCommand::Reveal {
            credential_id: credential_id.clone(),
            master_password: Some(password.to_string()),
        })
        .expect("reveal credential");
        assert!(
            matches!(&revealed, VaultReply::Revealed { secret, .. } if secret == "never-print-this-secret")
        );
        assert!(!format!("{revealed:?}").contains("never-print-this-secret"));

        assert!(matches!(
            super::execute_vault_command(&VaultCommand::Lock).expect("lock vault"),
            VaultReply::Locked
        ));
        // Passes metadata stays listable while locked; secrets do not.
        assert!(matches!(
            &super::execute_vault_command(&VaultCommand::List).expect("list while locked"),
            VaultReply::Credentials { credentials } if credentials.len() == 1
        ));
        assert!(matches!(
            &super::execute_vault_command(&VaultCommand::Status).expect("status"),
            VaultReply::Status { status } if status == "locked"
        ));
        assert!(matches!(
            super::execute_vault_command(&VaultCommand::UnlockWithPassword {
                master_password: password.to_string(),
            })
            .expect("unlock vault"),
            VaultReply::Unlocked
        ));
        assert!(matches!(
            super::execute_vault_command(&VaultCommand::Delete { credential_id })
                .expect("delete credential"),
            VaultReply::Deleted { deleted: true }
        ));
    }

    #[test]
    fn pass_request_popup_seals_into_passes_and_answers_with_metadata_only() {
        let home = tempfile::tempdir().expect("temp home");
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let request = crate::tools::passes::PassRequestInput {
            kind: "login".into(),
            reason: "send the weekly update".into(),
            title: Some("Gmail login".into()),
            site: Some("gmail.com".into()),
            fields: vec![],
            labels: Default::default(),
            username_hint: Some("me@gmail.com".into()),
            scope: "agent".into(),
            force_new: false,
        }
        .validate_and_normalize()
        .expect("valid request");
        let scope = crate::security::vault::CredentialScope::agent("avery");
        let ask = request.to_ask("avery", None, &scope);
        let ask_id = "pass-test0001";
        let _rx = crate::runtime::asks::register_with_payload(
            ask_id,
            "agent-avery",
            "Avery",
            &ask.questions,
            ask.approval.as_ref(),
        );
        // No master password exists and none is needed to save.
        let wrong_kind = VaultCommand::FulfillRequest {
            ask_id: ask_id.into(),
            kind: "card".into(),
            site: None,
            label: None,
            username: None,
            metadata_json: "{}".into(),
            secret: "4242424242424242".into(),
            fields: Default::default(),
        };
        assert!(super::execute_vault_command(&wrong_kind).is_err());
        let fulfill = VaultCommand::FulfillRequest {
            ask_id: ask_id.into(),
            kind: "login".into(),
            site: Some("accounts.google.com".into()),
            label: None,
            username: Some("me@gmail.com".into()),
            metadata_json: "{}".into(),
            secret: "popup-typed-password".into(),
            fields: std::collections::BTreeMap::from([("totp".to_string(), "JBSWY3DPEHPK3PXP".to_string())]),
        };
        assert!(!format!("{fulfill:?}").contains("popup-typed-password"));
        let reply = super::execute_vault_command(&fulfill).expect("fulfill");
        let (credential, answer) = match &reply {
            VaultReply::PassRequestFulfilled { credential, answer } => (credential.clone(), answer.clone()),
            other => panic!("unexpected reply {other:?}"),
        };
        assert_eq!(credential.scope, scope, "owner comes from the runtime-bound ask");
        assert_eq!(credential.kind, "password");
        assert_eq!(credential.site, "accounts.google.com");
        assert!(!answer.contains("popup-typed-password") && !answer.contains("JBSWY3DP"));
        assert_eq!(
            crate::tools::passes::decision_from_answer(&answer),
            crate::tools::passes::PassDecision::Saved { credential_id: credential.credential_id.clone() }
        );
        let revealed = crate::security::vault::Vault::open_default()
            .reveal(&credential.credential_id, &[scope.clone()])
            .expect("unprotected passes reveal");
        assert_eq!(revealed.secret(), "popup-typed-password");
        assert_eq!(crate::tools::passes::resolve_field(&revealed, Some("totp")).unwrap().len(), 6);
        // Once answered, the card cannot be fulfilled again.
        assert!(crate::runtime::asks::answer(ask_id, answer));
        assert!(super::execute_vault_command(&fulfill).is_err());
    }

    #[tokio::test]
    async fn subscription_returns_only_after_server_registration_ack() {
        use tokio::io::{AsyncBufReadExt, BufReader};

        let home = tempfile::tempdir().expect("temp home");
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let listener = tokio::net::UnixListener::bind(socket_path()).expect("bind test gateway");
        let (request_seen_tx, request_seen_rx) = tokio::sync::oneshot::channel();
        let (release_ack_tx, release_ack_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept subscription");
            let (read_half, mut write_half) = stream.into_split();
            let mut lines = BufReader::new(read_half).lines();
            let line = lines
                .next_line()
                .await
                .expect("read subscription")
                .expect("subscription line");
            assert!(matches!(
                serde_json::from_str::<WireRequest>(&line).expect("parse subscription"),
                WireRequest::Subscribe { session_id, .. }
                    if session_id == "settlement-handshake"
            ));
            let _ = request_seen_tx.send(());
            release_ack_rx.await.expect("release acknowledgement");
            send(&mut write_half, &WireResponse::Pong)
                .await
                .expect("send registration barrier");
            send(
                &mut write_half,
                &WireResponse::Event(crate::runtime::CliEvent::GatewayNotice(
                    "after barrier".to_string(),
                )),
            )
            .await
            .expect("send first subscribed event");
        });

        let client = tokio::spawn(subscribe_events("settlement-handshake".to_string()));
        request_seen_rx.await.expect("server saw request");
        tokio::task::yield_now().await;
        assert!(
            !client.is_finished(),
            "client must not report an attached subscription before Pong"
        );
        release_ack_tx.send(()).expect("release server ack");
        let mut events = client
            .await
            .expect("subscription task joins")
            .expect("subscription attaches");
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
                .await
                .expect("first event arrives"),
            Some(crate::runtime::CliEvent::GatewayNotice(text)) if text == "after barrier"
        ));
        server.await.expect("test gateway joins");
    }

    fn returned(agent: &str, ok: bool) -> crate::runtime::CliEvent {
        crate::runtime::CliEvent::BackgroundAgentReturned {
            agent: agent.to_string(),
            subject: format!("{agent} task"),
            ok,
            summary: format!("{agent} finished"),
            body: format!("{agent} evidence"),
            handoff_id: String::new(),
            requester: String::new(),
            receiver: String::new(),
            status: String::new(),
            reply_to: None,
            causation_id: None,
        }
    }

    #[test]
    fn settlement_waits_for_late_return_to_be_absorbed_and_reintegrated() {
        use crate::runtime::CliEvent;

        let mut tracker = BackgroundSettlementTracker::default();
        assert!(tracker
            .observe(&CliEvent::BackgroundAgentSpawned {
                agent: "researcher".to_string(),
                subject: "find release".to_string(),
                handoff_id: String::new(),
                requester: String::new(),
                receiver: String::new(),
                status: String::new(),
                causation_id: None,
            })
            .is_none());
        assert!(tracker.observe(&returned("researcher", true)).is_none());
        assert!(tracker
            .observe(&CliEvent::BackgroundResultsAbsorbed { count: 1 })
            .is_none());

        // A follow-up is already live. It returns during the first wake's
        // final provider call, so that wake's answer must not be terminal.
        assert!(tracker
            .observe(&CliEvent::BackgroundAgentSpawned {
                agent: "critic".to_string(),
                subject: "verify release".to_string(),
                handoff_id: String::new(),
                requester: String::new(),
                receiver: String::new(),
                status: String::new(),
                causation_id: None,
            })
            .is_none());
        assert!(tracker
            .observe(&CliEvent::FinalOutput("first integration".to_string()))
            .is_none());
        assert!(tracker.observe(&returned("critic", false)).is_none());
        assert!(tracker.observe(&CliEvent::Done).is_none());

        assert!(tracker
            .observe(&CliEvent::BackgroundResultsAbsorbed { count: 1 })
            .is_none());
        assert!(tracker
            .observe(&CliEvent::FinalOutput("terminal integration".to_string()))
            .is_none());
        assert_eq!(
            tracker.observe(&CliEvent::Done).as_deref(),
            Some("terminal integration")
        );
    }

    #[tokio::test]
    async fn zero_background_settlement_returns_without_waiting_for_events() {
        let (_tx, rx) = tokio::sync::mpsc::channel(1);
        let (_events, handle) = spawn_background_settlement(
            rx,
            settlement_summary(false, "foreground final"),
            std::time::Duration::from_secs(60),
        );
        let outcome = tokio::time::timeout(std::time::Duration::from_millis(100), handle)
            .await
            .expect("zero-background fast path must not wait")
            .expect("settlement task must join");
        match outcome {
            RemoteOutcome::Summary(summary) => {
                assert_eq!(summary.final_markdown, "foreground final");
                assert!(!summary.background_work_pending);
            }
            _ => panic!("expected immediate foreground summary"),
        }
    }

    #[tokio::test]
    async fn settlement_filter_emits_only_the_integrated_terminal_done() {
        use crate::runtime::CliEvent;

        let (tx, rx) = tokio::sync::mpsc::channel(16);
        for event in [
            CliEvent::BackgroundAgentSpawned {
                agent: "researcher".to_string(),
                subject: "find release".to_string(),
                handoff_id: String::new(),
                requester: String::new(),
                receiver: String::new(),
                status: String::new(),
                causation_id: None,
            },
            returned("researcher", true),
            CliEvent::BackgroundResultsAbsorbed { count: 1 },
            CliEvent::FinalOutput("integrated release".to_string()),
            CliEvent::Done,
        ] {
            tx.try_send(event).expect("fixture channel capacity");
        }
        drop(tx);

        let (mut events, handle) = spawn_background_settlement(
            rx,
            settlement_summary(true, "provisional"),
            std::time::Duration::from_secs(1),
        );
        let mut done_count = 0;
        let mut leaked_barrier = false;
        while let Some(event) = events.recv().await {
            done_count += usize::from(matches!(&event, CliEvent::Done));
            leaked_barrier |= matches!(&event, CliEvent::BackgroundResultsAbsorbed { .. });
        }
        assert_eq!(done_count, 1);
        assert!(!leaked_barrier);
        match handle.await.expect("settlement task must join") {
            RemoteOutcome::Summary(summary) => {
                assert_eq!(summary.final_markdown, "integrated release");
                assert!(!summary.background_work_pending);
            }
            _ => panic!("expected integrated summary"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn maintenance_success_marker_is_private_and_corruption_retries() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        record_maintenance_success().unwrap();
        let marker = maintenance_ts_path();
        assert_eq!(
            std::fs::metadata(&marker).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(maintenance_elapsed_since_success() < 5);

        crate::config::private_io::atomic_write_private(&marker, b"not-a-timestamp").unwrap();
        assert_eq!(maintenance_elapsed_since_success(), i64::MAX);

        record_maintenance_attempt().unwrap();
        let attempt = maintenance_attempt_ts_path();
        assert_eq!(
            std::fs::metadata(&attempt).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(maintenance_elapsed_since_attempt() < 5);
    }

    #[test]
    fn wake_fallback_preserves_completed_receipts_without_a_model_final() {
        let jobs = vec![crate::runtime::postbox::CompletedJob {
            kind: crate::runtime::postbox::ReturnKind::Specialist,
            delivery_id: String::new(),
            causation_id: None,
            agent: "planner".to_string(),
            subject: "integrated result".to_string(),
            ok: true,
            summary: "ready".to_string(),
            body: "Winner status unresolved; completed evidence is here.".to_string(),
            finished: chrono::Utc::now(),
        }];
        let fallback = completed_jobs_fallback(&jobs).expect("receipt must be deliverable");
        assert!(fallback.contains("durable result directly"));
        assert!(fallback.contains("Winner status unresolved"));
    }

    #[test]
    fn cancel_wire_request_preserves_agent_scope() {
        let request: WireRequest =
            serde_json::from_str(r#"{"Cancel":{"session_id":"main-1","target_agent":"browser"}}"#)
                .expect("targeted cancel parses");
        match request {
            WireRequest::Cancel {
                session_id,
                target_agent,
                ..
            } => {
                assert_eq!(session_id, "main-1");
                assert_eq!(target_agent.as_deref(), Some("browser"));
            }
            _ => panic!("wrong wire variant"),
        }
    }

    #[test]
    fn transcript_deletion_removes_exact_turn_boundaries() {
        use crate::session::{Message, Session};

        let mut session = Session::new_main_with_id("delete-turn", "model", "system");
        for message in [
            Message::User {
                content: "keep prompt".into(),
            },
            Message::Assistant {
                content: "keep answer".into(),
            },
            Message::User {
                content: "delete prompt".into(),
            },
            Message::Assistant {
                content: "planning loop".into(),
            },
            Message::ToolResult {
                tool_name: "read".into(),
                input: "{}".into(),
                success: true,
                output: "loop result".into(),
            },
            Message::User {
                content: "newest prompt".into(),
            },
            Message::Assistant {
                content: "newest answer".into(),
            },
        ] {
            session.push_message(message);
        }

        let (removed, deleted_prompt) = super::delete_transcript_turn_from_session(
            &mut session,
            1,
            "delete prompt",
            TranscriptDeleteScope::Agent,
        )
        .unwrap();
        assert_eq!(removed, 2);
        assert!(!deleted_prompt);
        assert!(session.messages.iter().any(
            |message| matches!(message, Message::User { content } if content == "delete prompt")
        ));
        assert!(!session.messages.iter().any(|message| match message {
            Message::Assistant { content } | Message::User { content } => {
                content.contains("planning loop") || content.contains("loop result")
            }
            Message::ToolResult { output, .. } => output.contains("loop result"),
            Message::Talk { body, .. } => body.contains("loop result"),
            Message::GroupContribution { body, .. } => body.contains("loop result"),
        }));

        let before_mismatch = session.messages.len();
        assert!(super::delete_transcript_turn_from_session(
            &mut session,
            1,
            "a different prompt",
            TranscriptDeleteScope::Prompt,
        )
        .is_err());
        assert_eq!(session.messages.len(), before_mismatch);

        let (removed, deleted_prompt) = super::delete_transcript_turn_from_session(
            &mut session,
            1,
            "delete prompt",
            TranscriptDeleteScope::Prompt,
        )
        .unwrap();
        assert_eq!(removed, 1);
        assert!(deleted_prompt);
        assert!(!session.messages.iter().any(
            |message| matches!(message, Message::User { content } if content == "delete prompt")
        ));
        assert!(session.messages.iter().any(
            |message| matches!(message, Message::Assistant { content } if content == "newest answer")
        ));
    }

    fn transcript_message_contains(message: &crate::session::Message, needle: &str) -> bool {
        use crate::session::Message;

        match message {
            Message::User { content } | Message::Assistant { content } => content.contains(needle),
            Message::Talk {
                from,
                to,
                subject,
                body,
                ..
            } => {
                from.contains(needle)
                    || to.contains(needle)
                    || subject.contains(needle)
                    || body.contains(needle)
            }
            Message::GroupContribution {
                turn_id,
                message_id,
                group_id,
                agent_id,
                internal_role,
                display_name,
                role_title,
                color,
                icon_seed,
                subject,
                body,
                ..
            } => {
                turn_id.contains(needle)
                    || message_id.contains(needle)
                    || group_id.contains(needle)
                    || agent_id.contains(needle)
                    || internal_role.contains(needle)
                    || display_name.contains(needle)
                    || role_title.contains(needle)
                    || color.contains(needle)
                    || icon_seed.contains(needle)
                    || subject.contains(needle)
                    || body.contains(needle)
            }
            Message::ToolResult {
                tool_name,
                input,
                output,
                ..
            } => tool_name.contains(needle) || input.contains(needle) || output.contains(needle),
        }
    }

    fn decode_transcript_archive(bytes: &[u8]) -> Vec<crate::session::Message> {
        std::str::from_utf8(bytes)
            .expect("archive replacement is UTF-8")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("archive replacement row is valid"))
            .collect()
    }

    #[test]
    fn prompt_deletion_crosses_archive_live_and_targets_the_older_duplicate() {
        use crate::session::{Message, Session};

        const SURVIVING_ARCHIVE: &str = "SURVIVING_ARCHIVE_SENTINEL";
        const DELETED_ARCHIVE: &str = "DELETED_ARCHIVE_RESPONSE_SENTINEL";
        const DELETED_TOOL: &str = "DELETED_TOOL_RESPONSE_SENTINEL";
        const DELETED_LIVE: &str = "DELETED_LIVE_RESPONSE_SENTINEL";
        const STALE_CONTINUATION: &str = "STALE_CONTINUATION_DELETED_SENTINEL";
        const NEWEST_RESPONSE: &str = "NEWEST_DUPLICATE_RESPONSE_SURVIVES";

        let archive = vec![
            Message::User {
                content: "keep this archived prompt".into(),
            },
            Message::Assistant {
                content: SURVIVING_ARCHIVE.into(),
            },
            Message::User {
                content: "repeat this prompt".into(),
            },
            Message::Assistant {
                content: DELETED_ARCHIVE.into(),
            },
            Message::ToolResult {
                tool_name: "read".into(),
                input: r#"{"path":"loop.txt"}"#.into(),
                success: true,
                output: DELETED_TOOL.into(),
            },
        ];
        let mut session = Session::new_main_with_id("delete-across-storage", "model", "system");
        for message in [
            Message::Assistant {
                content: format!(
                    "[AUTO-COMPACTED HISTORY — stale test anchor]\n{STALE_CONTINUATION}"
                ),
            },
            Message::Assistant {
                content: DELETED_LIVE.into(),
            },
            Message::User {
                content: "repeat this prompt".into(),
            },
            Message::Assistant {
                content: NEWEST_RESPONSE.into(),
            },
        ] {
            session.push_message(message);
        }

        let (removed, deleted_prompt, archive_replacement) =
            super::delete_transcript_turn_across_storage(
                &mut session,
                archive,
                1,
                "repeat this prompt",
                TranscriptDeleteScope::Prompt,
            )
            .expect("delete the older of two identical prompts");

        assert_eq!(
            removed, 4,
            "prompt plus its complete cross-storage response"
        );
        assert!(deleted_prompt);
        let rewritten_archive = decode_transcript_archive(&archive_replacement);
        assert_eq!(rewritten_archive.len(), 2);
        assert!(rewritten_archive
            .iter()
            .any(|message| transcript_message_contains(message, SURVIVING_ARCHIVE)));

        let all_survivors = rewritten_archive.iter().chain(session.messages.iter());
        for deleted in [
            DELETED_ARCHIVE,
            DELETED_TOOL,
            DELETED_LIVE,
            STALE_CONTINUATION,
        ] {
            assert!(
                !all_survivors
                    .clone()
                    .any(|message| transcript_message_contains(message, deleted)),
                "deleted material leaked through rebuilt state: {deleted}"
            );
        }
        assert!(session
            .messages
            .iter()
            .any(|message| transcript_message_contains(message, SURVIVING_ARCHIVE)));
        assert!(session
            .messages
            .iter()
            .any(|message| transcript_message_contains(message, NEWEST_RESPONSE)));
        assert_eq!(
            session
                .messages
                .iter()
                .filter(|message| matches!(message, Message::User { content } if content == "repeat this prompt"))
                .count(),
            1,
            "turns_from_end=1 must preserve the newer identical prompt"
        );
    }

    #[test]
    fn response_only_deletion_keeps_archived_prompt_and_removes_full_response() {
        use crate::session::{Message, Session};

        const TARGET_PROMPT: &str = "keep this prompt, erase only its agent turn";
        const DELETED_ARCHIVE: &str = "DELETE_RESPONSE_ARCHIVE_PART";
        const DELETED_LIVE: &str = "DELETE_RESPONSE_LIVE_PART";
        const NEWEST_RESPONSE: &str = "NEWEST_RESPONSE_MUST_SURVIVE";

        let archive = vec![
            Message::User {
                content: "older retained prompt".into(),
            },
            Message::Assistant {
                content: "older retained response".into(),
            },
            Message::User {
                content: TARGET_PROMPT.into(),
            },
            Message::Assistant {
                content: DELETED_ARCHIVE.into(),
            },
        ];
        let mut session = Session::new_main_with_id("delete-agent-across", "model", "system");
        for message in [
            Message::Assistant {
                content: "[AUTO-COMPACTED HISTORY — stale test anchor]".into(),
            },
            Message::ToolResult {
                tool_name: "browser".into(),
                input: "{}".into(),
                success: true,
                output: DELETED_LIVE.into(),
            },
            Message::User {
                content: "newest prompt".into(),
            },
            Message::Assistant {
                content: NEWEST_RESPONSE.into(),
            },
        ] {
            session.push_message(message);
        }

        let (removed, deleted_prompt, archive_replacement) =
            super::delete_transcript_turn_across_storage(
                &mut session,
                archive,
                1,
                TARGET_PROMPT,
                TranscriptDeleteScope::Agent,
            )
            .expect("delete only the response spanning archive and live storage");

        assert_eq!(removed, 2);
        assert!(!deleted_prompt);
        let rewritten_archive = decode_transcript_archive(&archive_replacement);
        assert!(rewritten_archive.iter().any(
            |message| matches!(message, Message::User { content } if content == TARGET_PROMPT)
        ));
        for deleted in [DELETED_ARCHIVE, DELETED_LIVE] {
            assert!(!rewritten_archive
                .iter()
                .chain(session.messages.iter())
                .any(|message| transcript_message_contains(message, deleted)));
        }
        assert!(session
            .messages
            .iter()
            .any(|message| transcript_message_contains(message, TARGET_PROMPT)));
        assert!(session
            .messages
            .iter()
            .any(|message| transcript_message_contains(message, NEWEST_RESPONSE)));
    }

    #[test]
    fn staged_transcript_deletion_is_recovered_once_and_cleans_its_manifest() {
        use crate::session::{Message, Session, SessionStore};

        const DELETED: &str = "INTERRUPTED_DELETE_SENTINEL";
        let home = tempfile::tempdir().expect("isolated Phoenix home");
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let root = home.path().join("sessions");
        let session_id = "recover-delete-turn";
        let channel_id = format!("channel_{}", "b".repeat(64));
        let receipt_key = super::super::channel_receipts::Key::new(session_id, Some(&channel_id), Some("phoenix"), Some(home.path()), "delete this prompt").unwrap();
        super::super::channel_receipts::save(&receipt_key, DELETED).unwrap();
        let mut original = Session::new_main_with_id(session_id, "model", "system");
        for message in [
            Message::User {
                content: "delete this prompt".into(),
            },
            Message::Assistant {
                content: DELETED.into(),
            },
            Message::User {
                content: "keep newest prompt".into(),
            },
            Message::Assistant {
                content: "keep newest response".into(),
            },
        ] {
            original.push_message(message);
        }
        let mut store = SessionStore::new(root.clone());
        store.upsert(original.clone());
        store
            .save_one(session_id)
            .expect("persist original session");

        let mut replacement = original;
        let (_, _, archive_replacement) = super::delete_transcript_turn_across_storage(
            &mut replacement,
            Vec::new(),
            1,
            "delete this prompt",
            TranscriptDeleteScope::Prompt,
        )
        .expect("prepare permanent replacement");
        let manifest = super::stage_transcript_deletion(
            &root,
            &replacement,
            &archive_replacement,
            super::PendingTranscriptDeletion {
                version: 1,
                session_id: session_id.into(),
                turns_from_end: 1,
                expected_prompt: "delete this prompt".into(),
                scope: TranscriptDeleteScope::Prompt,
                ask_ids: Vec::new(),
            },
        )
        .expect("stage deletion transaction");
        let (manifest_path, session_stage, archive_stage) =
            super::transcript_delete_paths(&root, session_id);
        assert!(manifest_path.is_file());
        assert!(session_stage.is_file());
        assert!(archive_stage.is_file());
        let before_recovery = SessionStore::read_one_from_disk(&root, session_id)
            .expect("read original session")
            .expect("original session exists");
        assert!(before_recovery
            .messages
            .iter()
            .any(|message| transcript_message_contains(message, DELETED)));

        assert_eq!(super::recover_pending_transcript_deletions().unwrap(), 1);
        let recovered = SessionStore::read_one_from_disk(&root, session_id)
            .expect("read recovered session")
            .expect("recovered session exists");
        assert!(!recovered
            .messages
            .iter()
            .any(|message| transcript_message_contains(message, DELETED)));
        assert!(recovered.messages.iter().any(
            |message| matches!(message, Message::User { content } if content == "keep newest prompt")
        ));
        assert!(super::super::channel_receipts::find(&receipt_key).unwrap().is_none());
        assert!(!manifest_path.exists());
        assert!(!session_stage.exists());
        assert!(!archive_stage.exists());
        assert_eq!(
            super::recover_pending_transcript_deletions().unwrap(),
            0,
            "a completed deletion must not replay on the next startup scan"
        );

        // Retain the value to make accidental schema changes visible in this
        // transaction-level test instead of silently accepting a wrong file.
        assert_eq!(manifest.session_id, session_id);
    }

    #[test]
    fn never_persisted_live_duplicate_does_not_delete_the_older_identical_turn() {
        use crate::session::{Message, Session, SessionStore};

        let home = tempfile::tempdir().expect("isolated Phoenix home");
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let root = home.path().join("sessions");
        let session_id = "delete-unsaved-live-duplicate";
        let mut session = Session::new_main_with_id(session_id, "model", "system");
        session.push_message(Message::User {
            content: "same prompt".into(),
        });
        session.push_message(Message::Assistant {
            content: "older answer must survive".into(),
        });
        let mut store = SessionStore::new(root.clone());
        store.upsert(session);
        store.save_one(session_id).expect("persist older turn");

        let (removed, deleted_prompt) = super::delete_transcript_turn_blocking(
            session_id,
            0,
            "same prompt",
            TranscriptDeleteScope::Prompt,
            true,
            Vec::new(),
        )
        .expect("delete an active prompt that never reached the session file");
        assert_eq!(removed, 0);
        assert!(deleted_prompt);

        let reloaded = SessionStore::read_one_from_disk(&root, session_id)
            .expect("read surviving session")
            .expect("session exists");
        assert!(reloaded.messages.iter().any(
            |message| matches!(message, Message::User { content } if content == "same prompt")
        ));
        assert!(reloaded.messages.iter().any(
            |message| matches!(message, Message::Assistant { content } if content == "older answer must survive")
        ));
    }

    #[test]
    fn onboarding_is_a_typed_sessionless_wire_operation() {
        let encoded = serde_json::to_string(&WireRequest::Onboarding(
            crate::onboarding::OnboardingCommand::Status,
        ))
        .unwrap();
        let decoded: WireRequest = serde_json::from_str(&encoded).unwrap();
        assert!(super::validate_wire_request_session_ids(&decoded).is_ok());
        assert!(matches!(
            decoded,
            WireRequest::Onboarding(crate::onboarding::OnboardingCommand::Status)
        ));
    }

    #[test]
    fn company_directory_is_a_typed_sessionless_wire_operation() {
        let encoded = serde_json::to_string(&WireRequest::CompanyDirectory(
            crate::runtime::company_control::CompanyDirectoryCommand::Status,
        ))
        .unwrap();
        let decoded: WireRequest = serde_json::from_str(&encoded).unwrap();
        assert!(super::validate_wire_request_session_ids(&decoded).is_ok());
        assert!(matches!(
            decoded,
            WireRequest::CompanyDirectory(
                crate::runtime::company_control::CompanyDirectoryCommand::Status
            )
        ));
    }

    #[test]
    fn group_activation_preview_and_turn_intent_have_stable_wire_shapes() {
        use crate::runtime::group_conversation::{
            GroupActivationIntent, GroupActivationPreview, GroupActivationSelection,
            GroupExecutionMode,
        };

        let preview_request = WireRequest::GroupActivationPreview {
            group_id: "launch-room".to_string(),
            user_request: "@everyone ship it".to_string(),
        };
        let preview_request_json = serde_json::to_value(&preview_request).unwrap();
        assert_eq!(
            preview_request_json,
            serde_json::json!({
                "GroupActivationPreview": {
                    "group_id": "launch-room",
                    "user_request": "@everyone ship it"
                }
            })
        );
        assert!(matches!(
            serde_json::from_value::<WireRequest>(preview_request_json).unwrap(),
            WireRequest::GroupActivationPreview { group_id, user_request }
                if group_id == "launch-room" && user_request == "@everyone ship it"
        ));

        let intent = GroupActivationIntent {
            tool_constraints: Default::default(),
            inspection_participants: Default::default(),
            group_id: "launch-room".to_string(),
            roster_fingerprint: "roster-v1".to_string(),
            selection: GroupActivationSelection::Everyone,
            active_agent_ids: vec!["planner".to_string(), "coder".to_string()],
            execution_mode: GroupExecutionMode::Parallel,
            execution_dependencies: None,
            execution_waves: vec![vec!["planner".to_string(), "coder".to_string()]],
        };
        let turn = WireRequest::Turn {
            session_id: "group-launch-room".to_string(),
            turn_id: Some("turn_launchroom_0001".to_string()),
            user_request: "@everyone ship it".to_string(),
            interaction_mode: crate::runtime::InteractionMode::Execute,
            permission_mode: Some(crate::tools::PermissionMode::Workspace),
            yolo: None,
            workspace: None,
            journal: true,
            target_agent: None,
            target_group: Some("launch-room".to_string()),
            group_activation: Some(intent.clone()),
            delivery: TurnDelivery::Queue,
            sticky_notes: None,
            viewport: None,
            attachments: None,
        };
        let mut turn_json = serde_json::to_value(&turn).unwrap();
        assert_eq!(
            turn_json["Turn"]["group_activation"],
            serde_json::json!({
                "group_id": "launch-room",
                "roster_fingerprint": "roster-v1",
                "selection": "everyone",
                "active_agent_ids": ["planner", "coder"],
                "execution_mode": "parallel",
                "execution_waves": [["planner", "coder"]]
            })
        );
        match serde_json::from_value::<WireRequest>(turn_json.clone()).unwrap() {
            WireRequest::Turn {
                target_group,
                group_activation,
                ..
            } => {
                assert_eq!(target_group.as_deref(), Some("launch-room"));
                assert_eq!(group_activation, Some(intent.clone()));
            }
            other => panic!("unexpected request: {other:?}"),
        }

        // Current Canvas no longer authors Plan/Execute state. Omitting the
        // historical field must still decode as the one default behavior.
        let mut default_mode_json = turn_json.clone();
        default_mode_json["Turn"]
            .as_object_mut()
            .unwrap()
            .remove("interaction_mode");
        match serde_json::from_value::<WireRequest>(default_mode_json).unwrap() {
            WireRequest::Turn {
                interaction_mode,
                ..
            } => assert_eq!(interaction_mode, crate::runtime::InteractionMode::Execute),
            other => panic!("unexpected request: {other:?}"),
        }

        turn_json["Turn"]
            .as_object_mut()
            .unwrap()
            .remove("group_activation");
        turn_json["Turn"].as_object_mut().unwrap().remove("turn_id");
        assert!(matches!(
            serde_json::from_value::<WireRequest>(turn_json).unwrap(),
            WireRequest::Turn {
                group_activation: None,
                turn_id: None,
                ..
            }
        ));

        let preview = GroupActivationPreview {
            inspection_participants: Default::default(),
            tool_constraints: Default::default(),
            group_id: "launch-room".to_string(),
            roster_fingerprint: "roster-v1".to_string(),
            selection: GroupActivationSelection::Everyone,
            active_agent_ids: vec!["planner".to_string(), "coder".to_string()],
            active_display_names: vec!["Plan".to_string(), "Build".to_string()],
            execution_mode: GroupExecutionMode::Parallel,
            execution_dependencies: None,
            execution_waves: vec![vec!["planner".to_string(), "coder".to_string()]],
            execution_wave_display_names: vec![vec!["Plan".to_string(), "Build".to_string()]],
        };
        let response_json =
            serde_json::to_value(WireResponse::GroupActivationPreview(preview.clone())).unwrap();
        assert_eq!(
            response_json["GroupActivationPreview"],
            serde_json::to_value(&preview).unwrap()
        );
        assert!(matches!(
            serde_json::from_value::<WireResponse>(response_json).unwrap(),
            WireResponse::GroupActivationPreview(decoded) if decoded == preview
        ));
    }

    #[test]
    fn group_activation_rejects_substitution_and_stale_authoritative_rosters() {
        use crate::runtime::company::{CompanyStore, GroupMemberInput};
        use crate::runtime::company_directory::{GroupProfile, HistoryAccess, LifecycleState};

        let home = tempfile::tempdir().expect("isolated Phoenix home");
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let store = CompanyStore::open(home.path().join("company/company.sqlite")).unwrap();
        store.ensure_full_catalog_team().unwrap();
        let initial = store.directory_snapshot().unwrap();
        let active_ids = initial
            .agents
            .iter()
            .filter(|agent| agent.profile.lifecycle == LifecycleState::Active)
            .map(|agent| agent.profile.agent_id.clone())
            .take(3)
            .collect::<Vec<_>>();
        assert!(
            active_ids.len() >= 3,
            "fixture needs three active coworkers"
        );
        let group_id = "activation-validation-room";
        store
            .create_group(
                "user",
                GroupProfile {
                    group_id: group_id.to_string(),
                    name: "Activation validation".to_string(),
                    description: "Wire-bound activation fixture".to_string(),
                    color: "#112233".to_string(),
                    icon_seed: "activation-validation".to_string(),
                    lifecycle: LifecycleState::Active,
                    pinned: false,
                    sort_order: 1,
                    canonical_session_id: Some("group-activation-validation-room".to_string()),
                    metadata_json: "{}".to_string(),
                    leader_agent_id: None,
                },
                active_ids[..2].to_vec(),
            )
            .unwrap();

        let snapshot = store.directory_snapshot().unwrap();
        let original_prompt = format!("@{} investigate", active_ids[0]);
        let preview = crate::runtime::group_conversation::preview_group_activation(
            &snapshot,
            group_id,
            &original_prompt,
        )
        .unwrap();
        let intent = preview.intent();
        assert_eq!(
            super::validated_group_activation_intent_against_snapshot(
                &snapshot,
                group_id,
                &original_prompt,
                Some(&intent),
            )
            .unwrap(),
            Some(intent.clone())
        );

        let substituted_prompt = format!("@{} investigate", active_ids[1]);
        let substitution = super::validated_group_activation_intent_against_snapshot(
            &snapshot,
            group_id,
            &substituted_prompt,
            Some(&intent),
        )
        .unwrap_err();
        assert!(substitution.to_string().contains("group mentions changed"));

        store
            .set_group_members(
                "user",
                group_id,
                vec![
                    GroupMemberInput {
                        agent_id: active_ids[0].clone(),
                        member_role: "member".to_string(),
                        history_access: HistoryAccess::Full,
                    },
                    GroupMemberInput {
                        agent_id: active_ids[2].clone(),
                        member_role: "member".to_string(),
                        history_access: HistoryAccess::Full,
                    },
                ],
            )
            .unwrap();
        let changed = store.directory_snapshot().unwrap();
        let stale = super::validated_group_activation_intent_against_snapshot(
            &changed,
            group_id,
            &original_prompt,
            Some(&intent),
        )
        .unwrap_err();
        assert!(stale.to_string().contains("group roster changed"));

        let non_group =
            super::validated_group_activation_intent(None, &original_prompt, Some(&intent))
                .unwrap_err();
        assert!(non_group.to_string().contains("non-group conversation"));
    }

    #[test]
    fn no_queue_in_the_group_send_path() {
        // Every Send while working is delivered, never queued: one-to-one
        // conversations steer into the running turn, group rooms route the
        // message to the running/idle members (`route_room_message`).
        for target in [None, Some("launch-room")] {
            for requested in [TurnDelivery::Queue, TurnDelivery::Steer] {
                validate_turn_delivery(target, requested).expect("every delivery is accepted");
                assert_eq!(effective_turn_delivery(target, requested), TurnDelivery::Steer);
            }
        }
        // The busy-room branch of the Turn handler routes and returns before
        // the durable FIFO; the old group queue lane is gone from the send path.
        let source = include_str!("daemon.rs");
        let handler = &source[source.find("WireRequest::Turn {\n            session_id,").unwrap()..];
        let handler = &handler[..handler.find("// A client that doesn't know the session's workspace").unwrap()];
        let room = handler.find("if let Some(group_id) = target_group.as_deref() {").unwrap();
        let queue = handler.find("super::turn_queue::enqueue(").unwrap();
        assert!(room < queue, "a busy room must be routed before any queueing");
        assert!(handler[room..queue].contains("route_room_message("));
        assert!(!handler.contains(concat!("group_queue", "_lane(")));
        assert!(!handler.contains("Queued after the current group turn"));
    }

    #[test]
    fn mid_turn_user_message_belongs_to_its_running_turn_for_deletion() {
        use crate::session::{Message, Session};

        let mut session = Session::new_main_with_id("delete-steered-turn", "model", "system");
        for message in [
            Message::User {
                content: "keep prompt".into(),
            },
            Message::Assistant {
                content: "keep answer".into(),
            },
            Message::User {
                content: "running prompt".into(),
            },
            Message::ToolResult {
                tool_name: "read".into(),
                input: "{}".into(),
                success: true,
                output: "first step".into(),
            },
            Message::User {
                content: crate::runtime::postbox::user_steer_content("use the blue palette"),
            },
            Message::Assistant {
                content: "done in blue".into(),
            },
        ] {
            session.push_message(message);
        }
        let steered = session.messages[4].clone();
        assert!(!super::is_authored_transcript_user(&steered));
        // "running prompt" is still the newest authored boundary (0 from end);
        // the steer is not counted as a turn of its own.
        super::delete_transcript_turn_from_session(
            &mut session,
            0,
            "running prompt",
            TranscriptDeleteScope::Prompt,
        )
        .expect("the steered message does not shift turn ordinals");
        assert!(!session.messages.iter().any(|message| matches!(
            message,
            Message::User { content } if content.contains("use the blue palette")
        )));
        assert!(session.messages.iter().any(
            |message| matches!(message, Message::User { content } if content == "keep prompt")
        ));
    }

    #[test]
    fn race_at_turn_end_hands_the_user_message_to_exactly_one_new_turn() {
        let session_id = format!("steer-race-{}", uuid::Uuid::new_v4().simple());
        assert!(crate::runtime::postbox::steer_user(
            &session_id,
            "orchestrator",
            Some("turn_race_0001"),
            "actually, stop after the outline",
        ));
        // A reconnect retry of the same composer message is not parked twice.
        assert!(!crate::runtime::postbox::steer_user(
            &session_id,
            "orchestrator",
            Some("turn_race_0001"),
            "actually, stop after the outline",
        ));
        assert!(crate::runtime::postbox::user_steer_seen(&session_id, "turn_race_0001"));
        // The live turn ended without draining: the wake adopts it as the new
        // turn's request ...
        let adopted = crate::runtime::postbox::take_first_user_steer(&session_id, "orchestrator")
            .expect("parked user message becomes the next turn");
        assert_eq!(adopted.body, "actually, stop after the outline");
        // ... and nothing remains for that turn's round-top drain to inject again.
        assert!(!crate::runtime::postbox::has_pending_steer(&session_id, "orchestrator"));
        assert!(crate::runtime::postbox::take_steer(&session_id, "orchestrator").is_empty());
    }

    #[test]
    fn teach_workflow_is_sessionless_and_redacts_typed_values_from_debug() {
        let request = WireRequest::TeachWorkflow(
            crate::runtime::workflow_teaching::TeachWorkflowCommand::Interact {
                teaching_id: "teach-123".to_string(),
                browser_action: crate::tools::browser_native::BrowserUserAction::Type {
                    text: "do-not-log-this-password".to_string(),
                    clear: true,
                    sensitive: true,
                    parameter_name: Some("password".to_string()),
                    target_hint: None,
                },
            },
        );
        let debug = format!("{request:?}");
        assert!(!debug.contains("do-not-log-this-password"));
        assert!(debug.contains("[REDACTED]"));
        let encoded = serde_json::to_string(&request).unwrap();
        let decoded: WireRequest = serde_json::from_str(&encoded).unwrap();
        assert!(super::validate_wire_request_session_ids(&decoded).is_ok());
        assert!(matches!(decoded, WireRequest::TeachWorkflow(_)));
    }

    #[test]
    fn successful_ask_answer_has_a_non_error_wire_acknowledgement() {
        let response = WireResponse::AskAnswered {
            disposition: "delivered".to_string(),
            continuation_turn_id: None,
        };
        let encoded = serde_json::to_value(response).unwrap();
        assert!(encoded.get("AskAnswered").is_some(), "{encoded}");
        assert!(encoded.get("Error").is_none(), "{encoded}");
        assert!(encoded["AskAnswered"].get("continuation_turn_id").is_none());
        let legacy: WireResponse = serde_json::from_value(serde_json::json!({
            "AskAnswered":{"disposition":"delivered"}
        })).unwrap();
        assert!(matches!(legacy, WireResponse::AskAnswered { continuation_turn_id: None, .. }));
        let resumed = WireResponse::AskAnswered {
            disposition: "late_answer_queued:queue-1".into(),
            continuation_turn_id: Some("ask_answer_stable".into()),
        };
        let encoded = serde_json::to_value(resumed).unwrap();
        assert_eq!(encoded["AskAnswered"]["continuation_turn_id"], "ask_answer_stable");
    }

    #[test]
    fn direct_browser_login_input_is_typed_and_redacted() {
        let request = WireRequest::BrowserInteract {
            instance: "nico".to_string(),
            browser_action: crate::tools::browser_native::BrowserUserAction::Type {
                text: "never-print-this-login-secret".to_string(),
                clear: false,
                sensitive: true,
                parameter_name: Some("password".to_string()),
                target_hint: None,
            },
        };
        let debug = format!("{request:?}");
        assert!(!debug.contains("never-print-this-login-secret"));
        assert!(debug.contains("[REDACTED]"));
        let encoded = serde_json::to_string(&request).unwrap();
        let decoded: WireRequest = serde_json::from_str(&encoded).unwrap();
        assert!(validate_wire_request_session_ids(&decoded).is_ok());
        assert!(matches!(decoded, WireRequest::BrowserInteract { .. }));
    }

    #[test]
    fn native_browser_surface_wire_shape_is_stable_and_sessionless() {
        let request = WireRequest::BrowserSurface {
            instance: "agent-nico".to_string(),
            action: crate::tools::browser_native::BrowserSurfaceAction::Open,
        };
        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(
            encoded,
            serde_json::json!({
                "BrowserSurface": {
                    "instance": "agent-nico",
                    "action": "open"
                }
            })
        );
        let decoded: WireRequest = serde_json::from_value(encoded).unwrap();
        assert!(validate_wire_request_session_ids(&decoded).is_ok());
        assert!(matches!(
            decoded,
            WireRequest::BrowserSurface {
                action: crate::tools::browser_native::BrowserSurfaceAction::Open,
                ..
            }
        ));

        let response =
            WireResponse::BrowserSurface(crate::tools::browser_native::BrowserSurfaceReply {
                supported: true,
                instance: "agent-nico".to_string(),
                pid: Some(4242),
                window_token: Some("abc123".to_string()),
                url: "https://example.com/".to_string(),
                attached: true,
                tabs: Vec::new(),
            });
        let debug = format!("{response:?}");
        assert!(!debug.contains("abc123"));
        assert!(debug.contains("[REDACTED]"));
        assert_eq!(
            serde_json::to_value(response).unwrap(),
            serde_json::json!({
                "BrowserSurface": {
                    "supported": true,
                    "instance": "agent-nico",
                    "pid": 4242,
                    "window_token": "abc123",
                    "url": "https://example.com/",
                    "attached": true,
                    // The tab strip travels with the reply. `serde(default)`
                    // only relaxes decoding, so the field is always encoded —
                    // empty here because this reply carries no tabs.
                    "tabs": []
                }
            })
        );
    }

    #[test]
    fn browser_surface_tab_carries_the_page_reported_favicon() {
        // Canvas draws the tab strip from this shape. The icon has to survive
        // the wire, and a page that declares none must encode as an empty
        // string rather than vanishing, so the UI can fall back to its own mark.
        let tab = crate::tools::browser_native::BrowserSurfaceTab {
            id: "target-1".to_string(),
            title: "Home / X".to_string(),
            url: "https://x.com/home".to_string(),
            active: true,
            favicon: "https://abs.twimg.com/favicons/twitter.ico".to_string(),
        };
        let encoded = serde_json::to_value(&tab).unwrap();
        assert_eq!(
            encoded["favicon"],
            serde_json::json!("https://abs.twimg.com/favicons/twitter.ico")
        );
        let decoded: crate::tools::browser_native::BrowserSurfaceTab =
            serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.favicon, tab.favicon);

        // A shell that predates the favicon field still decodes.
        let legacy: crate::tools::browser_native::BrowserSurfaceTab = serde_json::from_value(
            serde_json::json!({"id":"t","title":"","url":"about:blank","active":false}),
        )
        .unwrap();
        assert_eq!(legacy.favicon, "");
    }

    #[test]
    fn settings_is_a_typed_sessionless_wire_operation() {
        let request = WireRequest::Settings(crate::settings::SettingsCommand::Snapshot {
            scope: crate::settings::SettingsScope::Agent {
                id: "iris".to_string(),
            },
        });
        let encoded = serde_json::to_string(&request).unwrap();
        let decoded: WireRequest = serde_json::from_str(&encoded).unwrap();
        assert!(validate_wire_request_session_ids(&decoded).is_ok());
        assert!(matches!(decoded, WireRequest::Settings(_)));
    }

    #[test]
    fn every_wire_session_id_rejects_traversal_and_absolute_paths() {
        for session_id in ["../escape", "/tmp/absolute", r"C:\absolute", ".", "a/b"] {
            for request in [
                WireRequest::CompanySnapshot {
                    session_id: Some(session_id.to_string()),
                    owner: None,
                },
                WireRequest::Turn {
                    session_id: session_id.to_string(),
                    turn_id: None,
                    user_request: "hello".to_string(),
                    interaction_mode: crate::runtime::InteractionMode::Execute,
                    permission_mode: None,
                    yolo: None,
                    workspace: None,
                    journal: false,
                    target_agent: None,
                    target_group: None,
                    group_activation: None,
                    delivery: TurnDelivery::Queue,
                    sticky_notes: None,
                    viewport: None,
                    attachments: None,
                },
                WireRequest::Subscribe {
                    session_id: session_id.to_string(),
                    owner: None,
                },
                WireRequest::SubscribeJournal {
                    session_id: session_id.to_string(),
                    owner: None,
                },
                WireRequest::DigestSession {
                    session_id: session_id.to_string(),
                    owner: None,
                },
                WireRequest::Cancel {
                    session_id: session_id.to_string(),
                    target_agent: None,
                    owner: None,
                },
                WireRequest::AnswerAsk {
                    ask_id: "ask-invalid-scope".to_string(),
                    answer: "Approve".to_string(),
                    session_id: Some(session_id.to_string()),
                    owner: None,
                },
                WireRequest::DismissAsk {
                    ask_id: "ask-invalid-scope".to_string(),
                    session_id: Some(session_id.to_string()),
                    owner: None,
                },
            ] {
                assert!(
                    validate_wire_request_session_ids(&request).is_err(),
                    "wire boundary must reject {session_id:?}"
                );
            }
        }
        assert!(validate_wire_request_session_ids(&WireRequest::Subscribe {
            session_id: "main-safe_123".to_string(),
            owner: None,
        })
        .is_ok());
    }

    #[test]
    fn scoped_wire_requests_reject_mismatched_canonical_owners() {
        use crate::runtime::agent_conversation::{ConversationOwnerKind, ConversationOwnerRef};
        use crate::runtime::company_directory::{
            founding_team_profiles, AgentRecord, DirectorySnapshot, GroupProfile, GroupRecord,
            LifecycleState,
        };

        let mut snapshot = DirectorySnapshot {
            agents: founding_team_profiles(true)
                .into_iter()
                .map(|mut profile| {
                    profile.canonical_session_id = Some(format!("agent-{}", profile.agent_id));
                    AgentRecord {
                        profile,
                        archived_at: None,
                        delete_after: None,
                        created_at: "now".into(),
                        updated_at: "now".into(),
                        as_of_seq: 1,
                    }
                })
                .collect(),
            ..DirectorySnapshot::default()
        };
        snapshot.groups.push(GroupRecord {
            profile: GroupProfile {
                group_id: "launch-room".into(),
                name: "Launch room".into(),
                description: "Coordinate launch work".into(),
                color: "#112233".into(),
                icon_seed: "launch-room".into(),
                lifecycle: LifecycleState::Active,
                pinned: false,
                sort_order: 0,
                canonical_session_id: Some("group-launch-room".into()),
                metadata_json: "{}".into(),
                leader_agent_id: None,
            },
            archived_at: None,
            delete_after: None,
            created_at: "now".into(),
            updated_at: "now".into(),
            as_of_seq: 1,
        });

        let group_owner = ConversationOwnerRef {
            kind: ConversationOwnerKind::Group,
            id: "launch-room".into(),
        };
        let finance_owner = ConversationOwnerRef {
            kind: ConversationOwnerKind::Agent,
            id: "finance".into(),
        };
        let scoped_group_read = |owner| WireRequest::SubscribeJournal {
            session_id: "group-launch-room".into(),
            owner,
        };

        authorize_wire_request_owner_against_snapshot(
            &scoped_group_read(Some(group_owner.clone())),
            &snapshot,
        )
        .expect("the canonical group owner is authorized");
        assert!(authorize_wire_request_owner_against_snapshot(
            &scoped_group_read(Some(finance_owner.clone())),
            &snapshot,
        )
        .is_err());
        authorize_wire_request_owner_against_snapshot(&scoped_group_read(None), &snapshot)
            .expect("legacy clients remain compatible while Canvas always supplies an owner");

        for (owner, allowed) in [(group_owner.clone(), true), (finance_owner.clone(), false)] {
            let cancel = WireRequest::CancelQueuedTurn {
                session_id: "group-launch-room".into(),
                queue_id: "queued_owner_check".into(),
                owner: Some(owner),
            };
            assert_eq!(authorize_wire_request_owner_against_snapshot(&cancel, &snapshot).is_ok(), allowed,
                "cancel must pass canonical ownership before removal or wake");
        }

        let scoped_ask = |owner, session_id: &str| WireRequest::AnswerAsk {
            ask_id: "ask-launch-approval".into(),
            answer: "Approve".into(),
            session_id: Some(session_id.into()),
            owner,
        };
        authorize_ask_request_owner_against_snapshot(
            &scoped_ask(Some(group_owner.clone()), "group-launch-room"),
            Some("group-launch-room"),
            &snapshot,
        )
        .expect("the ask owner, submitted session, and durable ask scope all agree");
        assert!(authorize_ask_request_owner_against_snapshot(
            &scoped_ask(Some(finance_owner), "group-launch-room"),
            Some("group-launch-room"),
            &snapshot,
        )
        .is_err());
        assert!(
            authorize_ask_request_owner_against_snapshot(
                &scoped_ask(Some(group_owner), "group-launch-room"),
                Some("agent-finance"),
                &snapshot,
            )
            .is_err(),
            "an ask id cannot be substituted across sessions"
        );
        assert!(
            authorize_ask_request_owner_against_snapshot(
                &scoped_ask(None, "agent-finance"),
                Some("group-launch-room"),
                &snapshot,
            )
            .is_err(),
            "an explicit session must match even when the owner is omitted"
        );
        authorize_ask_request_owner_against_snapshot(
            &scoped_ask(None, "group-launch-room"), Some("group-launch-room"), &snapshot,
        ).expect("a matching session assertion does not require a newer owner field");
        assert!(authorize_ask_request_owner_against_snapshot(
            &scoped_ask(None, "group-launch-room"), None, &snapshot,
        ).is_err(), "an explicit session cannot resolve an unscoped ask");
        let legacy = WireRequest::AnswerAsk {
            ask_id: "legacy-ask".into(), answer: "yes".into(), session_id: None, owner: None,
        };
        authorize_ask_request_owner_against_snapshot(&legacy, None, &snapshot)
            .expect("legacy clients that assert no scope remain compatible");
        let dismiss = WireRequest::DismissAsk {
            ask_id: "ask-launch-approval".into(), session_id: Some("agent-finance".into()), owner: None,
        };
        assert!(authorize_ask_request_owner_against_snapshot(
            &dismiss, Some("group-launch-room"), &snapshot,
        ).is_err(), "dismissal must obey the same explicit conversation boundary");
    }

    #[test]
    fn one_tool_permission_decision_clears_identical_live_cards_only() {
        let home = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let session = "group-room-perm-test";
        let card = |fingerprint: &str| crate::tools::ask_user::ApprovalRequest {
            action: "tool_permission".into(), subject: "bash".into(), approved_option: "Allow once".into(),
            details: [("action_fingerprint".to_string(), fingerprint.to_string()), ("tool_name".to_string(), "bash".to_string())].into_iter().collect(),
        };
        let questions = [crate::tools::ask_user::AskUserQuestion {
            question: "Robin wants to use `bash` for npm install. Allow Full access for this call?".into(), header: Some("Permission".into()),
            options: vec!["Allow once".into(), "Keep current access".into()], multi_select: false,
        }];
        let _first = crate::runtime::asks::register_with_payload("permission-aaaa0001", session, "coder", &questions, Some(&card("fp-npm")));
        let mut twin = crate::runtime::asks::register_with_payload("permission-aaaa0002", session, "coder", &questions, Some(&card("fp-npm")));
        let mut other = crate::runtime::asks::register_with_payload("permission-aaaa0003", session, "coder", &questions, Some(&card("fp-rm")));
        let mut other_agent = crate::runtime::asks::register_with_payload("permission-aaaa0004", session, "frontend", &questions, Some(&card("fp-npm")));
        let record = crate::runtime::asks::decision_record_for("permission-aaaa0001").unwrap();
        assert!(crate::runtime::asks::answer("permission-aaaa0001", "Allow once".into()));
        super::settle_matching_tool_permission_asks(record.as_ref(), "Allow once");
        assert_eq!(twin.try_recv().unwrap(), "Allow once", "the identical exact action is cleared by one approval");
        assert!(other.try_recv().is_err(), "a different action keeps its own card");
        assert!(other_agent.try_recv().is_err(), "another coworker keeps its own card");
    }

    #[tokio::test]
    async fn ownerless_question_answers_obey_the_explicit_durable_session() {
        let home = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let ask_id = "channel-scope-regression";
        let mut receiver = crate::runtime::asks::register_with_payload(
            ask_id, "agent-avery", "avery",
            &[crate::tools::ask_user::AskUserQuestion {
                question: "Which folder?".into(), header: None,
                options: vec!["School".into(), "Work".into()], multi_select: false,
            }], None,
        );
        let answer = |session: &str| WireRequest::AnswerAsk {
            ask_id: ask_id.into(), answer: "School".into(),
            session_id: Some(session.into()), owner: None,
        };
        assert!(super::authorize_wire_request_owner(&answer("agent-finance")).is_err());
        assert!(matches!(receiver.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
        assert_eq!(crate::runtime::asks::record_for(ask_id).unwrap().status, "pending");
        super::authorize_wire_request_owner(&answer("agent-avery")).unwrap();
        assert!(crate::runtime::asks::answer(ask_id, "School".into()));
        assert_eq!(receiver.await.unwrap(), "School");
        super::authorize_wire_request_owner(&answer("agent-avery")).unwrap();
        assert!(super::authorize_wire_request_owner(&answer("agent-finance")).is_err());
        assert!(matches!(super::resolved_answer_ack(ask_id, "School").unwrap(),
            Some(WireResponse::AskAnswered { continuation_turn_id: None, .. })));
        assert!(super::resolved_answer_ack(ask_id, "Work").is_err());
    }

    #[test]
    fn archived_answer_retry_returns_receipt_without_requeueing() {
        use crate::runtime::asks;
        use crate::cli::turn_queue;
        let home = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let (session, ask_id, turn_id) = ("agent-avery", "retry-receipt", "exact-answer-successor");
        let rx = asks::register(ask_id, session);
        drop(rx);
        assert!(super::resolved_answer_ack(ask_id, "Blue").unwrap().is_none());
        let payload = turn_queue::frozen_answer_turn(session, ask_id, "Blue", || {
            Ok(serde_json::from_value(serde_json::json!({
                "turn_id":turn_id,"user_request":"Use Blue","target_agent":"avery",
                "origin":{"kind":"ask_answer","ask_id":ask_id,"agent_id":"avery","display":"Blue"}
            }))?)
        }).unwrap();
        let queue_id = turn_queue::enqueue(session, &payload).unwrap();
        asks::archive_late_answer(ask_id, "Blue");
        let expected = serde_json::json!({"AskAnswered":{
            "disposition":format!("late_answer_queued:{queue_id}"),"continuation_turn_id":turn_id
        }});
        let receipt = || serde_json::to_value(super::resolved_answer_ack(ask_id, "Blue").unwrap().unwrap()).unwrap();
        assert_eq!(receipt(), expected);
        assert_eq!(turn_queue::list(session).unwrap().len(), 1);
        assert_eq!(turn_queue::claim_next(session).unwrap().unwrap().queue_id, queue_id);
        assert_eq!(receipt(), expected);
        turn_queue::complete(&queue_id).unwrap();
        assert_eq!(receipt(), expected);
        assert!(super::resolved_answer_ack(ask_id, "Orange").is_err());
        assert!(turn_queue::list(session).unwrap().is_empty());
        assert!(turn_queue::claim_next(session).unwrap().is_none());
        asks::abandon(ask_id);
    }

    #[test]
    fn resolved_question_without_submission_cannot_be_recreated() {
        use crate::runtime::asks;
        let home = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        drop(asks::register("missing-receipt", "agent-avery"));
        asks::archive_late_answer("missing-receipt", "Blue");
        assert!(super::resolved_answer_ack("missing-receipt", "Blue").is_err());
        drop(asks::register("dismissed-receipt", "agent-avery"));
        asks::dismiss("dismissed-receipt");
        assert!(super::resolved_answer_ack("dismissed-receipt", "Not now").is_err());
        assert!(crate::cli::turn_queue::list("agent-avery").unwrap().is_empty());
        asks::abandon("missing-receipt");
    }

    #[tokio::test]
    async fn initial_unix_json_line_is_bounded_before_allocation() {
        let oversized = b"123456789";
        let mut reader = tokio::io::BufReader::new(&oversized[..]);
        let error = read_bounded_utf8_line(&mut reader, 8)
            .await
            .expect_err("unterminated oversized line must fail");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

        let exact = b"12345678\ntrailing";
        let mut reader = tokio::io::BufReader::new(&exact[..]);
        assert_eq!(
            read_bounded_utf8_line(&mut reader, 8).await.unwrap(),
            Some("12345678".to_string())
        );

        let invalid_utf8 = [0xff, b'\n'];
        let mut reader = tokio::io::BufReader::new(&invalid_utf8[..]);
        assert!(read_bounded_utf8_line(&mut reader, 8).await.is_err());
    }

    /// The fork-bomb guard, pinned.
    ///
    /// `ensure_cold_start_is_detached` below deliberately stops the gateway and
    /// re-spawns it, so under `cargo test` the spawn path runs for real with
    /// `current_exe()` pointing at `target/debug/deps/phoenix-<hash>`. When that
    /// was accepted as "the phoenix binary", the test suite spawned ITSELF
    /// detached (setsid + double-fork, reparented to init), ran this test again,
    /// and spawned again — 2,465 orphans holding 10.4 GB on 2026-07-25, which
    /// took the machine to 166 MB free.
    ///
    /// Every generation escaped the harness's process group, so nothing cargo
    /// did could reap them. If this assertion ever flips, `cargo test` becomes a
    /// machine-killer again, and it will not look like a test failure — it will
    /// look like the box dying.
    #[test]
    fn a_cargo_test_binary_is_never_spawned_as_the_gateway() {
        use super::is_cargo_test_binary;
        use std::path::Path;

        for exe in [
            "/home/u/proj/target/debug/deps/phoenix-b858c4053d38a2d2",
            "/home/u/proj/target/release/deps/phoenix-0123456789abcdef",
        ] {
            let p = Path::new(exe);
            let name = p.file_name().unwrap().to_str().unwrap();
            assert!(
                is_cargo_test_binary(p, name),
                "must be refused as a spawn target: {exe}"
            );
        }

        // The real CLI, in every shape it legitimately ships in, must still be
        // spawnable — a guard that rejects everything silently falls through to
        // PATH and breaks gateway autostart instead.
        for exe in [
            "/home/u/.local/bin/phoenix",
            "/usr/bin/phoenix",
            "/home/u/proj/target/release/phoenix",
            "/home/u/proj/target/debug/phoenix",
        ] {
            let p = Path::new(exe);
            let name = p.file_name().unwrap().to_str().unwrap();
            assert!(
                !is_cargo_test_binary(p, name),
                "the real CLI must remain spawnable: {exe}"
            );
        }
    }

    #[tokio::test]
    async fn readiness_uses_protocol_not_build_profile_or_executable_content() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        async fn serve_probe(listener: tokio::net::UnixListener, protocol: &str, binary_id: &str) {
            let (stream, _) = listener.accept().await.expect("accept probe");
            let (read_half, mut write_half) = stream.into_split();
            let request = BufReader::new(read_half)
                .lines()
                .next_line()
                .await
                .expect("read probe")
                .expect("probe line");
            assert!(matches!(
                serde_json::from_str::<WireRequest>(&request),
                Ok(WireRequest::ProtocolInfo)
            ));
            let mut response = serde_json::to_string(&WireResponse::ProtocolInfo {
                protocol: protocol.to_string(),
                package_version: "test".to_string(),
                binary_id: Some(binary_id.to_string()),
            })
            .expect("serialize response");
            response.push('\n');
            write_half
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        }

        let temp = tempfile::tempdir().expect("tempdir");
        let compatible_socket = temp.path().join("compatible.sock");
        let compatible_listener =
            tokio::net::UnixListener::bind(&compatible_socket).expect("bind compatible socket");
        let compatible_server = tokio::spawn(serve_probe(
            compatible_listener,
            GATEWAY_WIRE_PROTOCOL,
            "release-build-with-different-bytes",
        ));
        assert_eq!(
            gateway_probe_at(&compatible_socket).await,
            GatewayProbe::Compatible,
            "same-protocol debug/release builds must not trigger replacement"
        );
        compatible_server.await.expect("server task");

        let stale_socket = temp.path().join("stale.sock");
        let stale_listener =
            tokio::net::UnixListener::bind(&stale_socket).expect("bind stale socket");
        let stale_server = tokio::spawn(serve_probe(
            stale_listener,
            "phoenix-gateway-jsonl/stale-protocol",
            "same-or-different-build-is-irrelevant",
        ));
        assert!(
            matches!(
                gateway_probe_at(&stale_socket).await,
                GatewayProbe::Incompatible(_)
            ),
            "a stale wire protocol must still trigger safe replacement"
        );
        stale_server.await.expect("server task");
    }

    #[tokio::test]
    async fn transient_probe_failures_retry_then_fail_unresponsive() {
        let temp = tempfile::tempdir().expect("tempdir");
        let socket = temp.path().join("unresponsive.sock");
        let listener = tokio::net::UnixListener::bind(&socket).expect("bind socket");
        let server = tokio::spawn(async move {
            for _ in 0..PROTOCOL_PROBE_ATTEMPTS {
                let (_stream, _) = listener.accept().await.expect("accept probe");
                // Closing without a protocol response is a transport failure,
                // never affirmative evidence that this daemon is incompatible.
            }
        });
        assert!(matches!(
            gateway_probe_confirmed_at(&socket).await,
            GatewayProbe::Unresponsive(_)
        ));
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn lifecycle_lock_serializes_competing_supervisors_and_stays_private() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("gateway-lifecycle");
        let first = GatewayLifecycleLock::acquire_at(&path, std::time::Duration::from_secs(1))
            .await
            .expect("first lifecycle lease");
        let second =
            GatewayLifecycleLock::acquire_at(&path, std::time::Duration::from_millis(25)).await;
        assert!(
            second.is_err(),
            "a second supervisor must not enter stop/probe/spawn concurrently"
        );
        drop(first);
        GatewayLifecycleLock::acquire_at(&path, std::time::Duration::from_secs(1))
            .await
            .expect("kernel releases lease when owner drops");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path)
                    .expect("lock metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600,
                "the lifecycle lease carries process identity and stays private"
            );
        }
    }

    #[tokio::test]
    async fn concurrent_starters_never_unlink_the_winner_after_a_stale_socket() {
        let temp = tempfile::tempdir().expect("tempdir");
        let lifecycle = temp.path().join("gateway-lifecycle");
        let socket = temp.path().join("gateway.sock");
        let pidfile = temp.path().join("gateway.pid");
        drop(tokio::net::UnixListener::bind(&socket).expect("create stale socket"));

        let contender = |lifecycle: std::path::PathBuf,
                         socket: std::path::PathBuf,
                         pidfile: std::path::PathBuf| async move {
            let _lease =
                GatewayLifecycleLock::acquire_at(&lifecycle, std::time::Duration::from_secs(2))
                    .await?;
            claim_gateway_listener_locked(&socket, &pidfile).await
        };
        let first = contender(lifecycle.clone(), socket.clone(), pidfile.clone());
        let second = contender(lifecycle, socket.clone(), pidfile.clone());
        let (first, second) = tokio::join!(first, second);

        let (winner, loser) = match (first, second) {
            (Ok(listener), Err(error)) | (Err(error), Ok(listener)) => (listener, error),
            (Ok(_), Ok(_)) => panic!("only one starter may publish the gateway socket"),
            (Err(first), Err(second)) => {
                panic!("one starter must win: first={first:#}; second={second:#}")
            }
        };
        assert!(
            loser.to_string().contains("already running"),
            "the loser must observe the winner rather than unlink it: {loser:#}"
        );
        assert!(
            tokio::net::UnixStream::connect(&socket).await.is_ok(),
            "the losing starter must leave the winning endpoint connectable"
        );
        assert_eq!(
            std::fs::read_to_string(&pidfile)
                .expect("published pidfile")
                .trim(),
            std::process::id().to_string()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&socket)
                    .expect("gateway socket metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600,
                "the gateway endpoint itself must be owner-only regardless of umask"
            );
        }
        drop(winner);
    }

    #[tokio::test]
    async fn pidfile_publication_failure_withdraws_the_bound_socket() {
        let temp = tempfile::tempdir().expect("tempdir");
        let socket = temp.path().join("gateway.sock");
        let listener = tokio::net::UnixListener::bind(&socket).expect("bind test gateway");
        let pidfile = temp.path().join("gateway.pid");
        std::fs::create_dir(&pidfile).expect("make pidfile replacement fail");

        let error = match publish_gateway_pid(listener, &socket, &pidfile) {
            Ok(_) => panic!("pid publication must fail against a directory"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("could not publish its pidfile"),
            "failure must retain launch context: {error:#}"
        );
        assert!(
            !socket.exists(),
            "a listener without a verifiable pidfile must be withdrawn"
        );
        assert!(
            tokio::net::UnixStream::connect(&socket).await.is_err(),
            "the failed gateway must not remain connectable"
        );
    }

    #[test]
    fn stop_waits_for_the_exact_process_instance_not_socket_disappearance() {
        let temp = tempfile::tempdir().expect("tempdir");
        let proc_root = temp.path().join("proc");
        let process = proc_root.join("4242");
        std::fs::create_dir_all(&process).expect("fake process dir");
        let write_stat = |state: &str, start_time: &str| {
            let fields = (1..=18)
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
                .join(" ");
            std::fs::write(
                process.join("stat"),
                format!("4242 (phoenix (gateway)) {state} {fields} {start_time} 0\n"),
            )
            .expect("write fake proc stat");
        };

        write_stat("S", "9001");
        let start = process_start_time_at(&proc_root, 4242).expect("parse start time");
        assert_eq!(start, "9001", "comm spaces/parens must not shift fields");
        assert!(
            !process_instance_exited_at(&proc_root, 4242, Some(&start)),
            "a live process with the same start time is still the signalled gateway"
        );

        write_stat("S", "9002");
        assert!(
            process_instance_exited_at(&proc_root, 4242, Some(&start)),
            "PID reuse is exit of the verified process instance"
        );
        write_stat("Z", "9001");
        assert!(
            process_instance_exited_at(&proc_root, 4242, Some(&start)),
            "a zombie has exited and released its listeners even before its parent reaps it"
        );
        write_stat("X", "9001");
        assert!(
            process_instance_exited_at(&proc_root, 4242, Some(&start)),
            "a Linux dead-state process has released its listeners"
        );
        std::fs::write(process.join("stat"), "malformed").expect("write malformed fake stat");
        assert!(
            !process_instance_exited_at(&proc_root, 4242, Some(&start)),
            "an unreadable process identity is never proof that it exited"
        );
        std::fs::remove_file(process.join("stat")).expect("remove fake process");
        assert!(
            process_instance_exited_at(&proc_root, 4242, Some(&start)),
            "a missing proc entry is a completed exit"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn ensure_detects_a_rebuilt_binary_behind_the_same_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let proc_root = temp.path().join("proc");
        let process = proc_root.join("4242");
        std::fs::create_dir_all(&process).expect("fake proc process");
        let selected = temp.path().join("phoenix");
        let old = temp.path().join("old-phoenix");
        std::fs::write(&selected, b"new").expect("selected binary");
        std::fs::write(&old, b"old").expect("old binary");
        std::os::unix::fs::symlink(&old, process.join("exe")).expect("old proc exe");
        assert_eq!(
            process_uses_selected_executable_at(&proc_root, 4242, &selected),
            Some(false)
        );
        std::fs::remove_file(process.join("exe")).expect("replace proc exe");
        std::os::unix::fs::symlink(&selected, process.join("exe")).expect("new proc exe");
        assert_eq!(
            process_uses_selected_executable_at(&proc_root, 4242, &selected),
            Some(true)
        );
    }

    #[test]
    fn gateway_log_rotation_keeps_one_private_bounded_predecessor() {
        let temp = tempfile::tempdir().expect("tempdir");
        for name in ["gateway.log", "gateway-autostart.log"] {
            let log = temp.path().join(name);
            std::fs::write(&log, b"1234").expect("seed log");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o666))
                    .expect("make old log permissive");
            }
            rotate_log_if_oversized(&log, 4).expect("rotate");
            assert!(!log.exists(), "current path is reopened by the caller");
            let rotated = rotated_log_path(&log);
            assert_eq!(std::fs::read(&rotated).expect("rotated contents"), b"1234");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(rotated)
                        .expect("rotated metadata")
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600,
                    "rotated {name} contains the same sensitive data and stays private"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn fresh_gateway_home_and_both_logs_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("isolated-phoenix-home");
        for name in ["gateway.log", "gateway-autostart.log"] {
            let path = home.join(name);
            crate::config::private_io::prepare_private_parent_for(&path, &home)
                .expect("prepare fresh private home");
            drop(open_private_append_file(&path).expect("create private gateway log"));
            assert_eq!(
                std::fs::metadata(&path)
                    .expect("log metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600,
                "{name} must be private from creation"
            );
        }
        assert_eq!(
            std::fs::metadata(&home)
                .expect("home metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "fresh PHOENIX_HOME must not be exposed through the process umask"
        );
    }

    #[cfg(unix)]
    #[test]
    fn shared_custom_home_is_rejected_without_mutation() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("caller-owned-state-root");
        std::fs::create_dir(&home).expect("create custom home");
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o750))
            .expect("set caller-selected home mode");

        for name in ["gateway.log", "gateway-autostart.log"] {
            let path = home.join(name);
            std::fs::write(&path, b"old log").expect("seed old log");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666))
                .expect("make old log permissive");
            assert!(crate::config::private_io::prepare_private_parent_for(&path, &home).is_err());
            assert_eq!(
                std::fs::metadata(path)
                    .expect("log metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o666,
                "rejecting {name} must not mutate caller-owned state"
            );
        }
        assert_eq!(
            std::fs::metadata(home)
                .expect("custom home metadata")
                .permissions()
                .mode()
                & 0o777,
            0o750,
            "an unsafe custom PHOENIX_HOME must never be chmodded"
        );
    }

    #[cfg(unix)]
    #[test]
    fn private_custom_home_mode_is_preserved_while_logs_are_repaired() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("caller-owned-state-root");
        std::fs::create_dir(&home).expect("create custom home");
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))
            .expect("set private custom-home mode");

        for name in ["gateway.log", "gateway-autostart.log"] {
            let path = home.join(name);
            std::fs::write(&path, b"old log").expect("seed old log");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666))
                .expect("make old log permissive");
            crate::config::private_io::prepare_private_parent_for(&path, &home)
                .expect("prepare private custom home");
            drop(open_private_append_file(&path).expect("repair gateway log"));
            assert_eq!(
                std::fs::metadata(path)
                    .expect("log metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600,
                "{name} must be repaired to owner-only"
            );
        }
        assert_eq!(
            std::fs::metadata(home)
                .expect("custom home metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "a private custom PHOENIX_HOME keeps its caller-selected mode"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pidfile_must_name_phoenix_that_owns_the_exact_listener() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let proc_root = temp.path().join("proc");
        let process = proc_root.join("4242");
        std::fs::create_dir_all(proc_root.join("net")).expect("proc net");
        std::fs::create_dir_all(process.join("fd")).expect("proc fd");
        let socket = temp.path().join("gateway.sock");
        std::fs::write(
            proc_root.join("net/unix"),
            format!(
                "Num RefCount Protocol Flags Type St Inode Path\n000: 2 0 10000 1 01 7788 {}\n",
                socket.display()
            ),
        )
        .expect("proc unix table");
        let phoenix = temp.path().join("phoenix");
        std::fs::write(&phoenix, b"binary").expect("fake executable");
        symlink(&phoenix, process.join("exe")).expect("exe link");
        symlink("socket:[7788]", process.join("fd/9")).expect("fd link");

        assert!(pid_is_verified_gateway_at(&proc_root, 4242, &socket, None));
        assert!(
            !pid_is_verified_gateway_at(&proc_root, 4242, &temp.path().join("other.sock"), None,),
            "owning a different socket must never authorize a signal"
        );

        let custom = temp.path().join("phoenix-custom-build");
        std::fs::write(&custom, b"custom binary").expect("custom executable");
        std::fs::remove_file(process.join("exe")).expect("remove old exe link");
        symlink(&custom, process.join("exe")).expect("custom exe link");
        assert!(
            !pid_is_verified_gateway_at(&proc_root, 4242, &socket, None),
            "an arbitrary executable name is not enough"
        );
        assert!(
            pid_is_verified_gateway_at(&proc_root, 4242, &socket, Some(&custom)),
            "the exact explicitly selected executable remains stoppable"
        );
    }

    #[test]
    fn scheduled_preflight_failure_is_visible_to_owner_and_reconnect() {
        use crate::runtime::{postbox, story::{StoryEvent, StoryReducer}, CliEvent};

        let home = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let session_id = format!("routine-failure-{}", uuid::Uuid::new_v4());
        let other_session = format!("routine-other-{}", uuid::Uuid::new_v4());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (other_tx, mut other_rx) = tokio::sync::mpsc::unbounded_channel();
        postbox::subscribe_journal(&session_id, tx);
        postbox::subscribe_journal(&other_session, other_tx);
        postbox::begin_foreground_replay(&session_id);
        postbox::forward(&session_id, CliEvent::WakeTurn {
            prompt: "Check the existing routine".into(),
            turn_id: Some("routine:test:occurrence".into()),
            origin: Some(crate::runtime::TurnOrigin::Routine {
                routine_id: "test".into(),
                scheduled_for: "2026-09-30T21:42:33Z".into(),
                schedule: "every 1h".into(),
            }),
        });
        super::settle_scheduled_turn(&session_id, false,
            Some("Scheduled work failed: coworker `Rory` is dormant".into()));

        let mut story = StoryReducer::new();
        let mut rows = Vec::new();
        while let Ok(entry) = rx.try_recv() {
            rows.extend(story.push(&entry.event));
        }
        assert!(rows.iter().any(|row| matches!(row,
            StoryEvent::Failure { text, .. } if text.contains("Rory") && text.contains("dormant"))));
        assert!(other_rx.try_recv().is_err(), "Failure belongs only to the routine owner");
        let (reconnect_tx, _) = tokio::sync::mpsc::unbounded_channel();
        let replay = postbox::subscribe_journal(&session_id, reconnect_tx);
        assert_eq!(replay.iter().filter(|entry| matches!(entry.event, CliEvent::WakeTurn { .. })).count(), 1);
        assert_eq!(replay.iter().filter(|entry| matches!(entry.event, CliEvent::TerminalFailure { .. })).count(), 1);
        assert!(!replay.iter().any(|entry| matches!(entry.event, CliEvent::Done)),
            "A failed routine must not replay successful completion");
    }

    #[test]
    fn scheduled_agent_sessions_route_directly_to_their_owner() {
        assert_eq!(
            scheduled_agent_target("agent-school_coach").as_deref(),
            Some("school_coach")
        );
        assert_eq!(scheduled_agent_target("main-phoenix"), None);
        assert_eq!(scheduled_agent_target("agent-phoenix"), None);
    }

    #[test]
    fn scheduled_turn_ids_are_stable_per_occurrence_and_distinct_across_days() {
        let entry = |next_run| crate::cron::CronEntry {
            id: "morning-check".to_string(),
            session_id: "agent-school_coach".to_string(),
            canvas: None,
            prompt: "Check the school portal".to_string(),
            schedule: crate::cron::Schedule::Daily { hour: 5, minute: 0 },
            next_run,
            enabled: true,
            created_at: chrono::DateTime::parse_from_rfc3339("2026-08-01T11:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        };
        let first = entry(
            chrono::DateTime::parse_from_rfc3339("2026-08-26T11:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        let same_claim_retried = first.clone();
        let next_day = entry(
            chrono::DateTime::parse_from_rfc3339("2026-08-27T11:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );

        assert_eq!(
            scheduled_turn_id(&first),
            scheduled_turn_id(&same_claim_retried)
        );
        assert_ne!(scheduled_turn_id(&first), scheduled_turn_id(&next_day));
    }

    /// Cold-start: stop if needed, ensure, then prove gateway is not a child of
    /// the spawner (double-fork reparents to init). Skips if stop fails mid-turn.
    #[tokio::test]
    #[ignore = "process-level integration test; run serially with a freshly built PHOENIX_GATEWAY_BINARY"]
    async fn ensure_cold_start_is_detached() {
        use super::{pid_path, stop_daemon};

        // Best-effort stop so we exercise the spawn path.
        if tokio::net::UnixStream::connect(socket_path()).await.is_ok() {
            if let Err(err) = stop_daemon().await {
                eprintln!("skip ensure_cold_start_is_detached: stop failed: {err:#}");
                return;
            }
            // Wait for socket to go quiet.
            for _ in 0..50 {
                if tokio::net::UnixStream::connect(socket_path())
                    .await
                    .is_err()
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }

        ensure_gateway_running()
            .await
            .expect("ensure must bring gateway up from cold");

        // Read pidfile written by the daemon.
        let raw = std::fs::read_to_string(pid_path()).expect("gateway.pid after ensure");
        let pid: i32 = raw.trim().parse().expect("pid parse");
        assert!(pid > 1, "pid should be real process");

        // /proc/<pid>/stat field 4 is PPID.
        let stat =
            std::fs::read_to_string(format!("/proc/{pid}/stat")).expect("read /proc/pid/stat");
        // comm can contain spaces/parens — PPID is after the closing paren of comm.
        let after = stat.rsplit(')').next().expect("stat has comm paren").trim();
        // fields: state ppid ...
        let mut parts = after.split_whitespace();
        let _state = parts.next();
        let ppid: i32 = parts
            .next()
            .expect("ppid field")
            .parse()
            .expect("ppid parse");

        let our_pid = std::process::id() as i32;
        assert_ne!(
            ppid, our_pid,
            "gateway PPID must not be the test/spawner process (got PPID={ppid})"
        );
        // Also not phoenix-canvas if one is running.
        if let Ok(out) = std::process::Command::new("pgrep")
            .args(["-n", "phoenix-canvas"])
            .output()
        {
            if out.status.success() {
                if let Ok(canvas_pid) = String::from_utf8_lossy(&out.stdout).trim().parse::<i32>() {
                    assert_ne!(
                        ppid, canvas_pid,
                        "gateway PPID must not be phoenix-canvas (canvas={canvas_pid}, gateway_ppid={ppid})"
                    );
                }
            }
        }
        eprintln!("cold-start ok: gateway pid={pid} ppid={ppid} (spawner={our_pid})");

        // The process lifecycle is part of the contract: a detached daemon
        // must still honor the graceful SIGINT used by `phoenix stop`.
        stop_daemon()
            .await
            .expect("freshly detached gateway must stop cleanly");
        assert!(
            tokio::net::UnixStream::connect(socket_path())
                .await
                .is_err(),
            "gateway socket must be gone after stop"
        );
    }
}
