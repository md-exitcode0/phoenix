//! grep tool - search file contents

use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::{fd::AsRawFd, unix::process::CommandExt};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::{
    is_ignored_dir, relative_display, resolve_workspace_path, truncate_bytes, ToolOutput,
    IGNORED_DIR_NAMES,
};

const MAX_GREP_BYTES: usize = 32 * 1024;
const MAX_RG_STDERR_BYTES: usize = 64 * 1024;
const RG_TIMEOUT: Duration = Duration::from_secs(30);
const RG_POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Hard cap on files the fallback walker will open, so an accidental search of a
/// huge tree cannot peg CPU or exhaust memory.
const MAX_FALLBACK_FILES: usize = 5_000;

/// Cap on context lines, so a huge `context` can't dump whole files.
const MAX_CONTEXT_LINES: usize = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrepInput {
    pub pattern: String,
    #[serde(default)]
    pub path: Option<String>,
    /// Filter files by glob, e.g. `*.rs` or `src/**/*.ts` (like `rg --glob`).
    #[serde(default)]
    pub glob: Option<String>,
    /// Lines of context to show on each side of a match (like `rg -C`). Default 0.
    #[serde(default)]
    pub context: Option<usize>,
    /// Case-insensitive match (like `rg -i`). Default false.
    #[serde(default)]
    pub ignore_case: Option<bool>,
    /// Only list the files that contain a match, not the lines (like `rg -l`) —
    /// the fast way to find WHERE something lives before reading it.
    #[serde(default)]
    pub files_only: Option<bool>,
}

pub fn execute(workspace_root: &Path, confined: bool, input: GrepInput) -> Result<ToolOutput> {
    if input.pattern.trim().is_empty() {
        bail!("grep pattern cannot be empty");
    }

    let search_path = match &input.path {
        Some(path) => resolve_workspace_path(workspace_root, path, true, confined)?,
        None => resolve_workspace_path(workspace_root, ".", true, confined)?,
    };

    if command_exists("rg") {
        execute_rg(&input, &search_path)
    } else {
        execute_fallback(workspace_root, &input, &search_path)
    }
}

fn execute_rg(input: &GrepInput, search_path: &Path) -> Result<ToolOutput> {
    execute_rg_with_program(input, search_path, Path::new("rg"), RG_TIMEOUT)
}

