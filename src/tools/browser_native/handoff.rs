//! Legacy login-handoff recovery (2026-07-07): older Phoenix builds could open
//! a clean, non-automated Chrome window on the
//! shared managed profile, for the user to complete real account logins —
//! "Sign in with Google", GitHub OAuth, phone 2FA approvals.
//!
//! This separate-window route is no longer exposed for new work. Phoenix now
//! mounts user login inside the conversation pane. The module remains only so
//! an old receipt/process can be identified and closed safely after an update.
//! The historical implementation used a separate window because managed chrome
//! is CDP-driven, and Google's sign-in hard-blocks browsers that look
//! automated — even with the automation launch flags stripped, a login this
//! important should happen in a browser with NO debugging port at all. The
//! handoff window is plain chrome on the SAME profile dir: whatever the user
//! signs into is persisted natively (cookies AND localStorage — the part
//! cookie-porting can never carry; Netlify's `nf-session` and Discord's token
//! both live there), and the next managed launch simply reads it back.
//!
//! Session-bound accounts (Google DBSC) become native to this profile — no
//! transplant, so nothing for the server to invalidate and the user's real
//! browser (Zen) is never touched.
//!
//! Safety: while a handoff is open, every managed launch is refused and the
//! pre-launch stale-sweep is suspended — the sweep matches chromes by
//! `--user-data-dir=<profile>` and would SIGKILL the user's login window
//! mid-2FA otherwise.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const RECEIPT_VERSION: u32 = 1;
const RECEIPT_MAX_BYTES: usize = 64 * 1024;
const RECEIPT_FILE_CAP: usize = 256;

#[derive(Debug, Clone)]
struct Handoff {
    pid: u32,
    process_start_ticks: u64,
    instance: String,
    profile: PathBuf,
    url: String,
    started_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HandoffReceipt {
    version: u32,
    pid: u32,
    process_start_ticks: u64,
    instance: String,
    profile: PathBuf,
    url: String,
    started_at: String,
}

fn state() -> &'static Mutex<HashMap<String, Handoff>> {
    static STATE: OnceLock<Mutex<HashMap<String, Handoff>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(target_os = "linux")]
fn process_stat(pid: u32) -> Option<(char, u64)> {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return None;
    };
    let (_, rest) = stat.rsplit_once(") ")?;
    let fields = rest.split_whitespace().collect::<Vec<_>>();
    let state = fields.first()?.chars().next()?;
    // `/proc/<pid>/stat` field 22; `fields[0]` is original field 3.
    let start_ticks = fields.get(19)?.parse().ok()?;
    Some((state, start_ticks))
}

#[cfg(not(target_os = "linux"))]
fn process_stat(_pid: u32) -> Option<(char, u64)> {
    None
}

