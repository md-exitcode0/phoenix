//! Chromium-family login porting: read + DECRYPT the source browser's cookie
//! store (Chrome, Brave, Edge, Chromium, Vivaldi, Opera) so the Chrome Phoenix
//! drives is exactly as logged-in as the source browser right now.
//!
//! Unlike Firefox (`browser_cookies.rs`, plaintext `cookies.sqlite`), Chromium
//! encrypts every cookie value with OS-Crypt. On Linux:
//!   - `v10` payloads use a hardcoded password `"peanuts"` (the "basic" store).
//!   - `v11` payloads use a per-user secret from the GNOME/KWallet keyring
//!     ("<Browser> Safe Storage"), read over the Secret Service.
//! Both derive an AES-128 key via PBKDF2-HMAC-SHA1(pw, "saltysalt", 1, 16) and
//! encrypt AES-128-CBC with a 16-space IV. We read the DB read-only from a
//! snapshot copy and never touch the live browser.
//!
//! The device-bound exclusion (Google/Microsoft) is applied by the caller
//! (`browser_native::port_logins`) exactly as for Firefox — never ported.

use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::{
    ffi::OsStr,
    io::Read,
    process::{Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
use anyhow::{Context, Result};

use crate::tools::browser_cookies::PortedCookie;

type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

#[cfg(unix)]
const SECRET_TOOL_TIMEOUT: Duration = Duration::from_secs(3);
#[cfg(unix)]
const MAX_SECRET_TOOL_STDOUT_BYTES: usize = 64 * 1024;
#[cfg(unix)]
const MAX_SECRET_TOOL_STDERR_BYTES: usize = 64 * 1024;

/// Is this a Chromium-family source we can read + decrypt?
pub fn is_chromium_source(source: &str) -> bool {
    chromium_data_root(source).is_some()
}

/// Per-source config data root (Linux) + the keyring app-name Chromium uses for
/// its "<name> Safe Storage" secret. Only Linux is wired today.
fn chromium_data_root(source: &str) -> Option<(PathBuf, &'static str)> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let cfg = home.join(".config");
    let (rel, keyring_app): (&str, &str) = match source.trim().to_lowercase().as_str() {
        "chrome" | "google-chrome" => ("google-chrome", "chrome"),
        "chromium" => ("chromium", "chromium"),
        "brave" => ("BraveSoftware/Brave-Browser", "brave"),
        "brave-nightly" => ("BraveSoftware/Brave-Browser-Nightly", "brave"),
        "edge" | "microsoft-edge" => ("microsoft-edge", "chromium"),
        "vivaldi" => ("vivaldi", "chrome"),
        "opera" => ("opera", "chromium"),
        _ => return None,
    };
    Some((cfg.join(rel), keyring_app))
}

/// Find the cookie DB for a source. Modern Chromium keeps it at
/// `<profile>/Network/Cookies`; older builds at `<profile>/Cookies`. Prefer the
/// profile actually in use (freshest DB) across Default + `Profile N`.
fn locate_cookie_db(root: &Path) -> Result<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut profiles = vec![root.join("Default")];
    if let Ok(entries) = std::fs::read_dir(root) {
        for e in entries.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("Profile ") {
                profiles.push(e.path());
            }
        }
    }
    for p in profiles {
        for rel in ["Network/Cookies", "Cookies"] {
            let db = p.join(rel);
            if db.is_file() {
                candidates.push(db);
            }
        }
    }
    // Freshest cookie DB = the profile the user actually browses in.
    candidates.sort_by_key(|db| {
        std::fs::metadata(db)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0)
    });
    candidates.pop().with_context(|| {
        format!(
            "no Cookies DB under {} — open the browser once and sign in first",
            root.display()
        )
    })
}

/// Last activity for a Chromium-family source without opening or decrypting
/// its cookie database. Automatic source selection uses this metadata-only
/// probe so the browser the person used most recently wins.
pub fn source_activity_mtime(source: &str) -> Result<std::time::SystemTime> {
    let (root, _) = chromium_data_root(source)
        .with_context(|| format!("unsupported Chromium cookie source `{source}`"))?;
    let db = locate_cookie_db(&root)?;
    std::fs::metadata(&db)
        .and_then(|metadata| metadata.modified())
        .with_context(|| format!("reading activity time for {}", db.display()))
}

/// Derive the AES-128 key from a password using Chromium's fixed KDF params.
fn derive_key(password: &[u8]) -> [u8; 16] {
    let mut key = [0u8; 16];
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(password, b"saltysalt", 1, &mut key);
    key
}

