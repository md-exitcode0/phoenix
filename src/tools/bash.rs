//! bash tool - execute shell commands inside the workspace

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::{resolve_workspace_path, ToolCancellation, ToolOutput};

const MAX_COMMAND_OUTPUT_BYTES: usize = 64 * 1024;
const CAPTURE_HEAD_BYTES: usize = MAX_COMMAND_OUTPUT_BYTES / 2;
const CAPTURE_TAIL_BYTES: usize = MAX_COMMAND_OUTPUT_BYTES - CAPTURE_HEAD_BYTES;
/// Kill a command that emits this many bytes on either stream. Capture uses a
/// continuously-drained pipe and retains only [`MAX_COMMAND_OUTPUT_BYTES`], so
/// even a producer that can outrun the polling loop cannot consume disk or
/// unbounded memory.
const MAX_CAPTURE_STREAM_BYTES: u64 = 8 * 1024 * 1024;
/// Default ceiling on command runtime. Without it, one `npm run dev`,
/// `tail -f`, or accidentally-interactive command wedges the whole agent turn
/// forever. Callers may raise it per-call (`timeout_secs`) up to the max —
/// cold release builds legitimately run long.
const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 180;
const MAX_COMMAND_TIMEOUT_SECS: u64 = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellIsolation {
    Off,
    Auto,
    Require,
}