fn execute_rg_with_program(
    input: &GrepInput,
    search_path: &Path,
    program: &Path,
    timeout: Duration,
) -> Result<ToolOutput> {
    let mut command = Command::new(program);
    command
        .arg("--line-number")
        .arg("--no-heading")
        .arg("--color")
        .arg("never")
        // Bound per-file and per-line work so a pathological match set can't
        // blow up CPU/memory before MAX_GREP_BYTES truncation.
        .arg("--max-columns")
        .arg("400")
        .arg("--max-filesize")
        .arg("2M");
    if input.ignore_case.unwrap_or(false) {
        command.arg("--ignore-case");
    }
    if input.files_only.unwrap_or(false) {
        command.arg("--files-with-matches");
    } else if let Some(ctx) = input.context {
        if ctx > 0 {
            command
                .arg("--context")
                .arg(ctx.min(MAX_CONTEXT_LINES).to_string());
        }
    }
    if let Some(glob) = input.glob.as_deref().filter(|g| !g.trim().is_empty()) {
        command.arg("--glob").arg(glob);
    }
    // rg already honors .gitignore and skips hidden files, but heavy dirs like
    // donor clones may not be gitignored — exclude them explicitly.
    for dir in IGNORED_DIR_NAMES {
        command.arg("--glob").arg(format!("!**/{dir}/**"));
    }
    command.arg("--").arg(&input.pattern).arg(search_path);
    let output = run_rg_bounded(command, timeout, MAX_GREP_BYTES, MAX_RG_STDERR_BYTES)
        .context("failed to execute rg")?;
    let mut content = String::from_utf8_lossy(&output.stdout).into_owned();
    // Preserve UTF-8 boundaries when the byte cap split a multibyte sequence.
    truncate_bytes(&mut content, MAX_GREP_BYTES);
    let stderr = String::from_utf8_lossy(&output.stderr);

    match output.reason {
        RgStopReason::StdoutLimit => {
            content.push_str(&format!(
                "\n[grep output truncated at {MAX_GREP_BYTES} bytes; rg was stopped]\n"
            ));
            Ok(ToolOutput {
                summary: "grep found matches (output truncated).".to_string(),
                content,
            })
        }
        RgStopReason::TimedOut if !content.trim().is_empty() => {
            content.push_str("\n[grep timed out after partial results; rg was stopped]\n");
            Ok(ToolOutput {
                summary: "grep found partial matches before timing out.".to_string(),
                content,
            })
        }
        RgStopReason::TimedOut => bail!(
            "rg timed out after {:.1}s with no results; its process group was terminated and reaped",
            timeout.as_secs_f64()
        ),
        RgStopReason::StderrLimit => bail!(
            "rg stderr exceeded the {MAX_RG_STDERR_BYTES} byte limit; its process group was terminated and reaped: {}",
            stderr.trim()
        ),
        RgStopReason::Exited if output.descendants_terminated > 0 => bail!(
            "rg left {} background descendant(s); they were terminated before returning, so the search result is not trusted",
            output.descendants_terminated
        ),
        RgStopReason::Exited if output.status.success() => Ok(ToolOutput {
            summary: "grep found matches.".to_string(),
            content,
        }),
        RgStopReason::Exited if output.status.code() == Some(1) => Ok(ToolOutput {
            summary: "grep found no matches.".to_string(),
            content,
        }),
        RgStopReason::Exited => {
            let code = output
                .status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".to_string());
            let partial = if content.trim().is_empty() {
                String::new()
            } else {
                format!("\npartial stdout:\n{content}")
            };
            bail!("rg failed with exit {code}: {}{partial}", stderr.trim())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RgStopReason {
    Exited,
    TimedOut,
    StdoutLimit,
    StderrLimit,
}

#[derive(Debug)]
struct BoundedRgOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    reason: RgStopReason,
    descendants_terminated: usize,
}

/// Stream `rg` through finite nonblocking pipes. Once useful output reaches
/// the public result cap, stop the entire process group instead of letting rg
/// enumerate a huge tree only to truncate its already-buffered output later.
fn run_rg_bounded(
    mut command: Command,
    timeout: Duration,
    stdout_cap: usize,
    stderr_cap: usize,
) -> Result<BoundedRgOutput> {
    if stdout_cap == 0 || stderr_cap == 0 {
        bail!("rg capture limits must be non-zero");
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    command.process_group(0);
    let spawn_started=Instant::now();
    let mut child=loop {
        match command.spawn() {
            Ok(child)=>break child,
            Err(error)=>{
                #[cfg(unix)]
                let retry=error.raw_os_error()==Some(libc::ETXTBSY)
                    && spawn_started.elapsed()<timeout.min(Duration::from_millis(100));
                #[cfg(not(unix))]
                let retry=false;
                if !retry {return Err(error).context("spawn rg");}
                // exec did not start: retrying this transient open-writer
                // condition cannot duplicate an executed search.
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    };
    let pid = i32::try_from(child.id()).context("rg child pid did not fit i32")?;
    let mut stdout = child.stdout.take().context("rg stdout pipe missing")?;
    let mut stderr = child.stderr.take().context("rg stderr pipe missing")?;
    if let Err(error) = rg_set_nonblocking(&stdout)
        .context("make rg stdout nonblocking")
        .and_then(|_| rg_set_nonblocking(&stderr).context("make rg stderr nonblocking"))
    {
        rg_terminate_process_group(&mut child, pid)
            .context("terminate rg after capture setup failure")?;
        return Err(error);
    }

    let started = spawn_started;
    let mut stdout_bytes = Vec::with_capacity(stdout_cap.min(32 * 1024));
    let mut stderr_bytes = Vec::with_capacity(stderr_cap.min(32 * 1024));
    let reason = loop {
        let stdout_limited = match rg_drain_pipe(&mut stdout, &mut stdout_bytes, stdout_cap) {
            Ok(limited) => limited,
            Err(error) => {
                rg_terminate_process_group(&mut child, pid)
                    .context("terminate rg after stdout capture failure")?;
                return Err(error).context("capture rg stdout");
            }
        };
        let stderr_limited = match rg_drain_pipe(&mut stderr, &mut stderr_bytes, stderr_cap) {
            Ok(limited) => limited,
            Err(error) => {
                rg_terminate_process_group(&mut child, pid)
                    .context("terminate rg after stderr capture failure")?;
                return Err(error).context("capture rg stderr");
            }
        };
        if stdout_limited {
            break RgStopReason::StdoutLimit;
        }
        if stderr_limited {
            break RgStopReason::StderrLimit;
        }
        if started.elapsed() >= timeout {
            break RgStopReason::TimedOut;
        }
        match rg_exited_without_reap(&mut child, pid) {
            Ok(true) => break RgStopReason::Exited,
            Ok(false) => std::thread::sleep(RG_POLL_INTERVAL),
            Err(error) => {
                rg_terminate_process_group(&mut child, pid)
                    .context("terminate rg after status polling failure")?;
                return Err(error).context("poll rg");
            }
        }
    };

    let (status, descendants_terminated) = rg_terminate_process_group(&mut child, pid)
        .context("terminate and reap rg process group")?;
    let stdout_limited = rg_drain_pipe(&mut stdout, &mut stdout_bytes, stdout_cap)
        .context("finish capturing rg stdout")?;
    let stderr_limited = rg_drain_pipe(&mut stderr, &mut stderr_bytes, stderr_cap)
        .context("finish capturing rg stderr")?;
    let reason = if stdout_limited {
        RgStopReason::StdoutLimit
    } else if stderr_limited {
        RgStopReason::StderrLimit
    } else {
        reason
    };
    Ok(BoundedRgOutput {
        status,
        stdout: stdout_bytes,
        stderr: stderr_bytes,
        reason,
        descendants_terminated,
    })
}

#[cfg(unix)]
fn rg_set_nonblocking(stream: &impl AsRawFd) -> Result<()> {
    let fd = stream.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error()).context("read rg pipe flags");
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error()).context("set rg pipe nonblocking");
    }
    Ok(())
}

#[cfg(not(unix))]
fn rg_set_nonblocking(_stream: &impl Read) -> Result<()> {
    Ok(())
}

fn rg_drain_pipe(pipe: &mut impl Read, retained: &mut Vec<u8>, cap: usize) -> Result<bool> {
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
            Err(error) => return Err(error).context("read bounded rg pipe"),
        }
    }
}