/// The v11 keyring secret ("<Browser> Safe Storage") via the Secret Service.
/// Best-effort: if the keyring/library isn't reachable we return None and the
/// caller falls back so v10 cookies still port. Uses `secret-tool` if present
/// (libsecret CLI) — no heavy dbus dependency in the build.
fn keyring_secret(keyring_app: &str) -> Option<Vec<u8>> {
    let label = format!("{} Safe Storage", title_case(keyring_app));
    // Try libsecret's `secret-tool` first (present on most GNOME systems).
    #[cfg(unix)]
    if let Some(secret) = run_secret_tool_lookup(
        OsStr::new("secret-tool"),
        &["lookup", "application", keyring_app],
        SECRET_TOOL_TIMEOUT,
    ) {
        return Some(secret);
    }
    // Some stores key the secret by label instead of the application attribute.
    #[cfg(unix)]
    if let Some(secret) = run_secret_tool_lookup(
        OsStr::new("secret-tool"),
        &["lookup", "label", &label],
        SECRET_TOOL_TIMEOUT,
    ) {
        return Some(secret);
    }
    None
}

/// Run one keyring lookup in its own process group. Captured bytes are never
/// formatted into an error or tracing field: stdout is either returned as the
/// successful secret or dropped, and stderr is drained only to prevent a pipe
/// deadlock. Every outcome terminates the owned group and reaps its leader.
#[cfg(unix)]
fn run_secret_tool_lookup(program: &OsStr, args: &[&str], timeout: Duration) -> Option<Vec<u8>> {
    use std::os::unix::process::CommandExt;

    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: this closure invokes only async-signal-safe setpgid between fork
    // and exec. A dedicated group lets timeout/flood cleanup include helpers
    // spawned by secret-tool instead of abandoning pipe-holding descendants.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }

    let mut child = command.spawn().ok()?;
    let pgid = match i32::try_from(child.id()) {
        Ok(pgid) => pgid,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };
    let mut stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = terminate_secret_tool_group(&mut child, pgid);
            return None;
        }
    };
    let mut stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            let _ = terminate_secret_tool_group(&mut child, pgid);
            return None;
        }
    };
    if set_nonblocking(&stdout).is_err() || set_nonblocking(&stderr).is_err() {
        let _ = terminate_secret_tool_group(&mut child, pgid);
        return None;
    }

    let deadline = Instant::now() + timeout;
    let mut secret = Vec::with_capacity(256);
    let mut discarded_stderr = Vec::new();
    let mut stdout_total = 0usize;
    let mut stderr_total = 0usize;
    let mut stdout_overflow = false;
    let mut stderr_overflow = false;
    let mut capture_failed = false;
    let mut exited = false;
    loop {
        if drain_bounded_pipe(
            &mut stdout,
            &mut secret,
            &mut stdout_total,
            MAX_SECRET_TOOL_STDOUT_BYTES,
            true,
            &mut stdout_overflow,
        )
        .is_err()
            || drain_bounded_pipe(
                &mut stderr,
                &mut discarded_stderr,
                &mut stderr_total,
                MAX_SECRET_TOOL_STDERR_BYTES,
                false,
                &mut stderr_overflow,
            )
            .is_err()
        {
            capture_failed = true;
            break;
        }
        if stdout_overflow || stderr_overflow || Instant::now() >= deadline {
            break;
        }
        match secret_tool_child_exited(&mut child, pgid) {
            Ok(value) => exited = value,
            Err(_) => {
                capture_failed = true;
                break;
            }
        }
        if exited {
            // The final drain above ran after observing all writes that were
            // already readable. Cleanup closes any descendant-held writers.
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    let (status, cleanup_confirmed) = terminate_secret_tool_group(&mut child, pgid);
    // One final nonblocking drain after all owned writers were signalled.
    let final_stdout_ok = drain_bounded_pipe(
        &mut stdout,
        &mut secret,
        &mut stdout_total,
        MAX_SECRET_TOOL_STDOUT_BYTES,
        true,
        &mut stdout_overflow,
    )
    .is_ok();
    let final_stderr_ok = drain_bounded_pipe(
        &mut stderr,
        &mut discarded_stderr,
        &mut stderr_total,
        MAX_SECRET_TOOL_STDERR_BYTES,
        false,
        &mut stderr_overflow,
    )
    .is_ok();
    // Explicitly drop the stderr sink before deciding the outcome. Neither it
    // nor a failed/oversized stdout is ever interpolated into diagnostics.
    drop(discarded_stderr);

    let status = status?;
    if exited
        && cleanup_confirmed
        && !capture_failed
        && final_stdout_ok
        && final_stderr_ok
        && !stdout_overflow
        && !stderr_overflow
        && status.success()
        && !secret.is_empty()
    {
        Some(secret)
    } else {
        None
    }
}

#[cfg(unix)]
fn set_nonblocking(stream: &impl std::os::fd::AsRawFd) -> std::io::Result<()> {
    let descriptor = stream.as_raw_fd();
    // SAFETY: descriptor belongs to the live pipe borrowed by this function.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: F_SETFL changes flags on the same valid descriptor.
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn drain_bounded_pipe<R: Read>(
    reader: &mut R,
    kept: &mut Vec<u8>,
    total: &mut usize,
    limit: usize,
    retain: bool,
    overflow: &mut bool,
) -> std::io::Result<()> {
    let mut buffer = [0u8; 4096];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(read) => {
                *total = (*total).saturating_add(read);
                if retain {
                    let available = limit.saturating_sub(kept.len());
                    kept.extend_from_slice(&buffer[..read.min(available)]);
                }
                if *total > limit {
                    *overflow = true;
                    return Ok(());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) => return Err(error),
        }
    }
}

#[cfg(target_os = "linux")]
fn secret_tool_child_exited(_child: &mut std::process::Child, pid: i32) -> std::io::Result<bool> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: WNOWAIT observes our direct child without releasing its PID,
    // preventing group-id reuse before cleanup signals the owned group.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(info.si_signo == libc::SIGCHLD)
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn secret_tool_child_exited(child: &mut std::process::Child, _pid: i32) -> std::io::Result<bool> {
    child.try_wait().map(|status| status.is_some())
}

#[cfg(unix)]
fn terminate_secret_tool_group(
    child: &mut std::process::Child,
    pgid: i32,
) -> (Option<ExitStatus>, bool) {
    if pgid <= 1 || pgid == unsafe { libc::getpgrp() } {
        let _ = child.kill();
        return (child.wait().ok(), false);
    }
    let mut confirmed = true;
    // SAFETY: the child established pgid == child pid before exec. On Linux
    // its unreaped leader reserves this numeric group identity until wait().
    for signal in [libc::SIGTERM, libc::SIGKILL] {
        let result = unsafe { libc::kill(-pgid, signal) };
        if result < 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
            confirmed = false;
        }
        if signal == libc::SIGTERM {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let status = child.wait().ok();
    if status.is_none() {
        confirmed = false;
    }
    if !secret_tool_group_has_no_live_members(pgid).unwrap_or(false) {
        confirmed = false;
    }
    (status, confirmed)
}

#[cfg(target_os = "linux")]
fn secret_tool_group_has_no_live_members(pgid: i32) -> std::io::Result<bool> {
    for entry in std::fs::read_dir("/proc")? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
            .is_none()
        {
            continue;
        }
        let stat = match std::fs::read_to_string(entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let Some(after_name) = stat.rsplit_once(") ").map(|(_, fields)| fields) else {
            continue;
        };
        let mut fields = after_name.split_whitespace();
        let state = fields
            .next()
            .and_then(|field| field.as_bytes().first().copied());
        let _parent_pid = fields.next();
        let member_pgid = fields.next().and_then(|field| field.parse::<i32>().ok());
        if member_pgid == Some(pgid) && !matches!(state, Some(b'Z' | b'X')) {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn secret_tool_group_has_no_live_members(pgid: i32) -> std::io::Result<bool> {
    // SAFETY: signal zero performs an existence/permission probe only.
    let result = unsafe { libc::kill(-pgid, 0) };
    if result == 0 {
        Ok(false)
    } else {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(true)
        } else {
            Err(error)
        }
    }
}

fn title_case(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Decrypt one Chromium `encrypted_value` blob. Returns None if the scheme
/// isn't one we can handle (e.g. v11 with no reachable keyring).
fn decrypt_value(blob: &[u8], v10_key: &[u8; 16], v11_key: Option<&[u8; 16]>) -> Option<String> {
    if blob.len() < 3 {
        // Pre-encryption plaintext (very old profiles) — take as-is.
        return String::from_utf8(blob.to_vec()).ok();
    }
    let (key, body) = match &blob[..3] {
        b"v10" => (v10_key, &blob[3..]),
        b"v11" => (v11_key?, &blob[3..]),
        _ => return String::from_utf8(blob.to_vec()).ok(), // unencrypted
    };
    let iv = [0x20u8; 16]; // 16 spaces
    let mut buf = body.to_vec();
    let plain = Aes128CbcDec::new(key.into(), &iv.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .ok()?;
    // Chromium >= M130 prepends a 32-byte SHA-256 of the host to the plaintext
    // as a tamper check. A real cookie value is printable text, whereas the hash
    // prefix is arbitrary bytes — so if the leading 32 bytes contain a control
    // byte (NUL, etc.), they're the hash and we drop them. (A bare UTF-8 check
    // won't do: NUL is valid UTF-8.)
    let has_binary_prefix = plain.len() > 32
        && plain[..32]
            .iter()
            .any(|b| *b < 0x09 || (*b > 0x0d && *b < 0x20));
    let value = if has_binary_prefix {
        &plain[32..]
    } else {
        plain
    };
    std::str::from_utf8(value).ok().map(|s| s.to_string())
}

/// Chromium `expires_utc` is microseconds since 1601-01-01 UTC. Convert to Unix
/// seconds; 0 = session cookie.
fn chromium_expiry_to_unix_secs(raw: i64) -> Option<f64> {
    if raw <= 0 {
        return None;
    }
    // 11644473600 = seconds between 1601-01-01 and 1970-01-01.
    Some(raw as f64 / 1_000_000.0 - 11_644_473_600.0)
}

fn same_site(raw: i64) -> Option<String> {
    match raw {
        0 => Some("None".to_string()),
        1 => Some("Lax".to_string()),
        2 => Some("Strict".to_string()),
        _ => None, // -1 unspecified
    }
}

/// Read + decrypt every cookie from a Chromium-family source browser.
pub fn read_cookies(source: &str) -> Result<Vec<PortedCookie>> {
    let (root, keyring_app) = chromium_data_root(source)
        .with_context(|| format!("unsupported chromium source: {source}"))?;
    let db = locate_cookie_db(&root)?;

    let v10_key = derive_key(b"peanuts");
    let v11_key = keyring_secret(keyring_app).map(|s| derive_key(&s));

    let (cookies, v11_seen, v11_ok) = read_snapshot(&db, &v10_key, v11_key.as_ref())?;
    if v11_seen > 0 && v11_key.is_none() {
        tracing::warn!(
            "login port ({source}): {v11_seen} keyring-encrypted (v11) cookies skipped — no Secret Service key. Install `secret-tool` (libsecret) or unlock the login keyring so Brave/Edge sessions port."
        );
    }
    tracing::info!(
        "login port ({source}): decrypted {} cookies ({} v11 via keyring) from {}",
        cookies.len(),
        v11_ok,
        db.display()
    );
    Ok(cookies)
}

/// Read a Phoenix-managed Chromium profile without starting or attaching a
/// browser. This is the safe fallback for a disposable worker when its
/// spawning agent currently has no live CDP session: a private snapshot of
/// the cookie database is decrypted, filtered by the caller, then injected
/// into the worker's separate profile. No tabs, browser process, or profile
/// directory is shared.
pub(crate) fn read_managed_profile_cookies(profile_root: &Path) -> Result<Vec<PortedCookie>> {
    let root = match profile_root.canonicalize() {
        Ok(root) => root,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("resolve managed browser profile {}", profile_root.display())
            })
        }
    };
    let managed_root = crate::config::phoenix_home().join("browser");
    anyhow::ensure!(
        root.starts_with(&managed_root),
        "managed browser cookie snapshot must stay beneath Phoenix browser storage"
    );
    let Some(db) = [
        root.join("Default/Network/Cookies"),
        root.join("Default/Cookies"),
        root.join("Network/Cookies"),
        root.join("Cookies"),
    ]
    .into_iter()
    .find(|candidate| candidate.is_file()) else {
        // A spawning agent that never opened its browser has no cookie jar to
        // inherit. The worker still gets a clean, separate profile rather
        // than failing its first browser action.
        return Ok(Vec::new());
    };
    let v10_key = derive_key(b"peanuts");
    // Phoenix normally launches Chrome/Chromium. The Chromium v11 secret is
    // desktop-user scoped, so a profile snapshot can decrypt it without ever
    // opening the parent browser. If the user uses a browser-specific secret
    // we report a partial portable snapshot rather than copying opaque DBs.
    let v11_key = keyring_secret("chrome").map(|s| derive_key(&s));
    let (cookies, v11_seen, v11_ok) = read_snapshot(&db, &v10_key, v11_key.as_ref())?;
    if v11_seen > 0 && v11_key.is_none() {
        tracing::warn!(
            "worker browser snapshot: {v11_seen} keyring-encrypted parent cookies were unavailable; only portable cookies were copied"
        );
    }
    tracing::debug!(
        "worker browser snapshot: decrypted {} cookies ({} v11) from managed profile",
        cookies.len(),
        v11_ok
    );
    Ok(cookies)
}

/// Snapshot a possibly WAL-backed Chromium cookie database and decrypt it.
/// The temporary directory is process-private and RAII-cleaned, so neither a
/// live parent browser nor a disposable worker profile is ever modified.
fn read_snapshot(
    db: &Path,
    v10_key: &[u8; 16],
    v11_key: Option<&[u8; 16]>,
) -> Result<(Vec<PortedCookie>, usize, usize)> {
    let temp_dir = tempfile::Builder::new()
        .prefix("phoenix-chromium-cookies-")
        .tempdir()
        .context("prepare private Chromium cookie snapshot")?;
    let temp = temp_dir.path().join("Cookies");
    std::fs::copy(db, &temp).with_context(|| format!("failed to copy {}", db.display()))?;
    for ext in ["-wal", "-shm"] {
        let src = PathBuf::from(format!("{}{ext}", db.display()));
        if src.is_file() {
            let dst = PathBuf::from(format!("{}{ext}", temp.display()));
            std::fs::copy(&src, &dst)
                .with_context(|| format!("copy Chromium cookie sidecar {}", src.display()))?;
        }
    }
    read_from_db(&temp, v10_key, v11_key)
}

/// Returns (cookies, v11_seen, v11_decrypted).
fn read_from_db(
    path: &Path,
    v10_key: &[u8; 16],
    v11_key: Option<&[u8; 16]>,
) -> Result<(Vec<PortedCookie>, usize, usize)> {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .context("open chromium cookie snapshot")?;
    let mut stmt = conn
        .prepare(
            "SELECT host_key, name, encrypted_value, path, is_secure, is_httponly, \
             expires_utc, samesite FROM cookies",
        )
        .context("query chromium cookies (schema mismatch?)")?;
    let mut rows = stmt.query([]).context("run chromium cookie query")?;

    let mut out = Vec::new();
    let mut v11_seen = 0usize;
    let mut v11_ok = 0usize;
    while let Some(row) = rows.next()? {
        let domain: String = row.get(0)?;
        let name: String = row.get(1)?;
        let enc: Vec<u8> = row.get(2)?;
        let path: String = row.get(3)?;
        let is_secure: i64 = row.get(4)?;
        let is_http_only: i64 = row.get(5)?;
        let expires_utc: i64 = row.get(6)?;
        let samesite: i64 = row.get(7)?;

        let is_v11 = enc.len() >= 3 && &enc[..3] == b"v11";
        if is_v11 {
            v11_seen += 1;
        }
        let Some(value) = decrypt_value(&enc, v10_key, v11_key) else {
            continue; // unreadable (e.g. v11 without keyring) — skip, don't fail
        };
        if is_v11 {
            v11_ok += 1;
        }
        out.push(PortedCookie {
            domain,
            name,
            value,
            path,
            secure: is_secure != 0,
            http_only: is_http_only != 0,
            same_site: same_site(samesite),
            expires: chromium_expiry_to_unix_secs(expires_utc),
        });
    }
    Ok((out, v11_seen, v11_ok))
}

#[cfg(all(test, unix))]
mod secret_tool_runner_tests {
    use super::*;

    #[test]
    fn fake_secret_tool_success_returns_only_bounded_stdout() {
        let secret = run_secret_tool_lookup(
            OsStr::new("/bin/sh"),
            &["-c", "printf 'keyring-value'; printf 'ignored-stderr' >&2"],
            Duration::from_secs(1),
        );
        assert_eq!(secret.as_deref(), Some(b"keyring-value".as_slice()));
    }

    #[test]
    fn fake_secret_tool_timeout_kills_descendant_group() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("escaped");
        let marker = marker.to_str().unwrap();
        let started = Instant::now();
        let secret = run_secret_tool_lookup(
            OsStr::new("/bin/sh"),
            &[
                "-c",
                "(sleep 0.2; printf escaped > \"$1\") & wait",
                "phoenix-secret-tool-test",
                marker,
            ],
            Duration::from_millis(40),
        );
        assert!(secret.is_none());
        assert!(started.elapsed() < Duration::from_secs(1));
        std::thread::sleep(Duration::from_millis(350));
        assert!(!dir.path().join("escaped").exists());
    }

    #[test]
    fn fake_secret_tool_output_flood_is_cut_off() {
        let started = Instant::now();
        let secret = run_secret_tool_lookup(
            OsStr::new("/bin/sh"),
            &[
                "-c",
                "while :; do printf '0123456789abcdef0123456789abcdef'; done",
            ],
            Duration::from_secs(2),
        );
        assert!(secret.is_none());
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}

#[cfg(test)]
#[path = "chromium_cookies_tests.rs"]
mod chromium_cookies_tests;