fn shell_isolation(confined: bool) -> ShellIsolation {
    if !confined {
        return ShellIsolation::Off;
    }
    match crate::settings::effective_string(
        "permissions.shell_isolation",
        &crate::settings::SettingsScope::Global,
    )
    .as_deref()
    {
        Some("off") => ShellIsolation::Off,
        Some("require") => ShellIsolation::Require,
        _ => ShellIsolation::Auto,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BashInput {
    pub command: String,
    #[serde(default)]
    pub cwd: Option<String>,
    /// Optional runtime ceiling override in seconds (capped at 600).
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Explicit user-configured SSH runner id. Remote workspaces are
    /// pre-provisioned and available only in Workspace mode.
    #[serde(default)]
    pub runner: Option<String>,
}

pub fn execute(workspace_root: &Path, confined: bool, input: BashInput) -> Result<ToolOutput> {
    execute_cancellable(
        workspace_root,
        confined,
        input,
        &ToolCancellation::default(),
    )
}

pub(super) fn execute_cancellable(
    workspace_root: &Path,
    confined: bool,
    input: BashInput,
    cancellation: &ToolCancellation,
) -> Result<ToolOutput> {
    validate_input(workspace_root,confined,&input)?;
    // Destructive operations remain blocked in every mode. YOLO lifts location
    // and workflow restrictions; it is not permission to erase broad state.
    validate_destructive_command(&input.command)?;
    // The cwd-shape rule belongs only to confined mode. YOLO may use normal
    // shell navigation as well as an explicit `cwd`.
    if confined {
        validate_confined_command(&input.command)?;
    }
    let cwd = match &input.cwd {
        Some(cwd) => resolve_workspace_path(workspace_root, cwd, true, confined)?,
        None => resolve_workspace_path(workspace_root, ".", true, confined)?,
    };
    if !cwd.is_dir() {
        bail!("bash cwd is not a directory");
    }
    // Native tool callers commonly serialize an omitted optional selector as
    // `""`, and some models spell the default route as `"local"`. Neither is
    // a remote-runner request. Treating those values as `Some(..)` made a
    // Full Access turn fail with the deeply misleading "Workspace mode"
    // error even though its permission mode had propagated correctly.
    let runner = input
        .runner
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("local"));
    if runner.is_some() && !confined {
        bail!("remote runners are available only in Workspace mode");
    }

    let limit = input
        .timeout_secs
        .unwrap_or(DEFAULT_COMMAND_TIMEOUT_SECS)
        .clamp(1, MAX_COMMAND_TIMEOUT_SECS);

    #[cfg(not(unix))]
    {
        let _ = (cwd, limit, cancellation);
        bail!("bounded bash execution is supported only on Unix");
    }

    #[cfg(unix)]
    execute_unix(
        workspace_root,
        cwd.as_path(),
        &input.command,
        limit,
        cancellation,
        shell_isolation(confined),
        runner,
    )
}

/// Validate a managed job before accepting its asynchronous start. Execution
/// repeats validation immediately before launch; no permission is widened.
pub(super) fn validate_input(root:&Path, confined:bool,input:&BashInput)->Result<()> {
    validate_destructive_command(&input.command)?;
    if confined {validate_confined_command(&input.command)?;}
    let cwd=resolve_workspace_path(root,input.cwd.as_deref().unwrap_or("."),true,confined)?;
    anyhow::ensure!(cwd.is_dir(),"bash cwd is not a directory");
    let runner=input.runner.as_deref().map(str::trim)
        .filter(|value| !value.is_empty()&&!value.eq_ignore_ascii_case("local"));
    anyhow::ensure!(runner.is_none()||confined,"remote runners are available only in Workspace mode");
    Ok(())
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminationReason {
    Exited,
    Cancelled,
    TimedOut,
    OutputLimit,
}

#[cfg(unix)]
fn ordinary_bash_command(cwd: &Path, user_command: &str) -> Command {
    let mut command = Command::new("bash");
    command.args(["-lc", user_command]).current_dir(cwd);
    command
}

#[cfg(all(unix, not(target_os = "linux")))]
fn bash_command(
    _workspace_root: &Path,
    cwd: &Path,
    user_command: &str,
    isolation: ShellIsolation,
) -> Result<(Command, bool)> {
    if isolation == ShellIsolation::Require {
        bail!("Workspace shell isolation is required, but this operating system has no supported Bubblewrap boundary");
    }
    Ok((ordinary_bash_command(cwd, user_command), false))
}

#[cfg(target_os = "linux")]
fn bubblewrap_binary() -> Option<PathBuf> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    ["/usr/bin/bwrap", "/bin/bwrap", "/usr/local/bin/bwrap"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| {
            let Ok(metadata) = std::fs::metadata(path) else {
                return false;
            };
            metadata.is_file() && metadata.uid() == 0 && metadata.permissions().mode() & 0o111 != 0
        })
}

#[cfg(target_os = "linux")]
fn collect_overlay_directories(path: &Path, directories: &mut Vec<PathBuf>) -> Result<()> {
    let base = if path.starts_with("/home") {
        Path::new("/home")
    } else if path.starts_with("/tmp") {
        Path::new("/tmp")
    } else {
        bail!("sandbox mount destination must live below /home or /tmp");
    };
    let mut cursor = base.to_path_buf();
    for component in path
        .strip_prefix(base)
        .context("strip sandbox overlay root")?
        .components()
    {
        cursor.push(component.as_os_str());
        if !directories.contains(&cursor) {
            directories.push(cursor.clone());
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn safe_sandbox_path(system_home: Option<&Path>, mounts: &[(PathBuf, PathBuf)]) -> String {
    let mounted_home_paths = mounts
        .iter()
        .map(|(_, destination)| destination)
        .collect::<Vec<_>>();
    let mut entries = Vec::new();
    for entry in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        if !entry.is_absolute() {
            continue;
        }
        let under_hidden_home = system_home.is_some_and(|home| entry.starts_with(home));
        let available = !under_hidden_home
            || mounted_home_paths
                .iter()
                .any(|mount| entry.starts_with(mount) || mount.starts_with(&entry));
        if available && !entries.contains(&entry) {
            entries.push(entry);
        }
    }
    for fallback in [
        "/usr/local/sbin",
        "/usr/local/bin",
        "/usr/sbin",
        "/usr/bin",
        "/sbin",
        "/bin",
    ] {
        let fallback = PathBuf::from(fallback);
        if !entries.contains(&fallback) {
            entries.push(fallback);
        }
    }
    std::env::join_paths(entries)
        .unwrap_or_else(|_| std::ffi::OsString::from("/usr/local/bin:/usr/bin:/bin"))
        .to_string_lossy()
        .into_owned()
}

#[cfg(target_os = "linux")]
fn sandbox_cache_root(workspace_root: &Path) -> Result<PathBuf> {
    // Unit tests run in parallel while other modules temporarily redirect the
    // process-wide PHOENIX_HOME. A shell test must never bind another test's
    // short-lived home after its TempDir has been dropped; its workspace is
    // already the authoritative isolated root. Production continues to use
    // the configured private Phoenix home.
    let isolated_test = cfg!(test) || crate::config::test_isolated_from_live_home();
    let root = if isolated_test {
        workspace_root.join(".phoenix-shell-cache")
    } else {
        crate::config::phoenix_home()
            .join("sandboxes")
            .join("shell")
    };
    if isolated_test {
        std::fs::create_dir_all(&root).context("prepare isolated test shell cache")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .context("secure isolated test shell cache")?;
        }
    } else {
        crate::config::private_io::prepare_phoenix_directory(&root)
            .context("prepare private shell sandbox cache")?;
    }
    for child in ["cargo", "npm", "cache"] {
        let path = root.join(child);
        if isolated_test {
            std::fs::create_dir_all(&path)
                .with_context(|| format!("prepare isolated shell sandbox {child} cache"))?;
        } else {
            crate::config::private_io::prepare_phoenix_directory(&path)
                .with_context(|| format!("prepare shell sandbox {child} cache"))?;
        }
    }
    Ok(root)
}

#[cfg(target_os = "linux")]
fn bash_command(
    workspace_root: &Path,
    cwd: &Path,
    user_command: &str,
    isolation: ShellIsolation,
) -> Result<(Command, bool)> {
    if isolation == ShellIsolation::Off {
        return Ok((ordinary_bash_command(cwd, user_command), false));
    }
    let Some(bwrap) = bubblewrap_binary() else {
        if isolation == ShellIsolation::Require {
            bail!("Workspace shell isolation is required, but a root-owned Bubblewrap executable was not found");
        }
        return Ok((ordinary_bash_command(cwd, user_command), false));
    };

    let workspace_root = std::fs::canonicalize(workspace_root)
        .context("resolve workspace root for shell isolation")?;
    let cwd = std::fs::canonicalize(cwd).context("resolve bash cwd for shell isolation")?;
    let relative_cwd = cwd
        .strip_prefix(&workspace_root)
        .context("bash cwd escaped the isolated workspace")?;
    let sandbox_workspace =
        if workspace_root.starts_with("/home") || workspace_root.starts_with("/tmp") {
            workspace_root.clone()
        } else {
            PathBuf::from("/tmp/phoenix-workspace")
        };
    let sandbox_cwd = sandbox_workspace.join(relative_cwd);

    let system_home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && path.starts_with("/home"));
    let mut readonly_mounts = Vec::<(PathBuf, PathBuf)>::new();
    if let Some(home) = system_home.as_deref() {
        for relative in [".cargo/bin", ".rustup", ".nvm"] {
            let source = home.join(relative);
            if source.is_dir() {
                readonly_mounts.push((source.clone(), source));
            }
        }
    }
    let cache_root = sandbox_cache_root(&workspace_root)?;
    let sandbox_home = PathBuf::from("/home/phoenix");
    let mut directories = Vec::new();
    collect_overlay_directories(&sandbox_workspace, &mut directories)?;
    collect_overlay_directories(&sandbox_home, &mut directories)?;
    for (_, destination) in &readonly_mounts {
        collect_overlay_directories(destination, &mut directories)?;
    }
    directories.sort_by_key(|path| path.components().count());
    directories.dedup();

    let path = safe_sandbox_path(system_home.as_deref(), &readonly_mounts);
    let mut command = Command::new(bwrap);
    command
        .args([
            "--die-with-parent",
            "--new-session",
            "--unshare-pid",
            "--unshare-ipc",
            "--unshare-uts",
        ])
        .args(["--ro-bind", "/", "/"])
        .args(["--tmpfs", "/home", "--tmpfs", "/tmp"])
        .args(["--dev", "/dev", "--proc", "/proc"]);
    for directory in directories {
        command.arg("--dir").arg(directory);
    }
    command
        .arg("--bind")
        .arg(&workspace_root)
        .arg(&sandbox_workspace)
        .arg("--bind")
        .arg(&cache_root)
        .arg(&sandbox_home);
    for (source, destination) in &readonly_mounts {
        command.arg("--ro-bind").arg(source).arg(destination);
    }
    command
        .arg("--chdir")
        .arg(&sandbox_cwd)
        .arg("--setenv")
        .arg("HOME")
        .arg(&sandbox_home)
        .arg("--setenv")
        .arg("PATH")
        .arg(path)
        .arg("--setenv")
        .arg("CARGO_HOME")
        .arg(sandbox_home.join("cargo"))
        .arg("--setenv")
        .arg("NPM_CONFIG_CACHE")
        .arg(sandbox_home.join("npm"))
        .arg("--setenv")
        .arg("XDG_CACHE_HOME")
        .arg(sandbox_home.join("cache"))
        .args(["--setenv", "LANG", "C.UTF-8"])
        .args(["--setenv", "LC_ALL", "C.UTF-8"])
        .args(["--setenv", "TERM", "dumb"])
        .args(["--setenv", "USER", "phoenix"])
        .args(["--setenv", "LOGNAME", "phoenix"])
        .args(["--setenv", "GIT_TERMINAL_PROMPT", "0"])
        .args(["--setenv", "NO_COLOR", "1"]);
    if let Some(home) = system_home.as_deref() {
        if home.join(".rustup").is_dir() {
            command
                .arg("--setenv")
                .arg("RUSTUP_HOME")
                .arg(home.join(".rustup"));
        }
        if home.join(".nvm").is_dir() {
            command
                .arg("--setenv")
                .arg("NVM_DIR")
                .arg(home.join(".nvm"));
        }
    }
    command
        .arg("/usr/bin/bash")
        .args(["-c", user_command])
        .current_dir("/")
        .env_clear();
    Ok((command, true))
}

#[cfg(unix)]
fn execute_unix(
    workspace_root: &Path,
    cwd: &Path,
    user_command: &str,
    limit: u64,
    cancellation: &ToolCancellation,
    isolation: ShellIsolation,
    runner_id: Option<&str>,
) -> Result<ToolOutput> {
    // A file-backed capture remains writable after its pathname is unlinked:
    // `nohup server &` can therefore fill an invisible inode forever after the
    // tool returns. Nonblocking pipe drainers retain 64 KiB, discard the rest,
    // and close deterministically even if an explicitly detached service kept
    // a writer. Kernel pipe capacity is the only transient storage.
    let (mut command, isolated, remote_label) = if let Some(runner_id) = runner_id {
        // Remote cwd is mapped into the runner's workspace. Local Full Access
        // has already resolved its cwd without confinement and must not be
        // subjected to this workspace-only mapping.
        let workspace_root_canonical = std::fs::canonicalize(workspace_root)
            .context("resolve workspace root before remote shell execution")?;
        let cwd_canonical = std::fs::canonicalize(cwd).context("resolve shell cwd")?;
        let relative_cwd = cwd_canonical
            .strip_prefix(&workspace_root_canonical)
            .context("remote bash cwd escaped the workspace")?;
        let runner = super::remote_runner::find_enabled(runner_id)?;
        let command = super::remote_runner::ssh_command(&runner, relative_cwd, user_command)?;
        (
            command,
            false,
            Some(format!("{} ({})", runner.label, runner.id)),
        )
    } else {
        let (command, isolated) = bash_command(workspace_root, cwd, user_command, isolation)?;
        (command, isolated, None)
    };
    // Each mesh agent has a thread-local desktop scope. Apply its DISPLAY and
    // private Xauthority only to this command tree; never set process-global
    // DISPLAY, which would make simultaneous agents steal each other's GUI
    // programs. Full Access commands retain their normal OS reach while still
    // receiving this agent's terminal/desktop context.
    if let Some(desktop) = crate::tools::isolated_desktop::current_environment()? {
        desktop.apply_to_command(&mut command);
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // Give the command and every descendant a process group owned by this
    // invocation. Cancellation can then terminate a shell, its foreground
    // child, and any background child together instead of merely dropping the
    // `spawn_blocking` waiter while mutations keep running.
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to execute command: {user_command}"))?;
    let pid = i32::try_from(child.id()).context("bash child pid did not fit i32")?;
    let stdout = child
        .stdout
        .take()
        .context("bash stdout pipe was missing")?;
    let stderr = child
        .stderr
        .take()
        .context("bash stderr pipe was missing")?;
    let capture_setup = set_nonblocking(&stdout)
        .context("make bash stdout capture nonblocking")
        .and_then(|_| set_nonblocking(&stderr).context("make bash stderr capture nonblocking"));
    if let Err(error) = capture_setup {
        if terminate_process_group(&mut child, pid).is_err() {
            cancellation.mark_unconfirmed();
        }
        return Err(error);
    }

    let stop_capture = Arc::new(AtomicBool::new(false));
    let output_limited = Arc::new(AtomicBool::new(false));
    let stdout_reader = match spawn_capture_reader(
        stdout,
        "stdout",
        Arc::clone(&stop_capture),
        Arc::clone(&output_limited),
    ) {
        Ok(reader) => reader,
        Err(error) => {
            if terminate_process_group(&mut child, pid).is_err() {
                cancellation.mark_unconfirmed();
            }
            return Err(error).context("spawn bash stdout capture reader");
        }
    };
    let stderr_reader = match spawn_capture_reader(
        stderr,
        "stderr",
        Arc::clone(&stop_capture),
        Arc::clone(&output_limited),
    ) {
        Ok(reader) => reader,
        Err(error) => {
            if terminate_process_group(&mut child, pid).is_err() {
                cancellation.mark_unconfirmed();
            }
            stop_capture.store(true, Ordering::Release);
            let _ = stdout_reader.join();
            return Err(error).context("spawn bash stderr capture reader");
        }
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(limit);
    let mut poll_error = None;
    let reason = loop {
        if cancellation.is_requested() {
            break TerminationReason::Cancelled;
        }
        if output_limited.load(Ordering::Acquire) {
            break TerminationReason::OutputLimit;
        }
        if std::time::Instant::now() >= deadline {
            break TerminationReason::TimedOut;
        }
        match child_exited_without_reap(&mut child, pid).context("poll bash command") {
            Ok(true) => break TerminationReason::Exited,
            Ok(false) => {}
            Err(error) => {
                poll_error = Some(error);
                break TerminationReason::Cancelled;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };

    // Even a shell that exited successfully can leave same-PGID descendants
    // holding capture writers. They are not detached services and are always
    // terminated. A real service must detach into another session/cgroup (for
    // example systemd-run/setsid) *and* redirect stdout/stderr.
    let cleanup = terminate_process_group(&mut child, pid);
    stop_capture.store(true, Ordering::Release);
    let (stdout, stdout_capture_ok, stdout_limit_exceeded) =
        join_capture_reader(stdout_reader, "stdout");
    let (stderr, stderr_capture_ok, stderr_limit_exceeded) =
        join_capture_reader(stderr_reader, "stderr");
    let (status, cleaned_descendants) = match cleanup {
        Ok(cleanup) => cleanup,
        Err(error) => {
            cancellation.mark_unconfirmed();
            return Err(error).context(format!(
                "terminate and verify bash process group\nstdout:\n{stdout}\n\nstderr:\n{stderr}"
            ));
        }
    };
    if let Some(error) = poll_error {
        return Err(error).context(format!(
            "bash status polling failed; process group was terminated\nstdout:\n{stdout}\n\nstderr:\n{stderr}"
        ));
    }
    if !stdout_capture_ok || !stderr_capture_ok {
        bail!(
            "Bash output capture failed; command termination was confirmed but its result cannot be trusted.\nstdout:\n{stdout}\n\nstderr:\n{stderr}"
        );
    }
    let reason = if stdout_limit_exceeded || stderr_limit_exceeded {
        TerminationReason::OutputLimit
    } else {
        reason
    };
    if remote_label.is_some() && reason != TerminationReason::Exited {
        // Killing the local SSH process proves the transport is gone, but a
        // partition prevents Phoenix from proving the remote child exited.
        cancellation.mark_unconfirmed();
    }
    let code = status.code().unwrap_or(-1);
    let cleanup_note = if cleaned_descendants == 0 {
        String::new()
    } else {
        format!(
            " Terminated {cleaned_descendants} non-detached background descendant(s); launch persistent services with systemd-run/setsid and redirect their output."
        )
    };
    let mut summary = match reason {
        TerminationReason::Exited if status.success() && cleaned_descendants == 0 => {
            format!("Command exited with code 0.{cleanup_note}")
        }
        TerminationReason::Exited if status.success() => format!(
            "Command shell exited with code 0, but its non-detached background work was terminated and is NOT running.{cleanup_note}"
        ),
        TerminationReason::Exited => format!("Command FAILED with exit code {code}.{cleanup_note}"),
        TerminationReason::Cancelled => format!(
            "Command CANCELLED by the turn runtime; its owned process group was terminated and verified. Partial effects produced before cancellation may remain.{cleanup_note}"
        ),
        TerminationReason::TimedOut => format!(
            "Command KILLED after {limit}s timeout; its owned process group was terminated and verified. Partial effects may remain.{cleanup_note}"
        ),
        TerminationReason::OutputLimit => format!(
            "Command KILLED after stdout/stderr exceeded {} MiB; its owned process group was terminated and verified. Partial effects may remain.{cleanup_note}",
            MAX_CAPTURE_STREAM_BYTES / (1024 * 1024)
        ),
    };
    if isolated {
        summary.push_str(" Workspace shell isolation was active; its process namespace is now destroyed, so background services are not running.");
    }
    if let Some(remote) = remote_label {
        summary.push_str(&format!(
            " Executed on pinned remote runner {remote}; the local workspace was not uploaded or synchronized."
        ));
    }
    let content = format!("stdout:\n{stdout}\n\nstderr:\n{stderr}");
    if matches!(reason, TerminationReason::Exited) && status.success() && cleaned_descendants == 0 {
        Ok(ToolOutput { summary, content })
    } else {
        bail!("{summary}\n{content}")
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct CaptureResult {
    bytes: Vec<u8>,
    total: u64,
    error: Option<String>,
}

#[cfg(unix)]
fn set_nonblocking(stream: &impl std::os::fd::AsRawFd) -> Result<()> {
    let fd = stream.as_raw_fd();
    // SAFETY: fd is an open pipe owned by this process for the duration of both
    // fcntl calls. F_SETFL changes only the duplicated file description flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error()).context("read pipe flags");
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error()).context("set nonblocking pipe flag");
    }
    Ok(())
}

#[cfg(unix)]
fn spawn_capture_reader<R>(
    mut reader: R,
    label: &'static str,
    stop: Arc<AtomicBool>,
    output_limited: Arc<AtomicBool>,
) -> Result<std::thread::JoinHandle<CaptureResult>>
where
    R: Read + Send + 'static,
{
    std::thread::Builder::new()
        .name(format!("phoenix-bash-{label}"))
        .spawn(move || {
            // Head AND tail: build/test output puts the verdict (summary line,
            // final error, exit reason) at the END, so keeping only the first
            // bytes hid exactly what the agent needed and invited a re-run.
            let mut kept = Vec::with_capacity(CAPTURE_HEAD_BYTES.min(8 * 1024));
            let mut tail: std::collections::VecDeque<u8> = std::collections::VecDeque::new();
            let mut total = 0u64;
            let mut error = None;
            let mut buffer = [0u8; 8 * 1024];
            let mut reads_after_stop = 0usize;
            loop {
                let stopping = stop.load(Ordering::Acquire);
                if stopping && reads_after_stop >= 32 {
                    break;
                }
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        total = total.saturating_add(read as u64);
                        let remaining = CAPTURE_HEAD_BYTES.saturating_sub(kept.len());
                        let head_part = read.min(remaining);
                        kept.extend_from_slice(&buffer[..head_part]);
                        tail.extend(&buffer[head_part..read]);
                        let excess = tail.len().saturating_sub(CAPTURE_TAIL_BYTES);
                        tail.drain(..excess);
                        if total > MAX_CAPTURE_STREAM_BYTES {
                            output_limited.store(true, Ordering::Release);
                        }
                        if stopping {
                            reads_after_stop += 1;
                        }
                    }
                    Err(io_error) if io_error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(io_error) if io_error.kind() == std::io::ErrorKind::WouldBlock => {
                        if stopping {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Err(io_error) => {
                        error = Some(format!("{label} capture failed: {io_error}"));
                        break;
                    }
                }
            }
            let omitted = total.saturating_sub((kept.len() + tail.len()) as u64);
            if omitted > 0 {
                kept.extend_from_slice(
                    format!("\n…[{omitted} bytes of {label} omitted — head and tail kept; redirect to a file to read all of it]…\n").as_bytes(),
                );
            }
            kept.extend(tail);
            CaptureResult {
                bytes: kept,
                total,
                error,
            }
        })
        .context("spawn bounded bash capture reader")
}

#[cfg(unix)]
fn join_capture_reader(
    handle: std::thread::JoinHandle<CaptureResult>,
    label: &str,
) -> (String, bool, bool) {
    let capture = match handle.join() {
        Ok(capture) => capture,
        Err(_) => return (format!("[{label} capture reader panicked]"), false, false),
    };
    let succeeded = capture.error.is_none();
    let limit_exceeded = capture.total > MAX_CAPTURE_STREAM_BYTES;
    let text_bytes = &capture.bytes;
    let mut text = String::from_utf8_lossy(text_bytes).into_owned();
    if let Some(error) = capture.error {
        text.push_str(&format!("\n[{error}]"));
    }
    (text, succeeded, limit_exceeded)
}

#[cfg(target_os = "linux")]
fn child_exited_without_reap(_child: &mut std::process::Child, pid: i32) -> Result<bool> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a valid out pointer. WNOWAIT observes the direct child
    // without reaping it, which keeps the numeric PGID reserved until every
    // same-group descendant has been terminated.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result < 0 {
        return Err(std::io::Error::last_os_error()).context("waitid bash child");
    }
    Ok(info.si_signo == libc::SIGCHLD)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn child_exited_without_reap(child: &mut std::process::Child, _pid: i32) -> Result<bool> {
    Ok(child.try_wait().context("poll bash child")?.is_some())
}

#[cfg(target_os = "linux")]
fn live_process_group_members(pgid: i32) -> Result<Vec<i32>> {
    let mut members = Vec::new();
    for entry in std::fs::read_dir("/proc").context("scan /proc for bash process group")? {
        let entry = match entry {
            Ok(entry) => entry,
            // `/proc` is a live view: a PID may exit after `read_dir` has
            // yielded its name but before either the entry or its stat file is
            // read. Only disappearance errors are safe to ignore here. Any
            // other error means process-group termination cannot be proved.
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
            .and_then(|value| value.as_bytes().first().copied());
        let _parent_pid = fields.next();
        let member_pgid = fields.next().and_then(|value| value.parse::<i32>().ok());
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
fn live_process_group_members(pgid: i32) -> Result<Vec<i32>> {
    // Portable Unix has no /proc contract. Signal zero still proves whether a
    // signalable member exists, but may include an unreaped zombie leader.
    let result = unsafe { libc::kill(-pgid, 0) };
    if result == 0 {
        Ok(vec![pgid])
    } else if std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        Ok(Vec::new())
    } else {
        Err(std::io::Error::last_os_error()).context("probe bash process group")
    }
}

#[cfg(unix)]
fn signal_owned_process_group(pgid: i32, signal: i32) -> Result<()> {
    if pgid <= 1 || pgid == unsafe { libc::getpgrp() } {
        bail!("refusing to signal a non-child or caller process group ({pgid})");
    }
    let actual = unsafe { libc::getpgid(pgid) };
    if actual != pgid {
        let error = std::io::Error::last_os_error();
        // The leader can disappear between the live-member scan and this
        // ownership check. Treat that race as success only after a fresh scan
        // proves that the owned group has no live members. If anything remains,
        // refusing to signal an unverified numeric PGID is the safe outcome.
        #[cfg(target_os = "linux")]
        if actual == -1
            && error.raw_os_error() == Some(libc::ESRCH)
            && live_process_group_members(pgid)?.is_empty()
        {
            return Ok(());
        }
        if actual == -1 {
            return Err(error).context(format!(
                "verify bash process-group ownership for leader {pgid}"
            ));
        }
        bail!("refusing to signal unverified bash process group: pid={pgid}, actual_pgid={actual}");
    }
    let result = unsafe { libc::kill(-pgid, signal) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error).context("signal bash process group")
    }
}

#[cfg(unix)]
fn terminate_process_group(
    child: &mut std::process::Child,
    pgid: i32,
) -> Result<(std::process::ExitStatus, usize)> {
    let initial = live_process_group_members(pgid)?;
    let descendants = initial.iter().filter(|pid| **pid != pgid).count();
    if !initial.is_empty() {
        signal_owned_process_group(pgid, libc::SIGTERM)?;
        let term_deadline = std::time::Instant::now() + std::time::Duration::from_millis(750);
        while std::time::Instant::now() < term_deadline {
            if live_process_group_members(pgid)?.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if !live_process_group_members(pgid)?.is_empty() {
            signal_owned_process_group(pgid, libc::SIGKILL)?;
            let kill_deadline = std::time::Instant::now() + std::time::Duration::from_millis(750);
            while std::time::Instant::now() < kill_deadline {
                if live_process_group_members(pgid)?.is_empty() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    }
    let remaining = live_process_group_members(pgid)?;
    if !remaining.is_empty() {
        bail!(
            "bash process-group termination was not confirmed; live pids: {}",
            remaining
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let status = child.wait().context("reap bash process-group leader")?;
    Ok((status, descendants))
}

fn validate_destructive_command(command: &str) -> Result<()> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        bail!("bash command cannot be empty");
    }

    // Only genuinely destructive patterns. These are specific multi-token strings,
    // so a plain substring match is safe. (Earlier this list also had bare `env` /
    // `printenv`, which substring-matched the literal "env" inside ANY script — a
    // python heredoc mentioning `.env` or `environ` got "refused". Both are gone:
    // they aren't destructive.)
    let denied = [
        "rm -rf",
        "git reset --hard",
        "git checkout --",
        "mkfs",
        "dd if=",
        "chmod -R",
        "chown -R",
    ];
    if denied.iter().any(|pattern| trimmed.contains(pattern)) {
        bail!("refusing unsafe command (destructive operations stay blocked in every permission mode): {trimmed}");
    }

    Ok(())
}

fn validate_confined_command(command: &str) -> Result<()> {
    let trimmed = command.trim();
    let lower = trimmed.to_lowercase();
    if lower.starts_with("cd ")
        || lower.contains(" && cd ")
        || lower.contains("; cd ")
        || lower.contains("|| cd ")
    {
        bail!("do not use cd in the command; set the cwd parameter instead");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn full_access_runs_in_an_authorized_cwd_outside_workspace() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let output = execute(root.path(), false, BashInput {
            command: "pwd -P".into(), cwd: Some(outside.path().to_string_lossy().into_owned()),
            timeout_secs: Some(5), runner: Some("local".into()),
        }).unwrap();
        assert!(output.content.contains(&outside.path().canonicalize().unwrap().to_string_lossy().to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn workspace_rejects_outside_cwd_even_without_shell_isolation() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let input = BashInput { command: "printf should-not-run".into(),
            cwd: Some(outside.path().to_string_lossy().into_owned()), timeout_secs: Some(5), runner: None };
        assert!(execute(root.path(), true, input).unwrap_err().to_string().contains("path escapes workspace"));
        let link = root.path().join("outside-link");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        assert!(execute(root.path(), true, BashInput { command: "pwd".into(),
            cwd: Some(link.to_string_lossy().into_owned()), timeout_secs: Some(5), runner: None }).unwrap_err().to_string().contains("path escapes workspace"));
    }

    #[test]
    fn full_access_does_not_enable_remote_runner_or_destructive_commands() {
        let root = tempfile::tempdir().unwrap();
        for (command, runner, expected) in [("pwd", Some("remote".into()), "Workspace mode"),
            ("rm -rf /", None, "blocked")] {
            assert!(execute(root.path(), false, BashInput { command: command.into(), cwd: None,
                timeout_secs: Some(5), runner }).unwrap_err().to_string().to_lowercase().contains(&expected.to_lowercase()));
        }
    }

    #[test]
    fn validate_allows_scripts_that_merely_mention_env() {
        // The old `env` / `printenv` substring entries refused ANY command with
        // "env" in it — a python heredoc touching `.env` or `environ` got killed.
        assert!(validate_destructive_command("python3 -c \"print(open('.env').read())\"").is_ok());
        assert!(validate_destructive_command("grep -rn environ src/").is_ok());
        assert!(validate_destructive_command("python3 - <<'PY'\nimport os\nPY").is_ok());
    }

    #[test]
    fn validate_still_blocks_genuinely_destructive_commands() {
        assert!(validate_destructive_command("rm -rf /tmp/x").is_err());
        assert!(validate_destructive_command("dd if=/dev/zero of=/dev/sda").is_err());
        assert!(validate_destructive_command("git reset --hard").is_err());
    }

    #[test]
    fn yolo_lifts_shell_navigation_restrictions() {
        let dir = tempfile::tempdir().unwrap();
        let out = execute(
            dir.path(),
            false,
            BashInput {
                command: "cd /tmp && pwd".into(),
                cwd: None,
                timeout_secs: None,
                runner: None,
            },
        )
        .unwrap();
        assert!(out.content.contains("/tmp"));
    }

    #[test]
    fn full_access_blank_or_local_runner_stays_on_the_local_machine() {
        let dir = tempfile::tempdir().unwrap();
        for runner in ["", "  ", "local", "LOCAL"] {
            let out = execute(
                dir.path(),
                false,
                BashInput {
                    command: "printf local-runner".into(),
                    cwd: None,
                    timeout_secs: Some(10),
                    runner: Some(runner.into()),
                },
            )
            .unwrap_or_else(|error| panic!("runner {runner:?} should stay local: {error:#}"));
            assert!(out.content.contains("local-runner"));
        }
    }

    #[test]
    fn yolo_still_blocks_destructive_commands() {
        let dir = tempfile::tempdir().unwrap();
        let result = execute(
            dir.path(),
            false,
            BashInput {
                command: "rm -rf /tmp/phoenix-yolo-destructive-test".into(),
                cwd: None,
                timeout_secs: None,
                runner: None,
            },
        );
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("destructive operations"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn workspace_shell_hides_host_paths_and_preserves_project_writes() {
        if bubblewrap_binary().is_none() {
            return;
        }
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("host-secret");
        std::fs::write(&secret, "must stay outside the sandbox").unwrap();
        let command = format!(
            "test ! -e '{}' && printf sandboxed > proof.txt && python3 -c 'print(42)'",
            secret.display()
        );

        let output = execute(
            workspace.path(),
            true,
            BashInput {
                command,
                cwd: None,
                timeout_secs: Some(10),
                runner: None,
            },
        )
        .expect("isolated workspace command");
        assert!(output.summary.contains("shell isolation was active"));
        assert!(output.content.contains("42"));
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("proof.txt")).unwrap(),
            "sandboxed"
        );
        assert_eq!(
            std::fs::read_to_string(secret).unwrap(),
            "must stay outside the sandbox"
        );
    }

    #[test]
    fn runaway_command_is_killed_at_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let error = execute(
            dir.path(),
            true,
            BashInput {
                command: "sleep 30".into(),
                cwd: None,
                timeout_secs: Some(1),
                runner: None,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("KILLED after 1s timeout"));
    }

    #[test]
    fn timeout_escalates_to_kill_for_term_ignoring_descendant() {
        let dir = tempfile::tempdir().unwrap();
        let cancellation = ToolCancellation::default();
        let error = execute_unix(
            dir.path(),
            dir.path(),
            "bash -c 'trap \"\" TERM; while true; do sleep 1; done' & echo \"PID: $!\"; wait",
            1,
            &cancellation,
            ShellIsolation::Off,
            None,
        )
        .unwrap_err();
        let receipt = error.to_string();
        assert!(receipt.contains("KILLED after 1s timeout"), "{receipt}");
        let pid: i32 = receipt
            .lines()
            .find_map(|line| line.strip_prefix("PID: "))
            .expect("TERM-ignoring child pid")
            .trim()
            .parse()
            .unwrap();
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let state = stat
            .rsplit_once(") ")
            .and_then(|(_, rest)| rest.as_bytes().first().copied());
        assert!(
            stat.is_empty() || matches!(state, Some(b'Z' | b'X')),
            "TERM-ignoring child remained live: {stat}"
        );
    }

    #[test]
    fn backgrounded_server_cannot_wedge_the_tool() {
        // The 2026-07-17 freeze: `nohup server & echo PID` left the shell (or
        // an orphan) holding the output pipe forever, past every timeout.
        // The bounded drainers return the receipt and normal cleanup kills a
        // same-group background process. Persistent services must explicitly
        // detach and redirect their output.
        let dir = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let cancellation = ToolCancellation::default();
        let error = execute_unix(
            dir.path(),
            dir.path(),
            "nohup sleep 120 >/dev/null 2>&1 & echo \"PID: $!\" && echo ready",
            2,
            &cancellation,
            ShellIsolation::Off,
            None,
        )
        .unwrap_err();
        let receipt = error.to_string();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "returned promptly"
        );
        assert!(receipt.contains("PID:"), "{receipt}");
        assert!(receipt.contains("ready"), "{receipt}");
        assert!(receipt.contains("is NOT running"), "{receipt}");
        let pid: i32 = receipt
            .lines()
            .find_map(|line| line.strip_prefix("PID: "))
            .expect("background pid in capture")
            .trim()
            .parse()
            .unwrap();
        let still_running = |pid: i32| {
            if unsafe { libc::kill(pid, 0) } != 0 {
                return false;
            }
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
            let state = stat
                .rsplit_once(") ")
                .and_then(|(_, rest)| rest.as_bytes().first().copied());
            !matches!(state, Some(b'Z' | b'X'))
        };
        for _ in 0..100 {
            if !still_running(pid) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            !still_running(pid),
            "same-group background process escaped normal cleanup"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn isolated_background_work_dies_with_the_namespace() {
        if bubblewrap_binary().is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let output = execute(
            dir.path(),
            true,
            BashInput {
                command: "nohup sleep 120 >/dev/null 2>&1 & echo \"sandbox PID: $!\" && echo ready"
                    .into(),
                cwd: None,
                timeout_secs: Some(2),
                runner: None,
            },
        )
        .expect("namespace teardown should reap sandbox background work");
        assert!(
            output.content.contains("sandbox PID:"),
            "{}",
            output.content
        );
        assert!(output.content.contains("ready"), "{}", output.content);
        assert!(
            output
                .summary
                .contains("process namespace is now destroyed"),
            "{}",
            output.summary
        );
        assert!(
            output
                .summary
                .contains("background services are not running"),
            "{}",
            output.summary
        );
    }

    #[test]
    fn nonzero_exit_is_an_error_with_bounded_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let error = execute(
            dir.path(),
            true,
            BashInput {
                command: "printf useful-output; printf failed-detail >&2; exit 7".into(),
                cwd: None,
                timeout_secs: Some(5),
                runner: None,
            },
        )
        .unwrap_err();
        let receipt = error.to_string();
        assert!(receipt.contains("FAILED with exit code 7"), "{receipt}");
        assert!(receipt.contains("useful-output"), "{receipt}");
        assert!(receipt.contains("failed-detail"), "{receipt}");
    }

    #[test]
    fn natural_exit_124_is_not_misreported_as_a_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let error = execute(
            dir.path(),
            true,
            BashInput {
                command: "exit 124".into(),
                cwd: None,
                timeout_secs: Some(5),
                runner: None,
            },
        )
        .unwrap_err();
        let receipt = error.to_string();
        assert!(receipt.contains("FAILED with exit code 124"), "{receipt}");
        assert!(!receipt.contains("timeout"), "{receipt}");
    }

    #[test]
    fn long_output_keeps_the_tail_where_verdicts_live() {
        let dir = tempfile::tempdir().unwrap();
        let out = execute(
            dir.path(),
            true,
            BashInput {
                command: "echo FIRST-LINE; seq 1 40000; echo FINAL-VERDICT".into(),
                cwd: None,
                timeout_secs: Some(30),
                runner: None,
            },
        )
        .unwrap();
        let text = format!("{out:?}");
        assert!(text.contains("FIRST-LINE"));
        assert!(text.contains("FINAL-VERDICT"), "tail was dropped");
        assert!(text.contains("bytes of stdout omitted"));
    }

    #[test]
    fn output_flood_is_killed_without_unbounded_capture() {
        let dir = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let error = execute(
            dir.path(),
            true,
            BashInput {
                command: "yes flood".into(),
                cwd: None,
                timeout_secs: Some(30),
                runner: None,
            },
        )
        .unwrap_err();
        let receipt = error.to_string();
        assert!(
            receipt.contains("stdout/stderr exceeded 8 MiB"),
            "{receipt}"
        );
        assert!(receipt.len() < 2 * MAX_COMMAND_OUTPUT_BYTES + 2_000);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn normal_command_completes_within_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let out = execute(
            dir.path(),
            true,
            BashInput {
                command: "echo hi".into(),
                cwd: None,
                timeout_secs: None,
                runner: None,
            },
        )
        .unwrap();
        assert!(out.summary.contains("code 0"), "{}", out.summary);
        assert!(out.content.contains("hi"));
    }
}