fn process_identity_alive(handoff: &Handoff) -> bool {
    let Some((state, start_ticks)) = process_stat(handoff.pid) else {
        return false;
    };
    if matches!(state, 'Z' | 'X') || start_ticks != handoff.process_start_ticks {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;

        let proc_dir = PathBuf::from(format!("/proc/{}", handoff.pid));
        let Ok(metadata) = std::fs::metadata(&proc_dir) else {
            return false;
        };
        if metadata.uid() != unsafe { libc::geteuid() } {
            return false;
        }
        let Ok(command_line) = std::fs::read(proc_dir.join("cmdline")) else {
            return false;
        };
        let expected = format!("--user-data-dir={}", handoff.profile.display());
        command_line
            .split(|byte| *byte == 0)
            .any(|argument| argument == expected.as_bytes())
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

fn elapsed_secs(handoff: &Handoff) -> i64 {
    (chrono::Utc::now() - handoff.started_at)
        .num_seconds()
        .max(0)
}

fn receipts_dir() -> PathBuf {
    crate::config::phoenix_home().join("browser/login-handoffs")
}

fn receipt_path(instance: &str) -> Result<PathBuf> {
    anyhow::ensure!(
        instance.len() <= 256 && !instance.chars().any(char::is_control),
        "invalid browser instance id"
    );
    let digest = format!("{:x}", Sha256::digest(instance.as_bytes()));
    Ok(receipts_dir().join(format!("{digest}.json")))
}

fn receipt_for(handoff: &Handoff) -> HandoffReceipt {
    HandoffReceipt {
        version: RECEIPT_VERSION,
        pid: handoff.pid,
        process_start_ticks: handoff.process_start_ticks,
        instance: handoff.instance.clone(),
        profile: handoff.profile.clone(),
        url: handoff.url.clone(),
        started_at: handoff.started_at.to_rfc3339(),
    }
}

fn persist_receipt(handoff: &Handoff) -> Result<()> {
    ensure_receipt_capacity()?;
    let bytes = serde_json::to_vec_pretty(&receipt_for(handoff))?;
    anyhow::ensure!(
        bytes.len() <= RECEIPT_MAX_BYTES,
        "login handoff receipt is too large"
    );
    crate::config::private_io::atomic_write_private(&receipt_path(&handoff.instance)?, &bytes)
}

fn decode_receipt(path: &Path, bytes: &[u8]) -> Result<Handoff> {
    anyhow::ensure!(
        bytes.len() <= RECEIPT_MAX_BYTES,
        "login handoff receipt is unexpectedly large"
    );
    let receipt: HandoffReceipt =
        serde_json::from_slice(bytes).context("login handoff receipt is invalid JSON")?;
    anyhow::ensure!(
        receipt.version == RECEIPT_VERSION,
        "login handoff receipt version mismatch"
    );
    anyhow::ensure!(
        receipt.pid > 1
            && receipt.process_start_ticks > 0
            && receipt.instance.len() <= 256
            && !receipt.instance.chars().any(char::is_control)
            && receipt.url.len() <= 16 * 1024,
        "login handoff receipt contains invalid fields"
    );
    anyhow::ensure!(
        receipt_path(&receipt.instance)? == path,
        "login handoff receipt filename/identity mismatch"
    );
    let expected_profile = super::session::profile_dir_for_instance(
        &receipt.instance,
        super::session::browser_prefs().source.trim(),
    )?;
    anyhow::ensure!(
        receipt.profile == expected_profile,
        "login handoff receipt profile does not match this coworker"
    );
    let started_at = chrono::DateTime::parse_from_rfc3339(&receipt.started_at)
        .context("login handoff receipt start time is invalid")?
        .with_timezone(&chrono::Utc);
    Ok(Handoff {
        pid: receipt.pid,
        process_start_ticks: receipt.process_start_ticks,
        instance: receipt.instance,
        profile: receipt.profile,
        url: receipt.url,
        started_at,
    })
}

fn load_receipt(instance: &str) -> Result<Option<Handoff>> {
    let path = receipt_path(instance)?;
    let Some(bytes) =
        crate::config::private_io::read_private_file_limited(&path, RECEIPT_MAX_BYTES)?
    else {
        return Ok(None);
    };
    Ok(Some(decode_receipt(&path, &bytes)?))
}

fn remove_receipt(instance: &str) {
    if let Ok(path) = receipt_path(instance) {
        if let Err(error) = crate::config::private_io::remove_private_file(&path) {
            super::blog(&format!(
                "login handoff: could not remove receipt for {instance}: {error:#}"
            ));
        }
    }
}

fn hydrate_instance(instance: &str, guard: &mut HashMap<String, Handoff>) -> Result<()> {
    if guard.contains_key(instance) {
        return Ok(());
    }
    let Some(handoff) = load_receipt(instance)? else {
        return Ok(());
    };
    if process_identity_alive(&handoff) {
        super::blog(&format!(
            "login handoff: recovered live pid {} for {} after gateway restart",
            handoff.pid, handoff.instance
        ));
        guard.insert(instance.to_string(), handoff);
    } else {
        remove_receipt(instance);
    }
    Ok(())
}

fn hydrate_all_receipts(guard: &mut HashMap<String, Handoff>) -> Result<()> {
    let dir = receipts_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut count = 0usize;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        count += 1;
        anyhow::ensure!(count <= RECEIPT_FILE_CAP, "too many login handoff receipts");
        anyhow::ensure!(
            !metadata.file_type().is_symlink() && metadata.is_file(),
            "unsafe login handoff receipt entry {}",
            path.display()
        );
        let Some(bytes) =
            crate::config::private_io::read_private_file_limited(&path, RECEIPT_MAX_BYTES)?
        else {
            continue;
        };
        let handoff = decode_receipt(&path, &bytes)?;
        if process_identity_alive(&handoff) {
            guard.entry(handoff.instance.clone()).or_insert(handoff);
        } else {
            remove_receipt(&handoff.instance);
        }
    }
    Ok(())
}

fn ensure_receipt_capacity() -> Result<()> {
    let dir = receipts_dir();
    crate::config::private_io::prepare_phoenix_directory(&dir)?;
    let mut count = 0usize;
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        anyhow::ensure!(
            !metadata.file_type().is_symlink() && metadata.is_file(),
            "unsafe login handoff receipt entry {}",
            path.display()
        );
        count += 1;
        anyhow::ensure!(
            count < RECEIPT_FILE_CAP,
            "too many login handoff receipts; refusing to forget a potentially live login window"
        );
    }
    Ok(())
}

/// A human-readable description of the active handoff, if one is open.
/// `Session::open` and the stale-sweep consult this to stand down.
pub(super) fn active(instance: &str) -> Option<String> {
    let mut guard = state().lock().ok()?;
    if let Err(error) = hydrate_instance(instance, &mut guard) {
        // Fail closed: an unreadable latch must never authorize the stale
        // sweep to kill a possibly-live login window.
        return Some(format!(
            "login handoff receipt for this coworker is unreadable ({error:#}); managed browser launch is paused so Phoenix cannot destroy a possible login window"
        ));
    }
    match guard.get(instance) {
        Some(h) if process_identity_alive(h) => Some(format!(
            "login handoff window open on {} for {}s (pid {}, profile {})",
            h.url,
            elapsed_secs(h),
            h.pid,
            h.instance
        )),
        Some(_) => {
            // The user closed the window themselves — chrome exited cleanly on
            // its own, storage is flushed; clear the latch so launches resume.
            guard.remove(instance);
            remove_receipt(instance);
            None
        }
        None => None,
    }
}