#[cfg(target_os = "linux")]
fn rg_exited_without_reap(_child: &mut std::process::Child, pid: i32) -> Result<bool> {
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
        return Err(std::io::Error::last_os_error()).context("waitid rg child");
    }
    Ok(info.si_signo == libc::SIGCHLD)
}

#[cfg(not(target_os = "linux"))]
fn rg_exited_without_reap(child: &mut std::process::Child, _pid: i32) -> Result<bool> {
    Ok(child.try_wait().context("poll rg")?.is_some())
}

#[cfg(target_os = "linux")]
fn rg_live_group_members(pgid: i32) -> Result<Vec<i32>> {
    let mut members = Vec::new();
    for entry in std::fs::read_dir("/proc").context("scan /proc for rg process group")? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
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
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                continue;
            }
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

#[cfg(all(unix, not(target_os = "linux")))]
fn rg_live_group_members(pgid: i32) -> Result<Vec<i32>> {
    let result = unsafe { libc::kill(-pgid, 0) };
    if result == 0 {
        Ok(vec![pgid])
    } else if std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        Ok(Vec::new())
    } else {
        Err(std::io::Error::last_os_error()).context("probe rg process group")
    }
}

#[cfg(unix)]
fn rg_signal_group(pgid: i32, signal: i32) -> Result<()> {
    if pgid <= 1 || pgid == unsafe { libc::getpgrp() } {
        bail!("refusing to signal unsafe rg process group {pgid}");
    }
    let actual = unsafe { libc::getpgid(pgid) };
    if actual != pgid {
        bail!("rg process group ownership changed: pid={pgid}, pgid={actual}");
    }
    if unsafe { libc::kill(-pgid, signal) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error).context("signal rg process group")
    }
}