/// Close the handoff window gracefully (SIGTERM → chrome flushes cookies and
/// localStorage to the profile) and release the launch latch.
pub(super) fn finish(instance: &str) -> Result<String> {
    let mut guard = state().lock().unwrap_or_else(|p| p.into_inner());
    hydrate_instance(instance, &mut guard)?;
    let Some(h) = guard.remove(instance) else {
        bail!("no login handoff window is open — nothing to finish");
    };
    let closed = if process_identity_alive(&h) {
        super::engine::sigterm_and_wait(h.pid)
    } else {
        true // the user already closed it — chrome flushed on its own exit
    };
    super::blog(&format!(
        "login handoff: finished (pid {}, window open {}s, clean exit: {closed})",
        h.pid,
        elapsed_secs(&h)
    ));
    if closed {
        remove_receipt(instance);
        Ok(
            "Login window closed cleanly — cookies AND localStorage are flushed to the shared \
             coworker's private profile. Their next browser action relaunches the managed browser with the new \
             login in place: navigate to the site ONCE to verify it."
                .to_string(),
        )
    } else {
        guard.insert(instance.to_string(), h);
        Ok(
            "Login window did not exit within 5s of SIGTERM, so Phoenix kept the profile latch \
             closed. Close that login window manually, then finish the handoff again; Phoenix \
             will not launch or sweep another browser over potentially unflushed storage."
                .to_string(),
        )
    }
}

/// Gateway-exit variant: close a human login handoff inside the same global
/// deadline as managed browser sessions. A handoff is a Phoenix-owned Chrome
/// too; leaving it behind would keep the shared profile locked underneath the
/// replacement gateway.
pub(super) fn shutdown(deadline: std::time::Instant) {
    let lock_deadline = deadline.min(std::time::Instant::now() + super::SHUTDOWN_LOCK_BUDGET);
    let Some(mut guard) = super::try_lock_until(state(), lock_deadline) else {
        super::blog(
            "shutdown: login-handoff state stayed busy; its existing close operation owns cleanup",
        );
        return;
    };
    let handoffs = guard
        .drain()
        .map(|(_, handoff)| handoff)
        .collect::<Vec<_>>();
    drop(guard);
    for handoff in handoffs {
        let closed = !process_identity_alive(&handoff)
            || super::engine::sigterm_and_wait_until(handoff.pid, deadline);
        if closed {
            remove_receipt(&handoff.instance);
        }
        super::blog(&format!(
            "shutdown: login handoff pid {} for {} close completed (clean exit: {closed})",
            handoff.pid, handoff.instance
        ));
    }
}

/// Whether a human handoff currently owns this exact profile. The stale
/// process sweep uses profile identity, not a global latch, so one coworker's
/// login does not prevent another coworker from browsing.
pub(super) fn active_for_profile(profile: &std::path::Path) -> bool {
    let mut guard = state().lock().unwrap_or_else(|p| p.into_inner());
    if let Err(error) = hydrate_all_receipts(&mut guard) {
        super::blog(&format!(
            "login handoff: receipt scan failed closed before stale sweep: {error:#}"
        ));
        return true;
    }
    guard.retain(|instance, handoff| {
        let alive = process_identity_alive(handoff);
        if !alive {
            remove_receipt(instance);
        }
        alive
    });
    guard.values().any(|handoff| handoff.profile == profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_round_trip_binds_instance_profile_and_process_start() {
        let home = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let instance = "agent-handoff-test";
        let profile = super::super::session::profile_dir_for_instance(instance, "phoenix").unwrap();
        let handoff = Handoff {
            pid: std::process::id(),
            process_start_ticks: process_stat(std::process::id()).unwrap().1,
            instance: instance.to_string(),
            profile,
            url: "https://accounts.example.com/login".into(),
            started_at: chrono::Utc::now(),
        };
        persist_receipt(&handoff).unwrap();
        let loaded = load_receipt(instance).unwrap().unwrap();
        assert_eq!(loaded.pid, handoff.pid);
        assert_eq!(loaded.process_start_ticks, handoff.process_start_ticks);
        assert_eq!(loaded.profile, handoff.profile);
        assert!(
            !process_identity_alive(&loaded),
            "the test process does not own the exact Chrome profile argument"
        );
        assert!(
            !active_for_profile(&handoff.profile),
            "the receipt scan should discard a stale, identity-mismatched process without treating its persistent lock file as a handoff"
        );
        assert!(load_receipt(instance).unwrap().is_none());
    }
}