#[cfg(unix)]
fn rg_terminate_process_group(
    child: &mut std::process::Child,
    pgid: i32,
) -> Result<(ExitStatus, usize)> {
    let initial = rg_live_group_members(pgid)?;
    let descendants = initial.iter().filter(|pid| **pid != pgid).count();
    if !initial.is_empty() {
        rg_signal_group(pgid, libc::SIGTERM)?;
        let term_deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < term_deadline && !rg_live_group_members(pgid)?.is_empty() {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !rg_live_group_members(pgid)?.is_empty() {
            rg_signal_group(pgid, libc::SIGKILL)?;
            let kill_deadline = Instant::now() + Duration::from_millis(500);
            while Instant::now() < kill_deadline && !rg_live_group_members(pgid)?.is_empty() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    let remaining = rg_live_group_members(pgid)?;
    if !remaining.is_empty() {
        bail!(
            "rg process group still has live members: {}",
            remaining
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let status = child.wait().context("reap rg leader")?;
    Ok((status, descendants))
}

#[cfg(not(unix))]
fn rg_terminate_process_group(
    child: &mut std::process::Child,
    _pid: i32,
) -> Result<(ExitStatus, usize)> {
    if child.try_wait().context("poll rg")?.is_none() {
        child.kill().context("terminate rg")?;
    }
    Ok((child.wait().context("reap rg")?, 0))
}

fn execute_fallback(
    workspace_root: &Path,
    input: &GrepInput,
    search_path: &Path,
) -> Result<ToolOutput> {
    let ignore_case = input.ignore_case.unwrap_or(false);
    let files_only = input.files_only.unwrap_or(false);
    let context = input.context.unwrap_or(0).min(MAX_CONTEXT_LINES);
    let needle = if ignore_case {
        input.pattern.to_lowercase()
    } else {
        input.pattern.clone()
    };
    // Compile the file glob once. `*.rs` matches a basename; `src/**/*.rs`
    // matches a path — so a file is kept if EITHER form matches.
    let glob_pat = input
        .glob
        .as_deref()
        .filter(|g| !g.trim().is_empty())
        .map(glob::Pattern::new)
        .transpose()
        .context("invalid grep glob")?;
    let line_matches = |line: &str| {
        if ignore_case {
            line.to_lowercase().contains(&needle)
        } else {
            line.contains(&needle)
        }
    };

    let mut matches = Vec::new();
    // Track output size incrementally — joining the whole match vector on every
    // matching line is O(n^2) and was a CPU/memory bomb on large trees.
    let mut output_bytes = 0usize;
    let mut files_scanned = 0usize;
    let mut truncated = false;
    let mut stack = vec![search_path.to_path_buf()];

    'walk: while let Some(path) = stack.pop() {
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if is_ignored_dir(name) {
                continue;
            }
            let Ok(entries) = fs::read_dir(&path) else {
                continue;
            };
            for entry in entries.flatten() {
                stack.push(entry.path());
            }
            continue;
        }
        if !path.is_file() {
            continue;
        }
        files_scanned += 1;
        if files_scanned > MAX_FALLBACK_FILES {
            truncated = true;
            break;
        }
        let display = relative_display(workspace_root, &path);
        if let Some(pat) = &glob_pat {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !pat.matches(name) && !pat.matches(&display) {
                continue;
            }
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let lines: Vec<&str> = content.lines().collect();

        if files_only {
            if lines.iter().any(|l| line_matches(l)) {
                output_bytes += display.len() + 1;
                matches.push(display);
                if output_bytes > MAX_GREP_BYTES {
                    truncated = true;
                    break 'walk;
                }
            }
            continue;
        }

        // `last_emitted` (per file) collapses the overlapping context windows of
        // adjacent matches so a line is never printed twice.
        let mut last_emitted: Option<usize> = None;
        for (idx, line) in lines.iter().enumerate() {
            if !line_matches(line) {
                continue;
            }
            let start = idx.saturating_sub(context);
            let end = (idx + context + 1).min(lines.len());
            for ci in start..end {
                if last_emitted.is_some_and(|last| ci <= last) {
                    continue;
                }
                let row = format!("{}:{}:{}", display, ci + 1, lines[ci]);
                output_bytes += row.len() + 1;
                matches.push(row);
                last_emitted = Some(ci);
                if output_bytes > MAX_GREP_BYTES {
                    truncated = true;
                    break 'walk;
                }
            }
        }
    }

    if truncated {
        matches.push("[grep output truncated]".to_string());
    }

    Ok(ToolOutput {
        summary: if matches.is_empty() {
            "grep found no matches.".to_string()
        } else {
            "grep found matches.".to_string()
        },
        content: matches.join("\n"),
    })
}

fn command_exists(command: &str) -> bool {
    let candidate = Path::new(command);
    if candidate.components().count() > 1 {
        return grep_executable(candidate);
    }
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir.as_path()
        };
        grep_executable(&dir.join(command))
    })
}

#[cfg(unix)]
fn grep_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn grep_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[cfg(unix)]
    #[test]
    fn rg_startup_recovers_when_an_executable_writer_closes() {
        let root=tempdir().unwrap();
        let path=fake_program(root.path(),"busy-rg","echo 'fake.txt:1:needle'");
        let writer=fs::OpenOptions::new().write(true).open(&path).unwrap();
        let release=std::thread::spawn(move||{
            std::thread::sleep(Duration::from_millis(25));
            drop(writer);
        });
        let output=execute_rg_with_program(&gi("needle"),root.path(),&path,Duration::from_secs(1));
        release.join().unwrap();
        assert!(output.unwrap().content.contains("needle"));
    }

    #[cfg(unix)]
    fn fake_program(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }

    fn gi(pattern: &str) -> GrepInput {
        GrepInput {
            pattern: pattern.to_string(),
            path: None,
            glob: None,
            context: None,
            ignore_case: None,
            files_only: None,
        }
    }

    #[test]
    fn fallback_skips_ignored_directories() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("real.rs"), "let needle = 1;\n").unwrap();
        let heavy = root.path().join("target").join("deps");
        fs::create_dir_all(&heavy).unwrap();
        fs::write(heavy.join("artifact.rs"), "let needle = 2;\n").unwrap();

        let out = execute_fallback(root.path(), &gi("needle"), root.path()).unwrap();
        assert!(out.content.contains("real.rs"));
        assert!(
            !out.content.contains("target"),
            "fallback must not descend into ignored dirs: {}",
            out.content
        );
    }

    #[test]
    fn fallback_truncates_without_quadratic_blowup() {
        let root = tempdir().unwrap();
        // Many matching lines — the old code rejoined the whole vec per line.
        let body = "needle\n".repeat(20_000);
        fs::write(root.path().join("big.txt"), body).unwrap();

        let out = execute_fallback(root.path(), &gi("needle"), root.path()).unwrap();
        assert!(out.content.contains("[grep output truncated]"));
        assert!(out.content.len() <= MAX_GREP_BYTES + 1_024);
    }

    #[test]
    fn fallback_context_shows_surrounding_lines() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("f.rs"), "a\nb\nNEEDLE\nd\ne\n").unwrap();
        let input = GrepInput {
            context: Some(1),
            ..gi("NEEDLE")
        };
        let out = execute_fallback(root.path(), &input, root.path()).unwrap();
        assert!(out.content.contains("f.rs:2:b"), "{}", out.content);
        assert!(out.content.contains("f.rs:3:NEEDLE"), "{}", out.content);
        assert!(out.content.contains("f.rs:4:d"), "{}", out.content);
        assert!(!out.content.contains(":1:a"), "{}", out.content);
    }

    #[test]
    fn fallback_ignore_case_and_files_only() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("f.rs"), "let Needle = 1;\nplain\n").unwrap();

        let ci = GrepInput {
            ignore_case: Some(true),
            ..gi("needle")
        };
        assert!(execute_fallback(root.path(), &ci, root.path())
            .unwrap()
            .content
            .contains("f.rs:1:let Needle"));

        let fo = GrepInput {
            files_only: Some(true),
            ..gi("Needle")
        };
        let out = execute_fallback(root.path(), &fo, root.path()).unwrap();
        assert_eq!(out.content.trim(), "f.rs", "files_only lists the path only");
    }

    #[cfg(unix)]
    #[test]
    fn rg_nonzero_exit_is_an_error_with_bounded_stderr_truth() {
        let root = tempdir().unwrap();
        let fake = fake_program(
            root.path(),
            "fake-rg-error",
            "echo 'broken index' >&2\nexit 2",
        );
        let error =
            execute_rg_with_program(&gi("needle"), root.path(), &fake, Duration::from_secs(2))
                .unwrap_err();
        let error=format!("{error:#}");
        assert!(error.contains("exit 2"), "{error}");
        assert!(error.contains("broken index"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn rg_output_flood_returns_only_useful_capped_results() {
        let root = tempdir().unwrap();
        let fake = fake_program(
            root.path(),
            "fake-rg-flood",
            "while :; do echo 'fake.txt:1:needle'; done",
        );
        let started = Instant::now();
        let output =
            execute_rg_with_program(&gi("needle"), root.path(), &fake, Duration::from_secs(2))
                .unwrap();
        assert!(output.summary.contains("truncated"), "{}", output.summary);
        assert!(output.content.contains("fake.txt:1:needle"));
        assert!(output.content.contains("rg was stopped"));
        assert!(output.content.len() <= MAX_GREP_BYTES + 128);
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[cfg(unix)]
    #[test]
    fn rg_timeout_preserves_partial_results_but_errors_when_empty() {
        let root = tempdir().unwrap();
        let partial = fake_program(
            root.path(),
            "fake-rg-partial-timeout",
            "echo 'fake.txt:1:needle'\nsleep 30",
        );
        let output = execute_rg_with_program(
            &gi("needle"),
            root.path(),
            &partial,
            Duration::from_millis(100),
        )
        .unwrap();
        assert!(output.summary.contains("partial"));
        assert!(output.content.contains("timed out after partial results"));

        let silent = fake_program(root.path(), "fake-rg-silent-timeout", "sleep 30");
        let error = execute_rg_with_program(
            &gi("needle"),
            root.path(),
            &silent,
            Duration::from_millis(100),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("timed out"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn rg_runner_terminates_background_descendants_and_stderr_floods() {
        let mut background = Command::new("/bin/sh");
        background.args(["-c", "(sleep 30) & echo $!"]);
        let output = run_rg_bounded(background, Duration::from_secs(2), 4096, 4096).unwrap();
        assert_eq!(output.reason, RgStopReason::Exited);
        assert!(output.descendants_terminated >= 1);
        let descendant = String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        assert!(!rg_test_process_is_live(descendant));

        let mut flood = Command::new("/bin/sh");
        flood.args(["-c", "while :; do printf 0123456789 >&2; done"]);
        let output = run_rg_bounded(flood, Duration::from_secs(2), 4096, 4096).unwrap();
        assert_eq!(output.reason, RgStopReason::StderrLimit);
        assert_eq!(output.stderr.len(), 4096);
    }

    #[cfg(unix)]
    fn rg_test_process_is_live(pid: i32) -> bool {
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        stat.rsplit_once(") ")
            .and_then(|(_, rest)| rest.as_bytes().first().copied())
            .is_some_and(|state| !matches!(state, b'Z' | b'X'))
    }
}
