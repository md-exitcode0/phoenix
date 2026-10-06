//! Phoenix Desktop — the desktop shell (plan 015). A thin gateway client:
//! the webview talks to the WS bridge for turns/journal; this Rust side only
//! does what a webview can't — process control (start/stop the gateway),
//! and local reads under ~/.phoenix (token, sessions, config, log tail).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};
use zeroize::Zeroizing;

mod channels;
mod native_browser;
mod onboarding_reach;
mod powers;
mod term;

#[path = "../../src/wire_protocol.rs"]
mod wire_protocol;

#[path = "../../src/vital_memory_document.rs"]
mod vital_memory_document;

use wire_protocol::GATEWAY_WIRE_PROTOCOL;
// GTK can return zero when its final hidden relay window is destroyed during
// shutdown. Preserve a failed shell's status across that event-loop teardown.
static CHROMIUM_EXIT_CODE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
const GATEWAY_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(750);
const GATEWAY_PROBE_ATTEMPTS: usize = 3;
const GATEWAY_EXIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const GATEWAY_EXIT_POLL: std::time::Duration = std::time::Duration::from_millis(100);
const GATEWAY_LIFECYCLE_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(35);
const GATEWAY_LIFECYCLE_LOCK_POLL: std::time::Duration = std::time::Duration::from_millis(50);
const RESTART_LOG_MAX_BYTES: u64 = 20 * 1024 * 1024;
const VOICE_HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const VOICE_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const VOICE_CAPTURE_MAX_SECONDS: u64 = 5 * 60;
const VOICE_CAPTURE_MAX_BYTES: u64 = 10 * 1024 * 1024;
const RECORDER_STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const VOICE_TTS_INPUT_MAX_CHARS: usize = 4_096;
const VOICE_CONFIG_FIELD_MAX_CHARS: usize = 256;
const VOICE_STT_RESPONSE_MAX_BYTES: usize = 1024 * 1024;
const VOICE_TTS_RESPONSE_MAX_BYTES: usize = 25 * 1024 * 1024;
const VOICE_ERROR_BODY_MAX_BYTES: usize = 64 * 1024;
const VOICE_PLAYBACK_MAX_DURATION: std::time::Duration = std::time::Duration::from_secs(10 * 60);
const PLAYBACK_STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const VITALS_MAX_BYTES: usize = vital_memory_document::MAX_FILE_BYTES;
const PREFS_MAX_BYTES: usize = 8 * 1024 * 1024;
const FEEDS_MAX_BYTES: usize = 8 * 1024 * 1024;
const PRIVATE_DOCUMENT_MAX_BYTES: usize = 8 * 1024 * 1024;
const PRIVATE_SECRET_MAX_BYTES: usize = 64 * 1024;
const MAX_SCHEDULE_SECONDS: u64 = 100 * 366 * 24 * 60 * 60;
const PREFS_MISSING_REVISION: &str = "\0phoenix-canvas-prefs-missing";
const INLINE_IMAGE_MAX_BYTES: usize = 12 * 1024 * 1024;
const TEXT_FILE_MAX_BYTES: usize = 2 * 1024 * 1024;
const MEMORY_NOTE_MAX_BYTES: usize = 2 * 1024 * 1024;
const MEMORY_NOTE_MAX_FILES: usize = 10_000;
const WALLPAPER_MAX_BYTES: u64 = 512 * 1024 * 1024;
const WALLPAPER_HEADER_MAX_BYTES: usize = 64 * 1024;
const WALLPAPER_PREVIEW_MAX_DIMENSION: u32 = 16_384;
const WALLPAPER_PREVIEW_MAX_PIXELS: u64 = 100_000_000;
const WALLPAPER_PREVIEW_SOURCE_MAX_BYTES: u64 = 64 * 1024 * 1024;
const WALLPAPER_PREVIEW_MAX_BYTES: usize = 32 * 1024 * 1024;
const DATA_URL_META_MAX_BYTES: usize = 128;
const SESSION_DISPLAY_MAX_BYTES: usize = 32 * 1024 * 1024;
const GATEWAY_LOG_READ_MAX_BYTES: usize = 24 * 1024 * 1024;
const ACTION_AUDIT_MAX_BYTES: usize = 50 * 1024 * 1024;
const USAGE_TIMING_LOG_MAX_BYTES: usize = 16 * 1024 * 1024;
const USAGE_TIMING_LINE_MAX_BYTES: usize = 64 * 1024;
const GRAPH_EXPORT_MAX_BYTES: usize = 64 * 1024 * 1024;
const PROVIDER_LOGO_MAX_BYTES: usize = 2 * 1024 * 1024;
const PROVIDER_LOGO_MAX_FILES: usize = 256;
const PHOENIX_CLI_OUTPUT_MAX_BYTES: usize = 8 * 1024 * 1024;
const PHOENIX_CLI_STDIN_MAX_BYTES: usize = 16 * 1024;
const PHOENIX_CLI_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);
const PHOENIX_LOGIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(6 * 60);
const PHOENIX_DEVICE_LOGIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(31 * 60);
const PROMPT_OVERLAY_MAX_BYTES: usize = 4 * 1024 * 1024;

#[tauri::command]
fn perf_report(report: String) {
    // Test-fixture output from the packaged webview. Keeping this on stderr
    // lets the native renderer probe measure Tauri itself without browser
    // automation or a second WebKit process.
    if report.starts_with("PERF ") && report.len() <= 256 {
        eprintln!("PHOENIX_NATIVE_{report}");
    }
}

pub(crate) fn phoenix_home() -> PathBuf {
    let custom = std::env::var("PHOENIX_HOME").ok();
    phoenix_home_from(custom.as_deref(), &dirs_home())
}

fn phoenix_home_from(custom: Option<&str>, system_home: &std::path::Path) -> PathBuf {
    if let Some(home) = custom {
        let trimmed = home.trim();
        if !trimmed.is_empty() {
            // Match phoenix_agent::config::phoenix_home exactly.  The override
            // names the state root itself; appending `.phoenix` here split the
            // desktop and gateway across two sockets/config stores in tests
            // and in custom profiles.
            return PathBuf::from(trimmed);
        }
    }
    system_home.join(".phoenix")
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        // Match the backend's BaseDirs fallback. More importantly, never turn
        // a missing HOME into `/.phoenix`: mutation validation rejects this
        // relative fallback instead of accidentally treating the filesystem
        // root as an application-owned home.
        .unwrap_or_else(|| PathBuf::from("."))
}

fn secure_private_directory(path: &std::path::Path) -> Result<(), String> {
    let state_root = phoenix_home();
    let default_root = dirs_home().join(".phoenix");
    let custom_override = std::env::var("PHOENIX_HOME")
        .ok()
        .is_some_and(|value| !value.trim().is_empty());
    if path.starts_with(&state_root) {
        secure_private_directory_at(path, &state_root, &default_root, custom_override)
    } else {
        // Explicit path helpers are used by isolated tests and a few
        // file-oriented commands. Treat that directory as its own custom
        // private root: it must already be dedicated/owner-only and is never
        // chmodded as a side effect.
        secure_private_directory_at(path, path, &default_root, true)
    }
}

fn secure_private_directory_at(
    path: &std::path::Path,
    state_root: &std::path::Path,
    default_root: &std::path::Path,
    custom_override: bool,
) -> Result<(), String> {
    validate_state_root_for_mutation(state_root, default_root, custom_override)?;
    if !path.starts_with(state_root) {
        return Err(format!(
            "refusing Phoenix state path outside PHOENIX_HOME: {}",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

        // Never traverse an existing symlink while preparing a credential or
        // runtime-state directory. In particular, PHOENIX_HOME itself must be
        // a real directory owned by this user, not a redirect into some other
        // tree.
        let mut cursor = Some(path);
        while let Some(candidate) = cursor {
            if let Ok(metadata) = std::fs::symlink_metadata(candidate) {
                if metadata.file_type().is_symlink() {
                    return Err(format!(
                        "refusing symlinked Phoenix state directory {}",
                        candidate.display()
                    ));
                }
            }
            cursor = candidate.parent().filter(|parent| *parent != candidate);
        }

        match std::fs::symlink_metadata(path) {
            Ok(metadata) => {
                if !metadata.is_dir() {
                    return Err(format!("{} is not a directory", path.display()));
                }
                if metadata.uid() != unsafe { libc::geteuid() } {
                    return Err(format!(
                        "Phoenix state directory {} is not owned by this user",
                        path.display()
                    ));
                }
                let custom_existing_root =
                    custom_override && path == state_root && state_root != default_root;
                if custom_existing_root {
                    // A custom root can deliberately be an existing workspace
                    // or shared directory. Never chmod it behind the caller's
                    // back; require a private root instead.
                    if metadata.mode() & 0o077 != 0 {
                        return Err(format!(
                            "custom PHOENIX_HOME {} must already be private (mode 0700 or stricter)",
                            path.display()
                        ));
                    }
                    return Ok(());
                }
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                    .map_err(|error| error.to_string())?;
                return Ok(());
            }
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(error.to_string());
            }
            Err(_) => {}
        }

        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(path).map_err(|error| error.to_string())?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(path).map_err(|error| error.to_string())?;
    Ok(())
}

fn validate_state_root_for_mutation(
    state_root: &std::path::Path,
    default_root: &std::path::Path,
    custom_override: bool,
) -> Result<(), String> {
    use std::path::Component;

    if state_root.as_os_str().is_empty()
        || !state_root.is_absolute()
        || state_root
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(format!(
            "PHOENIX_HOME must be a dedicated absolute path without '.' or '..': {}",
            state_root.display()
        ));
    }
    if !custom_override || state_root == default_root {
        #[cfg(unix)]
        if let Ok(metadata) = std::fs::symlink_metadata(state_root) {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(format!(
                    "Phoenix state root must be a real directory: {}",
                    state_root.display()
                ));
            }
            if metadata.uid() != unsafe { libc::geteuid() } {
                return Err(format!(
                    "Phoenix state root {} is not owned by this user",
                    state_root.display()
                ));
            }
            std::fs::set_permissions(state_root, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| error.to_string())?;
        }
        return Ok(());
    }

    let system_home = default_root.parent();
    let dangerously_broad = state_root.parent().is_none()
        || state_root == std::path::Path::new("/")
        || state_root == std::env::temp_dir()
        || system_home.is_some_and(|home| state_root == home)
        || std::env::current_dir()
            .ok()
            .is_some_and(|cwd| state_root == cwd);
    if dangerously_broad {
        return Err(format!(
            "refusing dangerously broad PHOENIX_HOME {}; choose a dedicated private directory",
            state_root.display()
        ));
    }
    #[cfg(unix)]
    if let Ok(metadata) = std::fs::symlink_metadata(state_root) {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(format!(
                "custom PHOENIX_HOME must be a real directory: {}",
                state_root.display()
            ));
        }
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(format!(
                "custom PHOENIX_HOME {} is not owned by this user",
                state_root.display()
            ));
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(format!(
                "custom PHOENIX_HOME {} must already be private (mode 0700 or stricter)",
                state_root.display()
            ));
        }
    }
    Ok(())
}

fn private_sidecar(path: &std::path::Path, label: &str) -> Result<PathBuf, String> {
    static NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent", path.display()))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("phoenix-state");
    let clock = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let nonce = NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(parent.join(format!(
        ".{name}.{label}.{}.{clock}.{nonce}",
        std::process::id()
    )))
}

fn with_private_file_lock<T>(
    path: &std::path::Path,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;

    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent", path.display()))?;
    secure_private_directory(parent)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("phoenix-state");
    let lock_path = parent.join(format!(".{name}.lock"));
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&lock_path)
        .map_err(|error| format!("could not open {}: {error}", lock_path.display()))?;
    use std::os::unix::fs::PermissionsExt;
    lock.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("could not secure {}: {error}", lock_path.display()))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } == -1 {
        return Err(format!(
            "could not lock {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    let result = operation();
    let _ = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) };
    result
}

fn write_private_atomic_unlocked(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent", path.display()))?;
    secure_private_directory(parent)?;
    let tmp = private_sidecar(path, "tmp")?;
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&tmp)
            .map_err(|error| format!("could not create {}: {error}", tmp.display()))?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("could not secure {}: {error}", tmp.display()))?;
        file.write_all(bytes)
            .map_err(|error| format!("could not write {}: {error}", tmp.display()))?;
        file.sync_all()
            .map_err(|error| format!("could not sync {}: {error}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .map_err(|error| format!("could not replace {}: {error}", path.display()))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("could not secure {}: {error}", path.display()))?;
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("could not sync {}: {error}", parent.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn read_private_bounded_unlocked(
    path: &std::path::Path,
    max_bytes: usize,
    label: &str,
) -> Result<Option<Vec<u8>>, String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;

    let file = match std::fs::OpenOptions::new()
        .read(true)
        // O_NONBLOCK prevents a path swapped to a FIFO/device from hanging us
        // before the regular-file metadata check below.
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "could not read {label} {}: {error}",
                path.display()
            ))
        }
    };
    let metadata = file
        .metadata()
        .map_err(|error| format!("could not inspect {label} {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{label} {} is not a regular file", path.display()));
    }
    if metadata.len() > max_bytes as u64 {
        return Err(format!(
            "{label} {} exceeds the {max_bytes}-byte limit",
            path.display()
        ));
    }
    let mut bytes = Vec::with_capacity((metadata.len() as usize).min(max_bytes));
    file.take((max_bytes as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read {label} {}: {error}", path.display()))?;
    if bytes.len() > max_bytes {
        return Err(format!(
            "{label} {} exceeds the {max_bytes}-byte limit",
            path.display()
        ));
    }
    Ok(Some(bytes))
}

pub(crate) fn read_private_text(
    path: &std::path::Path,
    max_bytes: usize,
    label: &str,
) -> Result<Option<String>, String> {
    read_private_bounded_unlocked(path, max_bytes, label)?
        .map(|bytes| {
            String::from_utf8(bytes)
                .map_err(|error| format!("{label} {} is not UTF-8: {error}", path.display()))
        })
        .transpose()
}

fn read_private_text_required(
    path: &std::path::Path,
    max_bytes: usize,
    label: &str,
) -> Result<String, String> {
    read_private_text(path, max_bytes, label)?
        .ok_or_else(|| format!("{label} {} does not exist", path.display()))
}

fn private_backup_path(path: &std::path::Path) -> Result<PathBuf, String> {
    static NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent", path.display()))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("phoenix-state");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let nonce = NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(parent.join(format!("{name}.bak-{stamp}-{}-{nonce}", std::process::id())))
}

/// Compare-and-swap a private file under an inter-process lock.  `expected`
/// prevents a read/modify/write performed by Phoenix from silently erasing a
/// concurrent CLI or gateway edit. Existing contents are backed up privately
/// before the atomic replacement when requested.
pub(crate) fn replace_private_atomic(
    path: &std::path::Path,
    expected: Option<&[u8]>,
    bytes: &[u8],
    keep_backup: bool,
    max_bytes: usize,
    label: &str,
) -> Result<Option<PathBuf>, String> {
    if bytes.len() > max_bytes {
        return Err(format!(
            "replacement {label} exceeds the {max_bytes}-byte limit"
        ));
    }
    if expected.is_some_and(|expected| expected.len() > max_bytes) {
        return Err(format!(
            "expected {label} exceeds the {max_bytes}-byte limit"
        ));
    }
    with_private_file_lock(path, || {
        // Open without following symlinks and inspect the already-open file
        // before allocating. FIFOs/devices and oversized state are never read,
        // compared, backed up, or replaced.
        let current = read_private_bounded_unlocked(path, max_bytes, label)?;
        if let Some(expected) = expected {
            if current.as_deref() != Some(expected) {
                return Err(format!(
                    "{} changed in another process; reload and retry",
                    path.display()
                ));
            }
        }
        let backup = if keep_backup {
            current
                .as_deref()
                .map(|old| {
                    let backup = private_backup_path(path)?;
                    write_private_atomic_unlocked(&backup, old)?;
                    Ok::<PathBuf, String>(backup)
                })
                .transpose()?
        } else {
            None
        };
        write_private_atomic_unlocked(path, bytes)?;
        Ok(backup)
    })
}

struct VoiceCapture {
    capture_id: String,
    child: std::process::Child,
    path: PathBuf,
}

impl Drop for VoiceCapture {
    fn drop(&mut self) {
        let _ = stop_voice_child(
            &mut self.child,
            libc::SIGINT,
            RECORDER_STOP_TIMEOUT,
            "audio recorder",
        );
        let _ = std::fs::remove_file(&self.path);
    }
}

struct VoicePlayback {
    child: std::process::Child,
    path: PathBuf,
}

impl Drop for VoicePlayback {
    fn drop(&mut self) {
        let _ = stop_voice_child(
            &mut self.child,
            libc::SIGTERM,
            PLAYBACK_STOP_TIMEOUT,
            "audio playback",
        );
        let _ = std::fs::remove_file(&self.path);
    }
}

fn voice_capture_slot() -> &'static Mutex<Option<Arc<Mutex<VoiceCapture>>>> {
    static SLOT: OnceLock<Mutex<Option<Arc<Mutex<VoiceCapture>>>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

fn pending_voice_capture_slot() -> &'static Mutex<Option<String>> {
    static SLOT: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

fn consume_prepared_voice_capture(pending: &mut Option<String>, capture_id: &str) -> Result<(), String> {
    if pending.as_deref() != Some(capture_id) {
        return Err("this voice capture was canceled or was not prepared".into());
    }
    *pending = None;
    Ok(())
}

fn cancel_prepared_voice_capture(pending: &mut Option<String>, capture_id: &str) -> bool {
    if pending.as_deref() != Some(capture_id) { return false; }
    *pending = None;
    true
}

fn validate_voice_capture_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 128 || !id.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')) {
        return Err("invalid voice capture identity".into());
    }
    Ok(())
}

fn take_owned_voice_capture(
    slot: &Mutex<Option<Arc<Mutex<VoiceCapture>>>>,
    capture_id: &str,
) -> Result<Option<Arc<Mutex<VoiceCapture>>>, String> {
    validate_voice_capture_id(capture_id)?;
    let mut current = slot.lock().map_err(|_| "voice capture lock poisoned")?;
    let matches = match current.as_ref() {
        Some(capture) => capture.lock().map_err(|_| "voice capture state poisoned")?.capture_id == capture_id,
        None => false,
    };
    Ok(if matches { current.take() } else { None })
}

fn voice_playback_slot() -> &'static Mutex<Option<Arc<Mutex<VoicePlayback>>>> {
    static SLOT: OnceLock<Mutex<Option<Arc<Mutex<VoicePlayback>>>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

fn voice_temp_path(directory: &std::path::Path, label: &str) -> PathBuf {
    static NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let clock = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let nonce = NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    directory.join(format!(
        ".{label}-{}-{clock}-{nonce}.wav",
        std::process::id()
    ))
}

fn create_private_voice_file(path: &std::path::Path) -> Result<std::fs::File, String> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent", path.display()))?;
    secure_private_directory(parent)?;
    let file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("could not create private voice file: {error}"))?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("could not secure private voice file: {error}"))?;
    Ok(file)
}

fn write_private_voice_file(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;

    let result = (|| {
        let mut file = create_private_voice_file(path)?;
        file.write_all(bytes)
            .map_err(|error| format!("could not write synthesized speech: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("could not sync synthesized speech: {error}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(path);
    }
    result
}

fn configure_voice_child(command: &mut std::process::Command, file_limit: Option<u64>) {
    // SAFETY: pre_exec only invokes async-signal-safe libc operations before
    // exec. The limits are copied into the closure and no locks are touched.
    unsafe {
        command.pre_exec(move || {
            libc::umask(0o077);
            if let Some(bytes) = file_limit {
                let limit = libc::rlimit {
                    rlim_cur: bytes as libc::rlim_t,
                    rlim_max: bytes as libc::rlim_t,
                };
                if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            #[cfg(target_os = "linux")]
            {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() == 1 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "Phoenix exited before voice child started",
                    ));
                }
            }
            Ok(())
        });
    }
}

fn stop_voice_child(
    child: &mut std::process::Child,
    signal: libc::c_int,
    timeout: std::time::Duration,
    label: &str,
) -> Result<(), String> {
    match child.try_wait() {
        Ok(Some(_)) => return Ok(()),
        Ok(None) => {}
        Err(error) => return Err(format!("could not inspect {label}: {error}")),
    }
    #[cfg(unix)]
    {
        let result = unsafe { libc::kill(child.id() as i32, signal) };
        if result == -1 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(format!("could not stop {label}: {error}"));
            }
        }
    }
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{label} did not stop cleanly within {}s",
                    timeout.as_secs()
                ));
            }
            Err(error) => return Err(format!("could not wait for {label}: {error}")),
        }
    }
}

fn spawn_capture_watchdog(capture: Arc<Mutex<VoiceCapture>>) -> Result<(), String> {
    std::thread::Builder::new()
        .name("phoenix-voice-capture-watchdog".into())
        .spawn(move || {
            let deadline = std::time::Instant::now()
                + std::time::Duration::from_secs(VOICE_CAPTURE_MAX_SECONDS);
            loop {
                let finished = {
                    let mut capture = capture
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    match capture.child.try_wait() {
                        Ok(Some(_)) => true,
                        Ok(None) if std::time::Instant::now() >= deadline => {
                            let _ = stop_voice_child(
                                &mut capture.child,
                                libc::SIGINT,
                                RECORDER_STOP_TIMEOUT,
                                "audio recorder",
                            );
                            true
                        }
                        Ok(None) => false,
                        Err(_) => {
                            let _ = capture.child.kill();
                            let _ = capture.child.wait();
                            true
                        }
                    }
                };
                if finished {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        })
        .map(|_| ())
        .map_err(|error| format!("could not start audio recorder watchdog: {error}"))
}

fn clear_playback_if_current(playback: &Arc<Mutex<VoicePlayback>>) {
    let removed = {
        let mut slot = voice_playback_slot()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, playback))
        {
            slot.take()
        } else {
            None
        }
    };
    drop(removed);
}

fn spawn_playback_reaper(playback: Arc<Mutex<VoicePlayback>>) -> Result<(), String> {
    let reaper_playback = playback.clone();
    let result = std::thread::Builder::new()
        .name("phoenix-voice-playback-reaper".into())
        .spawn(move || {
            let deadline = std::time::Instant::now() + VOICE_PLAYBACK_MAX_DURATION;
            loop {
                let finished = {
                    let mut playback = reaper_playback
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    match playback.child.try_wait() {
                        Ok(Some(_)) => true,
                        Ok(None) if std::time::Instant::now() >= deadline => {
                            let _ = stop_voice_child(
                                &mut playback.child,
                                libc::SIGTERM,
                                PLAYBACK_STOP_TIMEOUT,
                                "audio playback",
                            );
                            true
                        }
                        Ok(None) => false,
                        Err(_) => {
                            let _ = playback.child.kill();
                            let _ = playback.child.wait();
                            true
                        }
                    }
                };
                if finished {
                    clear_playback_if_current(&reaper_playback);
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        });
    if let Err(error) = result {
        clear_playback_if_current(&playback);
        return Err(format!("could not start audio playback reaper: {error}"));
    }
    Ok(())
}

fn start_voice_playback(path: PathBuf) -> Result<(), String> {
    let mut slot = voice_playback_slot()
        .lock()
        .map_err(|_| "voice playback lock poisoned")?;
    if let Some(previous) = slot.take() {
        let mut previous = previous
            .lock()
            .map_err(|_| "voice playback state poisoned")?;
        stop_voice_child(
            &mut previous.child,
            libc::SIGTERM,
            PLAYBACK_STOP_TIMEOUT,
            "audio playback",
        )?;
    }

    let mut command = std::process::Command::new("aplay");
    command
        .args(["-q", "--"])
        .arg(&path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    configure_voice_child(&mut command, None);
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = std::fs::remove_file(&path);
            return Err(format!("could not play synthesized speech: {error}"));
        }
    };
    let playback = Arc::new(Mutex::new(VoicePlayback { child, path }));
    *slot = Some(playback.clone());
    drop(slot);
    spawn_playback_reaper(playback)
}

fn validate_voice_config_field(
    lane: &str,
    field: &str,
    value: String,
    required: bool,
) -> Result<String, String> {
    if required && value.is_empty() {
        return Err(format!("configure the {lane} {field} first"));
    }
    if value.chars().take(VOICE_CONFIG_FIELD_MAX_CHARS + 1).count() > VOICE_CONFIG_FIELD_MAX_CHARS {
        return Err(format!(
            "the configured {lane} {field} exceeds the {VOICE_CONFIG_FIELD_MAX_CHARS}-character request-field limit"
        ));
    }
    Ok(value)
}

fn voice_lane_config(prefix: &str) -> Result<(String, String, String), String> {
    let path = phoenix_home().join("config.toml");
    let raw = read_private_text_required(&path, PRIVATE_DOCUMENT_MAX_BYTES, "config.toml")?;
    let doc: toml::Value = raw
        .parse()
        .map_err(|error| format!("config.toml unreadable: {error}"))?;
    let llm = doc
        .get("profile")
        .and_then(|p| p.get("llm"))
        .ok_or("[profile.llm] is missing")?;
    let get = |suffix: &str| {
        let key = format!("{prefix}_{suffix}");
        llm.get(key.as_str())
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let provider = validate_voice_config_field(prefix, "provider", get("provider"), true)?;
    let model = validate_voice_config_field(prefix, "model", get("model"), true)?;
    let voice = validate_voice_config_field(prefix, "voice", get("voice"), false)?;
    Ok((provider, model, voice))
}

fn voice_provider_key(provider: &str) -> Result<String, String> {
    let env_name = match provider {
        "openai" => "OPENAI_API_KEY",
        "groq" => "GROQ_API_KEY",
        other => {
            return Err(format!(
                "voice HTTP transport is not implemented for provider `{other}`"
            ))
        }
    };
    if let Ok(value) = std::env::var(env_name) {
        if !value.trim().is_empty() {
            return Ok(value);
        }
    }
    let path = phoenix_home().join("auth-profiles.json");
    let raw = read_private_text_required(&path, PRIVATE_DOCUMENT_MAX_BYTES, "auth profile store")
        .map_err(|_| format!("no {env_name} or stored `{provider}` profile"))?;
    let doc: serde_json::Value =
        serde_json::from_str(&raw).map_err(|error| format!("auth store unreadable: {error}"))?;
    doc.get("profiles")
        .and_then(|v| v.as_object())
        .and_then(|profiles| {
            profiles.values().find_map(|entry| {
                (entry.get("provider").and_then(|v| v.as_str()) == Some(provider))
                    .then(|| entry.get("key").or_else(|| entry.get("access")))
                    .flatten()
                    .and_then(|v| v.as_str())
                    .filter(|v| !v.trim().is_empty())
                    .map(str::to_string)
            })
        })
        .ok_or_else(|| format!("no usable credential for voice provider `{provider}`"))
}

fn voice_api_base(provider: &str) -> Result<&'static str, String> {
    match provider {
        "openai" => Ok("https://api.openai.com/v1"),
        "groq" => Ok("https://api.groq.com/openai/v1"),
        other => Err(format!("voice API base is unknown for `{other}`")),
    }
}

fn voice_http_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .connect_timeout(VOICE_CONNECT_TIMEOUT)
        .timeout(VOICE_HTTP_TIMEOUT)
        .build()
        .map_err(|error| format!("could not create bounded voice client: {error}"))
}

fn validate_declared_body_size(
    declared: Option<u64>,
    limit: usize,
    label: &str,
) -> Result<(), String> {
    if declared.is_some_and(|bytes| bytes > limit as u64) {
        return Err(format!("{label} exceeds the {limit}-byte response limit"));
    }
    Ok(())
}

fn read_bounded_body(
    reader: impl std::io::Read,
    limit: usize,
    label: &str,
) -> Result<Vec<u8>, String> {
    use std::io::Read;

    let mut body = Vec::with_capacity(limit.min(64 * 1024));
    reader
        .take((limit as u64).saturating_add(1))
        .read_to_end(&mut body)
        .map_err(|error| format!("could not read {label}: {error}"))?;
    if body.len() > limit {
        return Err(format!("{label} exceeds the {limit}-byte response limit"));
    }
    Ok(body)
}

fn read_voice_response(
    response: reqwest::blocking::Response,
    limit: usize,
    label: &str,
) -> Result<Vec<u8>, String> {
    validate_declared_body_size(response.content_length(), limit, label)?;
    read_bounded_body(response, limit, label)
}

fn bounded_voice_error(response: reqwest::blocking::Response, label: &str) -> String {
    match read_voice_response(response, VOICE_ERROR_BODY_MAX_BYTES, label) {
        Ok(body) => {
            let text = String::from_utf8_lossy(&body);
            let trimmed = text.trim();
            if trimmed.is_empty() {
                "empty response body".to_string()
            } else {
                trimmed.to_string()
            }
        }
        Err(error) => format!("response body omitted: {error}"),
    }
}

fn validate_wav(bytes: &[u8], label: &str) -> Result<(), String> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(format!("{label} is not a valid WAV payload"));
    }
    Ok(())
}

fn validate_capture_file(path: &std::path::Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("could not inspect microphone capture: {error}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("microphone capture is not a regular file".into());
    }
    if metadata.len() > VOICE_CAPTURE_MAX_BYTES {
        return Err(format!(
            "microphone capture exceeds the {}-byte limit",
            VOICE_CAPTURE_MAX_BYTES
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err("microphone capture is not private and owner-controlled".into());
        }
    }
    let mut header = [0_u8; 12];
    use std::io::Read;
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .map_err(|error| format!("microphone capture is incomplete: {error}"))?;
    validate_wav(&header, "microphone capture")
}

fn validate_tts_input(text: &str) -> Result<&str, String> {
    if text.chars().take(VOICE_TTS_INPUT_MAX_CHARS + 1).count() > VOICE_TTS_INPUT_MAX_CHARS {
        return Err(format!(
            "speech text exceeds the {}-character limit",
            VOICE_TTS_INPUT_MAX_CHARS
        ));
    }
    let text = text.trim();
    if text.is_empty() {
        return Err("nothing to speak".into());
    }
    Ok(text)
}

/// Begin native ALSA capture. A second click calls `voice_capture_stop_owned`, so
/// WebKitGTK does not need getUserMedia or SpeechRecognition support.
/// The owned command name prevents a new renderer from accidentally starting
/// an identity-free recording against a still-running legacy desktop binary.
#[tauri::command]
async fn voice_capture_prepare_owned(capture_id: String) -> Result<String, String> {
    validate_voice_capture_id(&capture_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let slot = voice_capture_slot().lock().map_err(|_| "voice capture lock poisoned")?;
        if let Some(capture) = slot.as_ref() {
            let mut capture = capture.lock().map_err(|_| "voice capture state poisoned")?;
            if capture.capture_id == capture_id { return Ok(capture_id); }
            if capture.child.try_wait().map_err(|error| format!("could not inspect audio recorder: {error}"))?.is_none() {
                return Err("voice capture is already running".into());
            }
        }
        // Preparation does not open the microphone. Replacing an abandoned
        // preparation invalidates its delayed start, without stopping a capture.
        *pending_voice_capture_slot().lock().map_err(|_| "voice preparation lock poisoned")? = Some(capture_id.clone());
        Ok(capture_id)
    }).await.map_err(|error| format!("voice preparation task failed: {error}"))?
}

#[tauri::command]
async fn voice_capture_start_owned(capture_id: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || voice_capture_start_blocking(capture_id))
        .await.map_err(|error| format!("voice capture task failed: {error}"))?
}

fn voice_capture_start_blocking(capture_id: String) -> Result<String, String> {
    validate_voice_capture_id(&capture_id)?;
    // Do not light the composer up as "recording" when Phoenix has no way to
    // transcribe the result. Stop used to discover this only after recording,
    // leaving the UI active around a capture whose second click could never
    // complete successfully.
    let (provider, _, _) = voice_lane_config("stt")?;
    voice_api_base(&provider)?;
    voice_provider_key(&provider)?;
    let mut slot = voice_capture_slot()
        .lock()
        .map_err(|_| "voice capture lock poisoned")?;
    if let Some(existing) = slot.as_ref() {
        let finished = {
            let mut existing = existing.lock().map_err(|_| "voice capture state poisoned")?;
            // A lost acknowledgement may be retried without opening a second
            // microphone capture or replacing the already recorded bytes.
            if existing.capture_id == capture_id { return Ok(capture_id); }
            existing.child.try_wait()
                .map_err(|error| format!("could not inspect audio recorder: {error}"))?.is_some()
        };
        if !finished {
            return Err("voice capture is already running".into());
        }
        slot.take();
    }
    // Cancel and start share the capture lock. A reload can cancel the
    // preparation before this blocking task reaches ALSA; a delayed start then
    // fails here instead of opening a microphone with no surviving UI owner.
    {
        let mut pending = pending_voice_capture_slot().lock().map_err(|_| "voice preparation lock poisoned")?;
        consume_prepared_voice_capture(&mut pending, &capture_id)?;
    }
    let dir = phoenix_home().join("voice");
    secure_private_directory(&dir)?;
    let path = voice_temp_path(&dir, "capture");
    let file = create_private_voice_file(&path)?;
    let mut command = std::process::Command::new("arecord");
    command
        .args([
            "-q", "-t", "wav", "-f", "S16_LE", "-r", "16000", "-c", "1", "-d",
        ])
        .arg(VOICE_CAPTURE_MAX_SECONDS.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(file))
        .stderr(std::process::Stdio::null());
    configure_voice_child(&mut command, Some(VOICE_CAPTURE_MAX_BYTES));
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = std::fs::remove_file(&path);
            return Err(format!("could not start ALSA capture: {error}"));
        }
    };
    let capture = Arc::new(Mutex::new(VoiceCapture { capture_id: capture_id.clone(), child, path }));
    spawn_capture_watchdog(capture.clone())?;
    *slot = Some(capture);
    Ok(capture_id)
}

#[tauri::command]
async fn voice_capture_stop_owned(capture_id: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || voice_capture_stop_blocking(&capture_id))
        .await.map_err(|error| format!("voice transcription task failed: {error}"))?
}

fn voice_capture_stop_blocking(capture_id: &str) -> Result<String, String> {
    let capture = take_owned_voice_capture(voice_capture_slot(), capture_id)?
        .ok_or("this voice capture is no longer available")?;
    // SIGINT lets arecord finalize the WAV header; Child::kill (SIGKILL)
    // leaves an occasionally unreadable capture.
    let path = {
        let mut capture_state = capture.lock().map_err(|_| "voice capture state poisoned")?;
        stop_voice_child(
            &mut capture_state.child,
            libc::SIGINT,
            RECORDER_STOP_TIMEOUT,
            "audio recorder",
        )?;
        validate_capture_file(&capture_state.path)?;
        capture_state.path.clone()
    };
    let (provider, model, _) = voice_lane_config("stt")?;
    let key = voice_provider_key(&provider)?;
    let url = format!("{}/audio/transcriptions", voice_api_base(&provider)?);
    let form = reqwest::blocking::multipart::Form::new()
        .text("model", model)
        .file("file", &path)
        .map_err(|error| format!("could not read microphone capture: {error}"))?;
    let response = voice_http_client()?
        .post(url)
        .bearer_auth(key)
        .multipart(form)
        .send()
        .map_err(|error| format!("speech transcription failed: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        let detail = bounded_voice_error(response, "speech transcription error response");
        return Err(format!("speech transcription returned {status}: {detail}"));
    }
    let body = read_voice_response(
        response,
        VOICE_STT_RESPONSE_MAX_BYTES,
        "speech transcription response",
    )?;
    let value: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|error| format!("speech transcription returned invalid JSON: {error}"))?;
    value
        .get("text")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "speech transcription returned no text".to_string())
}

#[tauri::command]
async fn voice_capture_cancel(capture_id: String) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        validate_voice_capture_id(&capture_id)?;
        let (prepared, capture) = {
            let mut slot = voice_capture_slot().lock().map_err(|_| "voice capture lock poisoned")?;
            let mut pending = pending_voice_capture_slot().lock().map_err(|_| "voice preparation lock poisoned")?;
            let prepared = cancel_prepared_voice_capture(&mut pending, &capture_id);
            let owned = slot.as_ref().map(|capture| capture.lock().map(|capture| capture.capture_id == capture_id)
                .map_err(|_| "voice capture state poisoned")).transpose()?.unwrap_or(false);
            (prepared, if owned { slot.take() } else { None })
        };
        let Some(capture) = capture else { return Ok(prepared); };
        let mut capture = capture.lock().map_err(|_| "voice capture state poisoned")?;
        let stopped = stop_voice_child(&mut capture.child, libc::SIGINT, RECORDER_STOP_TIMEOUT, "audio recorder");
        // Cancellation never calls a transcription service. Only this request's
        // recorder and private file are touched, even after a late UI callback.
        let removed = std::fs::remove_file(&capture.path);
        stopped?;
        removed.map_err(|error| format!("could not remove canceled voice capture: {error}"))?;
        Ok(true)
    }).await.map_err(|error| format!("voice cancellation task failed: {error}"))?
}

#[tauri::command]
async fn voice_speak(text: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || voice_speak_blocking(text))
        .await.map_err(|error| format!("voice playback task failed: {error}"))?
}

fn voice_speak_blocking(text: String) -> Result<String, String> {
    let text = validate_tts_input(&text)?;
    let (provider, model, configured_voice) = voice_lane_config("tts")?;
    let key = voice_provider_key(&provider)?;
    let voice = if configured_voice.is_empty() {
        "alloy"
    } else {
        &configured_voice
    };
    let url = format!("{}/audio/speech", voice_api_base(&provider)?);
    let response = voice_http_client()?
        .post(url)
        .bearer_auth(key)
        .json(&serde_json::json!({
            "model": model,
            "voice": voice,
            "input": text,
            "response_format": "wav"
        }))
        .send()
        .map_err(|error| format!("speech synthesis failed: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        let detail = bounded_voice_error(response, "speech synthesis error response");
        return Err(format!("speech synthesis returned {status}: {detail}"));
    }
    let bytes = read_voice_response(
        response,
        VOICE_TTS_RESPONSE_MAX_BYTES,
        "speech synthesis response",
    )?;
    validate_wav(&bytes, "speech synthesis response")?;
    let dir = phoenix_home().join("voice");
    secure_private_directory(&dir)?;
    let path = voice_temp_path(&dir, "reply");
    write_private_voice_file(&path, &bytes)?;
    if let Err(error) = start_voice_playback(path.clone()) {
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok("speaking".into())
}

/// The gateway binary. Development Phoenix builds must use the CLI built from
/// the same checkout, otherwise restarting a fresh desktop can silently launch
/// a stale ~/.local/bin/phoenix and leave half of a fix inactive.
fn phoenix_binary() -> PathBuf {
    let explicit = std::env::var_os("PHOENIX_GATEWAY_BINARY").map(PathBuf::from);
    let executable = std::env::current_exe().ok();
    phoenix_binary_from(explicit.as_deref(), executable.as_deref(), &dirs_home())
}

fn phoenix_binary_from(
    explicit: Option<&std::path::Path>,
    executable: Option<&std::path::Path>,
    system_home: &std::path::Path,
) -> PathBuf {
    if let Some(path) = explicit {
        if path.is_file() {
            return path.to_path_buf();
        }
    }
    if let Some(exe) = executable {
        // Installed/bundled builds may ship the gateway beside the desktop.
        // Prefer that exact companion before looking at any user-wide install.
        if let Some(directory) = exe.parent() {
            let companion = directory.join("phoenix");
            if companion.is_file() {
                return companion;
            }
        }
        // .../canvas-app/target/{debug,release}/phoenix-desktop
        //                         ^ four parents to the repository root
        if let Some(repo) = exe.ancestors().nth(4) {
            let profile = exe
                .parent()
                .and_then(|directory| directory.file_name())
                .and_then(|name| name.to_str())
                .unwrap_or("release");
            let checkout = repo.join("target").join(profile).join("phoenix");
            if repo.join("Cargo.toml").is_file() && repo.join("canvas-app/Cargo.toml").is_file() {
                if checkout.is_file() {
                    return checkout;
                }
                // A release desktop is commonly rebuilt without also paying
                // for a second release build of the much larger CLI. The
                // debug CLI from THIS checkout is still protocol-compatible
                // and safer than a potentially stale user-wide install.
                let checkout_debug = repo.join("target/debug/phoenix");
                if checkout_debug.is_file() {
                    return checkout_debug;
                }
                // Keep the old fail-honestly behavior when this checkout has
                // no gateway at all. Never silently launch ~/.local/bin here.
                return checkout;
            }
        }
    }
    let local = system_home.join(".local/bin/phoenix");
    if local.exists() {
        return local;
    }
    PathBuf::from("phoenix")
}

fn command_path_for_metadata(command: &std::path::Path) -> Option<PathBuf> {
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
    let command = phoenix_binary();
    command_path_for_metadata(&command).map(|path| path.canonicalize().unwrap_or(path))
}

#[cfg(target_os = "linux")]
fn process_uses_selected_executable_at(
    proc_root: &std::path::Path,
    pid: i32,
    selected: &std::path::Path,
) -> Option<bool> {
    use std::os::unix::fs::MetadataExt;

    let running = std::fs::metadata(proc_root.join(pid.to_string()).join("exe")).ok()?;
    let selected = std::fs::metadata(selected).ok()?;
    Some(running.dev() == selected.dev() && running.ino() == selected.ino())
}

#[cfg(not(target_os = "linux"))]
fn process_uses_selected_executable_at(
    _proc_root: &std::path::Path,
    _pid: i32,
    _selected: &std::path::Path,
) -> Option<bool> {
    None
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
enum GatewayProbeResponse {
    ProtocolInfo {
        protocol: String,
        package_version: String,
        binary_id: Option<String>,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, PartialEq, Eq)]
enum GatewayProbe {
    Down,
    Compatible,
    Incompatible(String),
    Unresponsive(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GatewayProcessIdentity {
    pid: i32,
    start_time: String,
}

fn observed_gateway_slot() -> &'static Mutex<Option<GatewayProcessIdentity>> {
    static SLOT: OnceLock<Mutex<Option<GatewayProcessIdentity>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

fn remember_verified_gateway_instance(socket: &std::path::Path) {
    let pid = std::fs::read_to_string(phoenix_home().join("gateway.pid"))
        .ok()
        .and_then(|raw| raw.trim().parse::<i32>().ok());
    let Some(pid) = pid.filter(|pid| pid_is_verified_gateway(*pid, socket)) else {
        return;
    };
    let Some(start_time) = process_start_time(pid) else {
        return;
    };
    let mut observed = observed_gateway_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *observed = Some(GatewayProcessIdentity { pid, start_time });
}

fn classify_gateway_probe(line: &str) -> GatewayProbe {
    let response = match serde_json::from_str::<GatewayProbeResponse>(line) {
        Ok(response) => response,
        Err(error) => {
            return GatewayProbe::Unresponsive(format!(
                "gateway did not return protocol info: {error}"
            ));
        }
    };
    match response {
        GatewayProbeResponse::ProtocolInfo { protocol, .. }
            if protocol == GATEWAY_WIRE_PROTOCOL =>
        {
            GatewayProbe::Compatible
        }
        GatewayProbeResponse::ProtocolInfo { protocol, .. } => GatewayProbe::Incompatible(format!(
            "expected protocol {GATEWAY_WIRE_PROTOCOL}, got {protocol}"
        )),
        GatewayProbeResponse::Error { message }
            if message.contains("ProtocolInfo")
                && (message.contains("unknown variant") || message.contains("unknown request")) =>
        {
            GatewayProbe::Incompatible(format!(
                "gateway does not support the {GATEWAY_WIRE_PROTOCOL} handshake: {message}"
            ))
        }
        GatewayProbeResponse::Error { message } => {
            GatewayProbe::Unresponsive(format!("gateway refused the protocol probe: {message}"))
        }
    }
}

fn gateway_probe_at(socket: &std::path::Path) -> GatewayProbe {
    use std::io::{BufRead, Write};

    let mut stream = match std::os::unix::net::UnixStream::connect(socket) {
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
    let _ = stream.set_read_timeout(Some(GATEWAY_PROBE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(GATEWAY_PROBE_TIMEOUT));
    if let Err(error) = stream.write_all(b"\"ProtocolInfo\"\n") {
        return GatewayProbe::Unresponsive(format!("protocol probe write failed: {error}"));
    }
    let mut line = String::new();
    match std::io::BufReader::new(stream).read_line(&mut line) {
        Ok(0) => GatewayProbe::Unresponsive("gateway closed the protocol probe".to_string()),
        Ok(_) => classify_gateway_probe(line.trim_end()),
        Err(error) => GatewayProbe::Unresponsive(format!("protocol probe read failed: {error}")),
    }
}

fn gateway_probe_confirmed_at(socket: &std::path::Path) -> GatewayProbe {
    let mut last = GatewayProbe::Unresponsive("protocol probe was not attempted".to_string());
    for attempt in 0..GATEWAY_PROBE_ATTEMPTS {
        last = gateway_probe_at(socket);
        if !matches!(last, GatewayProbe::Unresponsive(_)) {
            return last;
        }
        if attempt + 1 < GATEWAY_PROBE_ATTEMPTS {
            std::thread::sleep(std::time::Duration::from_millis(100 * (attempt as u64 + 1)));
        }
    }
    last
}

fn gateway_probe() -> GatewayProbe {
    let socket = phoenix_home().join("gateway.sock");
    let probe = gateway_probe_confirmed_at(&socket);
    if matches!(&probe, GatewayProbe::Compatible) {
        // A new executable is an available update, not a protocol failure.
        // The watchdog must never cancel active work just because Cargo rebuilt
        // the companion. Apply binary changes only through an explicit restart.
        // Retain an exact PID/starttime identity. Older gateway builds removed
        // both readiness files before their final cleanup; this observation
        // still prevents the supervisor from overlapping their process.
        remember_verified_gateway_instance(&socket);
    }
    probe
}

fn gateway_local_lifecycle_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn gateway_lifecycle_lock_path_at(home: &std::path::Path) -> PathBuf {
    home.join("gateway-lifecycle")
}

fn gateway_lifecycle_lock_path() -> PathBuf {
    gateway_lifecycle_lock_path_at(&phoenix_home())
}

/// Cross-process lease shared with every CLI/gateway lifecycle writer.
///
/// The process-local mutex above keeps Phoenix commands from overlapping each
/// other, while this flock closes the wider probe -> reclaim/stop -> spawn
/// race with CLI clients and other Phoenix processes. The descriptor is
/// close-on-exec so a newly spawned daemon never inherits the lease past exec.
struct GatewayLifecycleLease(std::fs::File);

impl GatewayLifecycleLease {
    fn acquire() -> Result<Self, String> {
        Self::acquire_at(
            &gateway_lifecycle_lock_path(),
            GATEWAY_LIFECYCLE_LOCK_TIMEOUT,
        )
    }

    fn acquire_at(path: &std::path::Path, timeout: std::time::Duration) -> Result<Self, String> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

        let parent = path
            .parent()
            .ok_or_else(|| format!("{} has no parent", path.display()))?;
        secure_private_directory(parent)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)
            .map_err(|error| {
                format!(
                    "could not open gateway lifecycle lock {}: {error}",
                    path.display()
                )
            })?;
        let metadata = file.metadata().map_err(|error| {
            format!(
                "could not inspect gateway lifecycle lock {}: {error}",
                path.display()
            )
        })?;
        if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(format!(
                "gateway lifecycle lock {} is not a regular file owned by this user",
                path.display()
            ));
        }
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|error| {
                format!(
                    "could not secure gateway lifecycle lock {}: {error}",
                    path.display()
                )
            })?;

        let started = std::time::Instant::now();
        loop {
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(Self(file));
            }
            let error = std::io::Error::last_os_error();
            let code = error.raw_os_error();
            if code != Some(libc::EWOULDBLOCK) && code != Some(libc::EAGAIN) {
                return Err(format!(
                    "could not lock gateway lifecycle {}: {error}",
                    path.display()
                ));
            }
            let elapsed = started.elapsed();
            if elapsed >= timeout {
                return Err(format!(
                    "timed out after {}s waiting for gateway lifecycle lock {}",
                    timeout.as_secs(),
                    path.display()
                ));
            }
            std::thread::sleep(GATEWAY_LIFECYCLE_LOCK_POLL.min(timeout - elapsed));
        }
    }
}

impl Drop for GatewayLifecycleLease {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn rotated_log_path(path: &std::path::Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".1");
    PathBuf::from(name)
}

fn open_bounded_restart_log() -> std::io::Result<std::fs::File> {
    open_bounded_restart_log_at(&phoenix_home())
}

fn open_bounded_restart_log_at(home: &std::path::Path) -> std::io::Result<std::fs::File> {
    open_bounded_restart_log_at_with_limit(home, RESTART_LOG_MAX_BYTES)
}

/// Open one restart-log generation under the same inter-process lock used by
/// every Phoenix writer. The returned descriptor deliberately outlives the
/// lock: the detached gateway inherits it for stdout/stderr exactly as it did
/// before this hardening pass. Every later Phoenix open/append re-enters the
/// lock and rotates that shared path before acquiring a new descriptor.
fn open_bounded_restart_log_at_with_limit(
    home: &std::path::Path,
    max_bytes: u64,
) -> std::io::Result<std::fs::File> {
    with_restart_log_lock_at(home, |log| prepare_restart_log_unlocked(log, max_bytes, 0))
}

fn append_bounded_restart_log(bytes: &[u8]) -> std::io::Result<()> {
    append_bounded_restart_log_at_with_limit(&phoenix_home(), RESTART_LOG_MAX_BYTES, bytes)
}

fn append_bounded_restart_log_at_with_limit(
    home: &std::path::Path,
    max_bytes: u64,
    bytes: &[u8],
) -> std::io::Result<()> {
    use std::io::Write;

    let incoming = u64::try_from(bytes.len())
        .map_err(|_| std::io::Error::other("restart-log write length overflow"))?;
    if incoming > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "restart-log entry is {incoming} bytes; maximum generation is {max_bytes} bytes"
            ),
        ));
    }
    with_restart_log_lock_at(home, |log| {
        let mut file = prepare_restart_log_unlocked(log, max_bytes, incoming)?;
        file.write_all(bytes)?;
        file.flush()
    })
}

fn with_restart_log_lock_at<T>(
    home: &std::path::Path,
    operation: impl FnOnce(&std::path::Path) -> std::io::Result<T>,
) -> std::io::Result<T> {
    // This is the first state write on a fresh Phoenix launch. Create the root
    // before opening the lock, and pin the directory permissions so a
    // permissive umask cannot expose provider/runtime diagnostics.
    let default_root = dirs_home().join(".phoenix");
    secure_private_directory_at(home, home, &default_root, home != default_root)
        .map_err(std::io::Error::other)?;
    let log = home.join("gateway-restart.log");
    with_private_file_lock(&log, || {
        operation(&log).map_err(|error| format!("restart log {}: {error}", log.display()))
    })
    .map_err(std::io::Error::other)
}

fn open_private_restart_log_file(
    path: &std::path::Path,
    create: bool,
) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    let mut options = std::fs::OpenOptions::new();
    options
        .create(create)
        .append(true)
        // O_NONBLOCK makes a raced FIFO/device fail rather than hanging before
        // the descriptor metadata check. O_NOFOLLOW is the symlink boundary.
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .mode(0o600);
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("restart log is not a regular file: {}", path.display()),
        ));
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("restart log is not owned by this user: {}", path.display()),
        ));
    }
    if metadata.nlink() != 1 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "restart log has {} hard links; expected one: {}",
                metadata.nlink(),
                path.display()
            ),
        ));
    }
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

fn remove_previous_restart_log(path: &std::path::Path) -> std::io::Result<()> {
    match open_private_restart_log_file(path, false) {
        Ok(file) => drop(file),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    // Removing a directory entry never follows it. The no-follow open above
    // additionally rejects a pre-existing symlink/special file instead of
    // silently treating it as an expendable log generation.
    std::fs::remove_file(path)
}

fn prepare_restart_log_unlocked(
    path: &std::path::Path,
    max_bytes: u64,
    incoming_bytes: u64,
) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::MetadataExt;

    if max_bytes == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "restart-log maximum must be greater than zero",
        ));
    }
    let current = open_private_restart_log_file(path, true)?;
    let opened = current.metadata()?;
    let rotated = rotated_log_path(path);
    // The previous generation contains the same sensitive diagnostics. Repair
    // it on every transaction (not only when another rotation is due), and
    // fail closed if a symlink/special file was substituted there.
    match open_private_restart_log_file(&rotated, false) {
        Ok(previous) => drop(previous),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let should_rotate = opened.len() >= max_bytes
        || (incoming_bytes > 0 && opened.len().saturating_add(incoming_bytes) > max_bytes);
    if !should_rotate {
        return Ok(current);
    }

    // Verify the pathname still identifies the descriptor we inspected. This
    // closes the last swap window before rename even against a same-UID actor
    // that ignores the advisory flock.
    let named = std::fs::symlink_metadata(path)?;
    if !named.file_type().is_file() || named.dev() != opened.dev() || named.ino() != opened.ino() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "restart log changed while preparing rotation: {}",
                path.display()
            ),
        ));
    }
    drop(current);

    remove_previous_restart_log(&rotated)?;
    std::fs::rename(path, &rotated)?;
    // Re-open by pathname with O_NOFOLLOW and repair the predecessor before a
    // fresh current generation becomes visible.
    drop(open_private_restart_log_file(&rotated, false)?);
    open_private_restart_log_file(path, true)
}

#[cfg(target_os = "linux")]
fn socket_inode_from_proc(proc_root: &std::path::Path, socket: &std::path::Path) -> Option<String> {
    let table = std::fs::read_to_string(proc_root.join("net/unix")).ok()?;
    let wanted = socket.to_string_lossy();
    table.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        (fields.len() >= 8 && fields[7..].join(" ") == wanted.as_ref())
            .then(|| fields[6].to_string())
    })
}

#[cfg(target_os = "linux")]
fn pid_is_verified_gateway_at(
    proc_root: &std::path::Path,
    pid: i32,
    socket: &std::path::Path,
    selected_executable: Option<&std::path::Path>,
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
    let executable_name = std::path::Path::new(executable_without_deleted)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let matches_selected = selected_executable
        .is_some_and(|selected| selected.to_string_lossy() == executable_without_deleted);
    if !matches!(executable_name, "phoenix" | "phoenix-agent") && !matches_selected {
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
    _proc_root: &std::path::Path,
    _pid: i32,
    _socket: &std::path::Path,
    _selected_executable: Option<&std::path::Path>,
) -> bool {
    false
}

fn pid_is_verified_gateway(pid: i32, socket: &std::path::Path) -> bool {
    let selected = selected_gateway_binary_path();
    pid_is_verified_gateway_at(
        std::path::Path::new("/proc"),
        pid,
        socket,
        selected.as_deref(),
    )
}

#[derive(Serialize)]
struct GatewayStatus {
    running: bool,
    pid: Option<u32>,
    ws_port: u16,
    socket: String,
}

fn gateway_status_blocking() -> GatewayStatus {
    let socket = phoenix_home().join("gateway.sock");
    // Readiness means Phoenix and the daemon agree on the wire contract. A stale
    // socket accepting a TCP-style connect is explicitly not enough; executable
    // bytes are not compatibility because debug/release builds share a wire.
    let running = matches!(gateway_probe(), GatewayProbe::Compatible);
    let pid = std::fs::read_to_string(phoenix_home().join("gateway.pid"))
        .ok()
        .and_then(|raw| raw.trim().parse().ok())
        .filter(|_| running);
    let ws_port = std::env::var("PHOENIX_WS_PORT")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(7469);
    GatewayStatus {
        running,
        pid,
        ws_port,
        socket: socket.display().to_string(),
    }
}

#[tauri::command]
async fn gateway_status() -> GatewayStatus {
    tauri::async_runtime::spawn_blocking(gateway_status_blocking)
        .await
        .expect("gateway status worker panicked")
}

fn process_start_time_at(proc_root: &std::path::Path, pid: i32) -> Option<String> {
    let stat = std::fs::read_to_string(proc_root.join(pid.to_string()).join("stat")).ok()?;
    let (_, rest) = stat.rsplit_once(')')?;
    // The remainder begins with field 3 (state); field 22 (starttime) is
    // therefore zero-based index 19.
    rest.split_whitespace().nth(19).map(str::to_string)
}

fn process_start_time(pid: i32) -> Option<String> {
    process_start_time_at(std::path::Path::new("/proc"), pid)
}

fn process_instance_exited_at(
    proc_root: &std::path::Path,
    pid: i32,
    start_time: Option<&str>,
) -> bool {
    let stat = match std::fs::read_to_string(proc_root.join(pid.to_string()).join("stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
        // Permission and transient I/O failures are not proof of departure.
        // Fail closed so Phoenix cannot overlap two gateways merely because
        // procfs was briefly unreadable.
        Err(_) => return false,
    };
    let Some((_, rest)) = stat.rsplit_once(')') else {
        return false;
    };
    let mut fields = rest.split_whitespace();
    if fields.next() == Some("Z") {
        return true;
    }
    start_time.is_some_and(|expected| fields.nth(18).is_some_and(|actual| actual != expected))
}

fn wait_for_process_instance_exit_at(
    proc_root: &std::path::Path,
    pid: i32,
    start_time: Option<&str>,
    timeout: std::time::Duration,
    poll: std::time::Duration,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if process_instance_exited_at(proc_root, pid, start_time) {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "recorded gateway pid {pid} is still alive after {}s; refusing to start an overlapping gateway",
                timeout.as_secs()
            ));
        }
        std::thread::sleep(poll.min(deadline.saturating_duration_since(std::time::Instant::now())));
    }
}

fn wait_for_process_instance_exit(
    pid: i32,
    start_time: Option<&str>,
    timeout: std::time::Duration,
) -> Result<(), String> {
    wait_for_process_instance_exit_at(
        std::path::Path::new("/proc"),
        pid,
        start_time,
        timeout,
        GATEWAY_EXIT_POLL,
    )
}

fn wait_for_observed_gateway_exit() -> Result<(), String> {
    let observed = observed_gateway_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let Some(identity) = observed else {
        return Ok(());
    };
    wait_for_process_instance_exit(
        identity.pid,
        Some(&identity.start_time),
        GATEWAY_EXIT_TIMEOUT,
    )?;
    let mut slot = observed_gateway_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if slot.as_ref() == Some(&identity) {
        slot.take();
    }
    Ok(())
}

/// Clear dead gateway.sock / gateway.pid only after the exact process instance
/// recorded in the pidfile has departed. The Unix listener is deliberately
/// kept published by current gateways until their bounded cleanup finishes,
/// but this guard also handles older builds that withdrew it too early.
fn reclaim_stale_gateway_files_at(
    home: &std::path::Path,
    proc_root: &std::path::Path,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let socket = home.join("gateway.sock");
    let pidfile = home.join("gateway.pid");
    let recorded = match std::fs::read_to_string(&pidfile) {
        Ok(raw) => {
            let pid = raw
                .trim()
                .parse::<i32>()
                .map_err(|_| "gateway.pid is corrupt; refusing automatic reclaim".to_string())?;
            if pid <= 1 {
                return Err("gateway.pid is unsafe; refusing automatic reclaim".to_string());
            }
            Some((raw, pid, process_start_time_at(proc_root, pid)))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(format!(
                "could not inspect gateway pidfile {}: {error}",
                pidfile.display()
            ))
        }
    };

    if let Some((_, pid, start_time)) = recorded.as_ref() {
        wait_for_process_instance_exit_at(
            proc_root,
            *pid,
            start_time.as_deref(),
            timeout,
            GATEWAY_EXIT_POLL,
        )?;
    }

    // A gateway may have appeared while we waited. Never unlink an endpoint
    // that currently accepts connections, even if its pidfile changed.
    if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
        return Ok(());
    }

    if let Some((raw, _, _)) = recorded {
        match std::fs::read_to_string(&pidfile) {
            Ok(current) if current == raw => std::fs::remove_file(&pidfile)
                .map_err(|error| format!("could not remove {}: {error}", pidfile.display()))?,
            Ok(_) => {
                // Another process replaced the pidfile. Leave both files for
                // the caller's fresh protocol probe instead of erasing it.
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "could not recheck gateway pidfile {}: {error}",
                    pidfile.display()
                ))
            }
        }
    }

    if socket.exists() && std::os::unix::net::UnixStream::connect(&socket).is_err() {
        std::fs::remove_file(&socket)
            .map_err(|error| format!("could not remove {}: {error}", socket.display()))?;
    }
    Ok(())
}

fn reclaim_stale_gateway_files() -> Result<(), String> {
    // The remembered identity survives early deletion of both gateway.sock
    // and gateway.pid by older builds. Do this before file-based reclaim.
    wait_for_observed_gateway_exit()?;
    reclaim_stale_gateway_files_at(
        &phoenix_home(),
        std::path::Path::new("/proc"),
        GATEWAY_EXIT_TIMEOUT,
    )
}

fn stop_verified_gateway_blocking() -> Result<Option<i32>, String> {
    let home = phoenix_home();
    let socket = home.join("gateway.sock");
    let pid: i32 = match std::fs::read_to_string(home.join("gateway.pid")) {
        Ok(raw) => raw
            .trim()
            .parse()
            .map_err(|_| "gateway.pid is corrupt; refusing to signal it".to_string())?,
        Err(_) => {
            if std::os::unix::net::UnixStream::connect(&socket).is_err() {
                reclaim_stale_gateway_files()?;
                return Ok(None);
            }
            return Err(format!(
                "a process is listening on {} without a Phoenix pidfile; refusing to signal an unverified process",
                socket.display()
            ));
        }
    };
    if !pid_is_verified_gateway(pid, &socket) {
        if std::os::unix::net::UnixStream::connect(&socket).is_err() {
            // The listener may have been withdrawn by an older gateway build
            // while that same recorded process is still cleaning up. Reclaim
            // waits for the observed PID instance; it never interprets the
            // missing socket alone as proof of departure.
            reclaim_stale_gateway_files()?;
            return Ok(None);
        }
        return Err(format!(
            "pidfile names pid {pid}, but that same-user Phoenix process could not be proven to own {}; refusing to signal it",
            socket.display()
        ));
    }
    let start_time = process_start_time(pid);
    if unsafe { libc::kill(pid, libc::SIGINT) } == -1 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            reclaim_stale_gateway_files()?;
            return Ok(None);
        }
        return Err(format!(
            "failed to signal verified gateway pid {pid}: {error}"
        ));
    }
    wait_for_process_instance_exit(pid, start_time.as_deref(), GATEWAY_EXIT_TIMEOUT)
        .map_err(|_| format!("verified gateway pid {pid} did not finish cleanup within 15s"))?;
    reclaim_stale_gateway_files()?;
    Ok(Some(pid))
}

/// Spawn fully detached bare `phoenix` if not already running. Shared by start + ensure.
///
/// Unix: `setsid` + double-fork so the gateway is session leader and reparented
/// to init — never a long-lived child of Phoenix (kills the zombie pattern).
/// Intermediate child is reaped immediately after spawn.
fn spawn_gateway_detached() -> Result<std::path::PathBuf, String> {
    match gateway_probe() {
        GatewayProbe::Compatible => return Err("already running".to_string()),
        GatewayProbe::Down => {}
        GatewayProbe::Incompatible(reason) => {
            stop_verified_gateway_blocking().map_err(|error| {
                format!("incompatible gateway could not be safely replaced ({reason}): {error}")
            })?;
        }
        GatewayProbe::Unresponsive(reason) => {
            return Err(format!(
                "gateway accepted connections but did not complete a protocol handshake after {GATEWAY_PROBE_ATTEMPTS} attempts ({reason}); refusing to stop or replace it"
            ));
        }
    }
    // Dead socket/pid left behind would confuse bind and look like a crash.
    // This waits for a still-live recorded PID before touching either file.
    reclaim_stale_gateway_files()?;
    // Cross-process clients can race Phoenix while it waits. Probe again after
    // reclaim so a newly-active endpoint is never overlapped.
    match gateway_probe() {
        GatewayProbe::Compatible => return Err("already running".to_string()),
        GatewayProbe::Down => {}
        GatewayProbe::Incompatible(reason) => {
            return Err(format!(
                "a different incompatible gateway appeared during reclaim ({reason}); retry"
            ))
        }
        GatewayProbe::Unresponsive(reason) => {
            return Err(format!(
                "a gateway appeared during reclaim but did not complete the protocol handshake ({reason}); retry"
            ))
        }
    }

    let log = phoenix_home().join("gateway-restart.log");
    let log_file = open_bounded_restart_log().map_err(|error| error.to_string())?;
    let err_file = log_file.try_clone().map_err(|error| error.to_string())?;
    let mut cmd = std::process::Command::new(phoenix_binary());
    cmd.stdin(std::process::Stdio::null())
        .stdout(log_file)
        .stderr(err_file);
    // Full detach: session leader + double-fork; reap intermediate so Phoenix
    // never holds a defunct phoenix child.
    // SAFETY: pre_exec runs between fork and exec; only async-signal-safe calls.
    unsafe {
        cmd.pre_exec(|| {
            // Launchers may ignore SIGINT; ignored dispositions survive exec.
            // Reset them so the detached gateway still honors `phoenix stop`.
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
                0 => Ok(()), // grandchild continues to exec
                _ => {
                    libc::_exit(0); // intermediate exits → reparent grandchild to init
                }
            }
        });
    }
    let mut child = cmd
        .spawn()
        .map_err(|error| format!("failed to launch phoenix: {error}"))?;
    // Reap intermediate child only — the real gateway is the grandchild.
    let _ = child.wait();
    Ok(log)
}

/// Ensure the gateway is up, blocking until the socket accepts (~12s) or error.
/// Cheap no-op when already running. Shared by the `gateway_ensure` command and
/// the background supervisor so boot-start, lazy-ensure, and auto-restart all
/// follow the same detach + reclaim path.
fn ensure_gateway_up_blocking() -> Result<(), String> {
    let _local_lifecycle = gateway_local_lifecycle_lock()
        .lock()
        .map_err(|_| "gateway lifecycle lock poisoned".to_string())?;
    let log = {
        // Hold the shared lifecycle lease across the final probe and every
        // possible stop/reclaim/spawn mutation. Release it immediately after
        // the detached child is launched: the daemon acquires this same lease
        // during startup, so waiting for readiness while retaining it would
        // deadlock Phoenix against the process it just created.
        let _cross_process_lifecycle = GatewayLifecycleLease::acquire()?;
        if matches!(gateway_probe(), GatewayProbe::Compatible) {
            return Ok(());
        }
        match spawn_gateway_detached() {
            Ok(log) => log,
            // Race: another client started it between probe and spawn.
            Err(msg) if msg == "already running" => return Ok(()),
            Err(err) => return Err(err),
        }
    };
    let socket = phoenix_home().join("gateway.sock");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(12);
    loop {
        match gateway_probe_at(&socket) {
            GatewayProbe::Compatible => {
                remember_verified_gateway_instance(&socket);
                return Ok(());
            }
            GatewayProbe::Down => {}
            GatewayProbe::Unresponsive(_) => {
                // A just-spawned daemon may be initializing.  Keep waiting to
                // the outer deadline, but never reinterpret a transient read
                // failure as permission to kill it.
            }
            GatewayProbe::Incompatible(reason) => {
                return Err(format!(
                    "spawned gateway is not compatible with this Phoenix ({reason}) — check log at {}",
                    log.display()
                ));
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "gateway did not become ready within 12s — check log at {}",
                log.display()
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// One timestamped line into gateway-restart.log — the supervisor's diary.
fn supervisor_log(msg: &str) {
    // Avoid spawning `date` from the permanent supervisor path: an altered
    // PATH or wedged helper must never block gateway recovery. Epoch seconds
    // are stable, sortable, and require no fallible child process.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs().to_string())
        .unwrap_or_else(|_| "clock-before-epoch".to_string());
    let line = format!("[{stamp}] supervisor: {msg}\n");
    let _ = append_bounded_restart_log(line.as_bytes());
}

fn gateway_supervisor_backoff(failures: u32, error: &str) -> std::time::Duration {
    let lower = error.to_ascii_lowercase();
    if lower.contains("not compatible") || lower.contains("incompatible gateway") {
        // A protocol mismatch needs a different desktop/gateway build, not a
        // three-second respawn storm of the same incompatible executable.
        return std::time::Duration::from_secs(5 * 60);
    }
    let exponent = failures.saturating_sub(1).min(5);
    std::time::Duration::from_secs(3_u64.saturating_mul(1_u64 << exponent).min(120))
}

/// Background watchdog: keep exactly one gateway alive for the app's lifetime.
/// Every 3s it probes the socket; if the gateway is gone (crash, OOM kill,
/// manual stop, or a signal that slipped past detach) it restarts it detached.
/// This is what makes the gateway feel permanent to the user — start the app,
/// the gateway is always there, and it silently comes back if it ever dies.
fn start_gateway_supervisor() {
    std::thread::spawn(|| {
        // First tick doubles as boot autostart — the app never opens gateway-less.
        let mut failures = 0_u32;
        let mut next_attempt = std::time::Instant::now();
        loop {
            if gateway_status_blocking().running {
                failures = 0;
                next_attempt = std::time::Instant::now();
            } else if std::time::Instant::now() >= next_attempt {
                match ensure_gateway_up_blocking() {
                    Ok(()) => {
                        failures = 0;
                        next_attempt = std::time::Instant::now();
                        supervisor_log("gateway was down — auto-restarted ✓");
                    }
                    Err(error) => {
                        failures = failures.saturating_add(1);
                        let delay = gateway_supervisor_backoff(failures, &error);
                        next_attempt = std::time::Instant::now() + delay;
                        supervisor_log(&format!(
                            "auto-restart failed; next attempt in {}s: {error}",
                            delay.as_secs()
                        ));
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
    });
}

#[tauri::command]
async fn gateway_start() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let _local_lifecycle = gateway_local_lifecycle_lock()
            .lock()
            .map_err(|_| "gateway lifecycle lock poisoned".to_string())?;
        let _cross_process_lifecycle = GatewayLifecycleLease::acquire()?;
        if matches!(gateway_probe(), GatewayProbe::Compatible) {
            return Ok("already running".to_string());
        }
        let log = spawn_gateway_detached()?;
        Ok(format!("gateway starting — log at {}", log.display()))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Start the gateway if needed and wait until the socket accepts connections (~12s).
#[tauri::command]
async fn gateway_ensure() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        ensure_gateway_up_blocking().map(|()| "gateway ready".to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn gateway_stop() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let _local_lifecycle = gateway_local_lifecycle_lock()
            .lock()
            .map_err(|_| "gateway lifecycle lock poisoned".to_string())?;
        let _cross_process_lifecycle = GatewayLifecycleLease::acquire()?;
        match stop_verified_gateway_blocking()? {
            Some(pid) => Ok(format!("gateway stopped (pid {pid}).")),
            None => Ok("no gateway running.".to_string()),
        }
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn gateway_token() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let path = phoenix_home().join("gateway.token");
        read_private_text_required(&path, PRIVATE_SECRET_MAX_BYTES, "gateway token")
            .map(|token| token.trim().to_string())
            .map_err(|_| "no gateway token yet — start the gateway once".to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

fn gateway_log_tail_blocking(lines: usize) -> String {
    use std::io::{Read, Seek, SeekFrom};
    // Seek-tail: read only the last ~128KB of the log instead of the whole
    // file. gateway.log grows unbounded and this is polled every few seconds
    // by the health tab — reading the full 3MB+ each cycle was a top cause of
    // UI-thread jank. 128KB comfortably covers the 500-line clamp cap.
    let take = lines.clamp(10, 500);
    use std::os::unix::fs::OpenOptionsExt;
    let Ok(mut file) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(phoenix_home().join("gateway.log"))
    else {
        return String::new();
    };
    let Ok(metadata) = file.metadata() else {
        return String::new();
    };
    if !metadata.is_file() || metadata.len() > GATEWAY_LOG_READ_MAX_BYTES as u64 {
        return String::new();
    }
    let len = metadata.len();
    const WINDOW: u64 = 128 * 1024;
    let start = len.saturating_sub(WINDOW);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    let content = String::from_utf8_lossy(&buf);
    // Drop the first (likely partial) line when we seeked into the middle.
    let mut all: Vec<&str> = content.lines().collect();
    if start > 0 && !all.is_empty() {
        all.remove(0);
    }
    all[all.len().saturating_sub(take)..].join("\n")
}

#[tauri::command]
async fn gateway_log_tail(lines: usize) -> String {
    tauri::async_runtime::spawn_blocking(move || gateway_log_tail_blocking(lines))
        .await
        .expect("gateway log-tail worker panicked")
}

fn action_audit_recent_at(
    path: &std::path::Path,
    limit: usize,
) -> Result<Vec<serde_json::Value>, String> {
    let Some(raw) = read_private_text(path, ACTION_AUDIT_MAX_BYTES, "action ledger")? else {
        return Ok(Vec::new());
    };
    let mut rows = raw
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .take(limit.clamp(1, 500))
        .collect::<Vec<_>>();
    rows.reverse();
    Ok(rows)
}

#[tauri::command]
async fn action_audit_recent(limit: Option<usize>) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let path = phoenix_home().join("audit/actions.jsonl");
        let rows = action_audit_recent_at(&path, limit.unwrap_or(120))?;
        Ok(serde_json::json!({
            "path": path,
            "records": rows,
        }))
    })
    .await
    .map_err(|error| format!("action-ledger worker stopped: {error}"))?
}

#[derive(Debug, Clone, Default, Deserialize)]
struct UsageTelemetryRow {
    #[serde(default)]
    ts: String,
    #[serde(default)]
    agent: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    request_chars: u64,
    #[serde(default)]
    provider_ms: u64,
    #[serde(default)]
    provider_error: bool,
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    estimated_input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    tool_ms: u64,
    #[serde(default)]
    compress_saved_bytes: u64,
    #[serde(default)]
    cache_read_tokens: Option<u64>,
    #[serde(default)]
    cache_creation_tokens: Option<u64>,
    #[serde(default)]
    cost_micros: Option<u64>,
}

impl UsageTelemetryRow {
    fn input(&self) -> (u64, bool) {
        if self.input_tokens > 0 {
            (self.input_tokens, false)
        } else {
            (
                self.estimated_input_tokens
                    .unwrap_or(self.request_chars / 4),
                self.request_chars > 0,
            )
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
struct UsageBucket {
    rounds: u64,
    input_tokens: u64,
    output_tokens: u64,
    estimated_rounds: u64,
    provider_ms: u64,
    tool_ms: u64,
    provider_errors: u64,
    compression_saved_tokens: u64,
    cache_reported_rounds: u64,
    cache_reported_input_tokens: u64,
    cache_read_tokens: u64,
    cache_unknown_input_tokens: u64,
    cost_reported_rounds: u64,
    cost_micros: u64,
}

impl UsageBucket {
    fn add(&mut self, row: &UsageTelemetryRow) {
        let (input, estimated) = row.input();
        self.rounds = self.rounds.saturating_add(1);
        self.input_tokens = self.input_tokens.saturating_add(input);
        self.output_tokens = self.output_tokens.saturating_add(row.output_tokens);
        self.estimated_rounds = self
            .estimated_rounds
            .saturating_add(u64::from(estimated));
        self.provider_ms = self.provider_ms.saturating_add(row.provider_ms);
        self.tool_ms = self.tool_ms.saturating_add(row.tool_ms);
        self.provider_errors = self
            .provider_errors
            .saturating_add(u64::from(row.provider_error));
        self.compression_saved_tokens = self
            .compression_saved_tokens
            .saturating_add(row.compress_saved_bytes / 4);
        if let Some(cache_read) = row.cache_read_tokens {
            let anthropic_shape = row
                .provider
                .as_deref()
                .is_some_and(|provider| provider.contains("anthropic"))
                || row.model.to_ascii_lowercase().contains("claude");
            let prompt_tokens = if anthropic_shape {
                row.input_tokens
                    .saturating_add(cache_read)
                    .saturating_add(row.cache_creation_tokens.unwrap_or(0))
            } else {
                row.input_tokens
            };
            self.cache_reported_rounds = self.cache_reported_rounds.saturating_add(1);
            self.cache_reported_input_tokens = self
                .cache_reported_input_tokens
                .saturating_add(prompt_tokens);
            self.cache_read_tokens = self.cache_read_tokens.saturating_add(cache_read);
        } else {
            self.cache_unknown_input_tokens =
                self.cache_unknown_input_tokens.saturating_add(input);
        }
        if let Some(cost_micros) = row.cost_micros {
            self.cost_reported_rounds = self.cost_reported_rounds.saturating_add(1);
            self.cost_micros = self.cost_micros.saturating_add(cost_micros);
        }
    }
}

#[derive(Debug, Serialize)]
struct UsageLane {
    agent: String,
    provider: String,
    model: String,
    #[serde(flatten)]
    usage: UsageBucket,
}

#[derive(Debug, Serialize)]
struct UsageDay {
    date: String,
    #[serde(flatten)]
    usage: UsageBucket,
}

fn usage_dashboard_at(
    root: &std::path::Path,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<serde_json::Value, String> {
    let mut rows = Vec::new();
    let mut invalid_rows = 0_u64;
    let mut sources = Vec::new();
    for name in ["round_timings.prev.jsonl", "round_timings.jsonl"] {
        let path = root.join("runs").join(name);
        let Some(raw) = read_private_text(&path, USAGE_TIMING_LOG_MAX_BYTES, "usage telemetry")?
        else {
            continue;
        };
        sources.push(path);
        for line in raw.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if line.len() > USAGE_TIMING_LINE_MAX_BYTES {
                invalid_rows = invalid_rows.saturating_add(1);
                continue;
            }
            match serde_json::from_str::<UsageTelemetryRow>(line) {
                Ok(row) => rows.push(row),
                Err(_) => invalid_rows = invalid_rows.saturating_add(1),
            }
        }
    }
    rows.sort_by(|left, right| left.ts.cmp(&right.ts));

    let cutoff = now - chrono::Duration::days(7);
    let future_slop = now + chrono::Duration::minutes(5);
    let mut retained = UsageBucket::default();
    let mut week = UsageBucket::default();
    let mut lane_buckets: std::collections::BTreeMap<(String, String, String), UsageBucket> =
        std::collections::BTreeMap::new();
    let mut day_buckets: std::collections::BTreeMap<String, UsageBucket> =
        std::collections::BTreeMap::new();
    for row in &rows {
        retained.add(row);
        let Some(timestamp) = chrono::DateTime::parse_from_rfc3339(&row.ts)
            .ok()
            .map(|timestamp| timestamp.with_timezone(&chrono::Utc))
        else {
            continue;
        };
        if timestamp < cutoff || timestamp > future_slop {
            continue;
        }
        week.add(row);
        let agent = if row.agent.trim().is_empty() {
            "Unknown coworker".to_string()
        } else {
            row.agent.trim().to_string()
        };
        let provider = row
            .provider
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("Unknown provider")
            .to_string();
        let model = if row.model.trim().is_empty() {
            "Unknown model".to_string()
        } else {
            row.model.trim().to_string()
        };
        lane_buckets
            .entry((agent, provider, model))
            .or_default()
            .add(row);
        day_buckets
            .entry(timestamp.format("%Y-%m-%d").to_string())
            .or_default()
            .add(row);
    }
    let mut lanes = lane_buckets
        .into_iter()
        .map(|((agent, provider, model), usage)| UsageLane {
            agent,
            provider,
            model,
            usage,
        })
        .collect::<Vec<_>>();
    lanes.sort_by_key(|lane| std::cmp::Reverse(lane.usage.input_tokens));
    lanes.truncate(20);
    let days = day_buckets
        .into_iter()
        .map(|(date, usage)| UsageDay { date, usage })
        .collect::<Vec<_>>();

    Ok(serde_json::json!({
        "generated_at": now.to_rfc3339(),
        "window_days": 7,
        "sources": sources,
        "invalid_rows": invalid_rows,
        "retained": retained,
        "week": week,
        "lanes": lanes,
        "days": days,
        "cost_note": "Spend is shown only for rounds where a provider reports cost. Phoenix does not apply a guessed price table.",
    }))
}

#[tauri::command]
async fn usage_dashboard() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(|| usage_dashboard_at(&phoenix_home(), chrono::Utc::now()))
        .await
        .map_err(|error| format!("usage dashboard worker stopped: {error}"))?
}

const REMOTE_RUNNERS_MAX_BYTES: usize = 256 * 1024;
const REMOTE_RUNNERS_CAP: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DesktopRemoteRunner {
    id: String,
    label: String,
    host: String,
    #[serde(default = "desktop_runner_default_port")]
    port: u16,
    user: String,
    workspace_root: String,
    host_key: String,
    #[serde(default)]
    identity_file: Option<String>,
    #[serde(default = "desktop_runner_default_enabled")]
    enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DesktopRemoteRunnerStore {
    version: u32,
    runners: Vec<DesktopRemoteRunner>,
}

fn desktop_runner_default_port() -> u16 {
    22
}

fn desktop_runner_default_enabled() -> bool {
    true
}

fn remote_runners_path() -> PathBuf {
    phoenix_home().join("runners.json")
}

fn remote_runner_known_hosts_path(id: &str) -> PathBuf {
    phoenix_home()
        .join("runners/known-hosts")
        .join(format!("{id}.known_hosts"))
}

fn valid_remote_runner_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

#[cfg(unix)]
fn reject_remote_runner_symlink_path(path: &std::path::Path) -> Result<(), String> {
    let mut cursor = Some(path);
    while let Some(candidate) = cursor {
        match std::fs::symlink_metadata(candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "Refusing symlinked identity path component {}.",
                    candidate.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "Could not inspect identity path component {}: {error}",
                    candidate.display()
                ));
            }
        }
        cursor = candidate.parent().filter(|parent| *parent != candidate);
    }
    Ok(())
}

fn validate_desktop_remote_runner(runner: &mut DesktopRemoteRunner) -> Result<(), String> {
    runner.id = runner.id.trim().to_ascii_lowercase();
    runner.label = runner.label.trim().to_string();
    runner.host = runner.host.trim().to_ascii_lowercase();
    runner.user = runner.user.trim().to_string();
    runner.workspace_root = runner.workspace_root.trim().to_string();
    runner.host_key = runner.host_key.split_whitespace().collect::<Vec<_>>().join(" ");
    runner.identity_file = runner
        .identity_file
        .take()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if !valid_remote_runner_component(&runner.id) {
        return Err("Runner id may contain only letters, numbers, dot, dash, and underscore.".into());
    }
    if runner.label.is_empty() || runner.label.len() > 120 {
        return Err("Runner label must be 1–120 characters.".into());
    }
    if !valid_remote_runner_component(&runner.host) || runner.host.starts_with('.') {
        return Err("Runner host must be a plain DNS name or IPv4 address.".into());
    }
    if !valid_remote_runner_component(&runner.user) || runner.port == 0 {
        return Err("Runner user or port is invalid.".into());
    }
    let workspace = std::path::Path::new(&runner.workspace_root);
    if !workspace.is_absolute()
        || runner.workspace_root.chars().any(char::is_control)
        || workspace
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err("Remote workspace must be a clean absolute path.".into());
    }
    let mut host_key = runner.host_key.split_whitespace();
    let algorithm = host_key.next().unwrap_or_default();
    let encoded = host_key.next().unwrap_or_default();
    if !matches!(
        algorithm,
        "ssh-ed25519"
            | "ecdsa-sha2-nistp256"
            | "rsa-sha2-512"
            | "rsa-sha2-256"
            | "ssh-rsa"
    ) || encoded.len() < 40
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
        || host_key.next().is_some()
    {
        return Err("Host key must be one exact OpenSSH algorithm/key pair.".into());
    }
    if let Some(identity) = runner.identity_file.as_deref() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let path = std::path::Path::new(identity);
        if !path.is_absolute() {
            return Err("Identity file must use an absolute path.".into());
        }
        reject_remote_runner_symlink_path(path)?;
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|error| format!("Could not inspect identity file: {error}"))?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err("Identity file must be same-user, private (0600), and not symlinked.".into());
        }
    }
    Ok(())
}

fn load_remote_runners_unlocked() -> Result<Vec<DesktopRemoteRunner>, String> {
    let path = remote_runners_path();
    let Some(raw) = read_private_text(&path, REMOTE_RUNNERS_MAX_BYTES, "remote runner registry")?
    else {
        return Ok(Vec::new());
    };
    let mut store: DesktopRemoteRunnerStore =
        serde_json::from_str(&raw).map_err(|error| format!("Runner registry is invalid: {error}"))?;
    if store.version != 1 || store.runners.len() > REMOTE_RUNNERS_CAP {
        return Err("Runner registry version or size is invalid.".into());
    }
    let mut seen = std::collections::HashSet::new();
    for runner in &mut store.runners {
        validate_desktop_remote_runner(runner)?;
        if !seen.insert(runner.id.clone()) {
            return Err(format!("Duplicate remote runner id `{}`.", runner.id));
        }
    }
    Ok(store.runners)
}

#[tauri::command]
fn remote_runners_list() -> Result<Vec<DesktopRemoteRunner>, String> {
    load_remote_runners_unlocked()
}

#[tauri::command]
fn remote_runner_save(mut runner: DesktopRemoteRunner) -> Result<Vec<DesktopRemoteRunner>, String> {
    validate_desktop_remote_runner(&mut runner)?;
    let path = remote_runners_path();
    with_private_file_lock(&path, || {
        let mut runners = load_remote_runners_unlocked()?;
        if let Some(existing) = runners.iter_mut().find(|existing| existing.id == runner.id) {
            *existing = runner;
        } else {
            if runners.len() >= REMOTE_RUNNERS_CAP {
                return Err(format!("Phoenix supports at most {REMOTE_RUNNERS_CAP} remote runners."));
            }
            runners.push(runner);
        }
        runners.sort_by(|left, right| left.label.cmp(&right.label));
        let bytes = serde_json::to_vec_pretty(&DesktopRemoteRunnerStore {
            version: 1,
            runners: runners.clone(),
        })
        .map_err(|error| error.to_string())?;
        write_private_atomic_unlocked(&path, &bytes)?;
        Ok(runners)
    })
}

#[tauri::command]
fn remote_runner_remove(id: String) -> Result<Vec<DesktopRemoteRunner>, String> {
    if !valid_remote_runner_component(&id) {
        return Err("Invalid remote runner id.".into());
    }
    let path = remote_runners_path();
    with_private_file_lock(&path, || {
        let mut runners = load_remote_runners_unlocked()?;
        let before = runners.len();
        runners.retain(|runner| runner.id != id);
        if runners.len() == before {
            return Err(format!("Remote runner `{id}` does not exist."));
        }
        let bytes = serde_json::to_vec_pretty(&DesktopRemoteRunnerStore {
            version: 1,
            runners: runners.clone(),
        })
        .map_err(|error| error.to_string())?;
        write_private_atomic_unlocked(&path, &bytes)?;
        let known_hosts = remote_runner_known_hosts_path(&id);
        match std::fs::symlink_metadata(&known_hosts) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err("Refusing unsafe runner known-hosts path.".into());
            }
            Ok(_) => std::fs::remove_file(&known_hosts).map_err(|error| error.to_string())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        Ok(runners)
    })
}

#[derive(Serialize)]
struct SessionInfo {
    id: String,
    title: String,
    messages: usize,
    modified_secs_ago: u64,
    /// Some("coder") for a specialist sidecar ("main-x__coder.json") — the
    /// OpenClaw sessions list shows these behind its toggle with the agent
    /// prefix; canvas pickers filter them out client-side.
    specialist: Option<String>,
}

fn list_sessions_blocking() -> Vec<SessionInfo> {
    let dir = phoenix_home().join("sessions");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let now = std::time::SystemTime::now();
    // Two-pass: stat every session file (cheap) and sort by mtime FIRST, then
    // read + JSON-parse only the 60 we actually keep. The old code slurped and
    // parsed the full content of ALL ~275 session files (each the entire
    // message history) on every call — polled every few seconds by the health
    // tab, this was the single heaviest disk hit and a top lag cause.
    let mut stubs: Vec<(std::path::PathBuf, Option<String>, u64)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).ok()?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return None;
            }
            let name = path.file_name()?.to_str()?;
            if !name.ends_with(".json") {
                return None;
            }
            let specialist = name
                .trim_end_matches(".json")
                .split_once("__")
                .map(|(_, role)| role.to_string());
            let modified_secs_ago = metadata
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .map(|duration| duration.as_secs())
                .unwrap_or(u64::MAX);
            Some((path, specialist, modified_secs_ago))
        })
        .collect();
    stubs.sort_by_key(|(_, _, ago)| *ago);
    stubs.truncate(60);
    stubs
        .into_iter()
        .filter_map(|(path, specialist, modified_secs_ago)| {
            let content = read_private_text(&path, SESSION_DISPLAY_MAX_BYTES, "session")
                .ok()
                .flatten()?;
            let value: serde_json::Value = serde_json::from_str(&content).ok()?;
            let id = value.get("id")?.as_str()?.to_string();
            validate_session_id_for_canvas(&id).ok()?;
            let title = value
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            let messages = value
                .get("messages")
                .and_then(|m| m.as_array())
                .map(|m| m.len())
                .unwrap_or(0);
            Some(SessionInfo {
                id,
                title,
                messages,
                modified_secs_ago,
                specialist,
            })
        })
        .collect()
}

#[tauri::command]
async fn list_sessions() -> Vec<SessionInfo> {
    tauri::async_runtime::spawn_blocking(list_sessions_blocking)
        .await
        .expect("session-list worker panicked")
}

/// The config facts the dashboard shows (read-only v1 — editing stays with
/// `phoenix configure` until the full dashboard editor lands).
fn config_summary_blocking() -> Vec<(String, String)> {
    let path = phoenix_home().join("config.toml");
    let content = match read_private_text(&path, PRIVATE_DOCUMENT_MAX_BYTES, "config.toml") {
        Ok(Some(content)) => content,
        Ok(None) => {
            return vec![(
                "config".into(),
                "no config.toml — run `phoenix onboard`".into(),
            )]
        }
        Err(error) => return vec![("config".into(), format!("config.toml unreadable: {error}"))],
    };
    let Ok(value) = content.parse::<toml::Value>() else {
        return vec![("config".into(), "config.toml unreadable".into())];
    };
    let profile = value.get("profile");
    let get = |path: &[&str]| -> Option<String> {
        let mut current = profile?;
        for key in path {
            current = current.get(key)?;
        }
        Some(match current {
            toml::Value::String(s) => s.clone(),
            other => other.to_string(),
        })
    };
    let mut rows = Vec::new();
    let mut push = |label: &str, value: Option<String>| {
        if let Some(value) = value {
            rows.push((label.to_string(), value));
        }
    };
    push("Main model", get(&["llm", "model"]));
    push("Main provider", get(&["llm", "provider"]));
    push("Context window override", get(&["llm", "context_window"]));
    push("Vision", get(&["llm", "vision_model"]));
    push("Librarian", get(&["llm", "librarian_model"]));
    push("Speech to text", get(&["llm", "stt_model"]));
    push("Text to speech", get(&["llm", "tts_model"]));
    push("Live voice", get(&["llm", "realtime_model"]));
    rows
}

#[tauri::command]
async fn config_summary() -> Vec<(String, String)> {
    tauri::async_runtime::spawn_blocking(config_summary_blocking)
        .await
        .expect("config-summary worker panicked")
}

/* ── Cron jobs (~/.phoenix/crons.json — the daemon's own store) ────── */

fn crons_path() -> PathBuf {
    phoenix_home().join("crons.json")
}

fn validate_repeat_seconds(seconds: u64) -> Result<(), String> {
    if !(60..=MAX_SCHEDULE_SECONDS).contains(&seconds) {
        return Err(format!(
            "repeat interval must be between 60 and {MAX_SCHEDULE_SECONDS} seconds; got {seconds}"
        ));
    }
    Ok(())
}

fn validate_once_seconds(seconds: u64) -> Result<(), String> {
    if seconds == 0 || seconds > MAX_SCHEDULE_SECONDS {
        return Err(format!(
            "one-shot interval must be between 1 and {MAX_SCHEDULE_SECONDS} seconds; got {seconds}"
        ));
    }
    Ok(())
}

fn checked_cron_deadline(now: i64, seconds: u64, label: &str) -> Result<i64, String> {
    let delta = i64::try_from(seconds)
        .map_err(|_| format!("{label} interval cannot be represented as seconds"))?;
    now.checked_add(delta)
        .ok_or_else(|| format!("{label} next-run date exceeds the supported range"))
}

fn validate_session_id_for_canvas(value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 192 {
        return Err("session ids must contain 1..=192 bytes".to_string());
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err("session ids may contain only ASCII letters, digits, '_' and '-'".to_string());
    }
    Ok(())
}

fn local_time_at(timestamp: i64) -> Result<libc::tm, String> {
    let timestamp = libc::time_t::try_from(timestamp)
        .map_err(|_| "cron timestamp exceeds the platform time range".to_string())?;
    let mut local = unsafe { std::mem::zeroed::<libc::tm>() };
    // SAFETY: both pointers refer to initialized, properly aligned storage for
    // the duration of this call. localtime_r is thread-safe and does not retain
    // either pointer.
    if unsafe { libc::localtime_r(&timestamp, &mut local) }.is_null() {
        return Err("could not convert the cron timestamp to local time".to_string());
    }
    Ok(local)
}

fn local_date_ahead(base: &libc::tm, days: i32) -> Result<(i32, i32, i32, i32), String> {
    let mut date = unsafe { std::mem::zeroed::<libc::tm>() };
    date.tm_year = base.tm_year;
    date.tm_mon = base.tm_mon;
    date.tm_mday = base
        .tm_mday
        .checked_add(days)
        .ok_or_else(|| "local cron date overflow".to_string())?;
    // Noon avoids ordinary DST transition gaps while libc normalizes month
    // and year rollover for this calendar date.
    date.tm_hour = 12;
    date.tm_isdst = -1;
    // SAFETY: mktime receives a valid writable tm and does not retain it.
    if unsafe { libc::mktime(&mut date) } == -1 {
        return Err("could not normalize a local cron date".to_string());
    }
    Ok((date.tm_year, date.tm_mon, date.tm_mday, date.tm_wday))
}

/// Resolve a local wall-clock time only when it maps to exactly one instant.
/// This mirrors Chrono's `LocalResult::Single`: spring-forward gaps and the
/// duplicated hour at fall-back are skipped instead of silently normalized.
fn unique_local_timestamp(
    year: i32,
    month: i32,
    day: i32,
    hour: u8,
    minute: u8,
) -> Result<Option<i64>, String> {
    let mut candidates = Vec::with_capacity(2);
    for is_dst in [0, 1] {
        let mut local = unsafe { std::mem::zeroed::<libc::tm>() };
        local.tm_year = year;
        local.tm_mon = month;
        local.tm_mday = day;
        local.tm_hour = i32::from(hour);
        local.tm_min = i32::from(minute);
        local.tm_isdst = is_dst;
        // SAFETY: mktime receives a valid writable tm and does not retain it.
        let candidate = unsafe { libc::mktime(&mut local) };
        if candidate == -1 {
            continue;
        }
        let candidate = i64::try_from(candidate)
            .map_err(|_| "local cron timestamp exceeds the supported range".to_string())?;
        let roundtrip = local_time_at(candidate)?;
        if roundtrip.tm_year == year
            && roundtrip.tm_mon == month
            && roundtrip.tm_mday == day
            && roundtrip.tm_hour == i32::from(hour)
            && roundtrip.tm_min == i32::from(minute)
            && roundtrip.tm_sec == 0
        {
            candidates.push(candidate);
        }
    }
    candidates.sort_unstable();
    candidates.dedup();
    Ok((candidates.len() == 1).then(|| candidates[0]))
}

fn next_local_cron_time(
    now: i64,
    hour: u8,
    minute: u8,
    weekdays: Option<&[u8]>,
) -> Result<i64, String> {
    if hour > 23 || minute > 59 {
        return Err("cron hour must be 0..=23 and minute must be 0..=59".to_string());
    }
    if weekdays.is_some_and(|days| days.is_empty() || days.iter().any(|day| *day > 6)) {
        return Err("cron weekdays must be a non-empty list in 0..=6".to_string());
    }
    let base = local_time_at(now)?;
    // At most a week is normally needed. A 370-day fail-safe also handles a
    // pathological zone/calendar transition without an unbounded loop.
    for ahead in 0..=370 {
        let (year, month, day, weekday) = local_date_ahead(&base, ahead)?;
        if weekdays.is_some_and(|days| !days.contains(&(weekday as u8))) {
            continue;
        }
        if let Some(candidate) = unique_local_timestamp(year, month, day, hour, minute)? {
            if candidate > now {
                return Ok(candidate);
            }
        }
    }
    Err("could not find a valid local cron occurrence within one year".to_string())
}

fn required_cron_u64(
    schedule: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    entry_id: &str,
) -> Result<u64, String> {
    schedule
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| format!("cron {entry_id} needs an unsigned integer `{key}`"))
}

fn validate_cron_entries(entries: &[serde_json::Value]) -> Result<(), String> {
    let mut ids = std::collections::HashSet::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let entry = entry
            .as_object()
            .ok_or_else(|| format!("cron entry {index} must be an object"))?;
        let id = entry
            .get("id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("cron entry {index} needs a non-empty id"))?;
        if id.len() > 64
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(format!("cron id `{id}` contains unsafe characters"));
        }
        if !ids.insert(id) {
            return Err(format!("cron store contains duplicate id `{id}`"));
        }
        let session_id = entry
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("cron {id} needs a non-empty session_id"))?;
        validate_session_id_for_canvas(session_id)
            .map_err(|error| format!("cron {id} has an invalid session id: {error}"))?;
        if let Some(canvas) = entry.get("canvas").and_then(serde_json::Value::as_str) {
            validate_session_id_for_canvas(canvas)
                .map_err(|error| format!("cron {id} has an invalid canvas id: {error}"))?;
        }
        let prompt = entry
            .get("prompt")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("cron {id} needs a string prompt"))?;
        if prompt.len() > 256 * 1024 {
            return Err(format!("cron {id} prompt exceeds the 256 KiB limit"));
        }
        let next_run = entry
            .get("next_run")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("cron {id} needs a non-empty next_run"))?;
        if next_run.len() > 64 || !next_run.is_ascii() {
            return Err(format!("cron {id} has an invalid next_run"));
        }
        let schedule = entry
            .get("schedule")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| format!("cron {id} needs a schedule object"))?;
        let kind = schedule
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("cron {id} schedule needs a string kind"))?;
        match kind {
            "every" => validate_repeat_seconds(required_cron_u64(schedule, "seconds", id)?)?,
            "daily" => {
                let hour = required_cron_u64(schedule, "hour", id)?;
                let minute = required_cron_u64(schedule, "minute", id)?;
                if hour > 23 || minute > 59 {
                    return Err(format!(
                        "cron {id} has invalid daily time {hour:02}:{minute:02}"
                    ));
                }
            }
            "weekly" => {
                let hour = required_cron_u64(schedule, "hour", id)?;
                let minute = required_cron_u64(schedule, "minute", id)?;
                if hour > 23 || minute > 59 {
                    return Err(format!(
                        "cron {id} has invalid weekly time {hour:02}:{minute:02}"
                    ));
                }
                let days = schedule
                    .get("days")
                    .and_then(serde_json::Value::as_array)
                    .ok_or_else(|| format!("cron {id} needs a weekday array"))?;
                if days.is_empty()
                    || days
                        .iter()
                        .any(|day| day.as_u64().is_none_or(|day| day > 6))
                {
                    return Err(format!("cron {id} needs weekdays in the range 0..=6"));
                }
            }
            "once" => {}
            other => return Err(format!("cron {id} has unknown schedule kind `{other}`")),
        }
    }
    Ok(())
}

fn mutate_crons_at<T>(
    path: &std::path::Path,
    mutate: impl FnOnce(&mut Vec<serde_json::Value>) -> Result<T, String>,
) -> Result<T, String> {
    with_private_file_lock(path, || {
        let mut entries =
            match read_private_bounded_unlocked(path, PRIVATE_DOCUMENT_MAX_BYTES, "cron store")? {
                Some(bytes) if bytes.is_empty() => Vec::new(),
                Some(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
                    format!(
                        "cron store {} is invalid; refusing to erase it: {error}",
                        path.display()
                    )
                })?,
                None => Vec::new(),
            };
        validate_cron_entries(&entries).map_err(|error| {
            format!(
                "cron store {} is invalid; refusing to replace it: {error}",
                path.display()
            )
        })?;
        let result = mutate(&mut entries)?;
        validate_cron_entries(&entries).map_err(|error| {
            format!(
                "replacement cron store {} is invalid; refusing to write it: {error}",
                path.display()
            )
        })?;
        let replacement = serde_json::to_vec_pretty(&entries).map_err(|error| error.to_string())?;
        write_private_atomic_unlocked(path, &replacement)?;
        Ok(result)
    })
}

fn crons_list_blocking() -> Result<serde_json::Value, String> {
    let path = crons_path();
    let entries: Vec<serde_json::Value> =
        match read_private_bounded_unlocked(&path, PRIVATE_DOCUMENT_MAX_BYTES, "cron store")? {
            Some(bytes) if bytes.is_empty() => Vec::new(),
            Some(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| format!("cron store {} is invalid: {error}", path.display()))?,
            None => Vec::new(),
        };
    validate_cron_entries(&entries)
        .map_err(|error| format!("cron store {} is invalid: {error}", path.display()))?;
    Ok(serde_json::Value::Array(entries))
}

#[tauri::command]
async fn crons_list() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(crons_list_blocking)
        .await
        .map_err(|error| error.to_string())?
}

/// Add a cron entry in the daemon's exact shape. `schedule_kind` is
/// "every" (seconds) / "daily" (hour, minute) / "once" (delay_secs).
struct CronDraft {
    session_id: String,
    canvas: Option<String>,
    prompt: String,
    schedule_kind: String,
    seconds: Option<u64>,
    hour: Option<u8>,
    minute: Option<u8>,
    days: Option<Vec<u8>>,
}

#[tauri::command]
fn cron_add(
    session_id: String,
    canvas: Option<String>,
    prompt: String,
    schedule_kind: String,
    seconds: Option<u64>,
    hour: Option<u8>,
    minute: Option<u8>,
    days: Option<Vec<u8>>,
) -> Result<serde_json::Value, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_secs();
    let now = i64::try_from(now)
        .map_err(|_| "current time exceeds the supported cron date range".to_string())?;
    cron_add_at(
        &crons_path(),
        now,
        CronDraft {
            session_id,
            canvas,
            prompt,
            schedule_kind,
            seconds,
            hour,
            minute,
            days,
        },
    )
}

fn cron_add_at(
    path: &std::path::Path,
    now: i64,
    draft: CronDraft,
) -> Result<serde_json::Value, String> {
    let CronDraft {
        session_id,
        canvas,
        prompt,
        schedule_kind,
        seconds,
        hour,
        minute,
        days,
    } = draft;
    let iso = |secs: i64| chrono_free_iso(secs);
    let (schedule, next_run) = match schedule_kind.as_str() {
        "every" => {
            let s = seconds.unwrap_or(3600);
            validate_repeat_seconds(s)?;
            (
                serde_json::json!({"kind": "every", "seconds": s}),
                iso(checked_cron_deadline(now, s, "repeat")?),
            )
        }
        "daily" => {
            let (h, m) = (hour.unwrap_or(9), minute.unwrap_or(0));
            if h > 23 || m > 59 {
                return Err("daily hour must be 0..=23 and minute must be 0..=59".to_string());
            }
            (
                serde_json::json!({"kind": "daily", "hour": h, "minute": m}),
                iso(next_local_cron_time(now, h, m, None)?),
            )
        }
        "weekly" => {
            let (h, m) = (hour.unwrap_or(9), minute.unwrap_or(0));
            if h > 23 || m > 59 {
                return Err("weekly hour must be 0..=23 and minute must be 0..=59".to_string());
            }
            let d = days.unwrap_or_else(|| vec![1]);
            if d.is_empty() || d.iter().any(|day| *day > 6) {
                return Err("weekly days must be a non-empty list in 0..=6".to_string());
            }
            (
                serde_json::json!({"kind": "weekly", "hour": h, "minute": m, "days": d}),
                iso(next_local_cron_time(now, h, m, Some(&d))?),
            )
        }
        "once" => {
            // "once" = fire in N seconds ("in N minutes" is once with seconds=N*60).
            let s = seconds.unwrap_or(60);
            validate_once_seconds(s)?;
            (
                serde_json::json!({"kind": "once"}),
                iso(checked_cron_deadline(now, s, "one-shot")?),
            )
        }
        other => return Err(format!("unknown cron schedule kind `{other}`")),
    };
    let entry = serde_json::json!({
        "id": format!("cron-{}", &uuid_simple()[..8]),
        "session_id": session_id,
        "canvas": canvas,
        "prompt": prompt,
        "schedule": schedule,
        "next_run": next_run,
        "enabled": true,
        "created_at": iso(now),
    });
    mutate_crons_at(path, |entries| {
        entries.push(entry.clone());
        Ok(entry)
    })
}

#[tauri::command]
fn cron_remove(id: String) -> Result<(), String> {
    cron_remove_at(&crons_path(), &id)
}

fn cron_remove_at(path: &std::path::Path, id: &str) -> Result<(), String> {
    mutate_crons_at(path, |entries| {
        let matches: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                entry
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|entry_id| entry_id.starts_with(id))
            })
            .map(|(index, _)| index)
            .collect();
        match matches.as_slice() {
            [] => Err(format!("no cron with id {id}")),
            [index] => {
                entries.remove(*index);
                Ok(())
            }
            _ => Err(format!("id {id} is ambiguous — give more characters")),
        }
    })
}

#[tauri::command]
fn cron_set_enabled(id: String, enabled: bool) -> Result<serde_json::Value, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_secs();
    let now = i64::try_from(now)
        .map_err(|_| "current time exceeds the supported cron date range".to_string())?;
    cron_set_enabled_at(&crons_path(), &id, enabled, now)
}

fn cron_set_enabled_at(
    path: &std::path::Path,
    id: &str,
    enabled: bool,
    now: i64,
) -> Result<serde_json::Value, String> {
    mutate_crons_at(path, |entries| {
        let matches: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                entry
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|entry_id| entry_id.starts_with(id))
            })
            .map(|(index, _)| index)
            .collect();
        let index = match matches.as_slice() {
            [] => return Err(format!("no cron with id {id}")),
            [index] => *index,
            _ => return Err(format!("id {id} is ambiguous — give more characters")),
        };
        let entry = entries[index]
            .as_object_mut()
            .ok_or_else(|| format!("cron {id} must be an object"))?;
        let was_enabled = entry
            .get("enabled")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let now_iso = chrono_free_iso(now);
        let stale = entry
            .get("next_run")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|next| next <= now_iso.as_str());
        if enabled && !was_enabled && stale {
            let schedule = entry
                .get("schedule")
                .and_then(serde_json::Value::as_object)
                .ok_or_else(|| format!("cron {id} needs a schedule object"))?;
            let kind = schedule
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| format!("cron {id} schedule needs a string kind"))?;
            let next = match kind {
                "every" => {
                    let seconds = required_cron_u64(schedule, "seconds", id)?;
                    chrono_free_iso(checked_cron_deadline(now, seconds, "resume")?)
                }
                "daily" => chrono_free_iso(next_local_cron_time(
                    now,
                    required_cron_u64(schedule, "hour", id)? as u8,
                    required_cron_u64(schedule, "minute", id)? as u8,
                    None,
                )?),
                "weekly" => {
                    let days: Vec<u8> = schedule
                        .get("days")
                        .and_then(serde_json::Value::as_array)
                        .ok_or_else(|| format!("cron {id} needs a weekday array"))?
                        .iter()
                        .filter_map(|day| day.as_u64().and_then(|day| u8::try_from(day).ok()))
                        .collect();
                    chrono_free_iso(next_local_cron_time(
                        now,
                        required_cron_u64(schedule, "hour", id)? as u8,
                        required_cron_u64(schedule, "minute", id)? as u8,
                        Some(&days),
                    )?)
                }
                "once" => return Err("completed one-time schedule cannot be resumed".to_string()),
                other => return Err(format!("unknown cron schedule kind `{other}`")),
            };
            entry.insert("next_run".to_string(), serde_json::Value::String(next));
        }
        entry.insert("enabled".to_string(), serde_json::Value::Bool(enabled));
        Ok(serde_json::Value::Object(entry.clone()))
    })
}

fn uuid_simple() -> String {
    // No uuid dep here — 16 random-ish hex chars from the OS entropy via
    // /dev/urandom (this is an id, not a secret).
    let mut buf = [0u8; 8];
    let _ = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buf));
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// RFC3339 UTC from unix seconds, no chrono dependency.
fn chrono_free_iso(secs: i64) -> String {
    // Days-from-civil inverse (Howard Hinnant's algorithm).
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    format!("{y:04}-{mth:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/* ── Vitals (~/.phoenix/VITALS.md) — always-on durable memory ──────── */

const VITALS_SECTIONS: [(&str, &str); 4] = [
    ("preference", "## Preferences"),
    ("goal", "## Goals"),
    ("avoid", "## Do NOT"),
    ("instruction", "## Always do"),
];
const VITALS_MAX_PER_SECTION: usize = 12;
const VITALS_MAX_TOTAL: usize = 36;
const VITALS_MAX_NOTE_CHARS: usize = 240;

fn vitals_path() -> PathBuf {
    phoenix_home().join("VITALS.md")
}

fn vitals_canonical_category(raw: &str) -> Option<&'static str> {
    match raw.trim().to_lowercase().as_str() {
        "preference" | "preferences" | "pref" | "like" | "likes" => Some("preference"),
        "goal" | "goals" | "objective" => Some("goal"),
        "avoid" | "do not" | "dont" | "don't" | "never" | "dislike" => Some("avoid"),
        "instruction" | "instructions" | "always" | "do" | "always do" | "rule" => {
            Some("instruction")
        }
        _ => None,
    }
}

fn vitals_section_header(category: &str) -> &'static str {
    VITALS_SECTIONS
        .iter()
        .find(|(k, _)| *k == category)
        .map(|(_, h)| *h)
        .unwrap_or("## Preferences")
}

/// Parse VITALS.md into (category, bullets). Missing/empty → empty buckets.
fn vitals_parse(raw: &str) -> Vec<(&'static str, Vec<String>)> {
    let mut buckets: Vec<(&'static str, Vec<String>)> = VITALS_SECTIONS
        .iter()
        .map(|(key, _)| (*key, Vec::new()))
        .collect();
    let mut current: Option<usize> = None;
    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(stripped) = trimmed.strip_prefix("## ") {
            current = VITALS_SECTIONS
                .iter()
                .position(|(_, header)| header[3..].eq_ignore_ascii_case(stripped));
            continue;
        }
        if let Some(idx) = current {
            if let Some(bullet) = trimmed.strip_prefix("- ") {
                let bullet = bullet.trim();
                if !bullet.is_empty() {
                    buckets[idx].1.push(bullet.to_string());
                }
            }
        }
    }
    buckets
}

fn vitals_render(buckets: &[(&'static str, Vec<String>)]) -> String {
    let mut out = String::from("# VITALS — Phoenix's vital memory\n\n");
    out.push_str(
        "> Always-on. The user's durable preferences, goals, boundaries, and standing\n\
         > instructions. Curated by the orchestrator via `vital_memory_write`.\n\n",
    );
    for (key, header) in VITALS_SECTIONS.iter() {
        out.push_str(header);
        out.push('\n');
        if let Some((_, lines)) = buckets.iter().find(|(k, _)| k == key) {
            for line in lines {
                out.push_str("- ");
                out.push_str(line);
                out.push('\n');
            }
        }
        out.push('\n');
    }
    out
}

fn vitals_stable_id(category: &str, note: &str) -> String {
    // Stable-enough id for UI delete without a separate index: category + FNV-1a of note.
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in note.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{category}-{:016x}", hash)
}

fn vitals_list_blocking() -> Result<serde_json::Value, String> {
    let path = vitals_path();
    let raw = read_private_text(&path, VITALS_MAX_BYTES, "VITALS.md")?.unwrap_or_default();
    if raw.trim().is_empty() {
        return Ok(serde_json::json!([]));
    }
    let (active, _) = vital_memory_document::split_history(&raw)?;
    let buckets = vitals_parse(active);
    let mut out = Vec::new();
    for (category, notes) in buckets {
        let section = vitals_section_header(category);
        for note in notes {
            out.push(serde_json::json!({
                "id": vitals_stable_id(category, &note),
                "category": category,
                "note": note,
                "section": section,
            }));
        }
    }
    Ok(serde_json::Value::Array(out))
}

#[tauri::command]
async fn vitals_list() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(vitals_list_blocking)
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
fn vitals_write(note: String, category: String) -> Result<serde_json::Value, String> {
    vitals_write_at(&vitals_path(), note, category)
}

fn vitals_write_at(
    path: &std::path::Path,
    note: String,
    category: String,
) -> Result<serde_json::Value, String> {
    let note = note.trim().to_string();
    if note.is_empty() {
        return Err("vitals note must be non-empty".into());
    }
    if note.contains(['\n', '\r']) {
        return Err("vitals notes must be one line".into());
    }
    if note.chars().count() > VITALS_MAX_NOTE_CHARS {
        return Err(format!(
            "vitals notes must be ≤{VITALS_MAX_NOTE_CHARS} chars (got {})",
            note.chars().count()
        ));
    }
    let category = vitals_canonical_category(&category).ok_or_else(|| {
        format!("unknown category `{category}` — use preference|goal|avoid|instruction")
    })?;
    with_private_file_lock(path, || {
        let raw = match read_private_bounded_unlocked(path, VITALS_MAX_BYTES, "VITALS.md")? {
            Some(bytes) => String::from_utf8(bytes).map_err(|error| {
                format!("VITALS.md is not UTF-8; refusing to erase it: {error}")
            })?,
            None => String::new(),
        };
        let (active, revisions) = vital_memory_document::split_history(&raw)?;
        let mut buckets = vitals_parse(active);
        // Caps + dedupe before taking a mutable bucket borrow (avoids E0502).
        let section_len = buckets
            .iter()
            .find(|(k, _)| *k == category)
            .map(|(_, lines)| lines.len())
            .unwrap_or(0);
        let already = buckets
            .iter()
            .find(|(k, _)| *k == category)
            .map(|(_, lines)| lines.iter().any(|line| line.eq_ignore_ascii_case(&note)))
            .unwrap_or(false);
        if already {
            return Ok(serde_json::json!({
                "id": vitals_stable_id(category, &note),
                "category": category,
                "note": note,
                "section": vitals_section_header(category),
                "deduped": true,
            }));
        }
        if section_len >= VITALS_MAX_PER_SECTION {
            return Err(format!(
                "`{category}` section is full ({VITALS_MAX_PER_SECTION} max) — delete or consolidate first"
            ));
        }
        let total: usize = buckets.iter().map(|(_, lines)| lines.len()).sum();
        if total >= VITALS_MAX_TOTAL {
            return Err(format!(
                "VITALS.md is at capacity ({VITALS_MAX_TOTAL} entries) — delete stale ones first"
            ));
        }
        let bucket = buckets
            .iter_mut()
            .find(|(key, _)| *key == category)
            .expect("category bucket exists");
        bucket.1.push(note.clone());
        let rendered = vital_memory_document::append_history(&vitals_render(&buckets), &revisions)?;
        write_private_atomic_unlocked(path, rendered.as_bytes())?;
        Ok(serde_json::json!({
            "id": vitals_stable_id(category, &note),
            "category": category,
            "note": note,
            "section": vitals_section_header(category),
            "deduped": false,
        }))
    })
}

#[tauri::command]
fn vitals_delete(
    id: Option<String>,
    category: Option<String>,
    note: Option<String>,
) -> Result<serde_json::Value, String> {
    vitals_delete_at(&vitals_path(), id, category, note)
}

fn vitals_delete_at(
    path: &std::path::Path,
    id: Option<String>,
    category: Option<String>,
    note: Option<String>,
) -> Result<serde_json::Value, String> {
    with_private_file_lock(path, || {
        let raw = match read_private_bounded_unlocked(path, VITALS_MAX_BYTES, "VITALS.md")? {
            Some(bytes) => String::from_utf8(bytes).map_err(|error| {
                format!("VITALS.md is not UTF-8; refusing to erase it: {error}")
            })?,
            None => String::new(),
        };
        if raw.trim().is_empty() {
            return Ok(serde_json::json!({ "removed": 0 }));
        }
        let (active, revisions) = vital_memory_document::split_history(&raw)?;
        let mut buckets = vitals_parse(active);
        let mut removed = 0usize;

        if let Some(id) = id
            .as_ref()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
        {
            for (category, lines) in buckets.iter_mut() {
                let before = lines.len();
                lines.retain(|entry| vitals_stable_id(category, entry) != id);
                removed += before - lines.len();
            }
        } else if let (Some(category), Some(note)) = (category.as_ref(), note.as_ref()) {
            let category = vitals_canonical_category(category)
                .ok_or_else(|| format!("unknown category `{category}`"))?;
            let note = note.trim();
            if let Some((_, lines)) = buckets.iter_mut().find(|(key, _)| *key == category) {
                let before = lines.len();
                lines.retain(|entry| !entry.eq_ignore_ascii_case(note));
                removed += before - lines.len();
            }
        } else {
            return Err("vitals_delete needs `id` or both `category` + `note`".into());
        }

        if removed > 0 {
            let rendered = vital_memory_document::append_history(&vitals_render(&buckets), &revisions)?;
            write_private_atomic_unlocked(path, rendered.as_bytes())?;
        }
        Ok(serde_json::json!({ "removed": removed }))
    })
}

/* ── Memory (CLI bridge + local notes; no phoenix_agent link) ──────── */

/// The memory ROOT (the tiered store lives under it: HOT/WARM/COLD/…).
fn memory_notes_dir() -> PathBuf {
    phoenix_home().join("memory")
}

fn memory_cli_available() -> bool {
    let mut command = std::process::Command::new(phoenix_binary());
    command.arg("--help");
    run_command_bounded(
        &mut command,
        None,
        PHOENIX_CLI_TIMEOUT,
        "phoenix CLI availability probe",
    )
    .map(|output| output.status.success())
    .unwrap_or(false)
}

fn run_phoenix_memory(args: &[&str]) -> Result<String, String> {
    let bin = phoenix_binary();
    let mut command = std::process::Command::new(&bin);
    command.arg("memory").args(args);
    let output = run_command_bounded(
        &mut command,
        None,
        PHOENIX_CLI_TIMEOUT,
        "phoenix memory CLI",
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if output.status.success() {
        Ok(if stdout.is_empty() { stderr } else { stdout })
    } else {
        Err(if stderr.is_empty() {
            if stdout.is_empty() {
                format!("phoenix memory failed (exit {})", output.status)
            } else {
                stdout
            }
        } else {
            stderr
        })
    }
}

/// Every memory note on disk. The librarian writes into the TIERED store —
/// `memory/{HOT,WARM,COLD}/{environment,projects}/*.md` (plus knowledge/ and
/// curation/) — NOT the flat `memory/notes` folder this used to read. Reading
/// only that one (empty) folder is why the dashboard said "nothing indexed"
/// while the agent was happily writing memories. Walk the whole tree instead.
fn list_note_files_at(root: &std::path::Path) -> Result<Vec<PathBuf>, String> {
    fn walk(
        root: &std::path::Path,
        dir: &std::path::Path,
        out: &mut Vec<PathBuf>,
        depth: usize,
    ) -> Result<(), String> {
        if depth > 4 {
            return Err(format!(
                "Phoenix note tree below {} exceeds the supported depth",
                root.display()
            ));
        }
        if !dir.starts_with(root) {
            return Err(format!(
                "refusing Phoenix note directory outside {}: {}",
                root.display(),
                dir.display()
            ));
        }
        let entries = std::fs::read_dir(dir).map_err(|error| {
            format!("could not list Phoenix notes in {}: {error}", dir.display())
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                format!(
                    "could not read a Phoenix note entry in {}: {error}",
                    dir.display()
                )
            })?;
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue; // .index.sqlite, .access.json
            }
            let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
                format!(
                    "could not inspect Phoenix note entry {}: {error}",
                    path.display()
                )
            })?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                walk(root, &path, out, depth + 1)?;
            } else if metadata.is_file()
                && path
                    .extension()
                    .and_then(|x| x.to_str())
                    .map(|x| x.eq_ignore_ascii_case("md"))
                    .unwrap_or(false)
            {
                if out.len() >= MEMORY_NOTE_MAX_FILES {
                    return Err(format!(
                        "Phoenix note tree below {} exceeds the {MEMORY_NOTE_MAX_FILES}-file limit",
                        root.display()
                    ));
                }
                out.push(path);
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    let metadata = match std::fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(files),
        Err(error) => {
            return Err(format!(
                "could not inspect Phoenix note root {}: {error}",
                root.display()
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "Phoenix note root {} is not a real directory",
            root.display()
        ));
    }
    walk(root, root, &mut files, 0)?;
    files.sort();
    Ok(files)
}

fn list_note_files() -> Result<Vec<PathBuf>, String> {
    list_note_files_at(&memory_notes_dir())
}

fn validate_note_path_at(root: &std::path::Path, path: &std::path::Path) -> Result<(), String> {
    use std::path::Component;

    let relative = path.strip_prefix(root).map_err(|_| {
        format!(
            "refusing Phoenix note path outside {}: {}",
            root.display(),
            path.display()
        )
    })?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(format!("invalid Phoenix note path {}", path.display()));
    }
    let root_metadata = std::fs::symlink_metadata(root)
        .map_err(|error| format!("could not inspect note root {}: {error}", root.display()))?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(format!(
            "Phoenix note root {} is not a real directory",
            root.display()
        ));
    }

    let components: Vec<_> = relative.components().collect();
    let mut cursor = root.to_path_buf();
    for (index, component) in components.iter().enumerate() {
        cursor.push(component.as_os_str());
        let metadata = std::fs::symlink_metadata(&cursor)
            .map_err(|error| format!("could not inspect note {}: {error}", cursor.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "refusing symlinked Phoenix note path {}",
                cursor.display()
            ));
        }
        if index + 1 == components.len() {
            if !metadata.is_file()
                || cursor
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_none_or(|extension| !extension.eq_ignore_ascii_case("md"))
            {
                return Err(format!(
                    "Phoenix note {} is not a Markdown file",
                    cursor.display()
                ));
            }
        } else if !metadata.is_dir() {
            return Err(format!(
                "Phoenix note parent {} is not a directory",
                cursor.display()
            ));
        }
    }
    Ok(())
}

fn read_note_file_at(root: &std::path::Path, path: &std::path::Path) -> Result<String, String> {
    validate_note_path_at(root, path)?;
    let bytes = read_private_bounded_unlocked(path, MEMORY_NOTE_MAX_BYTES, "Phoenix note")?
        .ok_or_else(|| format!("Phoenix note {} disappeared", path.display()))?;
    String::from_utf8(bytes)
        .map_err(|error| format!("Phoenix note {} is not UTF-8: {error}", path.display()))
}

fn remove_note_file_at(root: &std::path::Path, path: &std::path::Path) -> Result<(), String> {
    validate_note_path_at(root, path)?;
    std::fs::remove_file(path)
        .map_err(|error| format!("could not delete Phoenix note {}: {error}", path.display()))
}

fn memory_status_blocking() -> Result<serde_json::Value, String> {
    let vitals = vitals_path();
    let notes_dir = memory_notes_dir();
    let cognee = phoenix_home().join("cognee");
    let vitals_count = if vitals.exists() {
        vitals_list_blocking()?
            .as_array()
            .map(|a| a.len())
            .unwrap_or(0)
    } else {
        0
    };
    let notes = list_note_files()?;
    let notes_count = notes.len();
    let cli = memory_cli_available();
    // Graph is "available" if CLI works or we can build a partial graph from disk.
    let graph_available = cli || vitals_count > 0 || notes_count > 0;
    let mut disk_bytes: u64 = 0;
    if vitals.exists() {
        disk_bytes += std::fs::metadata(&vitals).map(|m| m.len()).unwrap_or(0);
    }
    for p in &notes {
        disk_bytes += std::fs::symlink_metadata(p)
            .ok()
            .filter(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
            .map(|metadata| metadata.len())
            .unwrap_or(0);
    }
    if cognee.exists() {
        // Cheap: only top-level cognee.db size if present (not a full tree walk).
        let db = cognee.join("cognee.db");
        disk_bytes += std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0);
    }
    Ok(serde_json::json!({
        "vitals_path": vitals.display().to_string(),
        "vitals_count": vitals_count,
        "notes_dir": notes_dir.display().to_string(),
        "notes_count": notes_count,
        "cognee_root_exists": cognee.exists(),
        "cli_available": cli,
        "graph_available": graph_available,
        "disk_bytes": disk_bytes,
        "phoenix_binary": phoenix_binary().display().to_string(),
    }))
}

#[tauri::command]
async fn memory_status() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(memory_status_blocking)
        .await
        .map_err(|error| error.to_string())?
}

fn memory_search_blocking(query: String, top_k: Option<u32>) -> Result<serde_json::Value, String> {
    let query = query.trim().to_string();
    let top_k = top_k.unwrap_or(8).clamp(1, 50) as usize;
    if query.is_empty() {
        return Ok(serde_json::json!({
            "results": [],
            "source": "empty",
            "error": "query must be non-empty",
        }));
    }

    // Prefer CLI when available.
    Ok(match run_phoenix_memory(&[&query]) {
        Ok(stdout) => {
            let mut results: Vec<String> = stdout
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            // Also scan local notes for substring hits (honest extra surface).
            let q_lower = query.to_lowercase();
            let notes_root = memory_notes_dir();
            for path in list_note_files_at(&notes_root)? {
                let body = read_note_file_at(&notes_root, &path)?;
                if body.to_lowercase().contains(&q_lower) {
                    let label = format!(
                        "[note] {} — {}",
                        path.file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("note.md"),
                        body.lines().next().unwrap_or("").trim()
                    );
                    if !results.iter().any(|r| r == &label) {
                        results.push(label);
                    }
                }
            }
            results.truncate(top_k);
            serde_json::json!({
                "results": results,
                "source": "cli",
            })
        }
        Err(err) => {
            // Fallback: local notes + vitals substring search (never invent).
            let q_lower = query.to_lowercase();
            let mut results: Vec<String> = Vec::new();
            let notes_root = memory_notes_dir();
            for path in list_note_files_at(&notes_root)? {
                let body = read_note_file_at(&notes_root, &path)?;
                if body.to_lowercase().contains(&q_lower) {
                    results.push(format!(
                        "[note] {} — {}",
                        path.file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("note.md"),
                        body.lines().next().unwrap_or("").trim()
                    ));
                }
            }
            let vitals = vitals_list_blocking()?;
            if let Some(arr) = vitals.as_array() {
                for item in arr {
                    let note = item.get("note").and_then(|v| v.as_str()).unwrap_or("");
                    let cat = item.get("category").and_then(|v| v.as_str()).unwrap_or("");
                    if note.to_lowercase().contains(&q_lower) {
                        results.push(format!("[vitals:{cat}] {note}"));
                    }
                }
            }
            results.truncate(top_k);
            let source = if results.is_empty() { "empty" } else { "local" };
            serde_json::json!({
                "results": results,
                "source": source,
                "error": err,
            })
        }
    })
}

#[tauri::command]
async fn memory_search(query: String, top_k: Option<u32>) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || memory_search_blocking(query, top_k))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
fn memory_add(text: String) -> Result<serde_json::Value, String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("memory text must be non-empty".into());
    }
    if text.len() > MEMORY_NOTE_MAX_BYTES / 2 {
        return Err(format!(
            "Phoenix note exceeds the {}-byte input limit",
            MEMORY_NOTE_MAX_BYTES / 2
        ));
    }
    // This is explicitly a local Phoenix note. Runtime semantic recall remains
    // index-owned and there is no public typed remember command to bridge yet.
    let dir = memory_notes_dir().join("HOT").join("projects");
    secure_private_directory(&dir)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let now_i64 = i64::try_from(now)
        .map_err(|_| "current time exceeds the supported note date range".to_string())?;
    let id = format!("note-{}-{}", now, &uuid_simple()[..6]);
    let path = dir.join(format!("{id}.md"));
    let body = format!(
        "# Phoenix note\n\n- id: {id}\n- created: {}\n\n{text}\n",
        chrono_free_iso(now_i64)
    );
    write_private_atomic_unlocked(&path, body.as_bytes())?;

    Ok(serde_json::json!({
        "id": id,
        "path": path.display().to_string(),
        "stored": "local_phoenix_note",
        "cli_attempted": false,
        "cli_ok": false,
        "agent_recall": false,
        "note": "wrote a durable local Phoenix note; semantic agent recall is unchanged because no typed remember command is exposed",
    }))
}

#[tauri::command]
fn memory_delete(
    id: Option<String>,
    path: Option<String>,
    kind: Option<String>,
) -> Result<serde_json::Value, String> {
    let kind = kind.unwrap_or_else(|| "auto".into()).to_lowercase();
    // Vitals path: id looks like "preference-..." or kind=vitals
    if kind == "vitals" || id.as_ref().map(|s| s.contains('-')).unwrap_or(false) {
        if let Some(vid) = id.clone() {
            // Prefer vitals_delete by id when it matches a vitals entry.
            let list = vitals_list_blocking()?;
            if list
                .as_array()
                .map(|a| {
                    a.iter()
                        .any(|v| v.get("id").and_then(|x| x.as_str()) == Some(vid.as_str()))
                })
                .unwrap_or(false)
            {
                let r = vitals_delete(Some(vid), None, None)?;
                return Ok(serde_json::json!({
                    "deleted": "vitals",
                    "result": r,
                }));
            }
        }
    }

    // Local note file by id or path.
    if let Some(p) = path.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let pb = PathBuf::from(p);
        // Only real Markdown files below the non-symlinked memory root are
        // Phoenix-owned notes. Never canonicalize through a user-controlled
        // symlink and then unlink its external target.
        let notes = memory_notes_dir();
        remove_note_file_at(&notes, &pb)?;
        return Ok(serde_json::json!({
            "deleted": "note",
            "path": pb.display().to_string(),
        }));
    }

    if let Some(nid) = id.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        // Match note file by stem id.
        for file in list_note_files()? {
            if file.file_stem().and_then(|s| s.to_str()) == Some(nid)
                || file
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.starts_with(nid))
                    .unwrap_or(false)
            {
                remove_note_file_at(&memory_notes_dir(), &file)?;
                return Ok(serde_json::json!({
                    "deleted": "note",
                    "path": file.display().to_string(),
                    "id": nid,
                }));
            }
        }
        // Last chance: vitals id
        let r = vitals_delete(Some(nid.to_string()), None, None)?;
        if r.get("removed").and_then(|v| v.as_u64()).unwrap_or(0) > 0 {
            return Ok(serde_json::json!({ "deleted": "vitals", "result": r }));
        }
        return Err(format!(
            "nothing owned by Phoenix matched id `{nid}` — graph-only items cannot be deleted yet (no forget command)"
        ));
    }

    Err("memory_delete needs `id` or `path`".into())
}

fn memory_graph_blocking() -> Result<serde_json::Value, String> {
    // Layered graph (2026-07-16 user spec: "I don't want to see any entity or
    // entity types … by default I want to see vitals, and notes"). The graph
    // now ALWAYS carries the human-owned layer — vitals + notes + goals, each
    // node tagged with a `kind` — and appends the Cognee entity cloud as an
    // OPT-IN layer behind the canvas filter chips. The canvas filters by
    // `kind`; default visible set = {vital, note}.
    let mut nodes: Vec<serde_json::Value> = Vec::new();
    let mut edges: Vec<serde_json::Value> = Vec::new();

    let vitals = vitals_list_blocking()?;
    if let Some(arr) = vitals.as_array() {
        for item in arr {
            let id = item
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("vitals-unknown");
            let label = item
                .get("note")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let group = item
                .get("category")
                .and_then(|v| v.as_str())
                .unwrap_or("vitals");
            nodes.push(serde_json::json!({
                "id": id,
                "label": label.clone(),
                "text": label,
                "group": group,
                "kind": "vital",
            }));
        }
    }
    let notes_root = memory_notes_dir();
    for path in list_note_files_at(&notes_root)? {
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("note")
            .to_string();
        // A real, human title — skip YAML frontmatter (--- fences + `key: val`
        // lines) and markdown headings.
        let body = read_note_file_at(&notes_root, &path)?;
        let mut in_frontmatter = false;
        let label = body
            .lines()
            .find(|raw| {
                let l = raw.trim();
                if l == "---" {
                    in_frontmatter = !in_frontmatter;
                    return false;
                }
                if in_frontmatter || l.is_empty() || l.starts_with('#') {
                    return false;
                }
                !(l.contains(':') && !l.contains(' ') && l.len() < 40)
            })
            .map(|l| {
                l.trim()
                    .trim_start_matches(['-', '*', '>', ' '])
                    .to_string()
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| id.replace(['-', '_'], " "));
        let full = body
            .splitn(3, "---")
            .last()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| body.trim().to_string());
        let created = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        nodes.push(serde_json::json!({
            "id": id,
            "label": label,
            "text": full,
            "path": path.to_string_lossy(),
            "created": created,
            "group": "note",
            "kind": "note",
        }));
    }
    let local_count = nodes.len();

    // Cognee entity layer: the gateway exports the knowledge graph to
    // cognee/graph.json after every cognify/memify (the ladybug store itself
    // is single-writer and locked by the gateway). Appended kind-tagged so the
    // canvas can hide it — which it does by default.
    let export = phoenix_home().join("cognee").join("graph.json");
    let mut cognee_total = 0usize;
    if let Some(raw) = read_private_text(&export, GRAPH_EXPORT_MAX_BYTES, "Phoenix graph export")? {
        let mut data = serde_json::from_str::<serde_json::Value>(&raw).map_err(|error| {
            format!(
                "Phoenix graph export {} is invalid JSON: {error}",
                export.display()
            )
        })?;
        let n_nodes = data
            .get("nodes")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        let n_edges = data
            .get("edges")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        // The full graph is thousands of nodes; the canvas force-sim is
        // O(n²) per frame, so render the most-connected subgraph. The rest
        // stays searchable via memory_search (which asks Cognee itself).
        const MAX_RENDER_NODES: usize = 400;
        if n_nodes > MAX_RENDER_NODES {
            use std::collections::{HashMap, HashSet};
            let mut degree: HashMap<String, usize> = HashMap::new();
            if let Some(edges) = data.get("edges").and_then(|v| v.as_array()) {
                for e in edges {
                    for key in ["source", "target"] {
                        if let Some(id) = e.get(key).and_then(|v| v.as_str()) {
                            *degree.entry(id.to_string()).or_insert(0) += 1;
                        }
                    }
                }
            }
            if let Some(nodes) = data.get_mut("nodes").and_then(|v| v.as_array_mut()) {
                nodes.sort_by_key(|n| {
                    std::cmp::Reverse(
                        n.get("id")
                            .and_then(|v| v.as_str())
                            .and_then(|id| degree.get(id).copied())
                            .unwrap_or(0),
                    )
                });
                nodes.truncate(MAX_RENDER_NODES);
            }
            let kept: HashSet<String> = data
                .get("nodes")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|n| n.get("id").and_then(|v| v.as_str()).map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            if let Some(edges) = data.get_mut("edges").and_then(|v| v.as_array_mut()) {
                edges.retain(|e| {
                    e.get("source")
                        .and_then(|v| v.as_str())
                        .is_some_and(|s| kept.contains(s))
                        && e.get("target")
                            .and_then(|v| v.as_str())
                            .is_some_and(|t| kept.contains(t))
                });
            }
        }
        cognee_total = n_nodes;
        let _ = n_edges;
        // Append the capped subgraph, ensuring every node carries a kind
        // (the export uses `group`/`kind` = Entity/EntityType/DocumentChunk/
        // TextSummary/TextDocument) — the canvas filter keys off it.
        if let Some(arr) = data.get_mut("nodes").and_then(|v| v.as_array_mut()) {
            for n in arr.drain(..) {
                let mut n = n;
                if n.get("kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .is_empty()
                {
                    let k = n
                        .get("group")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Entity")
                        .to_string();
                    n["kind"] = serde_json::Value::String(k);
                }
                nodes.push(n);
            }
        }
        if let Some(arr) = data.get_mut("edges").and_then(|v| v.as_array_mut()) {
            edges.append(arr);
        }
    }

    let (node_count, edge_count) = (nodes.len(), edges.len());
    Ok(serde_json::json!({
        "nodes": nodes,
        "edges": edges,
        "meta": {
            "source": if cognee_total > 0 { "local+phoenix-index" } else { "local" },
            "node_count": node_count,
            "edge_count": edge_count,
            "local_nodes": local_count,
            "cognee_total": cognee_total,
            "note": "vitals + notes + goals (default layer) with the semantic entity cloud appended kind-tagged; Phoenix's filter chips choose what renders",
        },
    }))
}

#[tauri::command]
async fn memory_graph() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(memory_graph_blocking)
        .await
        .map_err(|error| error.to_string())?
}

/* ── Full config editing (raw TOML, validated, backed up) ──────────── */

#[tauri::command]
async fn config_read() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let path = phoenix_home().join("config.toml");
        read_private_text_required(&path, PRIVATE_DOCUMENT_MAX_BYTES, "config.toml")
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Memory maintenance status — did the background jobs actually run? Parses
/// gateway.log for the memify / cognify / LLM-gardening events + cognee db size,
/// so the user can SEE whether indexing happened and whether the 12h LLM prune
/// (which needs a working model) ran or was skipped (e.g. Grok 403).
fn memory_maintenance_blocking() -> Result<serde_json::Value, String> {
    let log_path = phoenix_home().join("gateway.log");
    let log = read_private_text(&log_path, GATEWAY_LOG_READ_MAX_BYTES, "gateway log")?
        .unwrap_or_default();
    // last line (with its bracketed timestamp) matching any of `needles`.
    let last = |needles: &[&str]| -> Option<(String, String)> {
        log.lines()
            .rev()
            .find(|l| needles.iter().any(|n| l.contains(n)))
            .map(|l| {
                let time = l
                    .split(']')
                    .next()
                    .and_then(|s| s.strip_prefix('['))
                    .unwrap_or("")
                    .to_string();
                let body = l.splitn(2, "] ").nth(1).unwrap_or(l).trim().to_string();
                (time, body)
            })
    };
    let cell = |v: Option<(String, String)>| match v {
        Some((t, b)) => serde_json::json!({ "at": t, "detail": b }),
        None => serde_json::Value::Null,
    };
    let db = phoenix_home().join("cognee").join("cognee.db");
    let db_bytes = std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0);
    // Was the LLM gardening skipped for lack of a model?
    let gardening_skipped = last(&[
        "management skipped",
        "reflector pass FAILED",
        "memify.*skipped",
    ]);
    Ok(serde_json::json!({
        "memify": cell(last(&["memify maintenance done", "cognee memify → completed"])),
        "cognify": cell(last(&["index — completed", "digest index", "cognify — completed", "digest cognify"])),
        "gardening_skipped": cell(gardening_skipped),
        "cognee_db_bytes": db_bytes,
        "note": "Phoenix indexing works headless; the 12h LLM gardening/prune needs a working model — if it is skipped, stale memories are not pruned.",
    }))
}

#[tauri::command]
async fn memory_maintenance() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(memory_maintenance_blocking)
        .await
        .map_err(|error| error.to_string())?
}

/// Provider brand logos the user drops into ~/.phoenix/logos/ — one image per
/// provider, named `<provider-id>.png|jpg|svg|webp` (e.g. xai.png, deepseek.png).
/// Returned as {stem: data-url} so the UI shows the real logo, no rebuild
/// needed when a new one is added — just drop it and refresh.
fn provider_logos_blocking() -> serde_json::Value {
    let dir = phoenix_home().join("logos");
    let mut out = serde_json::Map::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten().take(PROVIDER_LOGO_MAX_FILES) {
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if stem.is_empty()
                || stem.len() > 64
                || !stem
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
            {
                continue;
            }
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            let mime = match ext.as_str() {
                "png" => "image/png",
                "jpg" | "jpeg" => "image/jpeg",
                "svg" => "image/svg+xml",
                "webp" => "image/webp",
                "gif" => "image/gif",
                _ => continue,
            };
            if let Ok(Some(bytes)) =
                read_private_bounded_unlocked(&path, PROVIDER_LOGO_MAX_BYTES, "provider logo")
            {
                let valid = match ext.as_str() {
                    "png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
                    "jpg" | "jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
                    "webp" => {
                        bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP"
                    }
                    "gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
                    "svg" => std::str::from_utf8(&bytes).is_ok_and(|text| {
                        let lower = text.to_ascii_lowercase();
                        lower.contains("<svg")
                            && ![
                                "<script",
                                "javascript:",
                                "onload=",
                                "onerror=",
                                "<foreignobject",
                            ]
                            .iter()
                            .any(|needle| lower.contains(needle))
                    }),
                    _ => false,
                };
                if !valid {
                    continue;
                }
                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                out.insert(
                    stem.to_lowercase(),
                    serde_json::Value::String(format!("data:{mime};base64,{b64}")),
                );
            }
        }
    }
    serde_json::Value::Object(out)
}

#[tauri::command]
async fn provider_logos() -> serde_json::Value {
    tauri::async_runtime::spawn_blocking(provider_logos_blocking)
        .await
        .expect("provider-logo worker panicked")
}

/// Real files the agents produced: browser captures, image attachments, and
/// canvas wallpapers. Newest first. Powers the Artifacts page — no mock rows.
fn artifacts_list_blocking() -> serde_json::Value {
    use std::os::unix::fs::MetadataExt;

    const MAX_ARTIFACT_ENTRIES_PER_SOURCE: usize = 4_096;
    let home = phoenix_home();
    let sources: [(&str, PathBuf); 3] = [
        ("browser", home.join("browser").join("artifacts")),
        ("attachment", home.join("attachments")),
        ("wallpaper", home.join("wallpapers")),
    ];
    let mut items: Vec<serde_json::Value> = Vec::new();
    for (kind, dir) in sources {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.take(MAX_ARTIFACT_ENTRIES_PER_SOURCE).flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_file() {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.nlink() != 1 {
                continue;
            }
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            items.push(serde_json::json!({
                "name": name,
                "path": path.to_string_lossy(),
                "kind": kind,
                "ext": ext,
                "bytes": meta.len(),
                "modified": modified,
                "modified_iso": chrono_free_iso(modified),
            }));
        }
    }
    items.sort_by_key(|item| -(item["modified"].as_i64().unwrap_or(0)));
    items.truncate(200);
    serde_json::json!({ "items": items })
}

#[tauri::command]
async fn artifacts_list() -> serde_json::Value {
    tauri::async_runtime::spawn_blocking(artifacts_list_blocking)
        .await
        .expect("artifact-list worker panicked")
}

#[tauri::command]
fn config_write(content: String, expected_content: Option<String>) -> Result<String, String> {
    config_write_at(
        &phoenix_home().join("config.toml"),
        &content,
        expected_content.as_deref(),
    )
}

fn config_write_at(
    path: &std::path::Path,
    content: &str,
    expected_content: Option<&str>,
) -> Result<String, String> {
    // Refuse anything that doesn't parse — a typo must never brick the
    // gateway's next boot.
    content
        .parse::<toml::Value>()
        .map_err(|error| format!("not valid TOML — nothing saved: {error}"))?;
    // A caller that did not read a revision cannot safely replace an existing
    // document. Keeping this argument optional lets older clients receive an
    // actionable conflict instead of failing command deserialization, while
    // every successful write is a real compare-and-swap.
    let expected = expected_content.ok_or_else(|| {
        "config.toml save needs the content that was loaded; reload and retry".to_string()
    })?;
    let backup = replace_private_atomic(
        path,
        Some(expected.as_bytes()),
        content.as_bytes(),
        true,
        PRIVATE_DOCUMENT_MAX_BYTES,
        "config.toml",
    )?;
    Ok(match backup {
        Some(backup) => format!(
            "saved — previous config kept at {}",
            backup
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("backup")
        ),
        None => "saved".to_string(),
    })
}

/* ── Model lanes & fallback chains (dashboard config editor) ───────── */

/// Everything the dashboard's model editor needs in one read: each role's
/// fallback chain from `[profile.llm.fallback]`, and every auth profile id
/// available to drag into a chain (from auth-profiles.json).
fn llm_chains_blocking() -> Result<serde_json::Value, String> {
    let config_path = phoenix_home().join("config.toml");
    let config =
        read_private_text_required(&config_path, PRIVATE_DOCUMENT_MAX_BYTES, "config.toml")?;
    let doc: toml::Value = config
        .parse()
        .map_err(|error| format!("config.toml unreadable: {error}"))?;

    // Read an ordered string list from a toml value.
    let list = |value: Option<&toml::Value>| -> Vec<String> {
        value
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };

    let llm = doc.get("profile").and_then(|p| p.get("llm"));
    let fallback = llm
        .and_then(|l| l.get("fallback"))
        .and_then(|f| f.as_table());

    // Flat role chains, keyed by role name (excludes the nested `agents` table).
    let mut roles = serde_json::Map::new();
    let mut agents = serde_json::Map::new();
    if let Some(table) = fallback {
        for (role, value) in table {
            if role == "agents" {
                if let Some(agent_table) = value.as_table() {
                    for (agent, chain) in agent_table {
                        agents.insert(agent.clone(), serde_json::json!(list(Some(chain))));
                    }
                }
                continue;
            }
            roles.insert(role.clone(), serde_json::json!(list(Some(value))));
        }
    }

    // [profile.llm.auth_by_lane] — WHICH account is each lane's primary, and
    // [profile.llm.agent_models] — each specialist's own primary model. Both
    // are flat `key → string` tables; the dashboard needs them to draw
    // position 0 of a chain as a fact rather than a guess.
    let flat_str_table =
        |value: Option<&toml::Value>| -> serde_json::Map<String, serde_json::Value> {
            let mut out = serde_json::Map::new();
            if let Some(table) = value.and_then(|v| v.as_table()) {
                for (key, item) in table {
                    if let Some(text) = item.as_str().map(str::trim).filter(|t| !t.is_empty()) {
                        out.insert(key.clone(), serde_json::json!(text));
                    }
                }
            }
            out
        };
    let pins = flat_str_table(llm.and_then(|l| l.get("auth_by_lane")));
    let agent_models = flat_str_table(llm.and_then(|l| l.get("agent_models")));
    let efforts = flat_str_table(llm.and_then(|l| l.get("efforts")));

    // Web capability chains — [profile.web_fallback].
    let web_fallback = doc
        .get("profile")
        .and_then(|p| p.get("web_fallback"))
        .and_then(|f| f.as_table());
    let mut web = serde_json::Map::new();
    if let Some(table) = web_fallback {
        for (cap, value) in table {
            web.insert(cap.clone(), serde_json::json!(list(Some(value))));
        }
    }

    // Every stored auth profile as {id, provider, method} so the editor can
    // show "grok-cli:default · OAuth" and colour-dot by provider. The store
    // keeps profiles as a MAP of id → credential.
    let auth_path = phoenix_home().join("auth-profiles.json");
    let store =
        match read_private_text(&auth_path, PRIVATE_DOCUMENT_MAX_BYTES, "auth profile store")? {
            Some(raw) => serde_json::from_str::<serde_json::Value>(&raw)
                .map_err(|error| format!("auth profile store is invalid: {error}"))?,
            None => serde_json::json!({}),
        };
    let mut profiles: Vec<serde_json::Value> = store
        .get("profiles")
        .and_then(|p| p.as_object())
        .map(|map| {
            map.iter()
                .map(|(id, cred)| {
                    let provider = cred
                        .get("provider")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let method = cred
                        .get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("api")
                        .to_string();
                    serde_json::json!({ "id": id, "provider": provider, "method": method })
                })
                .collect()
        })
        .unwrap_or_default();
    profiles.sort_by(|a, b| {
        a["id"]
            .as_str()
            .unwrap_or("")
            .cmp(b["id"].as_str().unwrap_or(""))
    });

    // Per-lane model assignments (auth-profiles.json): profile id → lane →
    // {model, effort?}. This is what a fallback LINK actually runs — the UI
    // shows/edits it next to each chain chip. `models` is the legacy
    // single-model pin (assignments win when the lane is present).
    let assignments = store
        .get("assignments")
        .cloned()
        .unwrap_or(serde_json::json!({}));
    let legacy_models = store
        .get("models")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    Ok(serde_json::json!({
        "roles": roles,
        "agents": agents,
        "web": web,
        "profiles": profiles,
        "assignments": assignments,
        "legacy_models": legacy_models,
        "pins": pins,
        "agent_models": agent_models,
        "efforts": efforts,
    }))
}

#[tauri::command]
async fn llm_chains() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(llm_chains_blocking)
        .await
        .map_err(|error| error.to_string())?
}

/// Set (or clear, with an empty model) which MODEL a stored account runs when
/// it serves `lane` in a fallback chain — writes auth-profiles.json
/// `assignments[profile][lane] = {model}` (the shape `assignment_for` in the
/// runtime reads: exact lane → "specialist" umbrella → legacy pin). This is
/// the missing half of the fallback editor: chains chose ACCOUNTS but the
/// model each account would run was invisible and uneditable.
#[tauri::command]
fn assignment_set(
    profile_id: String,
    lane: String,
    model: String,
    effort: Option<String>,
) -> Result<String, String> {
    if lane.is_empty() || lane.contains(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
        return Err(format!("invalid lane: {lane}"));
    }
    let path = phoenix_home().join("auth-profiles.json");
    let raw = read_private_text_required(&path, PRIVATE_DOCUMENT_MAX_BYTES, "auth profile store")?;
    let mut doc: serde_json::Value =
        serde_json::from_str(&raw).map_err(|error| format!("auth store unreadable: {error}"))?;
    if !doc
        .get("profiles")
        .and_then(|p| p.get(&profile_id))
        .is_some()
    {
        return Err(format!("unknown profile: {profile_id}"));
    }
    let assignments = doc
        .as_object_mut()
        .ok_or("auth store is not an object")?
        .entry("assignments")
        .or_insert(serde_json::json!({}));
    let per_profile = assignments
        .as_object_mut()
        .ok_or("assignments is not an object")?
        .entry(profile_id.clone())
        .or_insert(serde_json::json!({}));
    let lanes = per_profile
        .as_object_mut()
        .ok_or("profile assignments is not an object")?;
    let model = model.trim().to_string();
    if model.is_empty() {
        lanes.remove(&lane);
    } else {
        let mut entry = serde_json::json!({ "model": model });
        if let Some(e) = effort
            .as_deref()
            .map(str::trim)
            .filter(|e| !e.is_empty() && *e != "default")
        {
            entry["effort"] = serde_json::json!(e);
        }
        lanes.insert(lane.clone(), entry);
    }
    let mut rendered = serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?;
    rendered.push(b'\n');
    replace_private_atomic(
        &path,
        Some(raw.as_bytes()),
        &rendered,
        true,
        PRIVATE_DOCUMENT_MAX_BYTES,
        "auth profile store",
    )?;
    Ok(if model.is_empty() {
        format!("{profile_id} · {lane} → auto (provider default)")
    } else {
        format!("{profile_id} · {lane} → {model}")
    })
}

/// `[profile.llm.agent_models].<agent>` — the PRIMARY model for one individual
/// specialist (Spark, Scout, Iris…). The config key existed and the runtime read
/// it (`LLMProfile::agent_model`), but nothing could write it: "pick the model
/// as the main model" for a roster agent had nowhere to go. An empty model
/// clears the key, so the agent inherits `specialist_model` again.
#[tauri::command]
fn agent_model_set(agent: String, model: String, effort: Option<String>) -> Result<String, String> {
    if agent.is_empty() || agent.contains(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
        return Err(format!("invalid agent: {agent}"));
    }
    let path = phoenix_home().join("config.toml");
    let raw = read_private_text_required(&path, PRIVATE_DOCUMENT_MAX_BYTES, "config.toml")?;
    let mut doc: toml_edit::DocumentMut = raw
        .parse()
        .map_err(|error| format!("config.toml unreadable: {error}"))?;
    let model = model.trim().to_string();
    if model.is_empty() {
        if let Some(table) = doc["profile"]["llm"]["agent_models"].as_table_mut() {
            table.remove(&agent);
        }
    } else {
        doc["profile"]["llm"]["agent_models"][agent.as_str()] = toml_edit::value(model.as_str());
    }
    // `[profile.llm.efforts]` is keyed by role OR agent name (types.rs), so the
    // same call can set the agent's reasoning effort. `None` leaves it alone;
    // "default"/"" clears it back to the global.
    match effort.as_deref().map(str::trim) {
        Some(e) if !e.is_empty() && e != "default" => {
            doc["profile"]["llm"]["efforts"][agent.as_str()] = toml_edit::value(e);
        }
        Some(_) => {
            if let Some(table) = doc["profile"]["llm"]["efforts"].as_table_mut() {
                table.remove(&agent);
            }
        }
        None => {}
    }
    backup_and_write_config(&path, &raw, doc.to_string())?;
    Ok(format!(
        "{agent} → {} — restart the gateway to apply",
        if model.is_empty() {
            "inherits the specialists' model".to_string()
        } else {
            model
        }
    ))
}

/// `[profile.llm.auth_by_lane].<key>` — pin WHICH stored account is a lane's
/// PRIMARY. `scope` is "role" or "agent" (both live in the same flat table:
/// role names and agent names never collide, and the runtime reads it that
/// way). `profile_id: None` clears the pin, so resolution falls back to the
/// env var and then the provider's own accounts.
///
/// Why this exists: `lane_set` writes a PROVIDER, never an account. With five
/// NVIDIA NIM accounts the runtime picked the alphabetically first one — so
/// "make nvidia:6 the main" was unexpressible.
#[tauri::command]
fn lane_pin_set(scope: String, key: String, profile_id: Option<String>) -> Result<String, String> {
    if !matches!(scope.as_str(), "role" | "agent") {
        return Err(format!(
            "lane pinning applies to role and agent lanes, not '{scope}'"
        ));
    }
    if key.is_empty() || key.contains(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
        return Err(format!("invalid lane key: {key}"));
    }
    let profile_id = profile_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty());
    if let Some(id) = profile_id {
        // Ids are `provider:slot`; anything else would resolve to nothing.
        if !id.contains(':')
            || id.contains(|c: char| {
                !c.is_ascii_alphanumeric() && c != ':' && c != '.' && c != '-' && c != '_'
            })
        {
            return Err(format!("not an auth-profile id: {id}"));
        }
    }
    let path = phoenix_home().join("config.toml");
    let raw = read_private_text_required(&path, PRIVATE_DOCUMENT_MAX_BYTES, "config.toml")?;
    let mut doc: toml_edit::DocumentMut = raw
        .parse()
        .map_err(|error| format!("config.toml unreadable: {error}"))?;
    match profile_id {
        Some(id) => doc["profile"]["llm"]["auth_by_lane"][key.as_str()] = toml_edit::value(id),
        None => {
            if let Some(table) = doc["profile"]["llm"]["auth_by_lane"].as_table_mut() {
                table.remove(&key);
            }
        }
    }
    backup_and_write_config(&path, &raw, doc.to_string())?;
    Ok(match profile_id {
        Some(id) => format!("{key} runs on {id} — restart the gateway to apply"),
        None => format!("{key} account pin cleared — restart the gateway to apply"),
    })
}

/// Composio's MCP key is a raw file (`~/.phoenix/composio-mcp.key`), NOT an auth
/// profile — `auth_set_key` validates the provider against the LLM catalog or a
/// `search:|crawl:|scrape:` web id and rejects `composio` outright.
#[tauri::command]
fn composio_key_set(key: String) -> Result<String, String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("paste your Composio MCP key first".into());
    }
    if !key.starts_with("ck_") {
        return Err("a Composio MCP key starts with `ck_`".into());
    }
    let dir = phoenix_home();
    let path = dir.join("composio-mcp.key");
    replace_private_atomic(
        &path,
        None,
        format!("{key}\n").as_bytes(),
        false,
        PRIVATE_SECRET_MAX_BYTES,
        "Composio key",
    )?;
    Ok("Composio key saved".into())
}

/// Whether a Composio key is reachable, and from where. There is no
/// `auth_probe` for Composio (it is not an LLM lane), so the Add-profile
/// wizard verifies with this instead of faking a live model call.
#[tauri::command]
fn composio_status() -> Result<serde_json::Value, String> {
    let file = phoenix_home().join("composio-mcp.key");
    let from_file = read_private_text(&file, PRIVATE_SECRET_MAX_BYTES, "Composio key")?
        .map(|raw| raw.trim().to_string())
        .filter(|key| !key.is_empty());
    let from_env = std::env::var("COMPOSIO_MCP_KEY")
        .ok()
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty());
    let key = from_file.as_ref().or(from_env.as_ref());
    let source = if from_file.is_some() {
        serde_json::json!("file")
    } else if from_env.is_some() {
        serde_json::json!("env")
    } else {
        serde_json::Value::Null
    };
    Ok(serde_json::json!({
        "configured": key.is_some(),
        "source": source,
        // Never the key itself — just enough to recognise which one is stored.
        "hint": key.map(|k| format!("{}…", k.chars().take(10).collect::<String>())),
        "path": file.to_string_lossy(),
    }))
}

/// Persist one role's reordered/edited fallback chain (compat shim over
/// `fallback_set` for the legacy config-tab editor).
#[tauri::command]
fn llm_chain_set(role: String, chain: Vec<String>) -> Result<String, String> {
    fallback_set("role".to_string(), role, chain)
}

/// Persist any fallback chain — role, per-agent override, or web capability —
/// to the right config.toml path. toml_edit keeps the user's comments and
/// formatting; a timestamped .bak is written first. An empty chain removes the
/// override. `scope`: "role" → [profile.llm.fallback][key]; "agent" →
/// [profile.llm.fallback.agents][key]; "web" → [profile.web_fallback][key].
#[tauri::command]
fn fallback_set(scope: String, key: String, chain: Vec<String>) -> Result<String, String> {
    if key.is_empty() || key.contains(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
        return Err(format!("invalid key name: {key}"));
    }
    let path = phoenix_home().join("config.toml");
    let raw = read_private_text_required(&path, PRIVATE_DOCUMENT_MAX_BYTES, "config.toml")?;
    let mut doc: toml_edit::DocumentMut = raw
        .parse()
        .map_err(|error| format!("config.toml unreadable: {error}"))?;
    let mut array = toml_edit::Array::new();
    for profile in &chain {
        array.push(profile.as_str());
    }
    // The dotted table path this scope's chain lives under, and the parent
    // table to prune from when the chain is emptied.
    let (parent_path, label): (&[&str], &str) = match scope.as_str() {
        "role" => (&["profile", "llm", "fallback"], "lane"),
        "agent" => (&["profile", "llm", "fallback", "agents"], "agent"),
        "web" => (&["profile", "web_fallback"], "web"),
        other => return Err(format!("unknown fallback scope '{other}'")),
    };
    if chain.is_empty() {
        // Walk to the parent table WITHOUT auto-vivifying, then remove the key
        // if the whole path exists. `doc` derefs to the root Table.
        let mut table: Option<&mut toml_edit::Table> = Some(doc.as_table_mut());
        for seg in parent_path {
            table = table
                .and_then(|t| t.get_mut(seg))
                .and_then(|i| i.as_table_mut());
            if table.is_none() {
                break;
            }
        }
        if let Some(t) = table {
            t.remove(&key);
        }
    } else {
        // IndexMut auto-vivifies missing tables along the path.
        let mut node = &mut doc[parent_path[0]];
        for seg in &parent_path[1..] {
            node = &mut node[*seg];
        }
        node[key.as_str()] = toml_edit::value(array);
    }
    backup_and_write_config(&path, &raw, doc.to_string())?;
    Ok(format!(
        "{label} order saved — restart the gateway to apply"
    ))
}

/* ── Headless config bridge (dashboard → phoenix CLI) ──────────────
All mutations shell out to the phoenix binary's headless commands so
config logic lives in ONE place; this file only ferries JSON. */

#[derive(Debug)]
struct BoundedChildOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn set_pipe_nonblocking(file: &impl std::os::fd::AsRawFd) -> Result<(), String> {
    let fd = file.as_raw_fd();
    // SAFETY: fd belongs to a live ChildStdout/ChildStderr for this call.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(format!(
            "could not make child output nonblocking: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn drain_child_pipe(
    pipe: &mut impl std::io::Read,
    output: &mut Vec<u8>,
    label: &str,
) -> Result<(), String> {
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) => return Ok(()),
            Ok(read) => {
                if output.len().saturating_add(read) > PHOENIX_CLI_OUTPUT_MAX_BYTES {
                    return Err(format!(
                        "{label} exceeded the {PHOENIX_CLI_OUTPUT_MAX_BYTES}-byte output limit"
                    ));
                }
                output.extend_from_slice(&chunk[..read]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) => return Err(format!("could not read {label}: {error}")),
        }
    }
}

fn child_exited_without_reap(child: &std::process::Child) -> Result<bool, String> {
    let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
    // SAFETY: info is valid writable storage. WNOWAIT observes the direct
    // child without consuming its status, keeping its PGID reserved while we
    // terminate and verify any same-group descendants.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id() as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result < 0 {
        return Err(format!(
            "could not inspect child status: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(info.si_signo == libc::SIGCHLD)
}

fn live_child_group_members(pgid: i32) -> Result<Vec<i32>, String> {
    let mut members = Vec::new();
    let entries = std::fs::read_dir("/proc")
        .map_err(|error| format!("could not scan /proc for child cleanup: {error}"))?;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("could not scan /proc entry: {error}")),
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
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("could not read process status: {error}")),
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

fn signal_child_group(pgid: i32, signal: i32) -> Result<(), String> {
    if pgid <= 1 || pgid == unsafe { libc::getpgrp() } {
        return Err(format!("refusing unsafe child process group {pgid}"));
    }
    // The unreaped direct child reserves this pid/PGID, proving the group is
    // ours rather than an unrelated group that reused the numeric id.
    if unsafe { libc::getpgid(pgid) } != pgid {
        return Err(format!("could not verify child process group {pgid}"));
    }
    if unsafe { libc::kill(-pgid, signal) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(format!(
            "could not signal child process group {pgid}: {error}"
        ))
    }
}

fn terminate_child_group(
    child: &mut std::process::Child,
) -> Result<(std::process::ExitStatus, usize), String> {
    let pgid = child.id() as i32;
    let initial = live_child_group_members(pgid)?;
    let descendants = initial.iter().filter(|pid| **pid != pgid).count();
    if !initial.is_empty() {
        signal_child_group(pgid, libc::SIGTERM)?;
        let term_deadline = std::time::Instant::now() + std::time::Duration::from_millis(750);
        while std::time::Instant::now() < term_deadline {
            if live_child_group_members(pgid)?.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if !live_child_group_members(pgid)?.is_empty() {
            signal_child_group(pgid, libc::SIGKILL)?;
            let kill_deadline = std::time::Instant::now() + std::time::Duration::from_millis(750);
            while std::time::Instant::now() < kill_deadline {
                if live_child_group_members(pgid)?.is_empty() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    }
    let remaining = live_child_group_members(pgid)?;
    if !remaining.is_empty() {
        return Err(format!(
            "child process-group termination was not confirmed; live pids: {}",
            remaining
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let status = child
        .wait()
        .map_err(|error| format!("could not reap child process: {error}"))?;
    Ok((status, descendants))
}

fn run_command_bounded(
    command: &mut std::process::Command,
    stdin: Option<&[u8]>,
    timeout: std::time::Duration,
    label: &str,
) -> Result<BoundedChildOutput, String> {
    run_command_bounded_observed(command, stdin, timeout, label, |_| {})
}

/// `std::process::Child` does not wait in Drop. Keep every bounded helper
/// cancellation-safe so a renderer reload, panic, or early `?` cannot leave a
/// finished Git/gh/Phoenix child as a permanent zombie under the desktop.
struct BoundedChildGuard(Option<std::process::Child>);

impl BoundedChildGuard {
    fn new(child: std::process::Child) -> Self {
        Self(Some(child))
    }
}

impl std::ops::Deref for BoundedChildGuard {
    type Target = std::process::Child;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref().expect("bounded child is present")
    }
}

impl std::ops::DerefMut for BoundedChildGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut().expect("bounded child is present")
    }
}

impl Drop for BoundedChildGuard {
    fn drop(&mut self) {
        let Some(mut child) = self.0.take() else {
            return;
        };
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        let _ = terminate_child_group(&mut child);
    }
}

/// Bounded child runner with a non-secret stdout observer. The observer sees
/// the complete stdout collected so far after every new chunk; provider login
/// uses it to surface a device verification code while the token itself stays
/// inside the child and private credential store.
fn run_command_bounded_observed<F>(
    command: &mut std::process::Command,
    stdin: Option<&[u8]>,
    timeout: std::time::Duration,
    label: &str,
    mut observe_stdout: F,
) -> Result<BoundedChildOutput, String>
where
    F: FnMut(&[u8]),
{
    use std::io::Write;

    if stdin.is_some_and(|input| input.len() > PHOENIX_CLI_STDIN_MAX_BYTES) {
        return Err(format!(
            "{label} input exceeds the {PHOENIX_CLI_STDIN_MAX_BYTES}-byte limit"
        ));
    }
    command
        .stdin(if stdin.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0);
    let child = command
        .spawn()
        .map_err(|error| format!("could not start {label}: {error}"))?;
    let mut child = BoundedChildGuard::new(child);
    if let Some(input) = stdin {
        let write_result = child
            .stdin
            .take()
            .ok_or_else(|| format!("{label} has no stdin"))?
            .write_all(input);
        if let Err(error) = write_result {
            let _ = terminate_child_group(&mut child);
            return Err(format!("could not send input to {label}: {error}"));
        }
    }
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("{label} has no stdout"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("{label} has no stderr"))?;
    if let Err(error) = set_pipe_nonblocking(&stdout).and_then(|()| set_pipe_nonblocking(&stderr)) {
        let _ = terminate_child_group(&mut child);
        return Err(format!("{label}: {error}"));
    }
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    let mut observed_stdout_len = 0;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Err(error) = drain_child_pipe(&mut stdout, &mut stdout_bytes, "child stdout")
            .and_then(|()| drain_child_pipe(&mut stderr, &mut stderr_bytes, "child stderr"))
        {
            let _ = terminate_child_group(&mut child);
            return Err(format!("{label}: {error}"));
        }
        if stdout_bytes.len() != observed_stdout_len {
            observe_stdout(&stdout_bytes);
            observed_stdout_len = stdout_bytes.len();
        }
        match child_exited_without_reap(&child) {
            Ok(true) => {
                if let Err(error) = drain_child_pipe(&mut stdout, &mut stdout_bytes, "child stdout")
                    .and_then(|()| drain_child_pipe(&mut stderr, &mut stderr_bytes, "child stderr"))
                {
                    let cleanup = terminate_child_group(&mut child);
                    return Err(match cleanup {
                        Ok(_) => format!("{label}: {error}"),
                        Err(cleanup) => {
                            format!("{label}: {error}; cleanup was not confirmed: {cleanup}")
                        }
                    });
                }
                if stdout_bytes.len() != observed_stdout_len {
                    observe_stdout(&stdout_bytes);
                }
                let (status, descendants) = terminate_child_group(&mut child)?;
                if descendants > 0 {
                    return Err(format!(
                        "{label} exited but left {descendants} non-detached background process(es); they were terminated"
                    ));
                }
                return Ok(BoundedChildOutput {
                    status,
                    stdout: stdout_bytes,
                    stderr: stderr_bytes,
                });
            }
            Ok(false) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(15));
            }
            Ok(false) => {
                terminate_child_group(&mut child)?;
                return Err(format!(
                    "{label} timed out after {}s and was terminated",
                    timeout.as_secs()
                ));
            }
            Err(error) => {
                let cleanup = terminate_child_group(&mut child);
                return Err(match cleanup {
                    Ok(_) => format!("could not wait for {label}: {error}"),
                    Err(cleanup) => format!(
                        "could not wait for {label}: {error}; cleanup was not confirmed: {cleanup}"
                    ),
                });
            }
        }
    }
}

fn run_phoenix(args: &[&str]) -> Result<String, String> {
    let mut command = std::process::Command::new(phoenix_binary());
    command.args(args);
    let output = run_command_bounded(&mut command, None, PHOENIX_CLI_TIMEOUT, "phoenix CLI")?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if output.status.success() {
        Ok(stdout)
    } else {
        Err(if stderr.is_empty() { stdout } else { stderr })
    }
}

/// The provider catalog + configured auth summaries + web provider catalog,
/// straight from `phoenix providers --json`.
#[tauri::command]
async fn providers_catalog() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let raw = run_phoenix(&["providers", "--json"])?;
        serde_json::from_str(&raw).map_err(|error| format!("bad catalog json: {error}"))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Store an API key auth profile. The key rides the child's stdin — it never
/// appears in argv or logs.
#[tauri::command]
async fn auth_set_key(profile_id: String, provider: String, key: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
    let key = Zeroizing::new(key);
    let input = Zeroizing::new(format!("{}\n", key.trim()));
    let mut command = std::process::Command::new(phoenix_binary());
    command.args(["auth", "set-key", &profile_id, &provider]);
    let out = run_command_bounded(
        &mut command,
        Some(input.as_bytes()),
        PHOENIX_CLI_TIMEOUT,
        "phoenix auth set-key",
    )?;
    if out.status.success() {
        Ok(format!("{profile_id} saved"))
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }

    }).await.map_err(|error| error.to_string())?
}

/// Create the first config from the native onboarding UI. This is deliberately
/// create-only: an existing five-month Phoenix configuration is never
/// replaced by onboarding, even if two windows race to finish setup.
#[tauri::command]
async fn provider_initialize_config(
    provider: String,
    model: String,
    auth_method: String,
    profile_id: Option<String>,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let provider = provider.trim();
        let model = model.trim();
        let auth_method = auth_method.trim();
        if provider.is_empty() || provider.len() > 128 {
            return Err("provider id is missing or too long".to_string());
        }
        if model.is_empty() || model.len() > 512 {
            return Err("model id is missing or too long".to_string());
        }
        if !matches!(auth_method, "api" | "oauth" | "token" | "none") {
            return Err(format!("unsupported authentication method `{auth_method}`"));
        }

        let raw_catalog = run_phoenix(&["providers", "--json"])?;
        let catalog: serde_json::Value = serde_json::from_str(&raw_catalog)
            .map_err(|error| format!("bad provider catalog json: {error}"))?;
        let selected = catalog
            .get("providers")
            .and_then(serde_json::Value::as_array)
            .and_then(|providers| providers.iter().find(|entry| entry["id"] == provider))
            .ok_or_else(|| format!("unknown provider `{provider}`"))?;
        let declared_method_supported = selected
            .get("auth_methods")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|methods| {
                methods.iter().any(|method| {
                    method["type"] == auth_method
                        && method["llm_supported"].as_bool().unwrap_or(true)
                })
            });
        let profile_id = profile_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if auth_method == "none" {
            if profile_id.is_some() {
                return Err("a no-auth provider cannot pin an account".to_string());
            }
            if !declared_method_supported {
                return Err(format!(
                    "provider `{provider}` does not support no-auth model use"
                ));
            }
        } else {
            let profile_id = profile_id.ok_or_else(|| "an account is required".to_string())?;
            if profile_id.len() > 128
                || !profile_id.starts_with(&format!("{provider}:"))
                || profile_id.contains(|ch: char| {
                    !ch.is_ascii_alphanumeric() && !matches!(ch, ':' | '.' | '-' | '_')
                })
            {
                return Err(format!("invalid account id `{profile_id}`"));
            }
            let stored_profile = selected
                .get("profiles")
                .and_then(serde_json::Value::as_array)
                .and_then(|profiles| profiles.iter().find(|entry| entry["id"] == profile_id));
            let Some(stored_profile) = stored_profile else {
                return Err(format!("provider account `{profile_id}` was not stored"));
            };
            let stored_method = stored_profile
                .get("method")
                .or_else(|| stored_profile.get("type"))
                .and_then(serde_json::Value::as_str)
                .map(|method| match method {
                    "device_code" | "cli_token" | "token" => "token",
                    "oauth" => "oauth",
                    _ => "api",
                });
            if stored_method.is_some_and(|method| method != auth_method) {
                return Err(format!(
                    "provider account `{profile_id}` uses `{}` rather than `{auth_method}`",
                    stored_method.unwrap_or("stored")
                ));
            }
            // A successfully stored device-code/CLI token is authoritative
            // even when the catalog calls its setup flow `device_code`
            // instead of the runtime config vocabulary `token`.
            if !declared_method_supported && stored_method.is_none() {
                return Err(format!(
                    "provider `{provider}` does not support `{auth_method}` for model use"
                ));
            }
        }

        let contents = build_initial_provider_config(provider, model, auth_method, profile_id)?;
        let path = phoenix_home().join("config.toml");
        with_private_file_lock(&path, || {
            if read_private_bounded_unlocked(&path, PRIVATE_DOCUMENT_MAX_BYTES, "config.toml")?
                .is_some()
            {
                return Err(format!(
                    "{} already exists; reload provider settings instead of replacing it",
                    path.display()
                ));
            }
            write_private_atomic_unlocked(&path, contents.as_bytes())
        })?;
        Ok(format!("{provider} · {model} configured"))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Render the create-only bootstrap document without hand-built user values.
/// Keeping this pure makes escaping and account pinning independently
/// testable; the caller is responsible for catalog/account validation.
fn build_initial_provider_config(
    provider: &str,
    model: &str,
    auth_method: &str,
    profile_id: Option<&str>,
) -> Result<String, String> {
    let mut document = "# Phoenix configuration\n\
[global]\n\
log_level = \"info\"\n\
\n\
[profile]\n\
name = \"default\"\n\
\n\
[profile.llm]\n\
provider = \"placeholder\"\n\
model = \"placeholder\"\n\
timeout_seconds = 0\n\
temperature = 0.0\n\
\n\
[profile.llm.auth]\n\
method = \"none\"\n\
source = \"none\"\n"
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| error.to_string())?;
    document["profile"]["llm"]["provider"] = toml_edit::value(provider);
    document["profile"]["llm"]["model"] = toml_edit::value(model);
    document["profile"]["llm"]["auth"]["method"] = toml_edit::value(auth_method);
    document["profile"]["llm"]["auth"]["source"] = toml_edit::value(if auth_method == "none" {
        "none"
    } else {
        "profile"
    });
    if let Some(profile_id) = profile_id {
        document["profile"]["llm"]["auth"]["profile"] = toml_edit::value(profile_id);
    }
    Ok(document.to_string())
}

fn validate_prompt_overlay_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(
            "prompt overlay name must contain lowercase letters, numbers, or underscores".into(),
        );
    }
    Ok(name)
}

fn prompt_overlay_revision(bytes: Option<&[u8]>) -> String {
    use sha2::{Digest, Sha256};
    match bytes {
        Some(bytes) => format!("{:x}", Sha256::digest(bytes)),
        None => "missing".to_string(),
    }
}

fn prompt_overlay_path(name: &str) -> Result<PathBuf, String> {
    Ok(phoenix_home()
        .join("prompts")
        .join(format!("{}.md", validate_prompt_overlay_name(name)?)))
}

fn prompt_overlay_read_at(path: &std::path::Path) -> Result<serde_json::Value, String> {
    let bytes = read_private_bounded_unlocked(path, PROMPT_OVERLAY_MAX_BYTES, "prompt overlay")?;
    let content = bytes
        .as_deref()
        .map(|bytes| {
            String::from_utf8(bytes.to_vec())
                .map_err(|error| format!("prompt overlay is not UTF-8: {error}"))
        })
        .transpose()?
        .unwrap_or_default();
    Ok(serde_json::json!({
        "exists": bytes.is_some(),
        "bytes": bytes.as_ref().map(Vec::len).unwrap_or(0),
        "revision": prompt_overlay_revision(bytes.as_deref()),
        "content": content,
    }))
}

#[tauri::command]
fn prompt_overlays_list() -> Result<serde_json::Value, String> {
    let mut names = [
        "orchestrator_system",
        "scribe_system",
        "planner_system",
        "finance_system",
        "coder_system",
        "frontend_system",
        "researcher_system",
        "presentation_system",
        "critic_system",
        "sales_system",
        "marketing_system",
        "personal_logistics_system",
        "librarian_system",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<std::collections::BTreeSet<_>>();
    let root = phoenix_home().join("prompts");
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_file() || file_type.is_symlink() {
                continue;
            }
            let Some(stem) = entry
                .path()
                .file_stem()
                .and_then(|value| value.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            if validate_prompt_overlay_name(&stem).is_ok() {
                names.insert(stem);
            }
        }
    }
    let rows = names
        .into_iter()
        .map(|name| {
            let path = prompt_overlay_path(&name)?;
            let mut row = prompt_overlay_read_at(&path)?;
            row["name"] = serde_json::json!(name);
            Ok(row)
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(serde_json::json!(rows))
}

#[tauri::command]
fn prompt_overlay_read(name: String) -> Result<serde_json::Value, String> {
    let path = prompt_overlay_path(&name)?;
    with_private_file_lock(&path, || {
        let mut row = prompt_overlay_read_at(&path)?;
        row["name"] = serde_json::json!(validate_prompt_overlay_name(&name)?);
        Ok(row)
    })
}

#[tauri::command]
fn prompt_overlay_write(
    name: String,
    content: String,
    expected_revision: String,
) -> Result<serde_json::Value, String> {
    let path = prompt_overlay_path(&name)?;
    if content.trim().is_empty() {
        return Err(
            "an empty overlay would silently fall back to the built-in prompt; use Reset instead"
                .into(),
        );
    }
    if content.len() > PROMPT_OVERLAY_MAX_BYTES {
        return Err("prompt overlay exceeds the 4 MiB limit".into());
    }
    let mut row = prompt_overlay_write_at(&path, &content, &expected_revision)?;
    row["name"] = serde_json::json!(validate_prompt_overlay_name(&name)?);
    Ok(row)
}

fn prompt_overlay_write_at(
    path: &std::path::Path,
    content: &str,
    expected_revision: &str,
) -> Result<serde_json::Value, String> {
    with_private_file_lock(path, || {
        let current =
            read_private_bounded_unlocked(&path, PROMPT_OVERLAY_MAX_BYTES, "prompt overlay")?;
        if prompt_overlay_revision(current.as_deref()) != expected_revision {
            return Err("prompt overlay changed in another process; reload before saving".into());
        }
        if let Some(bytes) = current.as_deref() {
            let backup = private_backup_path(&path)?;
            write_private_atomic_unlocked(&backup, bytes)?;
        }
        write_private_atomic_unlocked(path, content.as_bytes())?;
        prompt_overlay_read_at(path)
    })
}

#[tauri::command]
fn prompt_overlay_reset(name: String, expected_revision: String) -> Result<(), String> {
    let path = prompt_overlay_path(&name)?;
    prompt_overlay_reset_at(&path, &expected_revision)
}

fn prompt_overlay_reset_at(path: &std::path::Path, expected_revision: &str) -> Result<(), String> {
    with_private_file_lock(path, || {
        let current =
            read_private_bounded_unlocked(&path, PROMPT_OVERLAY_MAX_BYTES, "prompt overlay")?;
        if prompt_overlay_revision(current.as_deref()) != expected_revision {
            return Err(
                "prompt overlay changed in another process; reload before resetting".into(),
            );
        }
        let Some(bytes) = current else { return Ok(()) };
        let backup = private_backup_path(&path)?;
        write_private_atomic_unlocked(&backup, &bytes)?;
        std::fs::remove_file(path)
            .map_err(|error| format!("could not reset {}: {error}", path.display()))
    })
}

#[derive(Default, serde::Serialize)]
struct DataSnapshotStats {
    files: u64,
    directories: u64,
    bytes: u64,
    skipped_links: u64,
    skipped_special: u64,
}

fn copy_snapshot_tree(
    source: &std::path::Path,
    destination: &std::path::Path,
    depth: usize,
    stats: &mut DataSnapshotStats,
) -> Result<(), String> {
    if depth > 96 {
        return Err(format!(
            "snapshot directory depth exceeds 96 at {}",
            source.display()
        ));
    }
    secure_private_directory(destination)?;
    stats.directories = stats.directories.saturating_add(1);
    let mut entries = std::fs::read_dir(source)
        .map_err(|error| format!("could not read {}: {error}", source.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("could not enumerate {}: {error}", source.display()))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name();
        if depth == 0 && name == "backups" {
            continue;
        }
        if matches!(
            name.to_str(),
            Some("gateway.sock" | "gateway.pid" | "desktop.pid")
        ) {
            stats.skipped_special = stats.skipped_special.saturating_add(1);
            continue;
        }
        let metadata = std::fs::symlink_metadata(entry.path())
            .map_err(|error| format!("could not inspect {}: {error}", entry.path().display()))?;
        let destination_path = destination.join(&name);
        if metadata.file_type().is_symlink() {
            stats.skipped_links = stats.skipped_links.saturating_add(1);
        } else if metadata.is_dir() {
            copy_snapshot_tree(&entry.path(), &destination_path, depth + 1, stats)?;
        } else if metadata.is_file() {
            #[cfg(unix)]
            use std::os::unix::fs::OpenOptionsExt;
            let mut source_options = std::fs::OpenOptions::new();
            source_options.read(true);
            #[cfg(unix)]
            source_options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
            let mut source_file = source_options.open(entry.path()).map_err(|error| {
                format!(
                    "could not open {} for snapshot: {error}",
                    entry.path().display()
                )
            })?;
            if !source_file
                .metadata()
                .map_err(|error| format!("could not inspect open snapshot source: {error}"))?
                .is_file()
            {
                stats.skipped_special = stats.skipped_special.saturating_add(1);
                continue;
            }
            let mut destination_options = std::fs::OpenOptions::new();
            destination_options.write(true).create_new(true);
            #[cfg(unix)]
            destination_options
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
            let mut destination_file =
                destination_options
                    .open(&destination_path)
                    .map_err(|error| {
                        format!("could not create {}: {error}", destination_path.display())
                    })?;
            let bytes = std::io::copy(&mut source_file, &mut destination_file)
                .map_err(|error| format!("could not copy {}: {error}", entry.path().display()))?;
            destination_file.sync_all().map_err(|error| {
                format!("could not sync {}: {error}", destination_path.display())
            })?;
            stats.files = stats.files.saturating_add(1);
            stats.bytes = stats.bytes.saturating_add(bytes);
        } else {
            stats.skipped_special = stats.skipped_special.saturating_add(1);
        }
    }
    Ok(())
}

fn data_snapshots_root() -> PathBuf {
    phoenix_home().join("backups").join("snapshots")
}

#[tauri::command]
async fn data_snapshot_create() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let home = phoenix_home();
        secure_private_directory(&home)?;
        let root = data_snapshots_root();
        secure_private_directory(&root)?;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| format!("system clock is before Unix epoch: {error}"))?
            .as_nanos();
        let name = format!("snapshot-{nonce}");
        let partial = root.join(format!(".partial-{nonce}-{}", std::process::id()));
        let final_path = root.join(&name);
        if std::fs::symlink_metadata(&final_path).is_ok() {
            return Err(format!(
                "snapshot destination already exists: {}",
                final_path.display()
            ));
        }
        secure_private_directory(&partial)?;
        let mut stats = DataSnapshotStats::default();
        let result = copy_snapshot_tree(&home, &partial, 0, &mut stats).and_then(|()| {
            let manifest = serde_json::to_vec_pretty(&serde_json::json!({
                "version": 1,
                "name": name,
                "created_unix_nanos": nonce.to_string(),
                "files": stats.files,
                "directories": stats.directories,
                "bytes": stats.bytes,
                "skipped_links": stats.skipped_links,
                "skipped_special": stats.skipped_special,
            }))
            .map_err(|error| error.to_string())?;
            write_private_atomic_unlocked(&partial.join(".snapshot.json"), &manifest)?;
            std::fs::rename(&partial, &final_path)
                .map_err(|error| format!("could not publish snapshot: {error}"))?;
            Ok(serde_json::json!({
                "name": name,
                "path": final_path,
                "files": stats.files,
                "directories": stats.directories,
                "bytes": stats.bytes,
                "skipped_links": stats.skipped_links,
                "skipped_special": stats.skipped_special,
            }))
        });
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&partial);
        }
        result
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
fn data_snapshots_list() -> Result<serde_json::Value, String> {
    let root = data_snapshots_root();
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Ok(serde_json::json!([]));
    };
    let mut rows = Vec::new();
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir()
            || file_type.is_symlink()
            || entry.file_name().to_string_lossy().starts_with('.')
        {
            continue;
        }
        let manifest = entry.path().join(".snapshot.json");
        let Some(bytes) = read_private_bounded_unlocked(&manifest, 64 * 1024, "snapshot manifest")?
        else {
            continue;
        };
        let mut row: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("bad snapshot manifest {}: {error}", manifest.display()))?;
        row["path"] = serde_json::json!(entry.path());
        rows.push(row);
    }
    rows.sort_by(|left, right| {
        right["created_unix_nanos"]
            .as_str()
            .cmp(&left["created_unix_nanos"].as_str())
    });
    Ok(serde_json::json!(rows))
}

#[tauri::command]
fn auth_provider_order(provider: String, profile_ids: Option<Vec<String>>) -> Result<Vec<String>, String> {
    let path=phoenix_home().join("auth-profiles.json");
    let raw=read_private_text_required(&path,PRIVATE_DOCUMENT_MAX_BYTES,"auth profile store")?;
    let mut doc: serde_json::Value=serde_json::from_str(&raw).map_err(|e|e.to_string())?;
    let writing=profile_ids.is_some();
    let ordered=provider_account_order(&mut doc,&provider,profile_ids)?;
    if writing {
        let rendered=serde_json::to_vec_pretty(&doc).map_err(|e|e.to_string())?;
        replace_private_atomic(&path,Some(raw.as_bytes()),&rendered,true,PRIVATE_DOCUMENT_MAX_BYTES,"auth profile store")?;
    }
    Ok(ordered)
}

fn provider_account_order(doc: &mut serde_json::Value, provider: &str, profile_ids: Option<Vec<String>>) -> Result<Vec<String>,String> {
    let profiles=doc["profiles"].as_object().ok_or("Invalid auth store")?;
    let mut available=profiles.iter().filter(|(_,v)|v["provider"].as_str()==Some(provider)).map(|(id,_)|id.clone()).collect::<Vec<_>>();
    available.sort();
    let key=format!("provider:{provider}");
    let saved=doc["state"]["order"][&key].as_array().map(|a|a.iter().filter_map(|v|v.as_str().map(str::to_string)).collect::<Vec<_>>()).unwrap_or_default();
    let mut ordered=saved.into_iter().filter(|id|available.contains(id)).collect::<Vec<_>>();
    for id in &available {if !ordered.contains(id){ordered.push(id.clone());}}
    if let Some(ids)=profile_ids {
        let mut checked=ids.clone();checked.sort();
        if checked!=available{return Err("Accounts changed. Refresh the list before reordering.".into());}
        doc["state"]["order"][&key]=serde_json::json!(ids);
        return Ok(ids);
    }
    Ok(ordered)
}

#[tauri::command]
fn auth_rename(profile_id: String, label: String) -> Result<(), String> {
    let label = label.trim();
    if label.is_empty() || label.chars().count() > 80 || label.chars().any(char::is_control) {
        return Err("Account name must contain 1–80 characters without control characters.".into());
    }
    let path = phoenix_home().join("auth-profiles.json");
    let raw = read_private_text_required(&path, PRIVATE_DOCUMENT_MAX_BYTES, "auth profile store")?;
    let mut doc: serde_json::Value = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
    if doc.get("profiles").and_then(|profiles| profiles.get(&profile_id)).is_none() {
        return Err("Account no longer exists.".into());
    }
    let labels = doc.as_object_mut().ok_or("Invalid auth store")?
        .entry("labels").or_insert_with(|| serde_json::json!({}));
    labels.as_object_mut().ok_or("Invalid account labels")?
        .insert(profile_id, serde_json::json!(label));
    let rendered = serde_json::to_vec_pretty(&doc).map_err(|error| error.to_string())?;
    replace_private_atomic(&path, Some(raw.as_bytes()), &rendered, true,
        PRIVATE_DOCUMENT_MAX_BYTES, "auth profile store").map(|_| ())
}

#[tauri::command]
async fn auth_remove(profile_id: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
    let output = run_phoenix(&["auth", "remove", &profile_id])?;
    let detail = output.trim();
    Ok(if detail.is_empty() {
        format!("{profile_id} removed")
    } else {
        detail.to_string()
    })

    }).await.map_err(|error| error.to_string())?
}

/// LIVE-probe a stored profile (real 1-token call through the real provider
/// stack). Returns the CLI's JSON verdict — including `tier_gated` for
/// subscription OAuth accounts the provider refuses API access to. Async:
/// a probe is a network round-trip.
#[tauri::command]
async fn auth_probe(
    profile_id: String,
    model: Option<String>,
    effort: Option<String>,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut args = vec!["auth", "probe", profile_id.as_str()];
        if let Some(model) = model.as_deref().filter(|value| !value.trim().is_empty()) {
            args.extend(["--model", model]);
        }
        if let Some(effort) = effort.as_deref().filter(|value| !value.trim().is_empty()) {
            args.extend(["--effort", effort]);
        }
        let raw = run_phoenix(&args)?;
        serde_json::from_str(&raw).map_err(|error| format!("bad probe json: {error}"))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Set the global reasoning effort ([profile.llm].reasoning_effort).
#[tauri::command]
fn effort_set(effort: String) -> Result<String, String> {
    const ALLOWED: &[&str] = &["minimal", "low", "medium", "high", "xhigh", "max"];
    if !ALLOWED.contains(&effort.as_str()) {
        return Err(format!("unknown effort '{effort}'"));
    }
    let path = phoenix_home().join("config.toml");
    let raw = read_private_text_required(&path, PRIVATE_DOCUMENT_MAX_BYTES, "config.toml")?;
    let mut doc: toml_edit::DocumentMut = raw
        .parse()
        .map_err(|error| format!("config.toml unreadable: {error}"))?;
    doc["profile"]["llm"]["reasoning_effort"] = toml_edit::value(effort.as_str());
    backup_and_write_config(&path, &raw, doc.to_string())?;
    Ok(format!(
        "reasoning effort → {effort} — restart the gateway to apply"
    ))
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct ProviderDeviceAuthEvent {
    provider: String,
    verification_url: String,
    user_code: String,
}

fn device_auth_event_from_stdout(provider: &str, stdout: &[u8]) -> Option<ProviderDeviceAuthEvent> {
    let text = std::str::from_utf8(stdout).ok()?;
    let marker = text
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix("DEVICE_AUTH="))?;
    let value: serde_json::Value = serde_json::from_str(marker).ok()?;
    let verification_url = value.get("verification_url")?.as_str()?.trim();
    let user_code = value.get("user_code")?.as_str()?.trim();
    if provider.is_empty()
        || provider.len() > 128
        || !provider
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || !verification_url.starts_with("https://")
        || verification_url.len() > 8 * 1024
        || verification_url
            .bytes()
            .any(|byte| byte.is_ascii_whitespace())
        || user_code.is_empty()
        || user_code.len() > 256
        || !user_code.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return None;
    }
    Some(ProviderDeviceAuthEvent {
        provider: provider.to_string(),
        verification_url: verification_url.to_string(),
        user_code: user_code.to_string(),
    })
}

/// Native provider sign-in. OAuth flows open the system browser and wait for
/// a loopback callback. Device-code flows additionally stream a one-time
/// verification code to the modal while their eventual token remains in the
/// Rust process and private auth store.
#[tauri::command]
async fn oauth_login(
    app: tauri::AppHandle,
    provider: String,
    profile_id: Option<String>,
    auth_method: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let client_secret = client_secret.map(Zeroizing::new);
        let provider = provider.trim().to_string();
        let auth_method = auth_method
            .as_deref()
            .map(str::trim)
            .filter(|method| !method.is_empty())
            .unwrap_or("oauth")
            .to_string();
        if provider.is_empty()
            || provider.len() > 128
            || !provider
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err("invalid provider id".to_string());
        }
        if !matches!(
            auth_method.as_str(),
            "oauth" | "device_code" | "device_code_cn"
        ) {
            return Err(format!("unsupported native login method `{auth_method}`"));
        }
        let client_id = client_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let client_secret = client_secret
            .as_deref()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty());
        if client_id.is_some_and(|value| value.len() > 8 * 1024 || value.contains('\0'))
            || client_secret.is_some_and(|value| value.len() > 16 * 1024 || value.contains('\0'))
        {
            return Err("OAuth client configuration is oversized or invalid".to_string());
        }
        if (client_id.is_some() || client_secret.is_some())
            && !matches!(provider.as_str(), "google-gemini-cli" | "chutes")
        {
            return Err(format!(
                "provider `{provider}` does not accept custom OAuth client configuration"
            ));
        }
        // No profile id = the CLI picks the next free slot, so a second (third…)
        // subscription of the same provider is ADDED, never overwritten.
        let mut args: Vec<String> = vec!["login".into(), provider.clone()];
        args.push("--method".into());
        args.push(auth_method.clone());
        if let Some(id) = profile_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            if id.len() > 128
                || !id.starts_with(&format!("{provider}:"))
                || id.bytes().any(|byte| {
                    !byte.is_ascii_alphanumeric() && !matches!(byte, b':' | b'.' | b'-' | b'_')
                })
            {
                return Err(format!("invalid account id `{id}`"));
            }
            args.push("--profile".into());
            args.push(id.to_string());
        }
        let mut command = std::process::Command::new(phoenix_binary());
        command.args(&args);
        match provider.as_str() {
            "google-gemini-cli" => {
                if let Some(value) = client_id {
                    command.env("GEMINI_CLI_OAUTH_CLIENT_ID", value);
                }
                if let Some(value) = client_secret {
                    command.env("GEMINI_CLI_OAUTH_CLIENT_SECRET", value);
                }
            }
            "chutes" => {
                if let Some(value) = client_id {
                    command.env("CHUTES_CLIENT_ID", value);
                }
                if let Some(value) = client_secret {
                    command.env("CHUTES_CLIENT_SECRET", value);
                }
            }
            _ => {}
        }
        let timeout = if auth_method.starts_with("device_code") {
            PHOENIX_DEVICE_LOGIN_TIMEOUT
        } else {
            PHOENIX_LOGIN_TIMEOUT
        };
        let mut emitted_device_auth = false;
        let out = run_command_bounded_observed(
            &mut command,
            None,
            timeout,
            "phoenix provider login",
            |stdout| {
                if emitted_device_auth {
                    return;
                }
                if let Some(payload) = device_auth_event_from_stdout(&provider, stdout) {
                    emitted_device_auth = true;
                    let _ = app.emit("provider-device-auth", payload);
                }
            },
        )?;
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        if out.status.success() {
            Ok(format!("{provider} connected"))
        } else {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let url = stdout
                .lines()
                .find_map(|line| line.strip_prefix("AUTH_URL="))
                .unwrap_or("")
                .to_string();
            Err(if !stderr.is_empty() {
                stderr
            } else if url.is_empty() {
                "provider login failed — see gateway-restart.log".to_string()
            } else {
                format!("login timed out — open manually: {url}")
            })
        }
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Model lanes as configured right now (config.toml), one read for the
/// dashboard: provider+model per lane, plus the web lanes' providers.
fn lanes_get_blocking() -> Result<serde_json::Value, String> {
    let path = phoenix_home().join("config.toml");
    let raw = read_private_text_required(&path, PRIVATE_DOCUMENT_MAX_BYTES, "config.toml")?;
    let doc: toml::Value = raw
        .parse()
        .map_err(|error| format!("config.toml unreadable: {error}"))?;
    let llm = doc.get("profile").and_then(|p| p.get("llm"));
    let get = |t: Option<&toml::Value>, k: &str| {
        t.and_then(|v| v.get(k))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    // Per-lane reasoning effort: [profile.llm.efforts] keyed by ROLE name.
    // Dashboard lane → effort key. Empty = inherit global.
    let efforts = llm.and_then(|l| l.get("efforts"));
    let effort_of = |key: &str| {
        efforts
            .and_then(|e| e.get(key))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let mut lanes = serde_json::Map::new();
    for (lane, pk, mk, ek) in [
        ("main", "provider", "model", "orchestrator"),
        (
            "specialist",
            "specialist_provider",
            "specialist_model",
            "specialist",
        ),
        (
            "librarian",
            "librarian_provider",
            "librarian_model",
            "librarian",
        ),
        ("memory", "memory_provider", "memory_model", ""),
        ("vision", "vision_provider", "vision_model", ""),
        ("image", "image_provider", "image_model", ""),
        ("stt", "stt_provider", "stt_model", ""),
        ("tts", "tts_provider", "tts_model", ""),
        ("realtime", "realtime_provider", "realtime_model", ""),
    ] {
        let voice = match lane {
            "tts" => get(llm, "tts_voice"),
            "realtime" => get(llm, "realtime_voice"),
            _ => String::new(),
        };
        lanes.insert(
            lane.to_string(),
            serde_json::json!({
                "provider": get(llm, pk), "model": get(llm, mk),
                "voice": voice,
                "effort": if ek.is_empty() { serde_json::Value::Null } else { serde_json::json!(effort_of(ek)) },
            }),
        );
    }
    for cap in ["search", "crawl", "scrape"] {
        let t = doc.get("profile").and_then(|p| p.get(cap));
        lanes.insert(
            format!("web_{cap}"),
            serde_json::json!({ "provider": get(t, "provider") }),
        );
    }
    Ok(serde_json::json!({
        "lanes": lanes,
        "reasoning_effort": get(llm, "reasoning_effort"),
    }))
}

#[tauri::command]
async fn lanes_get() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(lanes_get_blocking)
        .await
        .map_err(|error| error.to_string())?
}

/// Timestamped .bak, then write — shared by every config.toml mutation here.
pub(crate) fn backup_and_write_config(
    path: &std::path::Path,
    original_content: &str,
    new_content: String,
) -> Result<(), String> {
    replace_private_atomic(
        path,
        Some(original_content.as_bytes()),
        new_content.as_bytes(),
        true,
        PRIVATE_DOCUMENT_MAX_BYTES,
        "config.toml",
    )?;
    Ok(())
}

/// Point one model lane at a provider + model. Comments and formatting in
/// config.toml survive (toml_edit); a .bak is written first.
#[tauri::command]
fn lane_set(
    lane: String,
    provider: String,
    model: String,
    effort: Option<String>,
    voice: Option<String>,
) -> Result<String, String> {
    if provider.trim().is_empty() || model.trim().is_empty() {
        return Err("provider and model are both required".into());
    }
    let (section, pk, mk) = match lane.as_str() {
        "main" => ("llm", "provider", "model"),
        "specialist" => ("llm", "specialist_provider", "specialist_model"),
        "librarian" => ("llm", "librarian_provider", "librarian_model"),
        "memory" => ("llm", "memory_provider", "memory_model"),
        "vision" => ("llm", "vision_provider", "vision_model"),
        "image" => ("llm", "image_provider", "image_model"),
        "stt" => ("llm", "stt_provider", "stt_model"),
        "tts" => ("llm", "tts_provider", "tts_model"),
        "realtime" => ("llm", "realtime_provider", "realtime_model"),
        other => return Err(format!("unknown lane '{other}'")),
    };
    // Dashboard lane → [profile.llm.efforts] role keys.
    let effort_keys: &[&str] = match lane.as_str() {
        "main" => &["orchestrator"],
        "specialist" => &["specialist"],
        "librarian" => &["librarian"],
        _ => &[],
    };
    let path = phoenix_home().join("config.toml");
    let raw = read_private_text_required(&path, PRIVATE_DOCUMENT_MAX_BYTES, "config.toml")?;
    let mut doc: toml_edit::DocumentMut = raw
        .parse()
        .map_err(|error| format!("config.toml unreadable: {error}"))?;
    // Read the provider BEFORE overwriting it: the pin below must only be
    // dropped when the provider actually changed. Clearing it on every write
    // meant "set this lane's model" silently un-chose the account the user had
    // just promoted to main.
    let provider_changed = doc["profile"][section][pk]
        .as_str()
        .map(|current| current.trim() != provider.trim())
        .unwrap_or(true);
    doc["profile"][section][pk] = toml_edit::value(provider.trim());
    doc["profile"][section][mk] = toml_edit::value(model.trim());
    if let Some(voice) = voice.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        match lane.as_str() {
            "tts" => doc["profile"]["llm"]["tts_voice"] = toml_edit::value(voice),
            "realtime" => doc["profile"]["llm"]["realtime_voice"] = toml_edit::value(voice),
            _ => {}
        }
    }
    if let Some(effort) = effort.as_deref().map(str::trim) {
        for key in effort_keys {
            if effort.is_empty() || effort == "default" {
                // Inherit the global reasoning_effort again.
                if let Some(t) = doc["profile"]["llm"]["efforts"].as_table_mut() {
                    t.remove(key);
                }
            } else {
                doc["profile"]["llm"]["efforts"][key] = toml_edit::value(effort);
            }
        }
    }
    // Lane-specific auth pins go stale when the provider CHANGES — the
    // cross-provider pin disease. They are perfectly valid when it does not,
    // so a model-only edit leaves them alone.
    if provider_changed {
        if lane == "main" {
            if let Some(llm) = doc["profile"]["llm"].as_table_mut() {
                llm.remove("auth");
            }
        }
        // The per-lane account pin ([profile.llm.auth_by_lane]) names an
        // account of the OLD provider; it can only fail against the new one.
        if let Some(chain_key) = match lane.as_str() {
            "main" => Some("orchestrator"),
            "specialist" => Some("specialist"),
            "librarian" => Some("librarian"),
            "vision" => Some("vision"),
            "image" => Some("image"),
            _ => None,
        } {
            if let Some(table) = doc["profile"]["llm"]["auth_by_lane"].as_table_mut() {
                table.remove(chain_key);
            }
        }
    }
    backup_and_write_config(&path, &raw, doc.to_string())?;
    Ok(format!(
        "{lane} → {} / {}{} — restart the gateway to apply",
        provider.trim(),
        model.trim(),
        effort
            .as_deref()
            .filter(|e| !e.trim().is_empty() && *e != "default")
            .map(|e| format!(" @ {e}"))
            .unwrap_or_default()
    ))
}

/// Configure a web lane (search/crawl/scrape): provider choice, optional API
/// key (stored as an auth profile via the phoenix CLI), and the config.toml
/// auth wiring.
#[tauri::command]
async fn web_lane_set(
    capability: String,
    provider: String,
    key: Option<String>,
    env_var: Option<String>,
) -> Result<String, String> {
    if !matches!(capability.as_str(), "search" | "crawl" | "scrape") {
        return Err(format!("unknown web capability '{capability}'"));
    }
    if provider.trim().is_empty() {
        return Err("provider is required".into());
    }
    let provider = provider.trim().to_string();
    let profile_id = format!("{capability}-{provider}");
    let has_key = key
        .as_deref()
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false);
    if has_key {
        auth_set_key(
            profile_id.clone(),
            format!("{capability}:{provider}"),
            key.unwrap_or_default(),
        ).await?;
    }
    let path = phoenix_home().join("config.toml");
    let raw = read_private_text_required(&path, PRIVATE_DOCUMENT_MAX_BYTES, "config.toml")?;
    let mut doc: toml_edit::DocumentMut = raw
        .parse()
        .map_err(|error| format!("config.toml unreadable: {error}"))?;
    doc["profile"][capability.as_str()]["provider"] = toml_edit::value(provider.as_str());
    if has_key {
        doc["profile"][capability.as_str()]["auth"]["source"] = toml_edit::value("profile");
        doc["profile"][capability.as_str()]["auth"]["profile"] =
            toml_edit::value(profile_id.as_str());
        if let Some(env) = env_var.as_deref().filter(|e| !e.is_empty()) {
            doc["profile"][capability.as_str()]["auth"]["env_var"] = toml_edit::value(env);
        }
    }
    backup_and_write_config(&path, &raw, doc.to_string())?;
    Ok(format!(
        "{capability} → {provider}{} — restart the gateway to apply",
        if has_key { " (key saved)" } else { "" }
    ))
}

/* ── Wallpapers (~/.phoenix/wallpapers) ────────────────────────────── */

fn validate_wallpaper_signature(extension: &str, header: &[u8]) -> Result<(), String> {
    if let Some(declared) = CanvasImageKind::from_extension(extension) {
        let detected = detect_canvas_image_kind(header)
            .ok_or_else(|| "wallpaper does not contain a supported image signature".to_string())?;
        if declared != detected {
            return Err(format!(
                "wallpaper extension .{extension} does not match its actual {} content",
                detected.mime()
            ));
        }
        return Ok(());
    }

    match extension {
        "webm"
            if header.starts_with(&[0x1a, 0x45, 0xdf, 0xa3])
                && header.windows(4).any(|window| window == b"webm") =>
        {
            Ok(())
        }
        "mp4" | "mov" if header.len() >= 16 && &header[4..8] == b"ftyp" => {
            let box_size = u32::from_be_bytes(
                header[0..4]
                    .try_into()
                    .map_err(|_| "invalid ISO media header".to_string())?,
            ) as usize;
            if !(16..=header.len()).contains(&box_size) {
                return Err("wallpaper has a truncated ISO media signature".to_string());
            }
            let mut brands = std::iter::once(&header[8..12]).chain(
                header[16..box_size]
                    .chunks_exact(4)
                    .map(|brand| brand as &[u8]),
            );
            let matches = if extension == "mov" {
                brands.any(|brand| brand == b"qt  ")
            } else {
                const MP4_BRANDS: [&[u8; 4]; 11] = [
                    b"isom", b"iso2", b"iso3", b"iso4", b"iso5", b"iso6", b"mp41", b"mp42",
                    b"avc1", b"M4V ", b"MSNV",
                ];
                brands.any(|brand| MP4_BRANDS.iter().any(|allowed| brand == *allowed))
            };
            if matches {
                Ok(())
            } else {
                Err(format!(
                    "wallpaper does not contain a recognized {extension} media brand"
                ))
            }
        }
        "mp4" | "mov" | "webm" => Err(format!(
            "wallpaper does not contain a recognized {extension} signature"
        )),
        _ => Err(format!("unsupported wallpaper extension .{extension}")),
    }
}

fn wallpaper_destination(directory: &std::path::Path, extension: &str) -> PathBuf {
    static NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let nonce = NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    directory.join(format!(
        "wallpaper-{}-{stamp}-{nonce}.{extension}",
        std::process::id()
    ))
}

fn copy_wallpaper_to_at(
    home: &std::path::Path,
    source: &std::path::Path,
    destination: &std::path::Path,
) -> Result<(), String> {
    use std::io::{Read, Seek, Write};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    let directory = home.join("wallpapers");
    if destination.parent() != Some(directory.as_path()) || destination.file_name().is_none() {
        return Err(format!(
            "refusing wallpaper destination outside {}",
            directory.display()
        ));
    }
    let extension = destination
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    const ALLOWED: [&str; 9] = [
        "jpg", "jpeg", "png", "webp", "gif", "avif", "mp4", "webm", "mov",
    ];
    if !ALLOWED.contains(&extension.as_str()) {
        return Err(format!("unsupported wallpaper extension .{extension}"));
    }

    let mut input = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(source)
        .map_err(|error| format!("could not open wallpaper source: {error}"))?;
    let metadata = input
        .metadata()
        .map_err(|error| format!("could not inspect wallpaper source: {error}"))?;
    if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err("wallpaper source must be a regular file owned by this user".to_string());
    }
    if metadata.len() == 0 || metadata.len() > WALLPAPER_MAX_BYTES {
        return Err(format!(
            "wallpaper source must be between 1 and {WALLPAPER_MAX_BYTES} bytes"
        ));
    }
    let mut header = Vec::with_capacity(
        usize::try_from(metadata.len())
            .unwrap_or(WALLPAPER_HEADER_MAX_BYTES)
            .min(WALLPAPER_HEADER_MAX_BYTES),
    );
    (&mut input)
        .take(WALLPAPER_HEADER_MAX_BYTES as u64)
        .read_to_end(&mut header)
        .map_err(|error| format!("could not read wallpaper signature: {error}"))?;
    validate_wallpaper_signature(&extension, &header)?;
    input
        .rewind()
        .map_err(|error| format!("could not rewind wallpaper source: {error}"))?;

    let default_root = dirs_home().join(".phoenix");
    secure_private_directory_at(&directory, home, &default_root, home != default_root)?;
    let temporary = private_sidecar(destination, "wallpaper-tmp")?;
    let result = (|| {
        let mut output = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&temporary)
            .map_err(|error| format!("could not create private wallpaper: {error}"))?;
        output
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("could not secure private wallpaper: {error}"))?;
        let mut copied = 0_u64;
        let mut chunk = [0_u8; 64 * 1024];
        loop {
            let read = input
                .read(&mut chunk)
                .map_err(|error| format!("could not read wallpaper source: {error}"))?;
            if read == 0 {
                break;
            }
            copied = copied
                .checked_add(read as u64)
                .ok_or_else(|| "wallpaper size overflow".to_string())?;
            if copied > WALLPAPER_MAX_BYTES {
                return Err(format!(
                    "wallpaper grew beyond the {WALLPAPER_MAX_BYTES}-byte limit while copying"
                ));
            }
            output
                .write_all(&chunk[..read])
                .map_err(|error| format!("could not write private wallpaper: {error}"))?;
        }
        if copied != metadata.len() {
            return Err("wallpaper changed size while it was being copied".to_string());
        }
        output
            .sync_all()
            .map_err(|error| format!("could not sync private wallpaper: {error}"))?;
        drop(output);
        // A hard-link publication is atomic and, unlike rename, cannot replace
        // an existing filename or symlink. The fully synced temporary inode is
        // invisible at the destination until this succeeds.
        std::fs::hard_link(&temporary, destination).map_err(|error| {
            format!(
                "could not publish wallpaper without replacing {}: {error}",
                destination.display()
            )
        })?;
        std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("could not secure published wallpaper: {error}"))?;
        std::fs::File::open(&directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("could not sync wallpaper directory: {error}"))?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&temporary);
    result
}

fn wallpaper_import_at(
    home: &std::path::Path,
    source: &std::path::Path,
) -> Result<PathBuf, String> {
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let destination = wallpaper_destination(&home.join("wallpapers"), &extension);
    copy_wallpaper_to_at(home, source, &destination)?;
    Ok(destination)
}

/// Copy the user's chosen image/video into ~/.phoenix/wallpapers and return
/// the absolute destination path (the webview turns it into an asset URL).
#[tauri::command]
async fn wallpaper_import(source: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let home = phoenix_home();
        let destination = wallpaper_import_at(&home, std::path::Path::new(&source))?;
        // Warm static derivatives immediately after an import. Preview failure
        // never rolls back the original; the catalog retains original-path
        // fallback and can retry lazily on its next call.
        let _ = wallpaper_previews_at(&home);
        Ok(destination.display().to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

fn wallpapers_list_blocking() -> Vec<String> {
    let dir = phoenix_home().join("wallpapers");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut list: Vec<String> = entries
        .flatten()
        .filter(|entry| {
            std::fs::symlink_metadata(entry.path())
                .ok()
                .is_some_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
        })
        .map(|e| e.path().display().to_string())
        .collect();
    list.sort();
    list
}

#[tauri::command]
async fn wallpapers_list() -> Vec<String> {
    tauri::async_runtime::spawn_blocking(wallpapers_list_blocking)
        .await
        .expect("wallpaper-list worker panicked")
}

#[derive(Serialize)]
struct WallpaperPreview {
    path: String,
    thumb_path: String,
    thumb_w: i32,
    thumb_h: i32,
    card_path: String,
    card_w: i32,
    card_h: i32,
}

#[derive(Clone, Copy)]
struct WallpaperPreviewVariant {
    name: &'static str,
    max_size: i32,
}

const WALLPAPER_THUMB_VARIANT: WallpaperPreviewVariant = WallpaperPreviewVariant {
    name: "thumb-512",
    max_size: 512,
};
const WALLPAPER_CARD_VARIANT: WallpaperPreviewVariant = WallpaperPreviewVariant {
    name: "card-2048",
    max_size: 2_048,
};

fn wallpaper_preview_generation_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[cfg(unix)]
struct WallpaperPreviewSource {
    file: std::fs::File,
    metadata: std::fs::Metadata,
    kind: CanvasImageKind,
}

#[cfg(unix)]
fn open_wallpaper_preview_source(
    path: &std::path::Path,
    wallpapers_dir: &std::path::Path,
) -> Result<WallpaperPreviewSource, String> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    if path.parent() != Some(wallpapers_dir) || path.file_name().is_none() {
        return Err(format!(
            "refusing wallpaper outside {}",
            wallpapers_dir.display()
        ));
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| format!("could not open wallpaper {}: {error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("could not inspect wallpaper {}: {error}", path.display()))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
    {
        return Err("wallpaper preview source must be a private regular file".to_string());
    }
    if metadata.len() == 0 || metadata.len() > WALLPAPER_PREVIEW_SOURCE_MAX_BYTES {
        return Err(format!(
            "wallpaper preview source must be between 1 and {WALLPAPER_PREVIEW_SOURCE_MAX_BYTES} bytes"
        ));
    }
    // detect_canvas_image_kind validates the complete 33-byte PNG header.
    // Keep this probe small, but never truncate that signature by one byte.
    let mut header = [0_u8; 64];
    let read = file
        .read(&mut header)
        .map_err(|error| format!("could not read wallpaper {}: {error}", path.display()))?;
    let kind = static_wallpaper_preview_kind(&header[..read])
        .ok_or_else(|| "wallpaper is not a static PNG or JPEG".to_string())?;
    Ok(WallpaperPreviewSource {
        file,
        metadata,
        kind,
    })
}

fn static_wallpaper_preview_kind(bytes: &[u8]) -> Option<CanvasImageKind> {
    match detect_canvas_image_kind(bytes) {
        Some(CanvasImageKind::Png) => Some(CanvasImageKind::Png),
        Some(CanvasImageKind::Jpeg) => Some(CanvasImageKind::Jpeg),
        _ => None,
    }
}

fn static_wallpaper_dimensions(bytes: &[u8], kind: CanvasImageKind) -> Option<(u32, u32)> {
    match kind {
        CanvasImageKind::Png if bytes.len() >= 24 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") => {
            let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
            let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
            (width > 0 && height > 0).then_some((width, height))
        }
        CanvasImageKind::Jpeg if bytes.starts_with(&[0xff, 0xd8, 0xff]) => {
            let mut cursor = 2_usize;
            while cursor + 1 < bytes.len() {
                while cursor < bytes.len() && bytes[cursor] != 0xff {
                    cursor += 1;
                }
                while cursor < bytes.len() && bytes[cursor] == 0xff {
                    cursor += 1;
                }
                let marker = *bytes.get(cursor)?;
                cursor += 1;
                if marker == 0xd9 || marker == 0xda {
                    return None;
                }
                if marker == 0x01 || marker == 0xd8 || (0xd0..=0xd7).contains(&marker) {
                    continue;
                }
                let length =
                    u16::from_be_bytes(bytes.get(cursor..cursor + 2)?.try_into().ok()?) as usize;
                if length < 2 || cursor.checked_add(length)? > bytes.len() {
                    return None;
                }
                if matches!(
                    marker,
                    0xc0 | 0xc1
                        | 0xc2
                        | 0xc3
                        | 0xc5
                        | 0xc6
                        | 0xc7
                        | 0xc9
                        | 0xca
                        | 0xcb
                        | 0xcd
                        | 0xce
                        | 0xcf
                ) && length >= 7
                {
                    let height =
                        u16::from_be_bytes(bytes.get(cursor + 3..cursor + 5)?.try_into().ok()?)
                            as u32;
                    let width =
                        u16::from_be_bytes(bytes.get(cursor + 5..cursor + 7)?.try_into().ok()?)
                            as u32;
                    return (width > 0 && height > 0).then_some((width, height));
                }
                cursor += length;
            }
            None
        }
        _ => None,
    }
}

fn validate_wallpaper_preview_dimensions(width: u32, height: u32) -> Result<(), String> {
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| "wallpaper dimensions overflow".to_string())?;
    if width > WALLPAPER_PREVIEW_MAX_DIMENSION
        || height > WALLPAPER_PREVIEW_MAX_DIMENSION
        || pixels > WALLPAPER_PREVIEW_MAX_PIXELS
    {
        return Err(format!(
            "wallpaper dimensions {width}x{height} exceed the preview safety limit"
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn wallpaper_preview_cache_stem(source: &std::path::Path, metadata: &std::fs::Metadata) -> String {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::MetadataExt;

    let mut digest = Sha256::new();
    digest.update(b"phoenix-wallpaper-preview-v1\0");
    use std::os::unix::ffi::OsStrExt;
    digest.update(source.as_os_str().as_bytes());
    for value in [
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime() as u64,
        metadata.mtime_nsec() as u64,
        metadata.ctime() as u64,
        metadata.ctime_nsec() as u64,
    ] {
        digest.update(value.to_le_bytes());
    }
    format!("{:x}", digest.finalize())
}

fn valid_wallpaper_preview(path: &std::path::Path) -> Option<(i32, i32)> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.len() == 0
        || metadata.len() > WALLPAPER_PREVIEW_MAX_BYTES as u64
    {
        return None;
    }
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let mut header = [0_u8; 64];
    let read = file.read(&mut header).ok()?;
    if !matches!(
        detect_canvas_image_kind(&header[..read]),
        Some(CanvasImageKind::Png)
    ) {
        return None;
    }
    let (_, width, height) = gdk_pixbuf::Pixbuf::file_info(path)?;
    (width > 0 && height > 0).then_some((width, height))
}

fn decode_wallpaper_preview(
    mut source: std::fs::File,
    expected_len: u64,
    kind: CanvasImageKind,
) -> Result<gdk_pixbuf::Pixbuf, String> {
    use gdk_pixbuf::prelude::*;
    use std::io::{Read, Seek};

    source
        .rewind()
        .map_err(|error| format!("could not rewind wallpaper preview source: {error}"))?;
    let mut header = Vec::with_capacity(WALLPAPER_HEADER_MAX_BYTES);
    source
        .by_ref()
        .take(WALLPAPER_HEADER_MAX_BYTES as u64)
        .read_to_end(&mut header)
        .map_err(|error| format!("could not read wallpaper dimensions: {error}"))?;
    let (header_width, header_height) = static_wallpaper_dimensions(&header, kind)
        .ok_or_else(|| "could not read static wallpaper dimensions".to_string())?;
    validate_wallpaper_preview_dimensions(header_width, header_height)?;

    let loader = gdk_pixbuf::PixbufLoader::new();
    let dimensions = Arc::new(Mutex::new(None::<(i32, i32)>));
    let dimensions_for_callback = Arc::clone(&dimensions);
    loader.connect_size_prepared(move |loader, width, height| {
        if width <= 0 || height <= 0 {
            return;
        }
        if let Ok(mut dimensions) = dimensions_for_callback.lock() {
            *dimensions = Some((width, height));
        }
        let largest = width.max(height);
        if largest > WALLPAPER_CARD_VARIANT.max_size {
            let scale = f64::from(WALLPAPER_CARD_VARIANT.max_size) / f64::from(largest);
            loader.set_size(
                (f64::from(width) * scale).round().max(1.0) as i32,
                (f64::from(height) * scale).round().max(1.0) as i32,
            );
        }
    });
    source
        .rewind()
        .map_err(|error| format!("could not rewind wallpaper preview source: {error}"))?;
    let mut chunk = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let read = source
            .read(&mut chunk)
            .map_err(|error| format!("could not read wallpaper preview source: {error}"))?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .ok_or_else(|| "wallpaper preview source size overflow".to_string())?;
        if total > WALLPAPER_PREVIEW_SOURCE_MAX_BYTES {
            return Err(format!(
                "wallpaper preview source exceeds {WALLPAPER_PREVIEW_SOURCE_MAX_BYTES} bytes"
            ));
        }
        loader
            .write(&chunk[..read])
            .map_err(|error| format!("could not decode wallpaper preview: {error}"))?;
    }
    if total != expected_len {
        return Err("wallpaper changed size while generating its preview".to_string());
    }
    loader
        .close()
        .map_err(|error| format!("could not finish wallpaper preview decode: {error}"))?;
    let source_dimensions = dimensions
        .lock()
        .map_err(|_| "wallpaper preview dimension lock poisoned".to_string())?
        .ok_or_else(|| "wallpaper decoder did not report dimensions".to_string())?;
    let (source_w, source_h) = source_dimensions;
    if source_w <= 0 || source_h <= 0 {
        return Err("wallpaper decoder reported invalid dimensions".to_string());
    }
    validate_wallpaper_preview_dimensions(source_w as u32, source_h as u32)?;

    let pixbuf = loader
        .pixbuf()
        .ok_or_else(|| "wallpaper decoder returned no pixels".to_string())?;
    Ok(pixbuf.apply_embedded_orientation().unwrap_or(pixbuf))
}

fn encode_wallpaper_preview(
    pixbuf: &gdk_pixbuf::Pixbuf,
    destination: &std::path::Path,
) -> Result<(i32, i32), String> {
    let mut options = Vec::new();
    let icc_profile = pixbuf.option("icc-profile").map(|value| value.to_string());
    if let Some(profile) = icc_profile.as_deref() {
        options.push(("icc-profile", profile));
    }
    let png = pixbuf
        .save_to_bufferv("png", &options)
        .map_err(|error| format!("could not encode wallpaper preview: {error}"))?;
    if png.len() > WALLPAPER_PREVIEW_MAX_BYTES {
        return Err(format!(
            "wallpaper preview exceeds the {WALLPAPER_PREVIEW_MAX_BYTES}-byte cache limit"
        ));
    }
    write_private_atomic_unlocked(destination, &png)?;
    Ok((pixbuf.width(), pixbuf.height()))
}

fn wallpaper_preview_path(
    cache_dir: &std::path::Path,
    stem: &str,
    variant: WallpaperPreviewVariant,
) -> PathBuf {
    cache_dir.join(format!("{stem}-{}.png", variant.name))
}

fn scaled_wallpaper_preview(
    pixbuf: &gdk_pixbuf::Pixbuf,
    max_size: i32,
) -> Result<gdk_pixbuf::Pixbuf, String> {
    let largest = pixbuf.width().max(pixbuf.height());
    if largest <= max_size {
        return Ok(pixbuf.clone());
    }
    let scale = f64::from(max_size) / f64::from(largest);
    let width = (f64::from(pixbuf.width()) * scale).round().max(1.0) as i32;
    let height = (f64::from(pixbuf.height()) * scale).round().max(1.0) as i32;
    pixbuf
        .scale_simple(width, height, gdk_pixbuf::InterpType::Bilinear)
        .ok_or_else(|| "could not scale wallpaper preview".to_string())
}

fn wallpaper_previews_at(home: &std::path::Path) -> Result<Vec<WallpaperPreview>, String> {
    let _generation = wallpaper_preview_generation_lock()
        .lock()
        .map_err(|_| "wallpaper preview generator lock poisoned".to_string())?;
    let wallpapers_dir = home.join("wallpapers");
    let cache_dir = home.join("cache").join("wallpaper-previews-v1");
    let default_root = dirs_home().join(".phoenix");
    secure_private_directory_at(&wallpapers_dir, home, &default_root, home != default_root)?;
    secure_private_directory_at(&cache_dir, home, &default_root, home != default_root)?;

    let Ok(entries) = std::fs::read_dir(&wallpapers_dir) else {
        return Ok(Vec::new());
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    let mut previews = Vec::new();
    for entry in entries {
        let source = entry.path();
        let preview_source = match open_wallpaper_preview_source(&source, &wallpapers_dir) {
            Ok(source) => source,
            Err(error) => {
                eprintln!(
                    "phoenix: wallpaper preview skipped {}: {error}",
                    source.display()
                );
                continue;
            }
        };
        // Eligibility comes from magic, not extension: existing users have
        // valid JPEGs named e.g. `photo.jpg.orig-8k` that WebKit displays.
        debug_assert!(matches!(
            preview_source.kind,
            CanvasImageKind::Png | CanvasImageKind::Jpeg
        ));
        let stem = wallpaper_preview_cache_stem(&source, &preview_source.metadata);
        let thumb_path = wallpaper_preview_path(&cache_dir, &stem, WALLPAPER_THUMB_VARIANT);
        let card_path = wallpaper_preview_path(&cache_dir, &stem, WALLPAPER_CARD_VARIANT);
        let thumb_hit = valid_wallpaper_preview(&thumb_path);
        let card_hit = valid_wallpaper_preview(&card_path);
        let ((thumb_w, thumb_h), (card_w, card_h)) = match (thumb_hit, card_hit) {
            (Some(thumb), Some(card)) => (thumb, card),
            (thumb, card) => {
                let card_pixbuf = match decode_wallpaper_preview(
                    preview_source.file,
                    preview_source.metadata.len(),
                    preview_source.kind,
                ) {
                    Ok(pixbuf) => pixbuf,
                    Err(error) => {
                        eprintln!(
                            "phoenix: wallpaper preview decode failed {}: {error}",
                            source.display()
                        );
                        continue;
                    }
                };
                let card = match card {
                    Some(card) => card,
                    None => match encode_wallpaper_preview(&card_pixbuf, &card_path) {
                        Ok(card) => card,
                        Err(error) => {
                            eprintln!(
                                "phoenix: wallpaper card preview failed {}: {error}",
                                source.display()
                            );
                            continue;
                        }
                    },
                };
                let thumb = match thumb {
                    Some(thumb) => thumb,
                    None => {
                        let thumb_pixbuf = match scaled_wallpaper_preview(
                            &card_pixbuf,
                            WALLPAPER_THUMB_VARIANT.max_size,
                        ) {
                            Ok(pixbuf) => pixbuf,
                            Err(error) => {
                                eprintln!(
                                    "phoenix: wallpaper thumbnail scale failed {}: {error}",
                                    source.display()
                                );
                                continue;
                            }
                        };
                        match encode_wallpaper_preview(&thumb_pixbuf, &thumb_path) {
                            Ok(thumb) => thumb,
                            Err(error) => {
                                eprintln!(
                                    "phoenix: wallpaper thumbnail preview failed {}: {error}",
                                    source.display()
                                );
                                continue;
                            }
                        }
                    }
                };
                (thumb, card)
            }
        };
        previews.push(WallpaperPreview {
            path: source.display().to_string(),
            thumb_path: thumb_path.display().to_string(),
            thumb_w,
            thumb_h,
            card_path: card_path.display().to_string(),
            card_w,
            card_h,
        });
    }
    Ok(previews)
}

#[tauri::command]
async fn wallpaper_previews() -> Result<Vec<WallpaperPreview>, String> {
    tauri::async_runtime::spawn_blocking(|| wallpaper_previews_at(&phoenix_home()))
        .await
        .map_err(|error| error.to_string())?
}

#[derive(Serialize)]
struct PrefsSnapshot {
    prefs: serde_json::Value,
    /// Opaque exact-content token. It deliberately survives Phoenix process and
    /// renderer reloads; the writer must present it for compare-and-swap.
    revision: String,
}

#[derive(Serialize)]
struct PrefsWriteReceipt {
    revision: String,
}

fn parse_prefs_document(bytes: &[u8]) -> Result<serde_json::Value, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|error| {
        format!("Phoenix preferences are invalid; refusing to erase them: {error}")
    })?;
    if !value.is_object() {
        return Err(
            "Phoenix preferences must contain a JSON object; refusing to erase them".into(),
        );
    }
    Ok(value)
}

fn prefs_revision_for(current: Option<&[u8]>) -> Result<String, String> {
    match current {
        Some(bytes) => String::from_utf8(bytes.to_vec())
            .map_err(|error| format!("Phoenix preferences are not UTF-8: {error}")),
        None => Ok(PREFS_MISSING_REVISION.to_string()),
    }
}

fn prefs_get_at(path: &std::path::Path) -> Result<PrefsSnapshot, String> {
    let current = read_private_bounded_unlocked(path, PREFS_MAX_BYTES, "Phoenix preferences")?;
    let prefs = match current.as_deref() {
        Some(bytes) => parse_prefs_document(bytes)?,
        None => serde_json::json!({}),
    };
    Ok(PrefsSnapshot {
        prefs,
        revision: prefs_revision_for(current.as_deref())?,
    })
}

#[tauri::command]
async fn prefs_get() -> Result<PrefsSnapshot, String> {
    tauri::async_runtime::spawn_blocking(|| prefs_get_at(&phoenix_home().join("canvas-prefs.json")))
        .await
        .map_err(|error| error.to_string())?
}

fn rolling_backup_path(path: &std::path::Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".bak");
    PathBuf::from(name)
}

fn prefs_set_at(
    path: &std::path::Path,
    prefs: serde_json::Value,
    expected_revision: Option<String>,
) -> Result<PrefsWriteReceipt, String> {
    if !prefs.is_object() {
        return Err("Phoenix preferences must be a JSON object".into());
    }
    let replacement = serde_json::to_vec_pretty(&prefs).map_err(|error| error.to_string())?;
    if replacement.len() > PREFS_MAX_BYTES {
        return Err(format!(
            "Phoenix preferences exceed the {PREFS_MAX_BYTES}-byte limit"
        ));
    }
    let expected_revision = expected_revision.ok_or_else(|| {
        "prefs_set needs the revision returned by prefs_get; reload and retry".to_string()
    })?;
    if expected_revision.len() > PREFS_MAX_BYTES {
        return Err("Phoenix preference revision exceeds the file limit".into());
    }

    with_private_file_lock(path, || {
        let current = read_private_bounded_unlocked(path, PREFS_MAX_BYTES, "Phoenix preferences")?;
        if let Some(bytes) = current.as_deref() {
            parse_prefs_document(bytes)?;
        }
        let current_revision = prefs_revision_for(current.as_deref())?;
        if current_revision != expected_revision {
            return Err(
                "Phoenix preferences changed in another writer; reload before saving".to_string(),
            );
        }
        if current.as_deref() == Some(replacement.as_slice()) {
            return Ok(PrefsWriteReceipt {
                revision: current_revision,
            });
        }
        if let Some(previous) = current.as_deref() {
            // A single rolling backup is bounded even though view persistence
            // can write many times per minute. It always contains the exact
            // last valid document immediately preceding the current one.
            write_private_atomic_unlocked(&rolling_backup_path(path), previous)?;
        }
        write_private_atomic_unlocked(path, &replacement)?;
        Ok(PrefsWriteReceipt {
            revision: String::from_utf8(replacement)
                .map_err(|error| format!("serialized preferences were not UTF-8: {error}"))?,
        })
    })
}

#[tauri::command]
async fn prefs_set(
    prefs: serde_json::Value,
    expected_revision: Option<String>,
) -> Result<PrefsWriteReceipt, String> {
    tauri::async_runtime::spawn_blocking(move || {
        prefs_set_at(
            &phoenix_home().join("canvas-prefs.json"),
            prefs,
            expected_revision,
        )
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
fn workspace_default() -> Result<String, String> {
    let path = workspace_default_path()?;
    Ok(path.display().to_string())
}

fn workspace_default_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("PHOENIX_WORKSPACE")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
    {
        return path
            .canonicalize()
            .map_err(|error| format!("Could not open PHOENIX_WORKSPACE: {error}"));
    }
    // Coworkers always work in Phoenix's own workspace, never in the folder
    // the app happened to be launched from (that put agents, their test runs
    // and artifacts inside Phoenix's source tree). Full access still lets
    // them work elsewhere when a task needs it.
    let path = phoenix_home().join("workspace");
    secure_private_directory(&path)?;
    Ok(path)
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct WorkspaceReviewChange {
    path: String,
    status: String,
    additions: usize,
    deletions: usize,
    diff: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct WorkspaceReviewSnapshot {
    workspace: String,
    additions: usize,
    deletions: usize,
    changes: Vec<WorkspaceReviewChange>,
    files: Vec<String>,
    truncated: bool,
}

const WORKSPACE_REVIEW_MAX_FILES: usize = 6_000;
const WORKSPACE_REVIEW_MAX_REPOS: usize = 64;
const WORKSPACE_REVIEW_MAX_CHANGES: usize = 300;
const WORKSPACE_REVIEW_MAX_DIFF_BYTES: usize = 384 * 1024;

fn review_ignored_directory(name: &str) -> bool {
    matches!(
        name,
        ".git" | "node_modules" | "target" | "dist" | "build" | "coverage" | ".cache"
    )
}

fn collect_workspace_review_tree(
    root: &std::path::Path,
) -> Result<(Vec<String>, Vec<PathBuf>, bool), String> {
    let mut files = Vec::new();
    let mut repos = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut truncated = false;
    while let Some((directory, depth)) = stack.pop() {
        if depth > 18 {
            truncated = true;
            continue;
        }
        let mut entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries.filter_map(Result::ok).collect::<Vec<_>>(),
            Err(error) if directory == root => {
                return Err(format!(
                    "Could not read workspace {}: {error}",
                    root.display()
                ))
            }
            Err(_) => continue,
        };
        entries.sort_by_key(|entry| entry.file_name());
        if directory.join(".git").exists() && repos.len() < WORKSPACE_REVIEW_MAX_REPOS {
            repos.push(directory.clone());
        }
        for entry in entries.into_iter().rev() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let file_type = match entry.file_type() {
                Ok(kind) => kind,
                Err(_) => continue,
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if !review_ignored_directory(&name) {
                    stack.push((entry.path(), depth + 1));
                }
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            if files.len() >= WORKSPACE_REVIEW_MAX_FILES {
                truncated = true;
                continue;
            }
            if let Ok(relative) = entry.path().strip_prefix(root) {
                files.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    files.sort();
    repos.sort();
    repos.dedup();
    Ok((files, repos, truncated))
}

fn review_git_output(command: &mut std::process::Command, label: &str) -> Result<Vec<u8>, String> {
    let output = run_command_bounded(command, None, std::time::Duration::from_secs(8), label)?;
    if !output.status.success() {
        return Err(format!("{label} failed"));
    }
    Ok(output.stdout)
}

fn review_status_entries(repo: &std::path::Path) -> Vec<(String, String)> {
    let mut command = std::process::Command::new("git");
    command
        .arg("-C")
        .arg(repo)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"]);
    let Ok(output) = review_git_output(&mut command, "git status") else {
        return Vec::new();
    };
    let records = output
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .collect::<Vec<_>>();
    let mut entries = Vec::new();
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        if record.len() < 4 {
            index += 1;
            continue;
        }
        let code = String::from_utf8_lossy(&record[..2]).to_string();
        let path = String::from_utf8_lossy(&record[3..]).to_string();
        entries.push((code.clone(), path));
        if code.bytes().any(|byte| matches!(byte, b'R' | b'C')) {
            index += 1; // porcelain -z includes the original rename path next
        }
        index += 1;
    }
    entries
}

fn review_diff_for_path(repo: &std::path::Path, path: &str, untracked: bool) -> String {
    if untracked {
        let source = repo.join(path);
        let Ok(metadata) = std::fs::metadata(&source) else {
            return String::new();
        };
        if !metadata.is_file() || metadata.len() as usize > WORKSPACE_REVIEW_MAX_DIFF_BYTES {
            return String::new();
        }
        let Ok(bytes) = std::fs::read(&source) else {
            return String::new();
        };
        let Ok(text) = String::from_utf8(bytes) else {
            return String::new();
        };
        let mut diff = format!("--- /dev/null\n+++ b/{path}\n@@ new file @@\n");
        for line in text.lines() {
            if diff.len() + line.len() + 2 > WORKSPACE_REVIEW_MAX_DIFF_BYTES {
                diff.push_str("+… diff truncated …\n");
                break;
            }
            diff.push('+');
            diff.push_str(line);
            diff.push('\n');
        }
        return diff;
    }
    let mut combined = Vec::new();
    for cached in [false, true] {
        let mut command = std::process::Command::new("git");
        command
            .arg("-C")
            .arg(repo)
            .args(["diff", "--no-ext-diff", "--no-color", "--unified=3"]);
        if cached {
            command.arg("--cached");
        }
        command.arg("--").arg(path);
        if let Ok(mut output) = review_git_output(&mut command, "git diff") {
            combined.append(&mut output);
        }
        if combined.len() >= WORKSPACE_REVIEW_MAX_DIFF_BYTES {
            combined.truncate(WORKSPACE_REVIEW_MAX_DIFF_BYTES);
            break;
        }
    }
    String::from_utf8_lossy(&combined).to_string()
}

fn workspace_review_at(root: PathBuf) -> Result<WorkspaceReviewSnapshot, String> {
    let root = root
        .canonicalize()
        .map_err(|error| format!("Could not open workspace: {error}"))?;
    if !root.is_dir() {
        return Err("The selected workspace is not a directory.".into());
    }
    let (mut files, repos, mut truncated) = collect_workspace_review_tree(&root)?;
    let mut changes = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for repo in repos {
        for (status, repo_path) in review_status_entries(&repo) {
            if changes.len() >= WORKSPACE_REVIEW_MAX_CHANGES {
                truncated = true;
                break;
            }
            let absolute = repo.join(&repo_path);
            let display = absolute
                .strip_prefix(&root)
                .unwrap_or(&absolute)
                .to_string_lossy()
                .replace('\\', "/");
            if !seen.insert(display.clone()) {
                continue;
            }
            if !files.iter().any(|path| path == &display) && absolute.exists() {
                files.push(display.clone());
            }
            let untracked = status == "??";
            let diff = review_diff_for_path(&repo, &repo_path, untracked);
            let mut additions = 0;
            let mut deletions = 0;
            for line in diff.lines() {
                if line.starts_with('+') && !line.starts_with("+++") {
                    additions += 1;
                } else if line.starts_with('-') && !line.starts_with("---") {
                    deletions += 1;
                }
            }
            changes.push(WorkspaceReviewChange {
                path: display,
                status,
                additions,
                deletions,
                diff,
            });
        }
    }
    files.sort();
    files.dedup();
    changes.sort_by(|left, right| left.path.cmp(&right.path));
    let additions = changes.iter().map(|change| change.additions).sum();
    let deletions = changes.iter().map(|change| change.deletions).sum();
    Ok(WorkspaceReviewSnapshot {
        workspace: root.display().to_string(),
        additions,
        deletions,
        changes,
        files,
        truncated,
    })
}

#[tauri::command]
async fn workspace_review(workspace: Option<String>) -> Result<WorkspaceReviewSnapshot, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let root = match workspace
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            Some(path) => PathBuf::from(path),
            None => workspace_default_path()?,
        };
        workspace_review_at(root)
    })
    .await
    .map_err(|error| error.to_string())?
}

/// The compact Environment panel needs one small, real snapshot instead of
/// inventing Git state in the renderer.  The workspace can itself be a repo,
/// or it can contain exactly the project repository Phoenix is reviewing.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct WorkspaceEnvironmentSnapshot {
    workspace: String,
    workspace_name: String,
    repo: Option<String>,
    branch: Option<String>,
    detached: bool,
    remote: Option<String>,
    upstream: Option<String>,
    changes: usize,
    additions: usize,
    deletions: usize,
    can_commit: bool,
    can_push: bool,
    can_create_pull_request: bool,
    gh_available: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct WorkspaceGitActionReceipt {
    action: String,
    message: String,
    url: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct WorkspaceGitBranches {
    current: Option<String>,
    branches: Vec<String>,
}

const WORKSPACE_GIT_STATUS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);
const WORKSPACE_GIT_ACTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);
const WORKSPACE_PULL_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const WORKSPACE_GIT_MESSAGE_MAX_CHARS: usize = 500;

fn bounded_output_text(bytes: &[u8]) -> String {
    let value = String::from_utf8_lossy(bytes).trim().to_string();
    let mut chars = value.chars();
    let prefix = chars.by_ref().take(1_999).collect::<String>();
    if chars.next().is_some() { format!("{prefix}…") } else { value }
}

fn git_failure(label: &str, output: &BoundedChildOutput) -> String {
    let detail = bounded_output_text(if output.stderr.is_empty() {
        &output.stdout
    } else {
        &output.stderr
    });
    if detail.is_empty() {
        format!("{label} failed")
    } else {
        format!("{label} failed: {detail}")
    }
}

fn git_command_at(
    repo: &std::path::Path,
    args: &[&str],
    timeout: std::time::Duration,
    label: &str,
) -> Result<BoundedChildOutput, String> {
    let mut command = std::process::Command::new("git");
    command.arg("-C").arg(repo).args(args);
    let output = run_command_bounded(&mut command, None, timeout, label)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(git_failure(label, &output))
    }
}

fn git_command_status_at(
    repo: &std::path::Path,
    args: &[&str],
    timeout: std::time::Duration,
    label: &str,
) -> Result<BoundedChildOutput, String> {
    let mut command = std::process::Command::new("git");
    command.arg("-C").arg(repo).args(args);
    run_command_bounded(&mut command, None, timeout, label)
}

fn git_stdout_at(repo: &std::path::Path, args: &[&str], label: &str) -> Result<String, String> {
    let output = git_command_at(repo, args, WORKSPACE_GIT_STATUS_TIMEOUT, label)?;
    Ok(bounded_output_text(&output.stdout))
}

fn workspace_root_from_input(workspace: Option<String>) -> Result<PathBuf, String> {
    let root = match workspace
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        Some(path) => PathBuf::from(path),
        None => workspace_default_path()?,
    };
    let root = root
        .canonicalize()
        .map_err(|error| format!("Could not open workspace: {error}"))?;
    if !root.is_dir() {
        return Err("The selected workspace is not a directory.".into());
    }
    Ok(root)
}

fn git_repo_at_workspace(root: &std::path::Path) -> Option<PathBuf> {
    let mut command = std::process::Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--show-toplevel"]);
    let output = run_command_bounded(
        &mut command,
        None,
        WORKSPACE_GIT_STATUS_TIMEOUT,
        "git repository lookup",
    )
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = PathBuf::from(bounded_output_text(&output.stdout));
    path.canonicalize().ok().filter(|path| path.is_dir())
}

fn workspace_git_repo(root: &std::path::Path) -> Result<PathBuf, String> {
    git_repo_at_workspace(root).ok_or_else(|| {
        "This workspace is not inside a Git repository. Choose a project folder first.".into()
    })
}

fn git_optional_stdout_at(repo: &std::path::Path, args: &[&str], label: &str) -> Option<String> {
    git_stdout_at(repo, args, label)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn git_remote_names_at(repo: &std::path::Path) -> Vec<String> {
    git_optional_stdout_at(repo, &["remote"], "git remote")
        .map(|value| {
            value
                .lines()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn gh_cli_available() -> bool {
    let mut command = std::process::Command::new("gh");
    command.arg("--version");
    run_command_bounded(
        &mut command,
        None,
        std::time::Duration::from_secs(4),
        "gh version",
    )
    .map(|output| output.status.success())
    .unwrap_or(false)
}

fn workspace_environment_at(root: PathBuf) -> Result<WorkspaceEnvironmentSnapshot, String> {
    let root = root
        .canonicalize()
        .map_err(|error| format!("Could not open workspace: {error}"))?;
    if !root.is_dir() {
        return Err("The selected workspace is not a directory.".into());
    }
    let review = workspace_review_at(root.clone())?;
    let workspace_name = root
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|name| !name.is_empty())
        .unwrap_or("Workspace")
        .to_string();
    let Some(repo) = git_repo_at_workspace(&root) else {
        return Ok(WorkspaceEnvironmentSnapshot {
            workspace: root.display().to_string(),
            workspace_name,
            repo: None,
            branch: None,
            detached: false,
            remote: None,
            upstream: None,
            changes: review.changes.len(),
            additions: review.additions,
            deletions: review.deletions,
            can_commit: false,
            can_push: false,
            can_create_pull_request: false,
            gh_available: false,
        });
    };
    let branch = git_optional_stdout_at(
        &repo,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        "git branch",
    );
    let detached = branch.is_none();
    let remote_names = git_remote_names_at(&repo);
    let remote_name = remote_names
        .iter()
        .find(|name| name.as_str() == "origin")
        .or_else(|| remote_names.first());
    let remote = remote_name.and_then(|name| {
        git_optional_stdout_at(&repo, &["remote", "get-url", name], "git remote URL")
    });
    let upstream = git_optional_stdout_at(
        &repo,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"],
        "git upstream",
    );
    let has_changes = git_optional_stdout_at(
        &repo,
        &["status", "--porcelain=v1", "--untracked-files=all"],
        "git status",
    )
    .is_some();
    let gh_available = gh_cli_available();
    let github_remote = remote
        .as_deref()
        .is_some_and(|url| url.to_ascii_lowercase().contains("github.com"));
    let has_remote = remote.is_some();
    let can_create_pull_request = !detached && github_remote && gh_available && upstream.is_some();
    Ok(WorkspaceEnvironmentSnapshot {
        workspace: root.display().to_string(),
        workspace_name,
        repo: Some(repo.display().to_string()),
        branch,
        detached,
        remote,
        upstream,
        changes: review.changes.len(),
        additions: review.additions,
        deletions: review.deletions,
        can_commit: has_changes,
        can_push: !detached && has_remote,
        can_create_pull_request,
        gh_available,
    })
}

fn validate_workspace_git_message(value: String, label: &str, required: bool) -> Result<String, String> {
    let value = value.trim().to_string();
    if required && value.is_empty() {
        return Err(format!("{label} is required."));
    }
    if value.chars().count() > WORKSPACE_GIT_MESSAGE_MAX_CHARS {
        return Err(format!(
            "{label} must be at most {WORKSPACE_GIT_MESSAGE_MAX_CHARS} characters."
        ));
    }
    if value.contains('\0') {
        return Err(format!("{label} cannot contain a NUL character."));
    }
    Ok(value)
}

fn workspace_git_commit_at(
    root: PathBuf,
    message: String,
    stage_all: bool,
) -> Result<WorkspaceGitActionReceipt, String> {
    let root = workspace_root_from_input(Some(root.display().to_string()))?;
    let repo = workspace_git_repo(&root)?;
    let message = validate_workspace_git_message(message, "Commit message", true)?;
    if stage_all {
        git_command_at(
            &repo,
            &["add", "--all"],
            WORKSPACE_GIT_ACTION_TIMEOUT,
            "git add",
        )?;
    }
    let staged = git_command_status_at(
        &repo,
        &["diff", "--cached", "--quiet"],
        WORKSPACE_GIT_STATUS_TIMEOUT,
        "git staged diff",
    )?;
    if staged.status.success() {
        return Err("There are no staged changes to commit. Stage files or enable “Stage all workspace changes”.".into());
    }
    git_command_at(
        &repo,
        &["commit", "-m", &message],
        WORKSPACE_GIT_ACTION_TIMEOUT,
        "git commit",
    )?;
    Ok(WorkspaceGitActionReceipt {
        action: "commit".into(),
        message: "Committed the staged workspace changes.".into(),
        url: None,
    })
}

fn workspace_git_push_at(root: PathBuf) -> Result<WorkspaceGitActionReceipt, String> {
    let root = workspace_root_from_input(Some(root.display().to_string()))?;
    let repo = workspace_git_repo(&root)?;
    let branch = git_optional_stdout_at(
        &repo,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        "git branch",
    )
    .ok_or_else(|| "A detached HEAD cannot be pushed from the Environment panel.".to_string())?;
    let upstream = git_optional_stdout_at(
        &repo,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"],
        "git upstream",
    );
    if upstream.is_some() {
        git_command_at(
            &repo,
            &["push"],
            WORKSPACE_GIT_ACTION_TIMEOUT,
            "git push",
        )?;
    } else {
        let remotes = git_remote_names_at(&repo);
        let remote = remotes
            .iter()
            .find(|name| name.as_str() == "origin")
            .or_else(|| remotes.first())
            .ok_or_else(|| "Add a Git remote before pushing this branch.".to_string())?;
        git_command_at(
            &repo,
            &["push", "--set-upstream", remote, &branch],
            WORKSPACE_GIT_ACTION_TIMEOUT,
            "git push",
        )?;
    }
    Ok(WorkspaceGitActionReceipt {
        action: "push".into(),
        message: format!("Pushed {branch}."),
        url: None,
    })
}

fn workspace_git_branches_at(root: PathBuf) -> Result<WorkspaceGitBranches, String> {
    let root = workspace_root_from_input(Some(root.display().to_string()))?;
    let repo = workspace_git_repo(&root)?;
    let current = git_optional_stdout_at(
        &repo,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        "git branch",
    );
    let branches = git_optional_stdout_at(
        &repo,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
        "git branch list",
    )
    .map(|value| {
        value
            .lines()
            .map(str::trim)
            .filter(|branch| !branch.is_empty())
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default();
    Ok(WorkspaceGitBranches { current, branches })
}

fn workspace_git_switch_branch_at(
    root: PathBuf,
    branch: String,
) -> Result<WorkspaceGitActionReceipt, String> {
    let root = workspace_root_from_input(Some(root.display().to_string()))?;
    let repo = workspace_git_repo(&root)?;
    let branch = branch.trim().to_string();
    let available = workspace_git_branches_at(root.clone())?.branches;
    if !available.iter().any(|candidate| candidate == &branch) {
        return Err("Choose one of the workspace's existing local branches.".into());
    }
    git_command_at(
        &repo,
        &["switch", "--", &branch],
        WORKSPACE_GIT_ACTION_TIMEOUT,
        "git switch",
    )?;
    Ok(WorkspaceGitActionReceipt {
        action: "switch_branch".into(),
        message: format!("Switched to {branch}."),
        url: None,
    })
}

fn workspace_git_create_pull_request_at(
    root: PathBuf,
    title: String,
    body: String,
    draft: bool,
) -> Result<WorkspaceGitActionReceipt, String> {
    let root = workspace_root_from_input(Some(root.display().to_string()))?;
    let repo = workspace_git_repo(&root)?;
    let title = validate_workspace_git_message(title, "Pull request title", true)?;
    let body = validate_workspace_git_message(body, "Pull request body", false)?;
    let branch = git_optional_stdout_at(
        &repo,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        "git branch",
    )
    .ok_or_else(|| "A detached HEAD cannot open a pull request.".to_string())?;
    if git_optional_stdout_at(
        &repo,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"],
        "git upstream",
    )
    .is_none()
    {
        return Err("Push this branch before creating a pull request.".into());
    }
    let remote = git_remote_names_at(&repo)
        .iter()
        .find_map(|name| git_optional_stdout_at(&repo, &["remote", "get-url", name], "git remote URL"))
        .ok_or_else(|| "Add a GitHub remote before creating a pull request.".to_string())?;
    if !remote.to_ascii_lowercase().contains("github.com") {
        return Err("Create pull request is available for GitHub remotes only.".into());
    }
    let mut command = std::process::Command::new("gh");
    command
        .arg("pr")
        .arg("create")
        .arg("--title")
        .arg(&title)
        .arg("--body")
        .arg(&body)
        .current_dir(&repo)
        .env("GH_PROMPT_DISABLED", "1");
    if draft {
        command.arg("--draft");
    }
    let output = run_command_bounded(
        &mut command,
        None,
        WORKSPACE_PULL_REQUEST_TIMEOUT,
        "gh pr create",
    )?;
    if !output.status.success() {
        return Err(git_failure("gh pr create", &output));
    }
    let stdout = bounded_output_text(&output.stdout);
    let url = stdout
        .split_whitespace()
        .find(|word| word.starts_with("https://"))
        .map(str::to_string);
    Ok(WorkspaceGitActionReceipt {
        action: "pull_request".into(),
        message: format!("Created a pull request for {branch}."),
        url,
    })
}

#[tauri::command]
async fn workspace_environment(
    workspace: Option<String>,
) -> Result<WorkspaceEnvironmentSnapshot, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let root = workspace_root_from_input(workspace)?;
        workspace_environment_at(root)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn workspace_git_commit(
    workspace: Option<String>,
    message: String,
    stage_all: bool,
) -> Result<WorkspaceGitActionReceipt, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let root = workspace_root_from_input(workspace)?;
        workspace_git_commit_at(root, message, stage_all)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn workspace_git_push(workspace: Option<String>) -> Result<WorkspaceGitActionReceipt, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let root = workspace_root_from_input(workspace)?;
        workspace_git_push_at(root)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn workspace_git_branches(workspace: Option<String>) -> Result<WorkspaceGitBranches, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let root = workspace_root_from_input(workspace)?;
        workspace_git_branches_at(root)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn workspace_git_switch_branch(
    workspace: Option<String>,
    branch: String,
) -> Result<WorkspaceGitActionReceipt, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let root = workspace_root_from_input(workspace)?;
        workspace_git_switch_branch_at(root, branch)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn workspace_git_create_pull_request(
    workspace: Option<String>,
    title: String,
    body: String,
    draft: bool,
) -> Result<WorkspaceGitActionReceipt, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let root = workspace_root_from_input(workspace)?;
        workspace_git_create_pull_request_at(root, title, body, draft)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn workspace_pick(
    app: tauri::AppHandle,
    current: Option<String>,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let picked = tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = app.dialog().file().set_title("Choose workspace");
        if let Some(path) = current
            .as_deref()
            .map(std::path::Path::new)
            .filter(|path| path.is_dir())
        {
            dialog = dialog.set_directory(path);
        }
        dialog.blocking_pick_folder()
    })
    .await
    .map_err(|error| error.to_string())?;
    let Some(picked) = picked else {
        return Ok(None);
    };
    let path = picked
        .into_path()
        .map_err(|error| format!("Could not read that folder: {error}"))?;
    if !path.is_dir() {
        return Err("Choose a folder the agent can work in.".into());
    }
    Ok(Some(path.display().to_string()))
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct PickedComposerAttachment {
    name: String,
    path: String,
    size: u64,
    #[serde(rename = "type")]
    mime_type: String,
}

fn composer_path_attachment(
    path: &std::path::Path,
    expect_directory: bool,
) -> Result<PickedComposerAttachment, String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("Could not inspect that attachment: {error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("Choose the real file or folder instead of a symbolic link.".into());
    }
    if expect_directory && !metadata.is_dir() {
        return Err("Choose a folder to attach.".into());
    }
    if !expect_directory && !metadata.is_file() {
        return Err("Choose a regular file to attach.".into());
    }
    let fallback = if expect_directory { "Folder" } else { "File" };
    let name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|name| !name.is_empty())
        .unwrap_or(fallback)
        .to_string();
    let image_mime = path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_ascii_lowercase)
        .and_then(|extension| match extension.as_str() {
            "png" => Some("image/png"),
            "jpg" | "jpeg" => Some("image/jpeg"),
            "gif" => Some("image/gif"),
            "webp" => Some("image/webp"),
            "avif" => Some("image/avif"),
            _ => None,
        });
    Ok(PickedComposerAttachment {
        name,
        path: path.display().to_string(),
        size: if expect_directory { 0 } else { metadata.len() },
        mime_type: if expect_directory {
            "inode/directory".into()
        } else {
            image_mime.unwrap_or("application/octet-stream").into()
        },
    })
}

fn screenshots_picker_directory() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    screenshots_picker_directory_from(&home)
}

fn screenshots_picker_directory_from(home: &std::path::Path) -> Option<PathBuf> {
    [
        home.join("Pictures").join("Screenshots"),
        home.join("Pictures"),
        home.to_path_buf(),
    ]
    .into_iter()
    .find(|path| path.is_dir())
}

#[tauri::command]
async fn attachment_pick_images(
    app: tauri::AppHandle,
) -> Result<Vec<PickedComposerAttachment>, String> {
    use tauri_plugin_dialog::DialogExt;
    let picked = tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = app
            .dialog()
            .file()
            .set_title("Attach images")
            .add_filter("Images", &["png", "jpg", "jpeg", "gif", "webp", "avif"]);
        if let Some(directory) = screenshots_picker_directory() {
            dialog = dialog.set_directory(directory);
        }
        dialog.blocking_pick_files()
    })
    .await
    .map_err(|error| error.to_string())?;
    let Some(picked) = picked else {
        return Ok(Vec::new());
    };
    picked
        .into_iter()
        .map(|picked| {
            let path = picked
                .into_path()
                .map_err(|error| format!("Could not read that image: {error}"))?;
            let attachment = composer_path_attachment(&path, false)?;
            if !attachment.mime_type.starts_with("image/") {
                return Err(format!("{} is not a supported image.", attachment.name));
            }
            Ok(attachment)
        })
        .collect()
}

#[tauri::command]
async fn attachment_pick_file(
    app: tauri::AppHandle,
) -> Result<Option<PickedComposerAttachment>, String> {
    use tauri_plugin_dialog::DialogExt;
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("Attach a file")
            .blocking_pick_file()
    })
    .await
    .map_err(|error| error.to_string())?;
    let Some(picked) = picked else {
        return Ok(None);
    };
    let path = picked
        .into_path()
        .map_err(|error| format!("Could not read that file: {error}"))?;
    composer_path_attachment(&path, false).map(Some)
}

#[tauri::command]
async fn attachment_pick_folder(
    app: tauri::AppHandle,
) -> Result<Option<PickedComposerAttachment>, String> {
    use tauri_plugin_dialog::DialogExt;
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("Attach a folder")
            .blocking_pick_folder()
    })
    .await
    .map_err(|error| error.to_string())?;
    let Some(picked) = picked else {
        return Ok(None);
    };
    let path = picked
        .into_path()
        .map_err(|error| format!("Could not read that folder: {error}"))?;
    composer_path_attachment(&path, true).map(Some)
}

/// Per-thread transcript feeds. These used to live inside the monolithic
/// canvas-prefs.json blob, so every pan/zoom/edit re-serialized hundreds of KB
/// of story HTML for EVERY thread ever opened (measured 1.25 MB, 880 KB of it
/// feeds) — the canvas got slower the more threads accumulated. Now each
/// thread's feeds are their own small file, written only for the active
/// thread and loaded only when that thread is opened.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct CanvasConversationOwnerRef {
    kind: CanvasConversationOwnerKind,
    id: String,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum CanvasConversationOwnerKind {
    Agent,
    Group,
}

impl CanvasConversationOwnerRef {
    fn kind_label(&self) -> &'static str {
        match self.kind {
            CanvasConversationOwnerKind::Agent => "agent",
            CanvasConversationOwnerKind::Group => "group",
        }
    }

    fn table_and_id_column(&self) -> (&'static str, &'static str) {
        match self.kind {
            CanvasConversationOwnerKind::Agent => ("company_agents", "agent_id"),
            CanvasConversationOwnerKind::Group => ("company_groups", "group_id"),
        }
    }
}

/// Bind every Canvas transcript/feed filesystem access to the immutable
/// company-directory owner selected in the UI. The desktop deliberately stays
/// a thin shell, so this is the read-only equivalent of
/// `resolve_canonical_owner`: only the two canonical projection columns are
/// queried and no runtime state is initialized or migrated here.
///
/// Homes without a company projection remain readable for legacy standalone
/// sessions and preview fixtures. Once either the requested owner or session
/// is known canonically, however, a mismatch fails closed.
fn validate_canvas_conversation_owner_at(
    home: &std::path::Path,
    session_id: &str,
    owner: &CanvasConversationOwnerRef,
) -> Result<(), String> {
    validate_session_id_for_canvas(session_id)?;
    if owner.id.trim().is_empty() || owner.id.len() > 160 || owner.id.chars().any(char::is_control)
    {
        return Err("invalid conversation owner id".into());
    }

    let database = home.join("company/company.sqlite");
    if let Ok(parent) = std::fs::symlink_metadata(home.join("company")) {
        if parent.file_type().is_symlink() || !parent.is_dir() {
            return Err("company directory is not a regular directory".into());
        }
    }
    let metadata = match std::fs::symlink_metadata(&database) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "could not inspect company directory {}: {error}",
                database.display()
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "company directory {} is not a regular file",
            database.display()
        ));
    }

    use rusqlite::{Connection, OpenFlags, OptionalExtension};
    let connection = Connection::open_with_flags(
        &database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| format!("could not open company directory read-only: {error}"))?;
    let has_agents = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='company_agents'",
            [],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| format!("could not inspect company agent directory: {error}"))?
        .is_some();
    let has_groups = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='company_groups'",
            [],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| format!("could not inspect company group directory: {error}"))?
        .is_some();
    if !has_agents && !has_groups {
        return Ok(());
    }
    if !has_agents || !has_groups {
        return Err("company directory owner projection is incomplete".into());
    }

    let agent_for_session = connection
        .query_row(
            "SELECT agent_id FROM company_agents WHERE canonical_session_id=?1",
            [session_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("could not resolve canonical agent conversation: {error}"))?;
    let group_for_session = connection
        .query_row(
            "SELECT group_id FROM company_groups WHERE canonical_session_id=?1",
            [session_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("could not resolve canonical group conversation: {error}"))?;
    if agent_for_session.is_some() && group_for_session.is_some() {
        return Err(format!(
            "canonical conversation `{session_id}` has more than one owner"
        ));
    }
    if let Some(agent_id) = agent_for_session.as_deref() {
        if owner.kind != CanvasConversationOwnerKind::Agent || owner.id != agent_id {
            return Err(format!(
                "conversation owner mismatch: `{session_id}` belongs to agent `{agent_id}`"
            ));
        }
    }
    if let Some(group_id) = group_for_session.as_deref() {
        if owner.kind != CanvasConversationOwnerKind::Group || owner.id != group_id {
            return Err(format!(
                "conversation owner mismatch: `{session_id}` belongs to group `{group_id}`"
            ));
        }
    }

    let (table, id_column) = owner.table_and_id_column();
    let query = format!("SELECT canonical_session_id FROM {table} WHERE {id_column}=?1");
    let owner_session = connection
        .query_row(&query, [&owner.id], |row| row.get::<_, Option<String>>(0))
        .optional()
        .map_err(|error| {
            format!(
                "could not resolve canonical {} `{}`: {error}",
                owner.kind_label(),
                owner.id
            )
        })?
        .flatten();
    if let Some(owner_session) = owner_session {
        if owner_session != session_id {
            return Err(format!(
                "conversation owner mismatch: {} `{}` belongs to `{owner_session}`, not `{session_id}`",
                owner.kind_label(),
                owner.id
            ));
        }
    }
    Ok(())
}

fn feeds_file(session_id: &str) -> Result<std::path::PathBuf, String> {
    if session_id.is_empty()
        || session_id.len() > 160
        || session_id.chars().any(|character| {
            !character.is_ascii_alphanumeric() && character != '-' && character != '_'
        })
    {
        return Err("invalid feed session id".into());
    }
    Ok(phoenix_home()
        .join("canvas-feeds")
        .join(format!("{session_id}.json")))
}

#[tauri::command]
async fn feeds_get(
    session_id: String,
    owner: CanvasConversationOwnerRef,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        validate_canvas_conversation_owner_at(&phoenix_home(), &session_id, &owner)?;
        let path = feeds_file(&session_id)?;
        feeds_get_at(&path)
    })
    .await
    .map_err(|error| error.to_string())?
}

fn feeds_get_at(path: &std::path::Path) -> Result<serde_json::Value, String> {
    let current =
        read_private_bounded_unlocked(path, FEEDS_MAX_BYTES, "Phoenix conversation feed")?;
    match current {
        None => Ok(serde_json::json!({})),
        Some(bytes) => {
            let feeds: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
                format!(
                    "Phoenix conversation feed {} is invalid; refusing to treat it as empty: {error}",
                    path.display()
                )
            })?;
            if !feeds.is_object() {
                return Err(format!(
                    "Phoenix conversation feed {} must be a JSON object",
                    path.display()
                ));
            }
            Ok(feeds)
        }
    }
}

/// Recover typed display rows from the authoritative session when the
/// disposable rendered-HTML feed is missing. Live rendering consumes Story
/// events; recovery must preserve that structure instead of flattening every
/// tool and agent return into fake user/plain-text chat.
fn routine_turn_metadata(text: &str) -> Option<(String, serde_json::Value)> {
    let close = text.find(']')?;
    let header = text.get(1..close)?;
    let mut parts = header.split(" | ");
    let routine_id = parts.next()?.strip_prefix("cron ")?.trim();
    let schedule = parts.next()?.strip_prefix("scheduled ")?.trim();
    let scheduled_for = parts.next()?.strip_prefix("occurrence ")?.trim();
    let turn_id = parts.next()?.strip_prefix("turn ")?.trim();
    if routine_id.is_empty()
        || schedule.is_empty()
        || scheduled_for.is_empty()
        || turn_id.is_empty()
        || parts.next().is_some()
    {
        return None;
    }
    Some((
        turn_id.to_string(),
        serde_json::json!({
            "kind": "routine",
            "routine_id": routine_id,
            "scheduled_for": scheduled_for,
            "schedule": schedule,
        }),
    ))
}

/// Recent history rows a conversation gets on load and catch-up. One long
/// agent run is a few hundred rows, so a small window showed only that run
/// and nothing before it once compaction had folded the saved transcript.
const CONTEXT_HISTORY_ROWS: usize = 400;

fn keep_recent_complete_context_turns(
    rows: Vec<serde_json::Value>,
    target_rows: usize,
) -> Vec<serde_json::Value> {
    if rows.len() <= target_rows {
        return rows;
    }
    let floor = rows.len().saturating_sub(target_rows);
    // Walk back to the authored boundary that owns `floor`. Cutting exactly at
    // 80 used to leave a routine's tools/answer visible while dropping the
    // Routine row itself. A single unusually long current turn may therefore
    // exceed the target, which is the only honest representation of that turn.
    let keep_from = (0..=floor)
        .rev()
        .find(|index| {
            matches!(
                rows[*index].get("role").and_then(|v| v.as_str()),
                Some("user" | "talk")
            )
        })
        .unwrap_or(0);
    rows.into_iter().skip(keep_from).collect()
}

/// Split the private model-facing attachment envelope from the authored text.
/// Canvas projects the paths back into typed chips and thumbnails; backend
/// prompt instructions must never appear as user-written conversation copy.
fn canvas_user_message_parts(text: &str) -> (String, Vec<PickedComposerAttachment>) {
    const MARKER: &str = "\n\n[The user attached the following item(s) to THIS message. ";
    let Some(marker_at) = text.rfind(MARKER) else {
        return canvas_legacy_image_message_parts(text);
    };
    let envelope = &text[marker_at + MARKER.len()..];
    let Some((_, attachment_block)) =
        envelope.split_once(" before answering; do not treat files or folders as images:\n")
    else {
        return (text.to_string(), Vec::new());
    };
    let Some(lines) = attachment_block.strip_suffix(']') else {
        return (text.to_string(), Vec::new());
    };
    let mut attachments = Vec::new();
    for line in lines.lines().filter(|line| !line.trim().is_empty()) {
        let line = line.trim();
        let Some((kind, encoded)) = ["image", "file", "folder"].into_iter().find_map(|kind| {
            line.strip_prefix(&format!("- {kind}: "))
                .map(|encoded| (kind, encoded))
        }) else {
            return (text.to_string(), Vec::new());
        };
        let Ok(path) = serde_json::from_str::<String>(encoded) else {
            return (text.to_string(), Vec::new());
        };
        if path.is_empty() {
            return (text.to_string(), Vec::new());
        }
        let path_ref = std::path::Path::new(&path);
        let mime_type = match kind {
            "folder" => "inode/directory",
            "image" => match path_ref
                .extension()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or_default()
                .to_ascii_lowercase()
                .as_str()
            {
                "png" => "image/png",
                "jpg" | "jpeg" => "image/jpeg",
                "gif" => "image/gif",
                "webp" => "image/webp",
                "avif" => "image/avif",
                _ => "image/*",
            },
            _ => "application/octet-stream",
        };
        let name = path_ref
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .filter(|name| !name.is_empty())
            .unwrap_or(if kind == "folder" { "Folder" } else { "File" })
            .to_string();
        let size = std::fs::metadata(path_ref)
            .ok()
            .filter(|metadata| metadata.is_file())
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        attachments.push(PickedComposerAttachment {
            name,
            path,
            size,
            mime_type: mime_type.to_string(),
        });
    }
    if attachments.is_empty() {
        return (text.to_string(), Vec::new());
    }
    (text[..marker_at].trim_end().to_string(), attachments)
}

fn canvas_legacy_image_message_parts(text: &str) -> (String, Vec<PickedComposerAttachment>) {
    const MARKER: &str = "\n\n[The user attached images to THIS message. ";
    let Some(marker_at) = text.rfind(MARKER) else {
        return (text.to_string(), Vec::new());
    };
    let envelope = &text[marker_at + MARKER.len()..];
    let Some((_, attachment_block)) = envelope.split_once(" before answering:\n") else {
        return (text.to_string(), Vec::new());
    };
    let Some(lines) = attachment_block.strip_suffix(']') else {
        return (text.to_string(), Vec::new());
    };
    let attachments = lines
        .lines()
        .filter_map(|line| line.trim().strip_prefix("- "))
        .filter(|path| !path.is_empty())
        .map(|path| {
            let path_ref = std::path::Path::new(path);
            let name = path_ref
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or("Image")
                .to_string();
            let mime_type = match path_ref
                .extension()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or_default()
                .to_ascii_lowercase()
                .as_str()
            {
                "png" => "image/png",
                "jpg" | "jpeg" => "image/jpeg",
                "gif" => "image/gif",
                "webp" => "image/webp",
                "avif" => "image/avif",
                _ => "image/*",
            };
            PickedComposerAttachment {
                name,
                path: path.to_string(),
                size: std::fs::metadata(path_ref)
                    .ok()
                    .map(|metadata| metadata.len())
                    .unwrap_or(0),
                mime_type: mime_type.to_string(),
            }
        })
        .collect::<Vec<_>>();
    if attachments.is_empty() {
        return (text.to_string(), Vec::new());
    }
    (text[..marker_at].trim_end().to_string(), attachments)
}

#[derive(Clone)]
struct CanvasGroupDelivery {
    id: String,
    handoff: String,
    reply: Option<String>,
    cause: Option<String>,
    from: String,
    to: String,
    from_role: String,
    to_role: String,
    subject: String,
    body: String,
    requested: bool,
    delivery_state: String,
    sequence: i64,
}

fn canvas_history_id_valid(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_-:.".contains(&byte))
}

fn canvas_group_bind_alias(
    aliases: &mut std::collections::HashMap<String, Option<String>>,
    id: &str,
    role: &str,
) {
    for alias in [Some(id), Some(role), (id == "phoenix" && role == "phoenix").then_some("orchestrator")].into_iter().flatten() {
        let saved = aliases.entry(alias.to_string()).or_insert_with(|| Some(id.to_string()));
        if saved.as_deref() != Some(id) { *saved = None; }
    }
}

/// Resolve only exact, same-conversation ancestry. Subjects, timestamps and
/// current selection are never substitutes for an authored turn identity.
fn canvas_group_delivery_turn(
    index: usize,
    deliveries: &[CanvasGroupDelivery],
    references: &std::collections::HashMap<String, usize>,
    turns: &std::collections::HashMap<String, std::collections::HashSet<String>>,
    path: &mut Vec<usize>,
) -> Option<String> {
    if path.len() >= 32 || path.contains(&index) { return None; }
    path.push(index);
    let row = &deliveries[index];
    let result = (|| {
        let resolve = |id: &str, path: &mut Vec<usize>| {
            if turns.contains_key(id) { Some(id.to_string()) }
            else { canvas_group_delivery_turn(*references.get(id)?, deliveries, references, turns, path) }
        };
        if row.requested {
            if row.reply.is_some() { return None; }
            let cause = row.cause.as_deref()?;
            let turn = resolve(cause, path)?;
            if let Some(actors) = turns.get(cause) {
                if !actors.contains(&row.from) { return None; }
            } else {
                let parent = &deliveries[*references.get(cause)?];
                if row.from != parent.from && row.from != parent.to { return None; }
            }
            Some(turn)
        } else {
            let parent_index = *references.get(row.reply.as_deref()?)?;
            let parent = &deliveries[parent_index];
            if !parent.requested || row.from != parent.to || row.to != parent.from { return None; }
            let turn = canvas_group_delivery_turn(parent_index, deliveries, references, turns, path)?;
            if let Some(cause) = row.cause.as_deref() {
                if resolve(cause, path)? != turn { return None; }
            }
            Some(turn)
        }
    })();
    path.pop();
    result
}

fn canvas_group_history_notice(session: &str, turn: &str, text: &str) -> serde_json::Value {
    serde_json::json!({"role":"notice", "historical":true, "turn_id":turn,
        "history_id":format!("group-history:{session}:{turn}:notice"), "text":text})
}

// Keep the desktop's read-only classification aligned with the runtime's
// AgentMessage::failed_result_parts without linking the runtime into this shell.
fn canvas_group_result_failed(subject: &str, body: &str) -> bool {
    subject.contains("turn failed") || subject.contains("turn hit an internal error")
        || body.starts_with("The provider became unavailable after")
        || body.starts_with("Phoenix stopped this agent at a hard runtime boundary:")
        || body.starts_with("I could not advance because my only next action repeated the already-blocked")
        || matches!(subject, "Unfinished workflow checks remain" | "Unfinished task items remain")
}

/// Recover published room deliveries and saved activation states, not actors'
/// private working transcripts or an invented replay of tools/thoughts. These
/// read-only projections do not route messages, settle work or write feeds.
fn canvas_group_history_at(
    home: &std::path::Path,
    session: &str,
    owner: &CanvasConversationOwnerRef,
    canonical: &[serde_json::Value],
) -> Result<Vec<serde_json::Value>, String> {
    use rusqlite::{Connection, OpenFlags, OptionalExtension};
    use std::collections::{HashMap, HashSet};
    const LIMIT: usize = 256;
    const BODY_LIMIT: usize = 64 * 1024;
    const RECOVERY_BYTES: usize = 1024 * 1024;
    let authored = canonical.iter().filter(|row| row["role"] == "user")
        .filter_map(|row| row["turn_id"].as_str()).filter(|id| canvas_history_id_valid(id))
        .collect::<Vec<_>>();
    if authored.is_empty() { return Ok(Vec::new()); }
    let unique = authored.iter().copied().collect::<HashSet<_>>();
    if unique.len() != authored.len() { return Err("Saved group turn boundaries are ambiguous.".into()); }
    let older_turns_omitted = authored.len() > 80;
    let authored = authored.into_iter().rev().take(80).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>();
    let database = home.join("company/company.sqlite");
    if !database.exists() { return Ok(Vec::new()); }
    let db = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|_| "Saved group history could not be opened read-only.")?;
    db.busy_timeout(std::time::Duration::from_millis(150)).map_err(|_| "Saved group history is busy.")?;
    db.execute_batch("BEGIN").map_err(|_| "Saved group history snapshot is unavailable.")?;
    let required = ["company_group_turns", "company_group_turn_members", "company_group_members", "company_messages"];
    let mut tables = 0;
    for name in required {
        tables += db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [name], |row| row.get::<_, bool>(0)).map_err(|_| "Saved group history schema is unreadable.")? as usize;
    }
    // Directory-only legacy homes have no execution history to project.
    if tables == 0 { return Ok(Vec::new()); }
    if tables != required.len() { return Err("Saved group history schema is incomplete.".into()); }
    let mut roster = HashSet::new();
    let mut aliases = HashMap::new();
    let mut roster_query = db.prepare("SELECT substr(a.agent_id,1,257),substr(a.internal_role,1,257) FROM company_group_members m
        JOIN company_agents a ON a.agent_id=m.agent_id WHERE m.group_id=?1 LIMIT 129")
        .map_err(|_| "Saved group membership is unreadable.")?;
    for member in roster_query.query_map([&owner.id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(|_| "Saved group membership is unreadable.")? {
        let (id, role) = member.map_err(|_| "Saved group membership is malformed.")?;
        if !canvas_history_id_valid(&id) || !canvas_history_id_valid(&role) { return Err("Saved group membership is malformed.".into()); }
        canvas_group_bind_alias(&mut aliases, &id, &role);
        roster.insert(id);
    }
    if roster.len() > 128 { return Err("Saved group membership exceeds the recovery limit.".into()); }
    let mut turns: HashMap<String, HashSet<String>> = HashMap::new();
    let mut statuses: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
    let mut omitted = usize::from(older_turns_omitted);
    let mut recovery_bytes = 0usize;
    for turn in &authored {
        let group = db.query_row("SELECT group_id FROM company_group_turns WHERE canonical_session_id=?1 AND turn_id=?2",
            rusqlite::params![session, turn], |row| row.get::<_, String>(0)).optional()
            .map_err(|_| "Saved group turn identity is unreadable.")?;
        if group.as_deref() != Some(owner.id.as_str()) { omitted += 1; continue; }
        let mut actors = HashSet::new();
        let mut query = db.prepare("SELECT activation_id,agent_id,substr(participant_json,1,16385),state,
            substr(status_detail,1,1025),receipt_id FROM company_group_turn_members
            WHERE canonical_session_id=?1 AND turn_id=?2 ORDER BY activation_ordinal LIMIT 33")
            .map_err(|_| "Saved group activation state is unreadable.")?;
        let rows = query.query_map(rusqlite::params![session, turn], |row| Ok((
            row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
            row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, Option<String>>(5)?)))
            .map_err(|_| "Saved group activation state is unreadable.")?;
        let mut count = 0;
        for row in rows {
            count += 1;
            if count > 32 { return Err("Saved group activations exceed the recovery limit.".into()); }
            let (activation, agent, snapshot, state, detail, receipt) = row.map_err(|_| "Saved group activation state is malformed.")?;
            let Ok(snapshot) = serde_json::from_str::<serde_json::Value>(&snapshot) else { omitted += 1; continue; };
            let role = snapshot["internal_role"].as_str().unwrap_or("");
            if !canvas_history_id_valid(&activation) || !canvas_history_id_valid(&agent) || !canvas_history_id_valid(role)
                || snapshot["agent_id"].as_str() != Some(agent.as_str())
                || !matches!(state.as_str(), "queued" | "working" | "waiting_user" | "blocked" | "done") {
                omitted += 1; continue;
            }
            actors.insert(agent.clone());
            // The immutable activated participant remains valid after removal.
            roster.insert(agent.clone());
            canvas_group_bind_alias(&mut aliases, &agent, role);
            let exact_receipt = receipt.as_ref().and_then(|id| canonical.iter().find(|row|
                row["role"] == "group_message" && row["message_id"].as_str() == Some(id.as_str())
                    && row["turn_id"].as_str() == Some(*turn) && row["group_id"].as_str() == Some(owner.id.as_str())
                    && row["agent_id"].as_str() == Some(agent.as_str())));
            let failed_receipt = exact_receipt.is_some_and(|row| canvas_group_result_failed(
                row["subject"].as_str().unwrap_or(""), row["markdown"].as_str().unwrap_or("")));
            let recovered_state = match state.as_str() {
                _ if failed_receipt => "blocked",
                "blocked" => "blocked", "done" if exact_receipt.is_some() => "done", _ => "inactive",
            };
            let recovered_detail = if failed_receipt {
                "The saved response reports that this work did not finish."
            } else if recovered_state == "inactive" {
                "Saved activity; no completed response is available in this history snapshot."
            } else { detail.as_str() };
            let public_text = |field: &str, max: usize| snapshot[field].as_str().unwrap_or("").chars().take(max).collect::<String>();
            let public_snapshot = serde_json::json!({"agent_id":agent,"internal_role":role,
                "display_name":public_text("display_name",160),"role_title":public_text("role_title",160),
                "color":public_text("color",32),"icon_seed":public_text("icon_seed",160)});
            let status = serde_json::json!({
                "role":"group_member_status", "kind":"group_member_status", "historical":true,
                "history_id":format!("group-history:{session}:{activation}"), "activation_id":activation,
                "group_id":owner.id, "turn_id":turn, "agent_id":agent,
                "agent_name":public_snapshot["display_name"], "agent_snapshot":public_snapshot,
                "state":recovered_state, "recorded_state":state, "detail":recovered_detail,
            });
            let size = status.to_string().len();
            if recovery_bytes + size > RECOVERY_BYTES - 1024 { omitted += 1; }
            else { recovery_bytes += size; statuses.entry(turn.to_string()).or_default().push(status); }
        }
        turns.insert(turn.to_string(), actors);
    }
    let mut query = db.prepare("SELECT substr(message_id,1,257),substr(handoff_id,1,257),substr(reply_to,1,257),
        substr(causation_id,1,257),substr(from_agent,1,257),substr(to_agent,1,257),
        substr(subject,1,1025),substr(body,1,65537),reply_expected,state,as_of_seq,
        length(CAST(body AS BLOB)),message_kind FROM company_messages
        WHERE session_id=?1 AND run_id=?1 ORDER BY as_of_seq DESC,message_id LIMIT 257")
        .map_err(|_| "Saved group deliveries are unreadable.")?;
    let rows = query.query_map([session], |row| Ok((CanvasGroupDelivery {
        id:row.get(0)?, handoff:row.get(1)?, reply:row.get(2)?, cause:row.get(3)?,
        from:row.get(4)?, to:row.get(5)?, from_role:row.get(4)?, to_role:row.get(5)?, subject:row.get(6)?, body:row.get(7)?,
        requested:row.get::<_, i64>(8)? != 0, delivery_state:row.get(9)?, sequence:row.get(10)?,
    }, row.get::<_, usize>(11)?, row.get::<_, String>(12)?, row.get::<_, i64>(8)?)))
        .map_err(|_| "Saved group deliveries are unreadable.")?;
    let mut deliveries = Vec::new();
    let mut count = 0;
    let mut bytes = 0;
    for row in rows {
        count += 1;
        if count > LIMIT { omitted += 1; break; }
        let (mut row, body_bytes, kind, requested_flag) = row.map_err(|_| "Saved group delivery is malformed.")?;
        let (Some(Some(from)), Some(Some(to))) = (aliases.get(&row.from), aliases.get(&row.to)) else {
            omitted += 1; continue;
        };
        row.from = from.clone(); row.to = to.clone();
        if body_bytes > BODY_LIMIT || row.subject.len() > 1024 || kind != "conversation" || !matches!(requested_flag, 0 | 1) || row.sequence < 0
            || ![&row.id, &row.handoff, &row.from, &row.to].into_iter().all(|id| canvas_history_id_valid(id))
            || row.reply.as_ref().is_some_and(|id| !canvas_history_id_valid(id))
            || row.cause.as_ref().is_some_and(|id| !canvas_history_id_valid(id))
            || !roster.contains(&row.from) || !roster.contains(&row.to) || row.from == row.to
            || !matches!(row.delivery_state.as_str(), "accepted" | "suspended" | "injected" | "canceled")
            || row.body.trim_start().starts_with("<!-- phoenix-message-priority:")
            || row.body.trim_start().starts_with("<!--phoenix-message-priority:") {
            omitted += 1; continue;
        }
        if bytes + body_bytes > RECOVERY_BYTES { omitted += 1; continue; }
        bytes += body_bytes;
        deliveries.push(row);
    }
    deliveries.sort_by(|a,b| a.sequence.cmp(&b.sequence).then_with(|| a.id.cmp(&b.id)));
    let mut references = HashMap::new();
    let mut ambiguous = HashSet::new();
    for (index, row) in deliveries.iter().enumerate() {
        for id in std::iter::once(&row.id).chain(row.requested.then_some(&row.handoff)) {
            if turns.contains_key(id) || references.insert(id.clone(), index).is_some_and(|prior| prior != index) {
                ambiguous.insert(id.clone());
            }
        }
    }
    for row in &deliveries {
        if ambiguous.contains(&row.id) || (row.requested && ambiguous.contains(&row.handoff)) {
            references.remove(&row.id);
            if row.requested { references.remove(&row.handoff); }
        }
    }
    let mut recovered: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
    for (index, row) in deliveries.iter().enumerate() {
        if references.get(&row.id) != Some(&index) { omitted += 1; continue; }
        let Some(turn) = canvas_group_delivery_turn(index, &deliveries, &references, &turns, &mut Vec::new()) else {
            omitted += 1; continue;
        };
        // A return is a delivered result only once its delivery was injected.
        // An accepted/canceled reply cannot close its originating handoff.
        if !row.requested && row.delivery_state != "injected" { omitted += 1; continue; }
        let failed = canvas_group_result_failed(&row.subject, &row.body);
        let role = if row.requested { "handoff" } else { "return" };
        let value = serde_json::json!({
            "role":role, "kind":role, "historical":true, "history_id":format!("group-history:{session}:{}:{role}", row.id),
            "group_id":owner.id, "turn_id":turn, "message_id":row.id, "handoff_id":row.handoff,
            "reply_to":row.reply, "causation_id":row.cause, "from":row.from, "to":row.to,
            "from_internal_role":row.from_role, "to_internal_role":row.to_role,
            "requester":if row.requested { &row.from } else { &row.to },
            "receiver":if row.requested { &row.to } else { &row.from },
            "agent":row.from, "subject":row.subject, "body":if row.requested { "" } else { &row.body },
            "reply_expected":row.requested, "delivery_state":row.delivery_state,
            "status":if row.requested { if row.delivery_state == "canceled" { "canceled" } else { "inactive" } }
                else if failed { "blocked" } else { "returned" },
            // The delivered body is evidence of a return, not proof of success.
            "ok":if !row.requested && failed { serde_json::json!(false) } else { serde_json::Value::Null },
        });
        let size = value.to_string().len();
        if recovery_bytes + size > RECOVERY_BYTES - 1024 { omitted += 1; }
        else { recovery_bytes += size; recovered.entry(turn).or_default().push(value); }
    }
    let mut output = Vec::new();
    for turn in &authored {
        output.extend(recovered.remove(*turn).unwrap_or_default());
        output.extend(statuses.remove(*turn).unwrap_or_default());
    }
    // Rows that cannot be verified are skipped quietly: a diagnostic banner
    // in the room reads as an error to the user and offers nothing to act on.
    if omitted > 0 {
        eprintln!("phoenix: skipped {omitted} unverifiable saved team rows in {session}");
    }
    Ok(output)
}

/// The newest folded messages from `<session>.archive.jsonl`, oldest first.
/// A missing, oversized or unreadable archive only costs old history rows.
/// Rebuild an edit's diff from its saved tool input, so a conversation
/// restored from history still shows real `+N −N` counts and a reviewable
/// diff (live events carry the diff; history rows only kept the path).
/// Lines shared at the start and end of old/new text are not changes.
fn history_edit_diff(tool: &str, input: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(input) else { return String::new() };
    let field = |keys: &[&str]| keys.iter().find_map(|key| value.get(*key).and_then(|v| v.as_str())).unwrap_or("");
    let mut out = String::new();
    match tool {
        "str_replace" | "edit" | "edit_file" | "replace" => {
            let old = field(&["old_str", "old_string", "old_text"]).lines().collect::<Vec<_>>();
            let new = field(&["new_str", "new_string", "new_text"]).lines().collect::<Vec<_>>();
            let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
            let suffix = old[prefix..].iter().rev().zip(new[prefix..].iter().rev()).take_while(|(a, b)| a == b).count();
            for line in &old[prefix..old.len() - suffix] { out.push('-'); out.push_str(line); out.push('\n'); }
            for line in &new[prefix..new.len() - suffix] { out.push('+'); out.push_str(line); out.push('\n'); }
        }
        "write" | "write_file" | "create_file" => {
            for line in field(&["content", "text", "contents"]).lines() { out.push('+'); out.push_str(line); out.push('\n'); }
        }
        "apply_patch" => {
            for line in field(&["patch", "input", "diff"]).lines() {
                if (line.starts_with('+') && !line.starts_with("+++")) || (line.starts_with('-') && !line.starts_with("---")) {
                    out.push_str(line); out.push('\n');
                }
            }
        }
        _ => {}
    }
    // Bounded like the live story diff; lines past the cap are dropped.
    if out.len() > 8000 {
        let mut end = 8000;
        while !out.is_char_boundary(end) { end -= 1; }
        out.truncate(end);
        if let Some(last) = out.rfind('\n') { out.truncate(last + 1); }
    }
    out
}

fn archived_session_messages(home: &std::path::Path, session_id: &str) -> Vec<serde_json::Value> {
    use std::io::{Read, Seek, SeekFrom};
    // History shows only recent turns, so read the archive's tail: it grows
    // for the life of the session and is read on every conversation refresh.
    const TAIL_BYTES: u64 = 2 * 1024 * 1024;
    const MAX_ARCHIVED_MESSAGES: usize = 4_000;
    let path = home.join("sessions").join(format!("{session_id}.archive.jsonl"));
    let Ok(metadata) = std::fs::symlink_metadata(&path) else { return Vec::new() };
    if !metadata.is_file() { return Vec::new(); }
    let Ok(mut file) = std::fs::File::open(&path) else { return Vec::new() };
    let start = metadata.len().saturating_sub(TAIL_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() { return Vec::new(); }
    let mut bytes = Vec::new();
    if file.take(TAIL_BYTES).read_to_end(&mut bytes).is_err() { return Vec::new(); }
    let raw = String::from_utf8_lossy(&bytes);
    // A mid-file start lands inside a line; drop that partial first line.
    let raw = if start > 0 { raw.split_once('\n').map_or("", |(_, rest)| rest) } else { &raw };
    let mut archived: Vec<serde_json::Value> = raw.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|message| message.get("type").and_then(|kind| kind.as_str()).is_some())
        .collect();
    if archived.len() > MAX_ARCHIVED_MESSAGES {
        archived.drain(..archived.len() - MAX_ARCHIVED_MESSAGES);
    }
    archived
}

fn session_context_get_blocking_at(
    home: &std::path::Path,
    session_id: String,
    owner: &CanvasConversationOwnerRef,
) -> Result<Vec<serde_json::Value>, String> {
    validate_canvas_conversation_owner_at(home, &session_id, owner)?;
    validate_session_id_for_canvas(&session_id)?;
    let path = home.join("sessions").join(format!("{session_id}.json"));
    let Some(raw) = read_private_text(&path, SESSION_DISPLAY_MAX_BYTES, "session")? else {
        return Ok(Vec::new());
    };
    let doc = serde_json::from_str::<serde_json::Value>(&raw)
        .map_err(|error| format!("session {} is invalid: {error}", path.display()))?;
    let current = doc
        .get("messages")
        .and_then(|v| v.as_array())
        .ok_or_else(|| format!("session {} has no message array", path.display()))?;
    // Compaction folds old messages, including every user prompt of a long
    // run, out of the session file and archives them verbatim. Without them
    // the conversation lost its prompts and merged separate turns into one.
    let mut combined = archived_session_messages(home, &session_id);
    combined.extend(current.iter().cloned());
    let messages = &combined;

    let compaction_text = |text: &str| text.contains("[AUTO-COMPACTED HISTORY");

    let late_answer_text = |text: &str| -> Option<(String, Option<String>)> {
        let trimmed = text.trim_start();
        let (agent_id, envelope) = if let Some(rest) = trimmed.strip_prefix('@') {
            let split = rest.find(char::is_whitespace)?;
            (Some(rest[..split].to_string()), rest[split..].trim_start())
        } else {
            (None, trimmed)
        };
        if !envelope.starts_with("[late ask answer]") {
            return None;
        }
        // Current wording: `answered the saved question <id>: "…". Continue …`;
        // older rows say `had already ended: "…". This answer supersedes`.
        // Only knowing the old one dropped every late answer from history.
        let (answer_start, answer_end) = if let Some(at) = envelope.find("answered the saved question ") {
            let rest = &envelope[at..];
            let start = at + rest.find(": \"")? + 3;
            let end = ["\". Continue", "\". This answer"]
                .iter()
                .filter_map(|marker| envelope.rfind(marker))
                .max()?;
            (start, end)
        } else {
            let start_marker = "had already ended: \"";
            let end_marker = "\". This answer supersedes";
            (envelope.find(start_marker)? + start_marker.len(), envelope.rfind(end_marker)?)
        };
        if answer_end < answer_start {
            return None;
        }
        let raw = &envelope[answer_start..answer_end];
        let answers = raw
            .lines()
            .filter_map(|line| line.trim().strip_prefix("A: "))
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
        let display = if answers.is_empty() {
            raw.trim().to_string()
        } else {
            answers.join("\n\n")
        };
        (!display.is_empty()).then_some((display, agent_id))
    };

    let synthetic_user = |text: &str| {
        let text = text.trim_start();
        text.starts_with("[background return]")
            || text.starts_with("GOAL HEARTBEAT")
            || text.starts_with("[watcher wake]")
            || text.starts_with("[late ask answer]")
            || text.starts_with("[queued wake]")
            || text
                .strip_prefix('@')
                .and_then(|value| value.split_once(char::is_whitespace))
                .is_some_and(|(_, value)| value.trim_start().starts_with("[late ask answer]"))
    };
    let short = |text: &str, limit: usize| -> String {
        let mut out: String = text.chars().take(limit).collect();
        if text.chars().count() > limit {
            out.push('…');
        }
        out
    };
    let tool_target = |input: &str, tool: &str| -> String {
        serde_json::from_str::<serde_json::Value>(input)
            .ok()
            .and_then(|value| {
                ["path", "query", "url", "to", "command", "cwd"]
                    .iter()
                    .find_map(|key| value.get(key).and_then(|v| v.as_str()))
                    .map(|value| short(value, 140))
            })
            .unwrap_or_else(|| {
                let trimmed = input.trim();
                if trimmed.is_empty() {
                    tool.to_string()
                } else {
                    short(trimmed, 140)
                }
            })
    };

    let mut recent = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        let kind = message.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let row = match kind {
            "User" => {
                let raw_text = message
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                let (text, attachments) = canvas_user_message_parts(raw_text);
                let text = text.trim();
                if text.is_empty() || compaction_text(text) {
                    continue;
                }
                if let Some((display, agent_id)) = late_answer_text(text) {
                    serde_json::json!({
                        "role": "user", "text": display,
                        "origin": {
                            "kind": "ask_answer", "ask_id": "",
                            "agent_id": agent_id, "display": display,
                        },
                    })
                } else if synthetic_user(text) {
                    continue;
                } else if let Some((turn_id, origin)) = routine_turn_metadata(text) {
                    serde_json::json!({
                        "role": "user", "text": text,
                        "turn_id": turn_id, "origin": origin,
                    })
                } else {
                    serde_json::json!({
                        "role": "user",
                        "text": text,
                        "attachments": attachments,
                        "turn_id": messages.get(index + 1).filter(|next| {
                            owner.kind == CanvasConversationOwnerKind::Group
                                && next.get("type").and_then(|v| v.as_str()) == Some("ToolResult")
                                && next.get("tool_name").and_then(|v| v.as_str()) == Some("__phoenix_group_user_boundary")
                        }).and_then(|next| next.get("input")).cloned().unwrap_or(serde_json::Value::Null),
                    })
                }
            }
            "Assistant" => {
                if owner.kind == CanvasConversationOwnerKind::Group {
                    // Canonical rooms persist one attributed GroupContribution
                    // per coworker. The compatibility Assistant aggregate is
                    // the same answers concatenated and would render a second,
                    // giant response after the real conversation.
                    continue;
                }
                let text = message
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if text.is_empty() || compaction_text(text) {
                    continue;
                }
                // Content immediately followed by a tool/talk is model
                // narration accompanying an action, not a user-facing answer.
                let next_real = messages[index + 1..].iter().find(|next| {
                    let next_kind = next.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    if next_kind == "Assistant" {
                        return next
                            .get("content")
                            .and_then(|v| v.as_str())
                            .is_some_and(|v| !v.trim().is_empty());
                    }
                    if next_kind == "User" {
                        let value = next.get("content").and_then(|v| v.as_str()).unwrap_or("");
                        // A late answer is the user replying: the text before
                        // it was a finished reply, not narration of more work.
                        return !synthetic_user(value) || late_answer_text(value).is_some();
                    }
                    true
                });
                if next_real.is_some_and(|next| {
                    matches!(
                        next.get("type").and_then(|v| v.as_str()),
                        Some("ToolResult" | "Talk")
                    )
                }) {
                    serde_json::json!({
                        "role": "narration",
                        "agent": owner.id,
                        "text": text,
                    })
                } else {
                    serde_json::json!({ "role": "answer", "text": text })
                }
            }
            "Talk" => {
                let from = message
                    .get("from")
                    .and_then(|v| v.as_str())
                    .unwrap_or("agent");
                let to = message
                    .get("to")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Phoenix");
                let body = message
                    .get("body")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                let subject = message
                    .get("subject")
                    .and_then(|v| v.as_str())
                    .unwrap_or("handoff");
                let reply_expected = message
                    .get("reply_expected")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let handoff_id = message
                    .get("handoff_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                // A message the user sent while the agent was working (a steer)
                // is stored as a note to the agent. Show it as the user's own
                // message, without the runtime's instruction suffix.
                if from == "user" {
                    const STEER_SUFFIX: &str = "(This arrived WHILE you are working.";
                    let text = body.split(STEER_SUFFIX).next().unwrap_or(body).trim();
                    if !text.is_empty() {
                        let row = serde_json::json!({"role": "user", "text": text, "steered": true});
                        if recent.last() != Some(&row) { recent.push(row); }
                    }
                    continue;
                }
                serde_json::json!({
                    "role": "talk", "from": from, "to": to, "subject": subject,
                    "text": body, "ok": true, "reply_expected": reply_expected,
                    "handoff_id": handoff_id,
                    "reply_to": message.get("reply_to").cloned().unwrap_or(serde_json::Value::Null),
                    "causation_id": message.get("causation_id").cloned().unwrap_or(serde_json::Value::Null),
                    "status": message.get("status").cloned().unwrap_or(serde_json::Value::Null),
                    "background": subject.contains("background return"),
                })
            }
            "GroupContribution" => {
                let turn_id = message
                    .get("turn_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                let message_id = message
                    .get("message_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                let agent_id = message
                    .get("agent_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or("agent");
                let display_name = message
                    .get("display_name")
                    .and_then(|value| value.as_str())
                    .unwrap_or(agent_id);
                let body = message
                    .get("body")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                serde_json::json!({
                    "role": "group_message",
                    "turn_id": turn_id,
                    "message_id": message_id,
                    "group_id": message.get("group_id").cloned().unwrap_or(serde_json::Value::Null),
                    "agent_id": agent_id,
                    "agent_name": display_name,
                    "markdown": body,
                    "subject": message.get("subject").cloned().unwrap_or(serde_json::Value::Null),
                    "reply_to": message.get("reply_to").cloned().unwrap_or(serde_json::Value::Null),
                    "causation_id": message.get("causation_id").cloned().unwrap_or(serde_json::Value::Null),
                    "agent_snapshot": {
                        "agent_id": agent_id,
                        "internal_role": message.get("internal_role").cloned().unwrap_or_else(|| serde_json::Value::String(agent_id.to_string())),
                        "display_name": display_name,
                        "role_title": message.get("role_title").cloned().unwrap_or(serde_json::Value::Null),
                        "color": message.get("color").cloned().unwrap_or(serde_json::Value::Null),
                        "icon_seed": message.get("icon_seed").cloned().unwrap_or(serde_json::Value::Null),
                        "avatar": message.get("avatar").cloned().unwrap_or(serde_json::Value::Null),
                    }
                })
            }
            "ToolResult" => {
                let tool = message
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool");
                if tool == "__phoenix_group_user_boundary" { continue; }
                let input = message.get("input").and_then(|v| v.as_str()).unwrap_or("");
                let success = message
                    .get("success")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let output = message.get("output").and_then(|v| v.as_str()).unwrap_or("");
                serde_json::json!({
                    "role": "tool", "agent": if owner.kind == CanvasConversationOwnerKind::Agent { Some(owner.id.as_str()) } else { None }, "tool": tool,
                    "target": tool_target(input, tool), "ok": success,
                    "detail": if success { "".to_string() } else { short(output, 400) },
                    "diff": if success { history_edit_diff(tool, input) } else { String::new() }
                })
            }
            _ => continue,
        };
        if recent.last() == Some(&row) {
            continue;
        }
        recent.push(row);
    }
    let recent = keep_recent_complete_context_turns(recent, CONTEXT_HISTORY_ROWS);
    if !matches!(owner.kind, CanvasConversationOwnerKind::Group) { return Ok(recent); }
    let recovered = match canvas_group_history_at(home, &session_id, owner, &recent) {
        Ok(rows) => rows,
        Err(reason) => recent.iter().rev().find(|row| row["role"] == "user")
            .and_then(|row| row["turn_id"].as_str()).map(|turn|
                vec![canvas_group_history_notice(&session_id, turn, &format!("Team history could not be recovered. {reason}"))])
            .unwrap_or_default(),
    };
    let mut output = Vec::new();
    for row in recent {
        let turn = if row["role"] == "user" { row["turn_id"].as_str().map(str::to_string) } else { None };
        output.push(row);
        if let Some(turn) = turn {
            output.extend(recovered.iter().filter(|row| row["turn_id"].as_str() == Some(turn.as_str())).cloned());
        }
    }
    Ok(output)
}

#[tauri::command]
async fn session_context_get(
    session_id: String,
    owner: CanvasConversationOwnerRef,
) -> Result<Vec<serde_json::Value>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        session_context_get_blocking_at(&phoenix_home(), session_id, &owner)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn feeds_set(
    session_id: String,
    owner: CanvasConversationOwnerRef,
    feeds: serde_json::Value,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        validate_canvas_conversation_owner_at(&phoenix_home(), &session_id, &owner)?;
        feeds_set_at(&feeds_file(&session_id)?, feeds)
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Apply only the terminal snapshots that changed. The read, merge, and
/// replacement share one private-file lease so concurrent agent streams cannot
/// lose each other's updates. String values set a feed; null removes one.
#[tauri::command]
async fn feeds_patch(
    session_id: String,
    owner: CanvasConversationOwnerRef,
    updates: serde_json::Value,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        validate_canvas_conversation_owner_at(&phoenix_home(), &session_id, &owner)?;
        feeds_patch_at(&feeds_file(&session_id)?, updates)
    })
    .await
    .map_err(|error| error.to_string())?
}

fn feeds_patch_at(path: &std::path::Path, updates: serde_json::Value) -> Result<(), String> {
    let serde_json::Value::Object(updates) = updates else {
        return Err("Phoenix conversation feed updates must be a JSON object".into());
    };
    if let Some((key, _)) = updates
        .iter()
        .find(|(_, value)| !value.is_string() && !value.is_null())
    {
        return Err(format!(
            "Phoenix conversation feed update '{key}' must be a string or null"
        ));
    }

    with_private_file_lock(path, || {
        let current =
            read_private_bounded_unlocked(path, FEEDS_MAX_BYTES, "Phoenix conversation feed")?;
        let mut feeds = match current.as_deref() {
            None => serde_json::Map::new(),
            Some(bytes) => {
                let existing: serde_json::Value =
                    serde_json::from_slice(bytes).map_err(|error| {
                        format!(
                            "Phoenix conversation feed {} is invalid; refusing to erase it: {error}",
                            path.display()
                        )
                    })?;
                let serde_json::Value::Object(existing) = existing else {
                    return Err(format!(
                        "Phoenix conversation feed {} must be a JSON object; refusing to erase it",
                        path.display()
                    ));
                };
                existing
            }
        };

        for (key, value) in updates {
            match value {
                serde_json::Value::String(value) => {
                    feeds.insert(key, serde_json::Value::String(value));
                }
                serde_json::Value::Null => {
                    feeds.remove(&key);
                }
                _ => unreachable!("feed patch values were validated before locking"),
            }
        }

        let json = serde_json::to_string(&feeds).map_err(|error| error.to_string())?;
        if json.len() > FEEDS_MAX_BYTES {
            return Err(format!(
                "Phoenix conversation feeds exceed the {FEEDS_MAX_BYTES}-byte limit"
            ));
        }
        write_private_atomic_unlocked(path, json.as_bytes())
    })
}

fn feeds_set_at(path: &std::path::Path, feeds: serde_json::Value) -> Result<(), String> {
    if !feeds.is_object() {
        return Err("Phoenix conversation feeds must be a JSON object".into());
    }
    let json = serde_json::to_string(&feeds).map_err(|error| error.to_string())?;
    if json.len() > FEEDS_MAX_BYTES {
        return Err(format!(
            "Phoenix conversation feeds exceed the {FEEDS_MAX_BYTES}-byte limit"
        ));
    }
    with_private_file_lock(path, || {
        if let Some(current) =
            read_private_bounded_unlocked(path, FEEDS_MAX_BYTES, "Phoenix conversation feed")?
        {
            let existing: serde_json::Value =
                serde_json::from_slice(&current).map_err(|error| {
                    format!(
                        "Phoenix conversation feed {} is invalid; refusing to erase it: {error}",
                        path.display()
                    )
                })?;
            if !existing.is_object() {
                return Err(format!(
                    "Phoenix conversation feed {} must be a JSON object; refusing to erase it",
                    path.display()
                ));
            }
        }
        write_private_atomic_unlocked(path, json.as_bytes())
    })
}

#[derive(Debug, Serialize)]
struct BrowserDownloadView {
    name: String,
    path: String,
    bytes: u64,
    modified_ms: u128,
    complete: bool,
}

fn browser_download_dir(profile_id: &str) -> Result<PathBuf, String> {
    let profile_id = if profile_id.is_empty() {
        "agent-phoenix"
    } else {
        profile_id
    };
    if profile_id.len() > 128
        || !profile_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return Err("invalid browser profile id".into());
    }
    Ok(phoenix_home().join("downloads").join(profile_id))
}

#[tauri::command]
fn browser_downloads_list(profile_id: String) -> Result<Vec<BrowserDownloadView>, String> {
    browser_downloads_list_at(&browser_download_dir(&profile_id)?)
}

fn browser_downloads_list_at(root: &std::path::Path) -> Result<Vec<BrowserDownloadView>, String> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&root).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        let metadata = entry.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() || file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        let raw_name = entry.file_name().to_string_lossy().into_owned();
        let complete = !raw_name.ends_with(".crdownload");
        let name = raw_name
            .strip_suffix(".crdownload")
            .unwrap_or(&raw_name)
            .to_string();
        let modified_ms = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis())
            .unwrap_or(0);
        files.push(BrowserDownloadView {
            name,
            path: path.display().to_string(),
            bytes: metadata.len(),
            modified_ms,
            complete,
        });
    }
    files.sort_by(|left, right| right.modified_ms.cmp(&left.modified_ms));
    files.truncate(32);
    Ok(files)
}

fn validate_browser_download_path(path: &str) -> Result<PathBuf, String> {
    let requested = std::fs::canonicalize(path).map_err(|error| error.to_string())?;
    let root = phoenix_home().join("downloads");
    std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    let root = std::fs::canonicalize(root).map_err(|error| error.to_string())?;
    if !requested.starts_with(&root) || !requested.is_file() {
        return Err("download path is outside Phoenix's managed download folder".into());
    }
    Ok(requested)
}

#[tauri::command]
fn browser_download_open(path: String, reveal: bool) -> Result<(), String> {
    let requested = validate_browser_download_path(&path)?;
    let target = if reveal {
        requested
            .parent()
            .ok_or_else(|| "download has no containing folder".to_string())?
            .to_path_buf()
    } else {
        requested
    };
    std::thread::Builder::new()
        .name("phoenix-download-opener".into())
        .spawn(move || {
            let _ = std::process::Command::new("xdg-open")
                .arg(target)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        })
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Open a Markdown-linked local artifact without granting a conversation an
/// arbitrary file launcher. Relative links resolve against the selected
/// workspace; absolute links must still remain inside that same boundary.
fn resolve_workspace_file(
    path: &str,
    workspace: Option<&str>,
) -> Result<std::path::PathBuf, String> {
    let workspace = workspace
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(workspace_default_path)?;
    let workspace = std::fs::canonicalize(&workspace)
        .map_err(|error| format!("could not resolve workspace: {error}"))?;
    let requested = PathBuf::from(path.trim());
    let requested = if requested.is_absolute() {
        requested
    } else {
        workspace.join(requested)
    };
    let requested = std::fs::canonicalize(&requested)
        .map_err(|error| format!("could not resolve linked file: {error}"))?;
    if requested.starts_with(&workspace) && (requested.is_file() || requested.is_dir()) {
        return Ok(requested);
    }
    // Agents also link the user's own files and folders elsewhere in home,
    // like a design saved to Downloads. Folders open in the file manager and
    // plain documents/media in their viewer; anything that could run code
    // outside the project stays blocked.
    const VIEWABLE: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "avif", "svg", "pdf", "txt", "md",
        "json", "csv", "html", "htm", "mp4", "webm", "mov", "mp3", "wav"];
    let home = std::env::var_os("HOME").map(PathBuf::from).and_then(|home| std::fs::canonicalize(home).ok());
    let viewable = requested.is_dir() || (requested.is_file() && requested.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| VIEWABLE.contains(&extension.to_ascii_lowercase().as_str())));
    if home.is_some_and(|home| requested.starts_with(home)) && viewable {
        return Ok(requested);
    }
    Err("linked file is outside the selected workspace".into())
}

/// The latest context reading the gateway saved for a conversation, so the
/// composer shows how full it is before the agent's next live sample.
#[tauri::command]
fn context_usage_get(session_id: String) -> Result<Option<serde_json::Value>, String> {
    validate_session_id_for_canvas(&session_id)?;
    let path = phoenix_home().join("context_usage").join(format!("{session_id}.json"));
    let Some(raw) = read_private_text(&path, 4096, "context usage")? else { return Ok(None) };
    Ok(serde_json::from_str(&raw).ok())
}

#[tauri::command]
fn open_workspace_file(path: String, workspace: Option<String>) -> Result<(), String> {
    let requested = resolve_workspace_file(&path, workspace.as_deref())?;
    std::thread::Builder::new()
        .name("phoenix-workspace-file-opener".into())
        .spawn(move || {
            let _ = std::process::Command::new("xdg-open")
                .arg(requested)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        })
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod workspace_file_link_tests {
    use super::resolve_workspace_file;

    #[test]
    fn local_markdown_links_stay_inside_the_selected_workspace() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let workspace = std::env::temp_dir().join(format!(
            "phoenix-workspace-link-test-{}-{nonce}",
            std::process::id()
        ));
        let outside_dir = std::env::temp_dir().join(format!(
            "phoenix-workspace-link-outside-{}-{nonce}",
            std::process::id()
        ));
        let artifact_dir = workspace.join("artifacts");
        std::fs::create_dir_all(&artifact_dir).unwrap();
        let artifact = artifact_dir.join("report.md");
        std::fs::write(&artifact, "# Report\n").unwrap();
        std::fs::create_dir(&outside_dir).unwrap();
        let outside = outside_dir.join("outside.md");
        std::fs::write(&outside, "# Outside\n").unwrap();
        let workspace_text = workspace.to_string_lossy();
        assert_eq!(
            resolve_workspace_file("artifacts/report.md", Some(&workspace_text)).unwrap(),
            artifact
        );
        assert!(resolve_workspace_file(
            outside.to_string_lossy().as_ref(),
            Some(&workspace_text)
        )
        .is_err());
        // Home folders and viewable files open; anything runnable does not.
        let home_dir = std::path::PathBuf::from(std::env::var_os("HOME").unwrap())
            .join(format!(".cache/phoenix-link-test-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&home_dir).unwrap();
        std::fs::write(home_dir.join("mark.svg"), "<svg/>").unwrap();
        std::fs::write(home_dir.join("run.sh"), "echo hi").unwrap();
        for (path, allowed) in [(home_dir.clone(), true), (home_dir.join("mark.svg"), true), (home_dir.join("run.sh"), false)] {
            assert_eq!(resolve_workspace_file(path.to_string_lossy().as_ref(), Some(&workspace_text)).is_ok(), allowed, "{}", path.display());
        }
        std::fs::remove_dir_all(&home_dir).unwrap();
        std::fs::remove_dir_all(&workspace).unwrap();
        std::fs::remove_dir_all(&outside_dir).unwrap();
    }
}

/// Open a URL in the system browser. WebKitGTK swallows `target="_blank"`
/// navigations inside the webview, so markdown links in answers were dead
/// clicks until routed out here.
#[tauri::command]
fn open_external(url: String) -> Result<(), String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("only http(s) links can be opened".into());
    }
    let child = std::process::Command::new("xdg-open")
        .arg(&url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|error| error.to_string())?;
    // `xdg-open` normally hands the URL to an existing browser and exits in
    // milliseconds. Keep and reap its Child handle so repeated link clicks do
    // not accumulate zombies; cap only the helper itself (never an unrelated
    // browser process it may have launched into a separate session).
    let child = Arc::new(Mutex::new(child));
    let reaper_child = child.clone();
    match std::thread::Builder::new()
        .name("phoenix-xdg-open-reaper".into())
        .spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                let mut child = reaper_child
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                match child.try_wait() {
                    Ok(Some(_)) => return,
                    Ok(None) if std::time::Instant::now() < deadline => {
                        drop(child);
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    Ok(None) | Err(_) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return;
                    }
                }
            }
        }) {
        Ok(_) => Ok(()),
        Err(error) => {
            let mut child = child
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let _ = child.kill();
            let _ = child.wait();
            Err(format!("could not start URL-opener reaper: {error}"))
        }
    }
}

/// Delete a thread's transcript file — canvas delete scrubs its threads so a
/// deleted canvas leaves nothing behind to resurrect at the next boot.
#[tauri::command]
fn feeds_delete(session_id: String) -> Result<(), String> {
    match std::fs::remove_file(feeds_file(&session_id)?) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CanvasImageKind {
    Png,
    Jpeg,
    Webp,
    Gif,
    Avif,
}

impl CanvasImageKind {
    fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
            Self::Gif => "image/gif",
            Self::Avif => "image/avif",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Webp => "webp",
            Self::Gif => "gif",
            Self::Avif => "avif",
        }
    }

    fn from_mime(mime: &str) -> Option<Self> {
        match mime {
            "image/png" => Some(Self::Png),
            "image/jpeg" => Some(Self::Jpeg),
            "image/webp" => Some(Self::Webp),
            "image/gif" => Some(Self::Gif),
            "image/avif" => Some(Self::Avif),
            _ => None,
        }
    }

    fn from_extension(extension: &str) -> Option<Self> {
        match extension {
            "png" => Some(Self::Png),
            "jpg" | "jpeg" => Some(Self::Jpeg),
            "webp" => Some(Self::Webp),
            "gif" => Some(Self::Gif),
            "avif" => Some(Self::Avif),
            _ => None,
        }
    }
}

fn detect_canvas_image_kind(bytes: &[u8]) -> Option<CanvasImageKind> {
    if bytes.len() >= 33
        && bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        && bytes[8..12] == [0, 0, 0, 13]
        && &bytes[12..16] == b"IHDR"
        && bytes[16..20] != [0, 0, 0, 0]
        && bytes[20..24] != [0, 0, 0, 0]
        && matches!(bytes[24], 1 | 2 | 4 | 8 | 16)
        && matches!(bytes[25], 0 | 2 | 3 | 4 | 6)
        && bytes[26] == 0
        && bytes[27] == 0
        && bytes[28] <= 1
    {
        return Some(CanvasImageKind::Png);
    }
    if bytes.len() >= 4 && bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some(CanvasImageKind::Jpeg);
    }
    if bytes.len() >= 10
        && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"))
        && bytes[6..8] != [0, 0]
        && bytes[8..10] != [0, 0]
    {
        return Some(CanvasImageKind::Gif);
    }
    if bytes.len() >= 16
        && bytes.starts_with(b"RIFF")
        && &bytes[8..12] == b"WEBP"
        && (&bytes[12..16] == b"VP8 " || &bytes[12..16] == b"VP8L" || &bytes[12..16] == b"VP8X")
    {
        return Some(CanvasImageKind::Webp);
    }
    if bytes.len() >= 16 && &bytes[4..8] == b"ftyp" {
        let box_size = u32::from_be_bytes(bytes[0..4].try_into().ok()?) as usize;
        if (16..=bytes.len()).contains(&box_size) {
            let is_avif_brand = |brand: &[u8]| brand == b"avif" || brand == b"avis";
            if is_avif_brand(&bytes[8..12])
                || bytes[16..box_size].chunks_exact(4).any(is_avif_brand)
            {
                return Some(CanvasImageKind::Avif);
            }
        }
    }
    None
}

fn canonical_allowed_file(
    path: &std::path::Path,
    workspace: &std::path::Path,
    home: &std::path::Path,
) -> Result<PathBuf, String> {
    let canonical = std::fs::canonicalize(path).map_err(|error| error.to_string())?;
    let workspace = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    if !canonical.starts_with(&workspace) && !canonical.starts_with(&home) {
        return Err("path outside allowed directories (workspace or home)".into());
    }
    Ok(canonical)
}

fn image_data_url_at(
    path: &std::path::Path,
    workspace: &std::path::Path,
    home: &std::path::Path,
) -> Result<String, String> {
    let canonical = canonical_allowed_file(path, workspace, home)?;
    let extension = canonical
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let declared = CanvasImageKind::from_extension(&extension)
        .ok_or_else(|| format!("not an image file: .{extension}"))?;
    let bytes = read_private_bounded_unlocked(&canonical, INLINE_IMAGE_MAX_BYTES, "Phoenix image")?
        .ok_or_else(|| "image disappeared while opening it".to_string())?;
    let detected = detect_canvas_image_kind(&bytes)
        .ok_or_else(|| "file does not contain a supported image signature".to_string())?;
    if detected != declared {
        return Err(format!(
            "image extension .{extension} does not match its actual {} content",
            detected.mime()
        ));
    }
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:{};base64,{encoded}", detected.mime()))
}

/// Read a local image as a `data:` URL for a canvas image window. The asset
/// protocol's scope can't cover agent screenshots (they land anywhere under
/// the workspace/home), and an out-of-scope asset:// URL fails SILENTLY —
/// that was "screenshots won't open on canvases". Metadata is checked before
/// a bounded read, and the extension must match the actual image signature.
#[tauri::command]
async fn image_data_url(path: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        image_data_url_at(
            std::path::Path::new(&path),
            &std::env::current_dir().map_err(|error| error.to_string())?,
            &dirs_home(),
        )
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Read a file from disk for @-mention injection into a turn. Security-bounded:
/// the path must resolve inside the workspace (current dir) or the user's home
/// directory — arbitrary system paths are rejected.
#[tauri::command]
async fn read_file(path: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        read_file_at(
            std::path::Path::new(&path),
            &std::env::current_dir().map_err(|error| error.to_string())?,
            &dirs_home(),
        )
    })
    .await
    .map_err(|error| error.to_string())?
}

fn read_file_at(
    path: &std::path::Path,
    workspace: &std::path::Path,
    home: &std::path::Path,
) -> Result<String, String> {
    let canonical = canonical_allowed_file(path, workspace, home)?;
    let bytes = read_private_bounded_unlocked(&canonical, TEXT_FILE_MAX_BYTES, "text file")?
        .ok_or_else(|| "file disappeared while opening it".to_string())?;
    String::from_utf8(bytes).map_err(|error| format!("file is not valid UTF-8 text: {error}"))
}

fn bounded_base64_decode(payload: &str, max_decoded_bytes: usize) -> Result<Vec<u8>, String> {
    let max_encoded_bytes = max_decoded_bytes
        .checked_add(2)
        .and_then(|value| value.checked_div(3))
        .and_then(|value| value.checked_mul(4))
        .ok_or_else(|| "attachment size limit overflow".to_string())?;
    if payload.is_empty() {
        return Err("base64 image payload is empty".into());
    }
    if payload.len() > max_encoded_bytes {
        return Err(format!(
            "encoded image exceeds the {max_decoded_bytes}-byte decoded limit"
        ));
    }
    if payload.len() % 4 != 0 {
        return Err("base64 image payload must use standard padding".into());
    }
    let padding = payload
        .as_bytes()
        .iter()
        .rev()
        .take_while(|byte| **byte == b'=')
        .count();
    if padding > 2 {
        return Err("base64 image payload has invalid padding".into());
    }
    let data_len = payload.len().saturating_sub(padding);
    if payload.as_bytes()[..data_len]
        .iter()
        .any(|byte| !byte.is_ascii_alphanumeric() && *byte != b'+' && *byte != b'/')
        || payload.as_bytes()[data_len..]
            .iter()
            .any(|byte| *byte != b'=')
    {
        return Err("base64 image payload contains invalid characters".into());
    }
    let decoded_len = payload
        .len()
        .checked_div(4)
        .and_then(|groups| groups.checked_mul(3))
        .and_then(|bytes| bytes.checked_sub(padding))
        .ok_or_else(|| "base64 image length overflow".to_string())?;
    if decoded_len > max_decoded_bytes {
        return Err(format!(
            "decoded image exceeds the {max_decoded_bytes}-byte limit"
        ));
    }
    let mut decoded = vec![0_u8; decoded_len];
    use base64::Engine;
    let written = base64::engine::general_purpose::STANDARD
        .decode_slice(payload, &mut decoded)
        .map_err(|error| format!("base64 decode failed: {error}"))?;
    decoded.truncate(written);
    Ok(decoded)
}

fn content_addressed_attachment_path(
    directory: &std::path::Path,
    bytes: &[u8],
    extension: &str,
) -> PathBuf {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    directory.join(format!("sha256-{digest}.{extension}"))
}

fn save_attachment_at(home: &std::path::Path, data_url: &str) -> Result<PathBuf, String> {
    let max_encoded_bytes = INLINE_IMAGE_MAX_BYTES.div_ceil(3).saturating_mul(4);
    if data_url.len() > "data:".len() + DATA_URL_META_MAX_BYTES + 1 + max_encoded_bytes {
        return Err("image data URL exceeds the attachment size limit".into());
    }
    let rest = data_url
        .strip_prefix("data:")
        .ok_or_else(|| "not a data URL".to_string())?;
    let comma = rest
        .find(',')
        .ok_or_else(|| "malformed data URL (no comma)".to_string())?;
    if comma > DATA_URL_META_MAX_BYTES {
        return Err("image data URL metadata is too long".into());
    }
    let (meta, payload_with_comma) = rest.split_at(comma);
    let payload = &payload_with_comma[1..];
    let mut parts = meta.split(';');
    let mime = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    let declared = CanvasImageKind::from_mime(&mime)
        .ok_or_else(|| format!("unsupported image MIME type `{mime}`"))?;
    let parameters: Vec<_> = parts.collect();
    if parameters.len() != 1 || !parameters[0].eq_ignore_ascii_case("base64") {
        return Err("image data URL must use exactly the `;base64` encoding".into());
    }
    let bytes = bounded_base64_decode(payload, INLINE_IMAGE_MAX_BYTES)?;
    let detected = detect_canvas_image_kind(&bytes)
        .ok_or_else(|| "attachment does not contain a supported image signature".to_string())?;
    if detected != declared {
        return Err(format!(
            "declared MIME {mime} does not match actual {} content",
            detected.mime()
        ));
    }

    let directory = home.join("attachments");
    let default_root = dirs_home().join(".phoenix");
    secure_private_directory_at(&directory, home, &default_root, home != default_root)?;
    let path = content_addressed_attachment_path(&directory, &bytes, detected.extension());
    match read_private_bounded_unlocked(&path, INLINE_IMAGE_MAX_BYTES, "attachment")? {
        Some(existing) if existing == bytes => return Ok(path),
        Some(_) => {
            return Err(format!(
                "content-addressed attachment {} does not match its digest",
                path.display()
            ))
        }
        None => write_private_atomic_unlocked(&path, &bytes)?,
    }
    let stored = read_private_bounded_unlocked(&path, INLINE_IMAGE_MAX_BYTES, "attachment")?
        .ok_or_else(|| "content-addressed attachment vanished after save".to_string())?;
    if stored != bytes {
        return Err("content-addressed attachment failed post-write verification".into());
    }
    Ok(path)
}

fn valid_avatar_id(value: &str) -> bool {
    let Some((stem, extension)) = value.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && stem.len() <= 96
        && stem
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        && ["png", "jpg", "jpeg", "webp", "avif"].contains(&extension.to_ascii_lowercase().as_str())
}

fn avatar_import_bytes_at(home: &std::path::Path, bytes: &[u8]) -> Result<String, String> {
    if bytes.is_empty() || bytes.len() > INLINE_IMAGE_MAX_BYTES {
        return Err(format!(
            "avatar image must be between 1 and {INLINE_IMAGE_MAX_BYTES} bytes"
        ));
    }
    let detected = detect_canvas_image_kind(bytes)
        .ok_or_else(|| "avatar does not contain a supported image signature".to_string())?;
    if detected == CanvasImageKind::Gif {
        return Err("animated GIF avatars are not supported; use PNG, JPEG, WebP, or AVIF".into());
    }
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let id = format!("avatar-{}.{}", &digest[..24], detected.extension());
    let directory = home.join("avatars");
    let default_root = dirs_home().join(".phoenix");
    secure_private_directory_at(&directory, home, &default_root, home != default_root)?;
    let destination = directory.join(&id);
    if !destination.exists() {
        write_private_atomic_unlocked(&destination, bytes)?;
    }
    Ok(id)
}

fn avatar_import_path_at(
    home: &std::path::Path,
    source: &std::path::Path,
) -> Result<String, String> {
    use std::io::Read as _;
    let file = std::fs::File::open(source)
        .map_err(|error| format!("could not open avatar image: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("could not inspect avatar image: {error}"))?;
    if !metadata.is_file() {
        return Err("avatar source must be a regular image file".into());
    }
    if metadata.len() == 0 || metadata.len() > INLINE_IMAGE_MAX_BYTES as u64 {
        return Err(format!(
            "avatar image must be between 1 and {INLINE_IMAGE_MAX_BYTES} bytes"
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(INLINE_IMAGE_MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read avatar image: {error}"))?;
    avatar_import_bytes_at(home, &bytes)
}

fn avatar_import_at(home: &std::path::Path, data_url: &str) -> Result<String, String> {
    let max_encoded_bytes = INLINE_IMAGE_MAX_BYTES.div_ceil(3).saturating_mul(4);
    if data_url.len() > "data:".len() + DATA_URL_META_MAX_BYTES + 1 + max_encoded_bytes {
        return Err("avatar data URL exceeds the image size limit".into());
    }
    let rest = data_url
        .strip_prefix("data:")
        .ok_or_else(|| "avatar is not an image data URL".to_string())?;
    let comma = rest
        .find(',')
        .ok_or_else(|| "malformed avatar data URL".to_string())?;
    if comma > DATA_URL_META_MAX_BYTES {
        return Err("avatar data URL metadata is too long".into());
    }
    let (meta, payload_with_comma) = rest.split_at(comma);
    let mut parts = meta.split(';');
    let mime = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    let declared = CanvasImageKind::from_mime(&mime)
        .ok_or_else(|| format!("unsupported avatar MIME type `{mime}`"))?;
    if declared == CanvasImageKind::Gif {
        return Err("animated GIF avatars are not supported; use PNG, JPEG, WebP, or AVIF".into());
    }
    let parameters: Vec<_> = parts.collect();
    if parameters.len() != 1 || !parameters[0].eq_ignore_ascii_case("base64") {
        return Err("avatar image must use base64 encoding".into());
    }
    let bytes = bounded_base64_decode(&payload_with_comma[1..], INLINE_IMAGE_MAX_BYTES)?;
    let detected = detect_canvas_image_kind(&bytes)
        .ok_or_else(|| "avatar does not contain a supported image signature".to_string())?;
    if detected != declared {
        return Err(format!(
            "declared MIME {mime} does not match actual {} content",
            detected.mime()
        ));
    }
    avatar_import_bytes_at(home, &bytes)
}

#[tauri::command]
async fn avatar_import(data_url: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || avatar_import_at(&phoenix_home(), &data_url))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn avatar_pick(app: tauri::AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("Choose coworker avatar")
            .add_filter("Images", &["png", "jpg", "jpeg", "webp", "avif"])
            .blocking_pick_file()
    })
    .await
    .map_err(|error| error.to_string())?;
    let Some(picked) = picked else {
        return Ok(None);
    };
    let path = picked
        .into_path()
        .map_err(|error| format!("Could not read that image: {error}"))?;
    tauri::async_runtime::spawn_blocking(move || avatar_import_path_at(&phoenix_home(), &path))
        .await
        .map_err(|error| error.to_string())?
        .map(Some)
}

#[tauri::command]
async fn avatar_data_url(image_id: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if !valid_avatar_id(&image_id) {
            return Err("invalid custom avatar id".to_string());
        }
        let home = phoenix_home();
        image_data_url_at(&home.join("avatars").join(image_id), &home, &dirs_home())
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Persist a pasted/dropped image (a `data:` URL) to ~/.phoenix/attachments/
/// and return its absolute path. The path rides on the turn's `attachments`
/// field so the agent can view it with the `image_analyze` (vision) tool.
#[tauri::command]
fn save_attachment(data_url: String) -> Result<String, String> {
    save_attachment_at(&phoenix_home(), &data_url).map(|path| path.display().to_string())
}

const INTEL_VENDOR_ID: &str = "0x8086";
const NVIDIA_VENDOR_ID: &str = "0x10de";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RenderGpuPreference {
    Integrated,
    Dedicated,
    System,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedWebviewRenderer {
    vendor_id: &'static str,
    dri_prime: String,
    backend_preference: RenderGpuPreference,
    nvidia_ui: bool,
}

fn render_gpu_preference(value: Option<&str>) -> RenderGpuPreference {
    match value.map(str::trim) {
        Some(value) if value.eq_ignore_ascii_case("system") => RenderGpuPreference::System,
        Some(value) if value.eq_ignore_ascii_case("dedicated") => RenderGpuPreference::Dedicated,
        // Keep the existing behavior for an unset, empty, `integrated`, or
        // unrecognized value: prefer the Intel Mesa renderer when available.
        _ => RenderGpuPreference::Integrated,
    }
}

fn dri_prime_selector_from_pci_address(address: &str) -> Option<String> {
    let (domain, remainder) = address.split_once(':')?;
    let (bus, remainder) = remainder.split_once(':')?;
    let (device, function) = remainder.split_once('.')?;
    let valid_hex = |part: &str, length: usize| {
        part.len() == length && part.bytes().all(|byte| byte.is_ascii_hexdigit())
    };
    if !valid_hex(domain, 4)
        || !valid_hex(bus, 2)
        || !valid_hex(device, 2)
        || !valid_hex(function, 1)
    {
        return None;
    }
    Some(format!(
        "pci-{}_{}_{}_{}",
        domain.to_ascii_lowercase(),
        bus.to_ascii_lowercase(),
        device.to_ascii_lowercase(),
        function.to_ascii_lowercase()
    ))
}

fn drm_prime_selector_for_vendor(drm_root: &std::path::Path, vendor_id: &str) -> Option<String> {
    let mut render_nodes: Vec<_> = std::fs::read_dir(drm_root)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("renderD"))
        .collect();
    render_nodes.sort_by_key(|entry| entry.file_name());

    render_nodes.into_iter().find_map(|entry| {
        let device = entry.path().join("device");
        let vendor = std::fs::read_to_string(device.join("vendor")).ok()?;
        if !vendor.trim().eq_ignore_ascii_case(vendor_id) {
            return None;
        }

        let canonical_device = std::fs::canonicalize(device).ok()?;
        canonical_device.ancestors().find_map(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .and_then(dri_prime_selector_from_pci_address)
        })
    })
}

fn gdk_backend_for_renderer(
    preference: RenderGpuPreference,
    wayland_available: bool,
    x11_available: bool,
) -> Option<&'static str> {
    match preference {
        // The hidden WebKit native-services relay is more stable on X11 on
        // NVIDIA. This choice is intentionally independent from the visible
        // Electron shell's Ozone platform.
        RenderGpuPreference::Integrated
        | RenderGpuPreference::Dedicated
        | RenderGpuPreference::System
            if x11_available =>
        {
            Some("x11")
        }
        // Intel WebKit remains usable on a Wayland-only session, but the
        // browser-surface command will report that native embedding is absent.
        RenderGpuPreference::Integrated if wayland_available => Some("wayland"),
        RenderGpuPreference::Integrated
        | RenderGpuPreference::Dedicated
        | RenderGpuPreference::System => None,
    }
}

/// Select the compositor for the visible Electron shell from the desktop
/// session, not from `GDK_BACKEND`. The latter is intentionally rewritten to
/// X11 for Phoenix's hidden WebKit native-services relay on NVIDIA systems.
fn visible_chromium_ozone_platform(
    explicit: Option<&str>,
    wayland_available: bool,
    x11_available: bool,
) -> Option<&'static str> {
    match explicit.map(str::trim) {
        Some(value) if value.eq_ignore_ascii_case("wayland") => Some("wayland"),
        Some(value) if value.eq_ignore_ascii_case("x11") => Some("x11"),
        _ if wayland_available => Some("wayland"),
        _ if x11_available => Some("x11"),
        _ => None,
    }
}

fn visible_chromium_ozone_platform_from_env() -> Option<&'static str> {
    visible_chromium_ozone_platform(
        std::env::var("PHOENIX_CHROMIUM_OZONE_PLATFORM").ok().as_deref(),
        std::env::var_os("WAYLAND_DISPLAY").is_some_and(|display| !display.is_empty()),
        std::env::var_os("DISPLAY").is_some_and(|display| !display.is_empty()),
    )
}

fn automatic_chromium_gpu_mode(
    nvidia_ui: bool,
    ozone_platform: Option<&str>,
) -> Option<&'static str> {
    // Electron explicitly rejects Vulkan on its Wayland Ozone backend. Keep
    // the normal ANGLE/OpenGL route there; the X11 diagnostic path may still
    // opt into Vulkan on NVIDIA.
    (nvidia_ui && ozone_platform != Some("wayland")).then_some("vulkan")
}

fn configure_gdk_backend(preference: RenderGpuPreference) {
    let wayland_available =
        std::env::var_os("WAYLAND_DISPLAY").is_some_and(|display| !display.is_empty());
    let x11_available = std::env::var_os("DISPLAY").is_some_and(|display| !display.is_empty());
    if let Some(backend) = gdk_backend_for_renderer(preference, wayland_available, x11_available) {
        std::env::set_var("GDK_BACKEND", backend);
    }
}

/// Choose the UI GPU from preference + discovered DRM vendors.
///
/// Default (`integrated`) still prefers Intel Mesa when that node exists.
/// On an NVIDIA-only machine that pin used to return early and leave WebKit
/// unconfigured; fall through to the NVIDIA render node instead. Dedicated
/// always means NVIDIA. System never selects.
fn resolve_webview_renderer(
    preference: RenderGpuPreference,
    drm_root: &std::path::Path,
) -> Option<ResolvedWebviewRenderer> {
    if preference == RenderGpuPreference::System {
        return None;
    }
    let intel = drm_prime_selector_for_vendor(drm_root, INTEL_VENDOR_ID);
    let nvidia = drm_prime_selector_for_vendor(drm_root, NVIDIA_VENDOR_ID);
    match preference {
        RenderGpuPreference::Integrated => {
            if let Some(dri_prime) = intel {
                Some(ResolvedWebviewRenderer {
                    vendor_id: INTEL_VENDOR_ID,
                    dri_prime,
                    backend_preference: RenderGpuPreference::Integrated,
                    nvidia_ui: false,
                })
            } else {
                nvidia.map(|dri_prime| ResolvedWebviewRenderer {
                    vendor_id: NVIDIA_VENDOR_ID,
                    dri_prime,
                    backend_preference: RenderGpuPreference::Dedicated,
                    nvidia_ui: true,
                })
            }
        }
        RenderGpuPreference::Dedicated => nvidia.map(|dri_prime| ResolvedWebviewRenderer {
            vendor_id: NVIDIA_VENDOR_ID,
            dri_prime,
            backend_preference: RenderGpuPreference::Dedicated,
            nvidia_ui: true,
        }),
        RenderGpuPreference::System => None,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WebviewRendererEnvPlan {
    dri_prime: String,
    backend_preference: RenderGpuPreference,
    nvidia_offload: bool,
    glx_vendor: Option<&'static str>,
    force_dmabuf: bool,
}

fn webview_renderer_env_plan(resolved: &ResolvedWebviewRenderer) -> WebviewRendererEnvPlan {
    WebviewRendererEnvPlan {
        dri_prime: resolved.dri_prime.clone(),
        backend_preference: resolved.backend_preference,
        nvidia_offload: resolved.nvidia_ui,
        glx_vendor: resolved.nvidia_ui.then_some("nvidia"),
        // Never force DMA-BUF: WebKitGTK 2.52 + NVIDIA + Wayland crashed
        // with protocol error 71.
        force_dmabuf: false,
    }
}

fn apply_webview_renderer(resolved: &ResolvedWebviewRenderer) {
    let plan = webview_renderer_env_plan(resolved);
    std::env::set_var("DRI_PRIME", &plan.dri_prime);
    configure_gdk_backend(plan.backend_preference);
    if plan.nvidia_offload {
        std::env::set_var("__NV_PRIME_RENDER_OFFLOAD", "1");
        std::env::set_var(
            "__GLX_VENDOR_LIBRARY_NAME",
            plan.glx_vendor.unwrap_or("nvidia"),
        );
        std::env::set_var("__VK_LAYER_NV_optimus", "NVIDIA_only");
    } else {
        std::env::remove_var("__NV_PRIME_RENDER_OFFLOAD");
        std::env::remove_var("__NV_PRIME_RENDER_OFFLOAD_PROVIDER");
        std::env::remove_var("__GLX_VENDOR_LIBRARY_NAME");
        std::env::remove_var("__VK_LAYER_NV_optimus");
        std::env::remove_var("WEBKIT_DISABLE_DMABUF_RENDERER");
    }
    if !plan.force_dmabuf {
        std::env::remove_var("WEBKIT_FORCE_DMABUF_RENDERER");
    }
}

fn configure_webview_renderer() {
    let preference = render_gpu_preference(std::env::var("PHOENIX_RENDER_GPU").ok().as_deref());
    // Backend selection is independent of GPU discovery (`system` deliberately
    // skips DRM selection), so apply it before the early return below.
    configure_gdk_backend(preference);
    let Some(resolved) =
        resolve_webview_renderer(preference, std::path::Path::new("/sys/class/drm"))
    else {
        eprintln!("phoenix renderer: no DRM GPU selected (preference={preference:?})");
        return;
    };
    eprintln!(
        "phoenix renderer: vendor={} dri_prime={} nvidia_ui={} backend={:?}",
        resolved.vendor_id, resolved.dri_prime, resolved.nvidia_ui, resolved.backend_preference
    );
    // `force_low_power_gpu` disables Chromium acceleration on an NVIDIA-only
    // host because there is no lower-power adapter to select. Keep the user
    // override authoritative, otherwise select the only usable adapter.
    if resolved.nvidia_ui && std::env::var_os("PHOENIX_CHROMIUM_GPU_POWER").is_none() {
        std::env::set_var("PHOENIX_CHROMIUM_GPU_POWER", "high");
    }
    if std::env::var_os("PHOENIX_CHROMIUM_GPU_MODE").is_none() {
        if let Some(mode) = automatic_chromium_gpu_mode(
            resolved.nvidia_ui,
            visible_chromium_ozone_platform_from_env(),
        ) {
            std::env::set_var("PHOENIX_CHROMIUM_GPU_MODE", mode);
        }
    }
    if resolved.nvidia_ui {
        std::env::set_var("PHOENIX_CHROMIUM_NVIDIA", "1");
    } else {
        std::env::remove_var("PHOENIX_CHROMIUM_NVIDIA");
    }
    apply_webview_renderer(&resolved);
}

#[cfg(test)]
mod gateway_compat_tests {
    use super::{
        action_audit_recent_at, append_bounded_restart_log_at_with_limit, avatar_import_at,
        avatar_import_path_at,
        bounded_base64_decode, browser_download_dir, browser_downloads_list_at,
        build_initial_provider_config, canvas_user_message_parts, chromium_bridge_token,
        chrono_free_iso, classify_gateway_probe, composer_path_attachment, config_write_at,
        copy_snapshot_tree, copy_wallpaper_to_at, create_private_voice_file, cron_add_at,
        cron_remove_at, cron_set_enabled_at, device_auth_event_from_stdout, feeds_file,
        feeds_get_at, feeds_patch_at, feeds_set_at, gateway_lifecycle_lock_path_at,
        gateway_supervisor_backoff, image_data_url_at, keep_recent_complete_context_turns,
        list_note_files_at, mutate_crons_at, next_local_cron_time, open_bounded_restart_log_at,
        open_bounded_restart_log_at_with_limit, open_wallpaper_preview_source, phoenix_binary_from,
        phoenix_home_from, prefs_get_at, prefs_set_at, process_instance_exited_at,
        process_start_time_at, process_uses_selected_executable_at, prompt_overlay_read_at,
        prompt_overlay_reset_at, prompt_overlay_revision, prompt_overlay_write_at,
        read_bounded_body, read_file_at, read_note_file_at, reclaim_stale_gateway_files_at,
        remove_note_file_at, replace_private_atomic, rotated_log_path, routine_turn_metadata,
        run_command_bounded, run_command_bounded_observed, save_attachment_at,
        screenshots_picker_directory_from, secure_private_directory_at,
        session_context_get_blocking_at, static_wallpaper_preview_kind, stop_voice_child,
        usage_dashboard_at, validate_capture_file, validate_declared_body_size,
        validate_desktop_remote_runner, validate_state_root_for_mutation, validate_tts_input,
        validate_voice_config_field, BoundedChildGuard, DesktopRemoteRunner,
        validate_wav, vitals_delete_at,
        vitals_parse, vitals_write_at, wallpaper_preview_cache_stem, wallpaper_preview_path,
        workspace_default, workspace_environment_at, workspace_git_branches_at,
        workspace_git_commit_at, workspace_git_switch_branch_at,
        CanvasConversationOwnerKind, CanvasConversationOwnerRef,
        CanvasImageKind, CronDraft, DataSnapshotStats, GatewayLifecycleLease, GatewayProbe,
        GATEWAY_WIRE_PROTOCOL, INLINE_IMAGE_MAX_BYTES, MAX_SCHEDULE_SECONDS, MEMORY_NOTE_MAX_BYTES,
        PREFS_MISSING_REVISION, TEXT_FILE_MAX_BYTES, VOICE_CAPTURE_MAX_BYTES,
        VOICE_CONFIG_FIELD_MAX_CHARS, VOICE_TTS_INPUT_MAX_CHARS, WALLPAPER_CARD_VARIANT,
        WALLPAPER_MAX_BYTES, WALLPAPER_THUMB_VARIANT,
    };

    fn isolated_dir(label: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "phoenix-canvas-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("isolated test directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .expect("private isolated test directory");
        }
        path
    }

    #[test]
    fn action_audit_reader_returns_bounded_recent_valid_records() {
        let root = isolated_dir("action-audit-reader");
        let path = root.join("actions.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"event_id\":\"one\",\"phase\":\"started\"}\n",
                "this is an interrupted partial record\n",
                "{\"event_id\":\"two\",\"phase\":\"finished\"}\n",
                "{\"event_id\":\"three\",\"phase\":\"finished\"}\n"
            ),
        )
        .expect("write action ledger fixture");

        let rows = action_audit_recent_at(&path, 2).expect("read action ledger fixture");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["event_id"], "two");
        assert_eq!(rows[1]["event_id"], "three");
        std::fs::remove_dir_all(root).expect("cleanup action ledger fixture");
    }

    #[test]
    fn usage_dashboard_keeps_estimates_and_reported_costs_distinct() {
        let root = isolated_dir("usage-dashboard");
        let runs = root.join("runs");
        std::fs::create_dir(&runs).expect("create runs directory");
        std::fs::write(
            runs.join("round_timings.jsonl"),
            concat!(
                "{\"ts\":\"2026-08-28T10:00:00Z\",\"agent\":\"Iris\",\"provider\":\"openai\",\"model\":\"gpt-test\",\"input_tokens\":1000,\"output_tokens\":80,\"cache_read_tokens\":700,\"provider_ms\":1200,\"cost_micros\":4500}\n",
                "{\"ts\":\"2026-08-29T10:00:00Z\",\"agent\":\"Iris\",\"provider\":\"openai\",\"model\":\"gpt-test\",\"request_chars\":4000,\"input_tokens\":0,\"output_tokens\":20,\"provider_ms\":800}\n",
                "interrupted partial line\n"
            ),
        )
        .expect("write usage fixture");
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-29T12:00:00Z")
            .expect("parse dashboard clock")
            .with_timezone(&chrono::Utc);

        let dashboard = usage_dashboard_at(&root, now).expect("build usage dashboard");
        assert_eq!(dashboard["week"]["rounds"], 2);
        assert_eq!(dashboard["week"]["input_tokens"], 2000);
        assert_eq!(dashboard["week"]["estimated_rounds"], 1);
        assert_eq!(dashboard["week"]["cache_read_tokens"], 700);
        assert_eq!(dashboard["week"]["cost_reported_rounds"], 1);
        assert_eq!(dashboard["week"]["cost_micros"], 4500);
        assert_eq!(dashboard["invalid_rows"], 1);
        assert_eq!(dashboard["lanes"].as_array().map(Vec::len), Some(1));
        std::fs::remove_dir_all(root).expect("cleanup usage dashboard fixture");
    }

    fn seed_canvas_owner_directory(
        home: &std::path::Path,
        agents: &[(&str, Option<&str>)],
        groups: &[(&str, Option<&str>)],
    ) {
        std::fs::create_dir_all(home.join("company")).expect("company directory fixture");
        let connection =
            rusqlite::Connection::open(home.join("company/company.sqlite")).expect("company owner fixture");
        connection
            .execute_batch(
                "CREATE TABLE company_agents(
                    agent_id TEXT PRIMARY KEY,
                    canonical_session_id TEXT
                 );
                 CREATE TABLE company_groups(
                    group_id TEXT PRIMARY KEY,
                    canonical_session_id TEXT
                 );",
            )
            .expect("company owner projection fixture");
        for (id, session_id) in agents {
            connection
                .execute(
                    "INSERT INTO company_agents(agent_id,canonical_session_id) VALUES(?1,?2)",
                    rusqlite::params![id, session_id],
                )
                .expect("agent owner fixture");
        }
        for (id, session_id) in groups {
            connection
                .execute(
                    "INSERT INTO company_groups(group_id,canonical_session_id) VALUES(?1,?2)",
                    rusqlite::params![id, session_id],
                )
                .expect("group owner fixture");
        }
    }

    fn canvas_owner(kind: CanvasConversationOwnerKind, id: &str) -> CanvasConversationOwnerRef {
        CanvasConversationOwnerRef {
            kind,
            id: id.to_string(),
        }
    }

    #[test]
    fn browser_download_listing_distinguishes_partial_and_finished_files() {
        let root = isolated_dir("browser-downloads");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("image.png"), b"finished").unwrap();
        std::fs::write(root.join("report.pdf.crdownload"), b"partial").unwrap();
        let listed = browser_downloads_list_at(&root).unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed
            .iter()
            .any(|file| file.name == "image.png" && file.complete));
        assert!(listed
            .iter()
            .any(|file| file.name == "report.pdf" && !file.complete));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn browser_download_profile_rejects_path_traversal() {
        assert!(browser_download_dir("../outside").is_err());
        assert!(browser_download_dir("agent-phoenix").is_ok());
    }

    #[test]
    fn native_provider_bootstrap_quotes_values_and_pins_the_selected_account() {
        let rendered = build_initial_provider_config(
            "openai",
            "model-with-\"quote\"",
            "api",
            Some("openai:work"),
        )
        .expect("render provider config");
        let parsed: toml::Value = rendered.parse().expect("valid TOML");
        let llm = &parsed["profile"]["llm"];
        assert_eq!(llm["provider"].as_str(), Some("openai"));
        assert_eq!(llm["model"].as_str(), Some("model-with-\"quote\""));
        assert_eq!(llm["auth"]["method"].as_str(), Some("api"));
        assert_eq!(llm["auth"]["source"].as_str(), Some("profile"));
        assert_eq!(llm["auth"]["profile"].as_str(), Some("openai:work"));

        let local = build_initial_provider_config("ollama", "qwen", "none", None)
            .expect("render no-auth config");
        let local: toml::Value = local.parse().expect("valid local TOML");
        assert_eq!(
            local["profile"]["llm"]["auth"]["source"].as_str(),
            Some("none")
        );
        assert!(local["profile"]["llm"]["auth"].get("profile").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn prompt_overlay_updates_are_private_revisioned_and_recoverable() {
        use std::os::unix::fs::PermissionsExt;

        let dir = isolated_dir("prompt-overlay");
        let path = dir.join("prompts").join("coder_system.md");
        let initial = prompt_overlay_read_at(&path).expect("missing overlay status");
        assert_eq!(initial["revision"], "missing");
        let written = prompt_overlay_write_at(&path, "Own the implementation.\n", "missing")
            .expect("write first overlay");
        let revision = written["revision"].as_str().expect("revision");
        assert_ne!(revision, "missing");
        assert_eq!(
            std::fs::metadata(&path)
                .expect("overlay metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(prompt_overlay_write_at(&path, "lost update", "missing").is_err());
        prompt_overlay_reset_at(&path, revision).expect("reset overlay");
        assert!(!path.exists());
        assert_eq!(prompt_overlay_revision(None), "missing");
        assert!(std::fs::read_dir(path.parent().expect("prompt parent"))
            .expect("backup directory")
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().contains(".bak-")));
        std::fs::remove_dir_all(dir).expect("cleanup prompt test directory");
    }

    #[cfg(unix)]
    #[test]
    fn data_snapshot_copy_is_private_and_never_follows_links() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let parent = isolated_dir("data-snapshot");
        let source = parent.join("source-state");
        let destination = parent.join("snapshot-state");
        std::fs::create_dir_all(source.join("memory")).expect("source tree");
        std::fs::write(source.join("config.toml"), b"private config").expect("source config");
        std::fs::write(source.join("memory").join("note.md"), b"durable note")
            .expect("source note");
        let external = parent.join("external-secret");
        std::fs::write(&external, b"must not be copied").expect("external fixture");
        symlink(&external, source.join("linked-secret")).expect("linked fixture");

        let mut stats = DataSnapshotStats::default();
        copy_snapshot_tree(&source, &destination, 0, &mut stats).expect("copy snapshot tree");
        assert_eq!(stats.files, 2);
        assert_eq!(stats.skipped_links, 1);
        assert!(!destination.join("linked-secret").exists());
        assert_eq!(
            std::fs::read(destination.join("memory").join("note.md")).expect("snapshotted note"),
            b"durable note"
        );
        assert_eq!(
            std::fs::metadata(destination.join("config.toml"))
                .expect("snapshot metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        std::fs::remove_dir_all(parent).expect("cleanup snapshot test directory");
    }

    fn proc_stat(pid: i32, state: &str, start_time: &str) -> String {
        // After `comm`, /proc stat fields begin at state (field 3). Eighteen
        // placeholder fields put starttime at field 22 / zero-based index 19.
        let mut fields = vec![state.to_string()];
        fields.extend(std::iter::repeat("0".to_string()).take(18));
        fields.push(start_time.to_string());
        format!("{pid} (phoenix worker) {}\n", fields.join(" "))
    }

    fn write_proc_stat(proc_root: &std::path::Path, pid: i32, state: &str, start_time: &str) {
        let process = proc_root.join(pid.to_string());
        std::fs::create_dir_all(&process).expect("fake process directory");
        std::fs::write(process.join("stat"), proc_stat(pid, state, start_time))
            .expect("fake process stat");
    }

    fn minimal_png_fixture() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        bytes.extend_from_slice(&13_u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&1_u32.to_be_bytes());
        bytes.extend_from_slice(&1_u32.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        bytes.extend_from_slice(&[0, 0, 0, 0]); // CRC is not decoded here.
        bytes
    }

    #[cfg(unix)]
    #[test]
    fn wallpaper_preview_magic_and_fingerprint_are_extension_independent() {
        let jpeg = [0xff, 0xd8, 0xff, 0xe0, 0, 16, b'J', b'F', b'I', b'F'];
        assert_eq!(
            static_wallpaper_preview_kind(&jpeg),
            Some(CanvasImageKind::Jpeg),
            "an existing .jpg.orig-8k wallpaper must remain previewable by magic"
        );
        assert_eq!(
            static_wallpaper_preview_kind(&minimal_png_fixture()),
            Some(CanvasImageKind::Png)
        );
        assert!(static_wallpaper_preview_kind(b"GIF89a").is_none());

        let dir = isolated_dir("wallpaper-preview-key");
        let source = dir.join("lake.jpg.orig-8k");
        std::fs::write(&source, jpeg).expect("preview source");
        let metadata = std::fs::metadata(&source).expect("preview metadata");
        let stem = wallpaper_preview_cache_stem(&source, &metadata);
        assert_eq!(stem.len(), 64);
        assert!(stem.bytes().all(|byte| byte.is_ascii_hexdigit()));
        let cache = dir.join("cache");
        let thumb = wallpaper_preview_path(&cache, &stem, WALLPAPER_THUMB_VARIANT);
        let card = wallpaper_preview_path(&cache, &stem, WALLPAPER_CARD_VARIANT);
        assert_eq!(thumb.parent(), Some(cache.as_path()));
        assert_ne!(thumb, card);

        std::fs::write(&source, [jpeg.as_slice(), &[1]].concat()).expect("change source");
        let changed = std::fs::metadata(&source).expect("changed preview metadata");
        assert_ne!(metadata.len(), changed.len());
        assert_ne!(
            stem,
            wallpaper_preview_cache_stem(&source, &changed),
            "source changes must invalidate derivatives"
        );
        std::fs::remove_dir_all(dir).expect("cleanup isolated test directory");
    }

    #[cfg(unix)]
    #[test]
    fn wallpaper_preview_source_reads_the_complete_png_signature() {
        let dir = isolated_dir("wallpaper-preview-png-header");
        let wallpapers = dir.join("wallpapers");
        std::fs::create_dir(&wallpapers).expect("wallpaper directory");
        let source = wallpapers.join("paper.png");
        std::fs::write(&source, minimal_png_fixture()).expect("PNG source");
        let opened = open_wallpaper_preview_source(&source, &wallpapers)
            .expect("the 33-byte validated PNG header must not be truncated by the probe");
        assert_eq!(opened.kind, CanvasImageKind::Png);
        std::fs::remove_dir_all(dir).expect("cleanup isolated test directory");
    }

    #[test]
    fn bounded_child_runner_captures_small_output() {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "printf hello; printf warning >&2"]);
        let output = run_command_bounded(
            &mut command,
            None,
            std::time::Duration::from_secs(2),
            "test child",
        )
        .expect("bounded child succeeds");
        assert!(output.status.success());
        assert_eq!(output.stdout, b"hello");
        assert_eq!(output.stderr, b"warning");
    }

    #[test]
    fn bounded_child_guard_reaps_an_early_drop() {
        use std::os::unix::process::CommandExt as _;
        let mut command = std::process::Command::new("sh");
        command
            .args(["-c", "exec sleep 30"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0);
        let child = command.spawn().expect("spawn guarded child");
        let pid = child.id() as i32;
        drop(BoundedChildGuard::new(child));
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH),
            "early-drop child survived or remained a zombie"
        );
    }

    #[test]
    fn bounded_child_observer_receives_streamed_stdout() {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "printf first; sleep 0.05; printf second"]);
        let mut observations = Vec::new();
        let output = run_command_bounded_observed(
            &mut command,
            None,
            std::time::Duration::from_secs(2),
            "observed child",
            |stdout| observations.push(stdout.to_vec()),
        )
        .expect("observed child succeeds");
        assert_eq!(output.stdout, b"firstsecond");
        assert!(observations.iter().any(|value| value.ends_with(b"first")));
        assert_eq!(
            observations.last().map(Vec::as_slice),
            Some(b"firstsecond".as_slice())
        );
    }

    #[test]
    fn device_auth_marker_parser_rejects_unsafe_or_incomplete_payloads() {
        let parsed = device_auth_event_from_stdout(
            "github-copilot",
            br#"starting
DEVICE_AUTH={"verification_url":"https://github.com/login/device","user_code":"ABCD-1234"}
waiting
"#,
        )
        .expect("valid device auth marker");
        assert_eq!(parsed.provider, "github-copilot");
        assert_eq!(parsed.user_code, "ABCD-1234");
        assert!(device_auth_event_from_stdout(
            "github-copilot",
            br#"DEVICE_AUTH={"verification_url":"http://attacker.invalid","user_code":"ABCD"}"#,
        )
        .is_none());
        assert!(device_auth_event_from_stdout(
            "../provider",
            br#"DEVICE_AUTH={"verification_url":"https://github.com/login/device","user_code":"ABCD"}"#,
        )
        .is_none());
        assert!(device_auth_event_from_stdout(
            "github-copilot",
            br#"DEVICE_AUTH={"verification_url":"https://github.com/login/device","user_code":"AB"#,
        )
        .is_none());
    }

    #[test]
    fn bounded_child_runner_kills_timeout_and_background_descendants() {
        let dir = isolated_dir("bounded-child");
        let timed_pid = dir.join("timed.pid");
        let mut timed = std::process::Command::new("sh");
        timed
            .arg("-c")
            .arg("printf '%s' $$ > \"$1\"; exec sleep 30")
            .arg("sh")
            .arg(&timed_pid);
        assert!(run_command_bounded(
            &mut timed,
            None,
            std::time::Duration::from_millis(100),
            "timed child",
        )
        .is_err());
        let pid: i32 = std::fs::read_to_string(&timed_pid)
            .expect("timeout pid receipt")
            .parse()
            .expect("timeout pid");
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );

        let background_pid = dir.join("background.pid");
        let mut background = std::process::Command::new("sh");
        background
            .arg("-c")
            .arg("sleep 30 & printf '%s' $! > \"$1\"")
            .arg("sh")
            .arg(&background_pid);
        let error = run_command_bounded(
            &mut background,
            None,
            std::time::Duration::from_secs(2),
            "background child",
        )
        .expect_err("non-detached background work must not escape");
        assert!(error.contains("non-detached background"), "{error}");
        let pid: i32 = std::fs::read_to_string(&background_pid)
            .expect("background pid receipt")
            .parse()
            .expect("background pid");
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        std::fs::remove_dir_all(dir).expect("cleanup child test directory");
    }

    #[test]
    fn bounded_child_runner_stops_output_floods() {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "head -c 9000000 /dev/zero"]);
        let error = run_command_bounded(
            &mut command,
            None,
            std::time::Duration::from_secs(5),
            "noisy child",
        )
        .expect_err("output cap must terminate the child");
        assert!(error.contains("output limit"), "{error}");
    }

    fn refresh_process_timezone() {
        unsafe extern "C" {
            fn tzset();
        }
        // SAFETY: tzset has no arguments and refreshes libc's process-global
        // timezone cache after the guarded TZ update below.
        unsafe { tzset() };
    }

    struct TestTimezone {
        previous: Option<std::ffi::OsString>,
    }

    impl TestTimezone {
        fn mountain_dst() -> Self {
            let previous = std::env::var_os("TZ");
            std::env::set_var("TZ", "MST7MDT,M3.2.0/2,M11.1.0/2");
            refresh_process_timezone();
            Self { previous }
        }
    }

    impl Drop for TestTimezone {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var("TZ", value),
                None => std::env::remove_var("TZ"),
            }
            refresh_process_timezone();
        }
    }

    #[test]
    fn protocol_info_ignores_claimed_build_id_but_rejects_stale_protocol() {
        let response = serde_json::json!({
            "ProtocolInfo": {
                "protocol": GATEWAY_WIRE_PROTOCOL,
                "package_version": "test",
                "binary_id": "release-build-with-different-bytes"
            }
        })
        .to_string();
        assert_eq!(
            classify_gateway_probe(&response),
            GatewayProbe::Compatible,
            "same-protocol debug/release builds must not trigger replacement"
        );
        let stale = serde_json::json!({
            "ProtocolInfo": {
                "protocol": "phoenix-gateway-jsonl/stale-protocol",
                "package_version": "test",
                "binary_id": "same-build-id-would-not-help"
            }
        })
        .to_string();
        assert!(matches!(
            classify_gateway_probe(&stale),
            GatewayProbe::Incompatible(_)
        ));
        assert!(matches!(
            classify_gateway_probe(r#"{"Pong":null}"#),
            GatewayProbe::Unresponsive(_)
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rebuilt_gateway_binary_is_not_mistaken_for_the_running_old_inode() {
        let dir = isolated_dir("gateway-executable-identity");
        let proc_root = dir.join("proc");
        let process = proc_root.join("4242");
        std::fs::create_dir_all(&process).expect("fake proc process");
        let selected = dir.join("phoenix");
        let old = dir.join("phoenix-old");
        std::fs::write(&selected, b"new gateway").expect("selected binary");
        std::fs::write(&old, b"old gateway").expect("old binary");
        std::os::unix::fs::symlink(&old, process.join("exe")).expect("fake proc exe");
        assert_eq!(
            process_uses_selected_executable_at(&proc_root, 4242, &selected),
            Some(false)
        );
        std::fs::remove_file(process.join("exe")).expect("replace fake proc exe");
        std::os::unix::fs::symlink(&selected, process.join("exe")).expect("current proc exe");
        assert_eq!(
            process_uses_selected_executable_at(&proc_root, 4242, &selected),
            Some(true)
        );
        std::fs::remove_dir_all(dir).expect("cleanup executable identity fixture");
    }

    #[test]
    fn gateway_supervisor_backs_off_instead_of_respawning_incompatible_builds() {
        assert_eq!(
            gateway_supervisor_backoff(1, "spawned gateway is not compatible with this Phoenix"),
            std::time::Duration::from_secs(5 * 60)
        );
        assert_eq!(
            gateway_supervisor_backoff(1, "gateway did not become ready"),
            std::time::Duration::from_secs(3)
        );
        assert_eq!(
            gateway_supervisor_backoff(4, "gateway did not become ready"),
            std::time::Duration::from_secs(24)
        );
    }

    #[cfg(unix)]
    #[test]
    fn gateway_lifecycle_lease_matches_cli_path_mode_and_contention() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let dir = isolated_dir("gateway-lifecycle-lock");
        let path = gateway_lifecycle_lock_path_at(&dir);
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("gateway-lifecycle")
        );

        let first = GatewayLifecycleLease::acquire_at(&path, std::time::Duration::from_secs(1))
            .expect("first lifecycle lease");
        assert_eq!(
            std::fs::metadata(&path)
                .expect("lifecycle lock metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let contention =
            match GatewayLifecycleLease::acquire_at(&path, std::time::Duration::from_millis(20)) {
                Ok(_) => panic!("separate descriptor must contend on the cross-process flock"),
                Err(error) => error,
            };
        assert!(
            contention.contains("timed out"),
            "unexpected error: {contention}"
        );
        drop(first);
        drop(
            GatewayLifecycleLease::acquire_at(&path, std::time::Duration::from_secs(1))
                .expect("lease becomes available after drop"),
        );

        std::fs::remove_file(&path).expect("remove real lock");
        let target = dir.join("lock-target");
        std::fs::write(&target, b"untouched").expect("seed symlink target");
        symlink(&target, &path).expect("symlink lifecycle lock");
        assert!(
            GatewayLifecycleLease::acquire_at(&path, std::time::Duration::from_millis(20)).is_err(),
            "O_NOFOLLOW must reject a substituted lifecycle lock"
        );
        assert_eq!(
            std::fs::read(&target).expect("target preserved"),
            b"untouched"
        );
        std::fs::remove_dir_all(dir).expect("cleanup isolated test directory");
    }

    #[test]
    fn local_cron_first_fire_tracks_dst_and_skips_invalid_wall_times() {
        static TZ_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        let _timezone_lock = TZ_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _timezone = TestTimezone::mountain_dst();

        // Saturday 09:00 MST -> Sunday 09:00 MDT is 23 elapsed hours.
        let before_spring_forward = 1_772_899_200;
        let sunday_nine_mdt = 1_772_982_000;
        assert_eq!(
            next_local_cron_time(before_spring_forward, 9, 0, None).expect("next daily"),
            sunday_nine_mdt
        );
        assert_eq!(
            next_local_cron_time(before_spring_forward, 9, 0, Some(&[0])).expect("next Sunday"),
            sunday_nine_mdt
        );
        // 02:30 does not exist on spring-forward Sunday, so mirror Chrono's
        // LocalResult::Single behavior and choose Monday instead.
        assert_eq!(
            next_local_cron_time(before_spring_forward, 2, 30, None)
                .expect("skip nonexistent local time"),
            1_773_045_000
        );

        // 01:30 occurs twice on fall-back Sunday. An ambiguous wall time is
        // skipped, again matching the core scheduler, so Monday is selected.
        assert_eq!(
            next_local_cron_time(1_793_458_800, 1, 30, None).expect("skip ambiguous local time"),
            1_793_608_200
        );
    }

    #[test]
    fn phoenix_home_override_is_the_exact_trimmed_root() {
        assert_eq!(
            phoenix_home_from(
                Some("  /tmp/phoenix-isolated  "),
                std::path::Path::new("/home/u")
            ),
            std::path::PathBuf::from("/tmp/phoenix-isolated")
        );
        assert_eq!(
            phoenix_home_from(Some("  "), std::path::Path::new("/home/u")),
            std::path::PathBuf::from("/home/u/.phoenix")
        );
        assert_eq!(
            phoenix_home_from(None, std::path::Path::new(".")),
            std::path::PathBuf::from("./.phoenix"),
            "a missing OS home must not become /.phoenix"
        );
    }

    #[test]
    fn workspace_default_honors_phoenix_home_and_is_private() {
        const CHILD_ROOT: &str = "PHOENIX_WORKSPACE_DEFAULT_TEST_ROOT";

        // Exercise the environment-based command in a child test process so
        // PHOENIX_HOME cannot leak into concurrently running unit tests.
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let root = std::path::PathBuf::from(root);
            let expected = root.join("workspace");
            assert_eq!(
                workspace_default().expect("workspace default"),
                expected.display().to_string()
            );
            assert!(expected.is_dir());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(expected)
                        .expect("workspace metadata")
                        .permissions()
                        .mode()
                        & 0o777,
                    0o700
                );
            }
            return;
        }

        let root = isolated_dir("workspace-default");
        let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .arg("--exact")
            .arg("gateway_compat_tests::workspace_default_honors_phoenix_home_and_is_private")
            .arg("--nocapture")
            .env("PHOENIX_HOME", &root)
            .env(CHILD_ROOT, &root)
            .status()
            .expect("run isolated workspace-default test");
        assert!(status.success(), "isolated workspace-default test failed");
        std::fs::remove_dir_all(root).expect("cleanup isolated test directory");
    }

    #[test]
    fn workspace_review_collects_nested_repositories_and_untracked_files() {
        let root = isolated_dir("workspace-review");
        let repo = root.join("nested");
        std::fs::create_dir_all(repo.join("src")).expect("create nested repository");
        assert!(std::process::Command::new("git")
            .arg("init")
            .arg("--quiet")
            .arg(&repo)
            .status()
            .expect("initialize test repository")
            .success());
        std::fs::write(repo.join("src/lib.rs"), "pub fn ready() -> bool { true }\n")
            .expect("write staged source");
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["add", "src/lib.rs"])
            .status()
            .expect("stage source")
            .success());
        std::fs::write(repo.join("notes.md"), "review me\n").expect("write untracked source");

        let snapshot = crate::workspace_review_at(root.clone()).expect("inspect workspace");
        assert!(snapshot.files.contains(&"nested/src/lib.rs".to_string()));
        assert!(snapshot.files.contains(&"nested/notes.md".to_string()));
        assert!(snapshot
            .changes
            .iter()
            .any(|change| change.path == "nested/src/lib.rs" && change.additions == 1));
        assert!(snapshot
            .changes
            .iter()
            .any(|change| change.path == "nested/notes.md" && change.additions == 1));
        std::fs::remove_dir_all(root).expect("cleanup review workspace");
    }

    #[test]
    fn workspace_environment_reports_git_state_and_commits_only_after_explicit_action() {
        let root = isolated_dir("workspace-environment");
        assert!(std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(&root)
            .status()
            .expect("initialize workspace repository")
            .success());
        for (key, value) in [("user.name", "Phoenix test"), ("user.email", "test@phoenix.local")] {
            assert!(std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(["config", key, value])
                .status()
                .expect("configure repository identity")
                .success());
        }
        std::fs::write(root.join("environment.txt"), "real workspace state\n")
            .expect("write workspace change");

        let before = workspace_environment_at(root.clone()).expect("read environment");
        assert_eq!(before.workspace, root.display().to_string());
        assert!(before.repo.as_deref().is_some_and(|repo| repo.ends_with(root.to_string_lossy().as_ref())));
        assert!(before.branch.is_some(), "a normal repository has a branch name");
        assert!(before.can_commit, "uncommitted files enable the commit flow");
        assert!(!before.can_push, "a repository without a remote cannot push");
        assert!(!before.can_create_pull_request, "a repository without GitHub cannot create a PR");
        let receipt = workspace_git_commit_at(root.clone(), "Add environment receipt".into(), true)
            .expect("commit the explicitly staged workspace change");
        assert_eq!(receipt.action, "commit");
        let after = workspace_environment_at(root.clone()).expect("refresh environment");
        assert!(!after.can_commit, "the committed repository is clean");
        let branches = workspace_git_branches_at(root.clone()).expect("list local branches");
        assert_eq!(branches.current, after.branch);
        assert!(branches.current.is_some_and(|branch| branches.branches.contains(&branch)));
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["branch", "environment-panel"])
            .status()
            .expect("create selectable branch")
            .success());
        let switched = workspace_git_switch_branch_at(root.clone(), "environment-panel".into())
            .expect("switch only to a listed local branch");
        assert_eq!(switched.action, "switch_branch");
        assert_eq!(
            workspace_git_branches_at(root.clone())
                .expect("read selected branch")
                .current
                .as_deref(),
            Some("environment-panel")
        );
        std::fs::remove_dir_all(root).expect("cleanup environment workspace");
    }

    #[test]
    fn chromium_bridge_token_survives_desktop_restarts_privately() {
        const CHILD_ROOT: &str = "PHOENIX_CHROMIUM_TOKEN_TEST_ROOT";

        // This command reads PHOENIX_HOME, so isolate it exactly like the
        // default-workspace test instead of mutating process-global env here.
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let root = std::path::PathBuf::from(root);
            let first = chromium_bridge_token().expect("create bridge token");
            let second = chromium_bridge_token().expect("reuse bridge token");
            assert_eq!(
                first, second,
                "a desktop restart must reuse the gateway credential"
            );
            assert_eq!(first.len(), 64);
            assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
            let path = root.join("chromium-bridge.token");
            assert_eq!(std::fs::read_to_string(&path).expect("token file"), first);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(path)
                        .expect("token metadata")
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
            return;
        }

        let root = isolated_dir("chromium-bridge-token");
        let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .arg("--exact")
            .arg("gateway_compat_tests::chromium_bridge_token_survives_desktop_restarts_privately")
            .arg("--nocapture")
            .env("PHOENIX_HOME", &root)
            .env(CHILD_ROOT, &root)
            .status()
            .expect("run isolated Chromium-token test");
        assert!(status.success(), "isolated Chromium-token test failed");
        std::fs::remove_dir_all(root).expect("cleanup isolated test directory");
    }

    #[test]
    fn composer_path_attachments_keep_files_and_folders_distinct() {
        let root = isolated_dir("composer-path-attachments");
        let file = root.join("brief.txt");
        let folder = root.join("sources");
        std::fs::write(&file, b"hello").expect("seed file");
        std::fs::create_dir(&folder).expect("seed folder");

        let picked_file = composer_path_attachment(&file, false).expect("file attachment");
        assert_eq!(picked_file.name, "brief.txt");
        assert_eq!(picked_file.path, file.display().to_string());
        assert_eq!(picked_file.size, 5);
        assert_eq!(picked_file.mime_type, "application/octet-stream");

        let picked_folder = composer_path_attachment(&folder, true).expect("folder attachment");
        assert_eq!(picked_folder.name, "sources");
        assert_eq!(picked_folder.path, folder.display().to_string());
        assert_eq!(picked_folder.size, 0);
        assert_eq!(picked_folder.mime_type, "inode/directory");
        assert!(composer_path_attachment(&file, true).is_err());
        assert!(composer_path_attachment(&folder, false).is_err());

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&file, root.join("brief-link"))
                .expect("seed attachment symlink");
            assert!(composer_path_attachment(&root.join("brief-link"), false).is_err());
        }
        std::fs::remove_dir_all(root).expect("cleanup attachment test directory");
    }

    #[test]
    fn checkout_desktop_never_falls_back_to_a_stale_user_gateway() {
        let dir = isolated_dir("gateway-selection");
        let repo = dir.join("checkout");
        let desktop = repo.join("canvas-app/target/debug/phoenix-desktop");
        std::fs::create_dir_all(desktop.parent().unwrap()).unwrap();
        std::fs::write(repo.join("Cargo.toml"), b"[workspace]\n").unwrap();
        std::fs::write(
            repo.join("canvas-app/Cargo.toml"),
            b"[package]\nname='canvas'\n",
        )
        .unwrap();
        let home = dir.join("home");
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        std::fs::write(home.join(".local/bin/phoenix"), b"stale").unwrap();

        assert_eq!(
            phoenix_binary_from(None, Some(&desktop), &home),
            repo.join("target/debug/phoenix"),
            "a missing matching gateway must fail honestly instead of launching the user install"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn release_desktop_uses_the_same_checkout_debug_gateway_when_needed() {
        let dir = isolated_dir("gateway-debug-fallback");
        let repo = dir.join("checkout");
        let desktop = repo.join("canvas-app/target/release/phoenix-desktop");
        let debug_gateway = repo.join("target/debug/phoenix");
        std::fs::create_dir_all(desktop.parent().unwrap()).unwrap();
        std::fs::create_dir_all(debug_gateway.parent().unwrap()).unwrap();
        std::fs::write(repo.join("Cargo.toml"), b"[workspace]\n").unwrap();
        std::fs::write(
            repo.join("canvas-app/Cargo.toml"),
            b"[package]\nname='canvas'\n",
        )
        .unwrap();
        std::fs::write(&debug_gateway, b"same checkout").unwrap();

        assert_eq!(
            phoenix_binary_from(None, Some(&desktop), &dir.join("home")),
            debug_gateway
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bundled_gateway_and_explicit_override_win_before_checkout_detection() {
        let dir = isolated_dir("gateway-bundle-selection");
        let desktop = dir.join("bundle/phoenix-desktop");
        let companion = dir.join("bundle/phoenix");
        let explicit = dir.join("override/phoenix");
        std::fs::create_dir_all(desktop.parent().unwrap()).unwrap();
        std::fs::create_dir_all(explicit.parent().unwrap()).unwrap();
        std::fs::write(&companion, b"bundled").unwrap();
        std::fs::write(&explicit, b"explicit").unwrap();

        assert_eq!(phoenix_binary_from(None, Some(&desktop), &dir), companion);
        assert_eq!(
            phoenix_binary_from(Some(&explicit), Some(&desktop), &dir),
            explicit
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn proc_identity_requires_exit_zombie_or_starttime_change() {
        let dir = isolated_dir("proc-identity");
        let proc_root = dir.join("proc");
        let pid = 4242;
        write_proc_stat(&proc_root, pid, "S", "1234");
        assert_eq!(
            process_start_time_at(&proc_root, pid).as_deref(),
            Some("1234")
        );
        assert!(!process_instance_exited_at(&proc_root, pid, Some("1234")));
        assert!(process_instance_exited_at(
            &proc_root,
            pid,
            Some("older-instance")
        ));

        write_proc_stat(&proc_root, pid, "Z", "1234");
        assert!(process_instance_exited_at(&proc_root, pid, Some("1234")));

        std::fs::write(proc_root.join(pid.to_string()).join("stat"), "malformed")
            .expect("malformed fake stat");
        assert!(
            !process_instance_exited_at(&proc_root, pid, Some("1234")),
            "an unreadable identity is not proof that the process departed"
        );
        std::fs::remove_dir_all(proc_root.join(pid.to_string())).expect("remove fake process");
        assert!(process_instance_exited_at(&proc_root, pid, Some("1234")));
        std::fs::remove_dir_all(dir).expect("cleanup isolated test directory");
    }

    #[test]
    fn stale_reclaim_waits_for_recorded_pid_not_only_socket_departure() {
        let dir = isolated_dir("reclaim-waits");
        let home = dir.join("home");
        let proc_root = dir.join("proc");
        std::fs::create_dir_all(&home).expect("fake Phoenix home");
        let pid = 5252;
        std::fs::write(home.join("gateway.pid"), format!("{pid}\n")).expect("fake pidfile");
        std::fs::write(home.join("gateway.sock"), b"stale socket inode")
            .expect("fake stale socket");
        write_proc_stat(&proc_root, pid, "S", "777");

        assert!(
            reclaim_stale_gateway_files_at(&home, &proc_root, std::time::Duration::ZERO).is_err(),
            "a missing listener is not proof that its recorded process exited"
        );
        assert!(home.join("gateway.pid").exists());
        assert!(home.join("gateway.sock").exists());

        write_proc_stat(&proc_root, pid, "Z", "777");
        reclaim_stale_gateway_files_at(&home, &proc_root, std::time::Duration::ZERO)
            .expect("zombie process has released all gateway resources");
        assert!(!home.join("gateway.pid").exists());
        assert!(!home.join("gateway.sock").exists());
        std::fs::remove_dir_all(dir).expect("cleanup isolated test directory");
    }

    #[cfg(unix)]
    #[test]
    fn voice_files_and_payloads_are_private_and_bounded() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let dir = isolated_dir("voice-bounds");
        let wav = dir.join("capture.wav");
        let mut file = create_private_voice_file(&wav).expect("private voice file");
        file.write_all(b"RIFF\x04\0\0\0WAVE")
            .expect("minimal WAV header");
        file.sync_all().expect("sync voice file");
        drop(file);
        assert_eq!(
            std::fs::metadata(&wav)
                .expect("voice metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        validate_capture_file(&wav).expect("private bounded WAV capture");

        let oversized = dir.join("oversized.wav");
        let oversized_file =
            create_private_voice_file(&oversized).expect("private oversized fixture");
        oversized_file
            .set_len(VOICE_CAPTURE_MAX_BYTES + 1)
            .expect("sparse oversized fixture");
        drop(oversized_file);
        assert!(validate_capture_file(&oversized).is_err());

        assert_eq!(
            read_bounded_body(std::io::Cursor::new(b"1234"), 4, "fixture")
                .expect("body at exact limit"),
            b"1234".to_vec()
        );
        assert!(read_bounded_body(std::io::Cursor::new(b"12345"), 4, "fixture").is_err());
        assert!(validate_declared_body_size(Some(5), 4, "fixture").is_err());
        assert!(validate_declared_body_size(Some(4), 4, "fixture").is_ok());
        validate_wav(b"RIFF\x04\0\0\0WAVE", "fixture").expect("WAV signature");
        assert!(validate_wav(b"not a wave", "fixture").is_err());

        let at_limit = "🪽".repeat(VOICE_TTS_INPUT_MAX_CHARS);
        assert!(validate_tts_input(&at_limit).is_ok());
        assert!(validate_tts_input(&(at_limit + "x")).is_err());
        assert!(validate_tts_input(" \n\t ").is_err());
        assert!(validate_voice_config_field(
            "tts",
            "model",
            "m".repeat(VOICE_CONFIG_FIELD_MAX_CHARS),
            true
        )
        .is_ok());
        assert!(validate_voice_config_field(
            "tts",
            "model",
            "m".repeat(VOICE_CONFIG_FIELD_MAX_CHARS + 1),
            true
        )
        .is_err());
        assert!(validate_voice_config_field("stt", "model", String::new(), true).is_err());
        std::fs::remove_dir_all(dir).expect("cleanup isolated test directory");
    }

    #[cfg(unix)]
    #[test]
    fn bounded_voice_stop_reaps_the_child() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn inert voice-child fixture");
        stop_voice_child(
            &mut child,
            libc::SIGTERM,
            std::time::Duration::from_secs(1),
            "voice-child fixture",
        )
        .expect("fixture should honor SIGTERM within the bound");
        assert!(
            child.try_wait().expect("inspect reaped fixture").is_some(),
            "the bounded stop path must reap the child, not only signal it"
        );
    }

    #[cfg(unix)]
    #[test]
    fn voice_capture_identity_fences_late_stop_and_cancel_without_microphone_access() {
        use std::sync::{Arc, Mutex};
        let dir = isolated_dir("voice-owned-capture");
        let path = dir.join("capture.wav");
        super::write_private_voice_file(&path, b"RIFF\x04\0\0\0WAVE").unwrap();
        let child = std::process::Command::new("sleep").arg("30")
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()).spawn().unwrap();
        let capture = Arc::new(Mutex::new(super::VoiceCapture { capture_id:"new-capture".into(), child, path:path.clone() }));
        let slot = Mutex::new(Some(capture.clone()));
        assert!(super::take_owned_voice_capture(&slot, "old-capture").unwrap().is_none());
        assert!(super::take_owned_voice_capture(&slot, "../capture").is_err());
        assert!(super::take_owned_voice_capture(&slot, "").is_err());
        assert!(super::take_owned_voice_capture(&slot, &"a".repeat(129)).is_err());
        assert!(capture.lock().unwrap().child.try_wait().unwrap().is_none(), "a stale cancellation must not signal a newer capture");
        assert!(path.exists());
        let owned = super::take_owned_voice_capture(&slot, "new-capture").unwrap().unwrap();
        assert!(Arc::ptr_eq(&owned, &capture));
        assert!(slot.lock().unwrap().is_none());
        assert!(super::take_owned_voice_capture(&slot, "new-capture").unwrap().is_none());
        drop(owned);drop(capture);
        assert!(!path.exists(), "owned capture Drop removes its private file after reaping its child");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn voice_preparation_cancel_fences_delayed_start_and_newer_owner() {
        let mut pending = Some("old-capture".to_string());
        assert!(!super::cancel_prepared_voice_capture(&mut pending, "foreign"));
        assert_eq!(pending.as_deref(), Some("old-capture"));
        assert!(super::cancel_prepared_voice_capture(&mut pending, "old-capture"));
        assert!(super::consume_prepared_voice_capture(&mut pending, "old-capture").is_err());
        assert!(!super::cancel_prepared_voice_capture(&mut pending, "old-capture"));
        pending = Some("new-capture".into());
        assert!(super::consume_prepared_voice_capture(&mut pending, "old-capture").is_err());
        assert!(!super::cancel_prepared_voice_capture(&mut pending, "old-capture"));
        assert_eq!(pending.as_deref(), Some("new-capture"));
        super::consume_prepared_voice_capture(&mut pending, "new-capture").unwrap();
        assert!(pending.is_none());
        assert!(super::consume_prepared_voice_capture(&mut pending, "new-capture").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn vitals_mutations_share_one_private_lock_and_preserve_unreadable_data() {
        use std::os::unix::fs::PermissionsExt;

        let dir = isolated_dir("vitals-rmw");
        let path = dir.join("VITALS.md");
        std::thread::scope(|scope| {
            for index in 0..8 {
                let path = path.clone();
                scope.spawn(move || {
                    vitals_write_at(
                        &path,
                        format!("concurrent preference {index}"),
                        "preference".to_string(),
                    )
                    .expect("locked VITALS append");
                });
            }
        });
        let raw = std::fs::read_to_string(&path).expect("read VITALS");
        let preferences = vitals_parse(&raw)
            .into_iter()
            .find(|(category, _)| *category == "preference")
            .expect("preference bucket")
            .1;
        assert_eq!(preferences.len(), 8, "no concurrent write may be lost");
        assert_eq!(
            std::fs::metadata(&path)
                .expect("VITALS metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(dir.join(".VITALS.md.lock"))
                .expect("shared VITALS lock")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        vitals_delete_at(
            &path,
            None,
            Some("preference".to_string()),
            Some("concurrent preference 0".to_string()),
        )
        .expect("locked VITALS delete");
        assert!(!std::fs::read_to_string(&path)
            .expect("read deleted VITALS")
            .contains("concurrent preference 0\n"));

        std::fs::write(&path, [0xff, 0xfe]).expect("seed unreadable VITALS");
        assert!(vitals_write_at(&path, "must not land".into(), "goal".into()).is_err());
        assert!(
            vitals_delete_at(&path, None, Some("goal".into()), Some("anything".into())).is_err()
        );
        assert_eq!(
            std::fs::read(&path).expect("preserved VITALS"),
            [0xff, 0xfe]
        );
        std::fs::remove_dir_all(dir).expect("cleanup isolated test directory");
    }

    #[cfg(unix)]
    #[test]
    fn prefs_revision_is_durable_exact_cas_with_rolling_backup() {
        use std::os::unix::fs::PermissionsExt;

        let dir = isolated_dir("prefs-cas");
        let path = dir.join("canvas-prefs.json");
        let missing = prefs_get_at(&path).expect("missing prefs snapshot");
        assert_eq!(missing.prefs, serde_json::json!({}));
        assert_eq!(missing.revision, PREFS_MISSING_REVISION);

        let first = prefs_set_at(
            &path,
            serde_json::json!({"view": {"x": 1}}),
            Some(missing.revision.clone()),
        )
        .expect("initial prefs CAS");
        assert!(prefs_set_at(
            &path,
            serde_json::json!({"view": {"x": 999}}),
            Some(missing.revision)
        )
        .is_err());
        assert_eq!(
            prefs_get_at(&path).expect("current prefs").prefs,
            serde_json::json!({"view": {"x": 1}})
        );

        let expected = first.revision;
        let outcomes = std::thread::scope(|scope| {
            let left_path = path.clone();
            let left_revision = expected.clone();
            let left = scope.spawn(move || {
                prefs_set_at(
                    &left_path,
                    serde_json::json!({"writer": "left"}),
                    Some(left_revision),
                )
            });
            let right_path = path.clone();
            let right = scope.spawn(move || {
                prefs_set_at(
                    &right_path,
                    serde_json::json!({"writer": "right"}),
                    Some(expected),
                )
            });
            [
                left.join().expect("left writer"),
                right.join().expect("right writer"),
            ]
        });
        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(outcomes.iter().filter(|result| result.is_err()).count(), 1);
        assert_eq!(
            std::fs::metadata(&path)
                .expect("prefs metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let backup = dir.join("canvas-prefs.json.bak");
        assert!(prefs_get_at(&path).is_ok());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &std::fs::read(&backup).expect("rolling backup")
            )
            .expect("valid rolling backup"),
            serde_json::json!({"view": {"x": 1}})
        );
        assert_eq!(
            std::fs::metadata(&backup)
                .expect("backup metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        std::fs::write(&path, b"{broken").expect("seed corrupt prefs");
        assert!(prefs_get_at(&path).is_err());
        assert!(prefs_set_at(
            &path,
            serde_json::json!({"would": "erase corruption"}),
            Some("{broken".to_string())
        )
        .is_err());
        assert_eq!(
            std::fs::read(&path).expect("preserved corrupt prefs"),
            b"{broken"
        );
        std::fs::remove_dir_all(dir).expect("cleanup isolated test directory");
    }

    #[cfg(unix)]
    #[test]
    fn feed_reads_only_treat_not_found_as_empty() {
        use std::os::unix::fs::PermissionsExt;

        let dir = isolated_dir("feed-errors");
        let path = dir.join("session.json");
        assert_eq!(
            feeds_get_at(&path).expect("missing feed"),
            serde_json::json!({})
        );
        feeds_set_at(&path, serde_json::json!({"orchestrator": "<p>hello</p>"}))
            .expect("write private feed");
        assert_eq!(
            feeds_get_at(&path).expect("read feed"),
            serde_json::json!({"orchestrator": "<p>hello</p>"})
        );
        assert_eq!(
            std::fs::metadata(&path)
                .expect("feed metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        std::fs::write(&path, b"not json").expect("seed corrupt feed");
        assert!(feeds_get_at(&path).is_err());
        assert!(feeds_set_at(&path, serde_json::json!({"would": "erase it"})).is_err());
        assert_eq!(std::fs::read(&path).expect("preserved feed"), b"not json");
        assert!(feeds_file("").is_err());
        assert!(feeds_file("../../other-session").is_err());
        std::fs::remove_dir_all(dir).expect("cleanup isolated test directory");
    }

    #[test]
    fn group_owner_cannot_read_a_direct_agents_canonical_session() {
        let home = isolated_dir("canonical-owner-mismatch");
        seed_canvas_owner_directory(
            &home,
            &[("iris", Some("agent-iris"))],
            &[("design", Some("group-design"))],
        );
        let sessions = home.join("sessions");
        std::fs::create_dir_all(&sessions).expect("session fixture directory");
        // Invalid JSON proves authorization runs before even parsing the
        // transcript: a cross-owner request must expose zero rows/content.
        std::fs::write(
            sessions.join("agent-iris.json"),
            b"PRIVATE DIRECT TRANSCRIPT",
        )
        .expect("private direct fixture");

        let error = session_context_get_blocking_at(
            &home,
            "agent-iris".to_string(),
            &canvas_owner(CanvasConversationOwnerKind::Group, "design"),
        )
        .expect_err("group must not read a direct agent transcript");
        assert!(
            error.contains("owner mismatch"),
            "unexpected error: {error}"
        );
        assert!(
            !error.contains("PRIVATE DIRECT TRANSCRIPT") && !error.contains("invalid"),
            "authorization must reject before transcript bytes are parsed: {error}"
        );
        std::fs::remove_dir_all(home).expect("cleanup owner mismatch fixture");
    }

    #[test]
    fn canonical_direct_group_and_phoenix_owners_read_only_their_sessions() {
        let home = isolated_dir("canonical-owner-success");
        seed_canvas_owner_directory(
            &home,
            &[
                ("phoenix", Some("company-phoenix")),
                ("iris", Some("agent-iris")),
            ],
            &[("design", Some("group-design"))],
        );
        let sessions = home.join("sessions");
        std::fs::create_dir_all(&sessions).expect("session fixture directory");
        for (session_id, prompt) in [
            ("company-phoenix", "Phoenix prompt"),
            ("agent-iris", "Iris prompt"),
            ("group-design", "Design prompt"),
        ] {
            std::fs::write(
                sessions.join(format!("{session_id}.json")),
                serde_json::to_vec(&serde_json::json!({
                    "messages": [{"type":"User", "content":prompt}]
                }))
                .expect("session JSON fixture"),
            )
            .expect("session fixture");
        }

        for (session_id, owner, expected) in [
            (
                "company-phoenix",
                canvas_owner(CanvasConversationOwnerKind::Agent, "phoenix"),
                "Phoenix prompt",
            ),
            (
                "agent-iris",
                canvas_owner(CanvasConversationOwnerKind::Agent, "iris"),
                "Iris prompt",
            ),
            (
                "group-design",
                canvas_owner(CanvasConversationOwnerKind::Group, "design"),
                "Design prompt",
            ),
        ] {
            let rows = session_context_get_blocking_at(&home, session_id.to_string(), &owner)
                .expect("correct canonical owner must read its transcript");
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0]["text"], expected);
        }
        std::fs::remove_dir_all(home).expect("cleanup owner success fixture");
    }

    #[test]
    fn direct_history_preserves_canonical_work_owner() {
        let home = isolated_dir("history-work-owner");
        seed_canvas_owner_directory(&home, &[("researcher", Some("agent-researcher"))], &[]);
        std::fs::create_dir_all(home.join("sessions")).unwrap();
        std::fs::write(home.join("sessions/agent-researcher.json"), serde_json::to_vec(&serde_json::json!({
            "messages": [
                {"type":"User","content":"Check the source"},
                {"type":"Assistant","content":"Checking the publication date."},
                {"type":"ToolResult","tool_name":"read","input":"source.md","success":true,"output":"Date verified"},
                {"type":"Talk","from":"researcher","to":"scribe","subject":"Prepare the evidence","body":"Use the verified date","reply_expected":true}
            ]
        })).unwrap()).unwrap();
        let rows = session_context_get_blocking_at(&home, "agent-researcher".into(), &canvas_owner(CanvasConversationOwnerKind::Agent, "researcher")).unwrap();
        for row in rows.iter().filter(|row|row["role"]=="tool"||row["role"]=="narration") {
            assert_eq!(row["agent"], "researcher");
        }
        let talk=rows.iter().find(|row|row["role"]=="talk").unwrap();
        assert!(talk.get("turn_id").is_none(), "handoffs cannot invent a new user turn");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn a_steer_the_user_sent_mid_turn_shows_as_their_message() {
        let home = isolated_dir("steer-shows-as-user");
        seed_canvas_owner_directory(&home, &[("phoenix", Some("agent-phoenix"))], &[]);
        std::fs::create_dir_all(home.join("sessions")).unwrap();
        std::fs::write(home.join("sessions/agent-phoenix.json"), serde_json::to_vec(&serde_json::json!({
            "messages": [
                {"type":"User","content":"Animate the marks"},
                {"type":"Talk","from":"user","to":"orchestrator","subject":"MID-TASK MESSAGE — queued prompt steered now",
                 "body":"Layer one morphs in while layer two morphs out.\n\n(This arrived WHILE you are working. Apply the correction or redirect on your next action.)","reply_expected":false}
            ]
        })).unwrap()).unwrap();
        let rows = session_context_get_blocking_at(&home, "agent-phoenix".into(), &canvas_owner(CanvasConversationOwnerKind::Agent, "phoenix")).unwrap();
        let prompts: Vec<_> = rows.iter().filter(|row| row["role"] == "user").map(|row| row["text"].as_str().unwrap_or("")).collect();
        assert_eq!(prompts, ["Animate the marks", "Layer one morphs in while layer two morphs out."]);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn current_late_answer_wording_stays_a_turn_boundary() {
        let home = isolated_dir("late-answer-boundary");
        seed_canvas_owner_directory(&home, &[("phoenix", Some("agent-phoenix"))], &[]);
        std::fs::create_dir_all(home.join("sessions")).unwrap();
        std::fs::write(home.join("sessions/agent-phoenix.json"), serde_json::to_vec(&serde_json::json!({
            "messages": [
                {"type":"User","content":"make your checks hourly"},
                {"type":"Assistant","content":"Done, checks are now hourly."},
                {"type":"User","content":"[late ask answer] The user answered the saved question ask-6011138c: \"Collected 1 answer(s) from user:\n\n  [Reply] Q: May I send it?\n  A: Approve replacement\". Continue the work this answer unblocks."},
                {"type":"Assistant","content":"Sent."}
            ]
        })).unwrap()).unwrap();
        let rows = session_context_get_blocking_at(&home, "agent-phoenix".into(), &canvas_owner(CanvasConversationOwnerKind::Agent, "phoenix")).unwrap();
        let prompts: Vec<_> = rows.iter().filter(|row| row["role"] == "user").map(|row| row["text"].as_str().unwrap_or("").to_string()).collect();
        assert_eq!(prompts.len(), 2, "{rows:?}");
        assert!(prompts[1].contains("Approve replacement"), "{prompts:?}");
        assert!(rows.iter().any(|row| row["role"] == "answer" && row["text"] == "Done, checks are now hourly."), "{rows:?}");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn compacted_prompts_come_back_from_the_session_archive() {
        let home = isolated_dir("compacted-prompts-archive");
        seed_canvas_owner_directory(&home, &[("frontend", Some("agent-frontend"))], &[]);
        std::fs::create_dir_all(home.join("sessions")).unwrap();
        std::fs::write(home.join("sessions/agent-frontend.archive.jsonl"), [
            r#"{"type":"User","content":"Build the Kornblume bakery site"}"#,
            r#"{"type":"Assistant","content":"Kornblume is ready."}"#,
            r#"{"type":"User","content":"continue"}"#,
        ].join("\n")).unwrap();
        std::fs::write(home.join("sessions/agent-frontend.json"), serde_json::to_vec(&serde_json::json!({
            "messages": [
                {"type":"Assistant","content":"[AUTO-COMPACTED HISTORY — 3 earlier messages were folded]"},
                {"type":"Assistant","content":"The preview is running."}
            ]
        })).unwrap()).unwrap();
        let rows = session_context_get_blocking_at(&home, "agent-frontend".into(), &canvas_owner(CanvasConversationOwnerKind::Agent, "frontend")).unwrap();
        let prompts: Vec<_> = rows.iter().filter(|row| row["role"] == "user").map(|row| row["text"].as_str().unwrap_or("")).collect();
        assert_eq!(prompts, ["Build the Kornblume bakery site", "continue"]);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn direct_agent_projection_keeps_the_latest_answer_after_repeated_prompts() {
        let home = isolated_dir("direct-agent-latest-answer");
        seed_canvas_owner_directory(&home, &[("iris", Some("agent-iris"))], &[]);
        let sessions = home.join("sessions");
        std::fs::create_dir_all(&sessions).expect("session fixture directory");
        std::fs::write(
            sessions.join("agent-iris.json"),
            serde_json::to_vec(&serde_json::json!({
                "messages": [
                    {"type":"User", "content":"Build the right sidebar."},
                    {"type":"Assistant", "content":"I researched the interaction and started the implementation."},
                    {"type":"User", "content":"Build the right sidebar."},
                    {"type":"Assistant", "content":"The sidebar is implemented and visually verified."}
                ]
            }))
            .expect("direct session JSON fixture"),
        )
        .expect("direct session fixture");

        let rows = session_context_get_blocking_at(
            &home,
            "agent-iris".to_string(),
            &canvas_owner(CanvasConversationOwnerKind::Agent, "iris"),
        )
        .expect("Iris reads her canonical direct transcript");
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[3]["role"], "answer");
        assert_eq!(
            rows[3]["text"],
            "The sidebar is implemented and visually verified."
        );
        std::fs::remove_dir_all(home).expect("cleanup direct agent fixture");
    }

    #[test]
    fn group_history_keeps_repeated_prompts_with_their_authored_turns() {
        let home = isolated_dir("group-authored-turns");
        seed_canvas_owner_directory(&home, &[], &[("design", Some("group-design"))]);
        std::fs::create_dir_all(home.join("sessions")).unwrap();
        std::fs::write(home.join("sessions/group-design.json"), serde_json::to_vec(&serde_json::json!({
            "messages": [
                {"type":"User","content":"Continue"},
                {"type":"ToolResult","tool_name":"__phoenix_group_user_boundary","input":"turn-one","success":true,"output":""},
                {"type":"User","content":"Continue"},
                {"type":"ToolResult","tool_name":"__phoenix_group_user_boundary","input":"turn-two","success":true,"output":""}
            ]
        })).unwrap()).unwrap();
        let rows = session_context_get_blocking_at(&home, "group-design".into(),
            &canvas_owner(CanvasConversationOwnerKind::Group, "design")).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["turn_id"], "turn-one");
        assert_eq!(rows[1]["turn_id"], "turn-two");
        assert!(rows.iter().all(|row| row["role"] == "user"));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn canonical_group_contribution_recovers_immutable_author_snapshot() {
        let home = isolated_dir("canonical-group-contribution");
        seed_canvas_owner_directory(&home, &[], &[("design", Some("group-design"))]);
        let sessions = home.join("sessions");
        std::fs::create_dir_all(&sessions).expect("session fixture directory");
        std::fs::write(
            sessions.join("group-design.json"),
            serde_json::to_vec(&serde_json::json!({
                "messages": [{
                    "type":"GroupContribution",
                    "turn_id":"turn-42",
                    "message_id":"group-message-7",
                    "group_id":"design",
                    "agent_id":"iris",
                    "internal_role":"frontend",
                    "display_name":"Iris Before Rename",
                    "role_title":"Product Designer",
                    "color":"#d46a43",
                    "icon_seed":"iris-flame",
                    "avatar":{"shape":"wild","expression":"curious"},
                    "subject":"UI findings",
                    "body":"The composer needs a stable preview row.",
                    "reply_to":"turn-42",
                    "causation_id":"turn-42"
                }]
            }))
            .expect("group contribution JSON"),
        )
        .expect("group session fixture");

        let rows = session_context_get_blocking_at(
            &home,
            "group-design".to_string(),
            &canvas_owner(CanvasConversationOwnerKind::Group, "design"),
        )
        .expect("group owner reads canonical contribution");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["role"], "group_message");
        assert_eq!(rows[0]["turn_id"], "turn-42");
        assert_eq!(rows[0]["message_id"], "group-message-7");
        assert_eq!(rows[0]["agent_id"], "iris");
        assert_eq!(rows[0]["agent_name"], "Iris Before Rename");
        assert_eq!(rows[0]["agent_snapshot"]["role_title"], "Product Designer");
        assert_eq!(rows[0]["agent_snapshot"]["color"], "#d46a43");
        assert_eq!(rows[0]["agent_snapshot"]["avatar"]["shape"], "wild");
        assert_eq!(
            rows[0]["markdown"],
            "The composer needs a stable preview row."
        );
        assert_eq!(rows[0]["reply_to"], "turn-42");
        assert_eq!(rows[0]["causation_id"], "turn-42");
        std::fs::remove_dir_all(home).expect("cleanup group contribution fixture");
    }

    #[test]
    fn composer_attachment_envelope_projects_as_typed_media_not_prompt_text() {
        let source = concat!(
            "Please inspect this screenshot.\n\n",
            "[The user attached the following item(s) to THIS message. inspect images with `image_analyze`, and inspect files with `read` before answering; do not treat files or folders as images:\n",
            "- image: \"/home/example/Pictures/Screenshots/problem.png\"\n",
            "- file: \"/home/example/brief.pdf\"]"
        );
        let (text, attachments) = canvas_user_message_parts(source);
        assert_eq!(text, "Please inspect this screenshot.");
        assert_eq!(attachments.len(), 2);
        assert_eq!(attachments[0].name, "problem.png");
        assert_eq!(attachments[0].mime_type, "image/png");
        assert_eq!(attachments[1].name, "brief.pdf");
        assert_eq!(attachments[1].mime_type, "application/octet-stream");
        assert!(!text.contains("The user attached"));

        let legacy = concat!(
            "Older prompt.\n\n",
            "[The user attached images to THIS message. View each one with the `image_analyze` tool before answering:\n",
            "- /home/example/old-photo.jpg]"
        );
        let (legacy_text, legacy_attachments) = canvas_user_message_parts(legacy);
        assert_eq!(legacy_text, "Older prompt.");
        assert_eq!(legacy_attachments[0].mime_type, "image/jpeg");
    }

    #[test]
    fn image_picker_prefers_screenshots_then_pictures_then_home() {
        let home = isolated_dir("screenshots-picker");
        let pictures = home.join("Pictures");
        let screenshots = pictures.join("Screenshots");

        assert_eq!(screenshots_picker_directory_from(&home), Some(home.clone()));
        std::fs::create_dir_all(&pictures).expect("pictures fixture");
        assert_eq!(
            screenshots_picker_directory_from(&home),
            Some(pictures.clone())
        );
        std::fs::create_dir_all(&screenshots).expect("screenshots fixture");
        assert_eq!(screenshots_picker_directory_from(&home), Some(screenshots));
        std::fs::remove_dir_all(home).expect("cleanup screenshots picker fixture");
    }

    fn seed_canvas_group_history(label: &str) -> (std::path::PathBuf, rusqlite::Connection) {
        let home = isolated_dir(label);
        seed_canvas_owner_directory(&home,
            &[("frontend", Some("agent-frontend")), ("researcher", Some("agent-researcher")), ("coder", Some("agent-coder"))],
            &[("design", Some("group-design"))]);
        let db = rusqlite::Connection::open(home.join("company/company.sqlite")).unwrap();
        db.execute_batch("ALTER TABLE company_agents ADD COLUMN internal_role TEXT;
            UPDATE company_agents SET internal_role=agent_id;
            CREATE TABLE company_group_members(group_id TEXT,agent_id TEXT);
            INSERT INTO company_group_members SELECT 'design',agent_id FROM company_agents;
            CREATE TABLE company_group_turns(canonical_session_id TEXT,turn_id TEXT,group_id TEXT,PRIMARY KEY(canonical_session_id,turn_id));
            CREATE TABLE company_group_turn_members(canonical_session_id TEXT,turn_id TEXT,activation_id TEXT,activation_ordinal INTEGER,
                agent_id TEXT,participant_json TEXT,state TEXT,status_detail TEXT,receipt_id TEXT);
            CREATE TABLE company_messages(message_id TEXT PRIMARY KEY,handoff_id TEXT,reply_to TEXT,causation_id TEXT,
                from_agent TEXT,to_agent TEXT,subject TEXT,body TEXT,reply_expected INTEGER,state TEXT,as_of_seq INTEGER,
                message_kind TEXT,session_id TEXT,run_id TEXT);").unwrap();
        for turn in ["turn-one", "turn-two"] {
            db.execute("INSERT INTO company_group_turns VALUES('group-design',?1,'design')", [turn]).unwrap();
            let snapshot = serde_json::json!({"agent_id":"frontend","internal_role":"frontend","display_name":"Iris Before Rename"});
            db.execute("INSERT INTO company_group_turn_members VALUES('group-design',?1,?2,0,'frontend',?3,'blocked','Task did not finish',?4)",
                rusqlite::params![turn,format!("activation-{turn}"),snapshot.to_string(),
                    (turn == "turn-two").then_some("owner-result")]).unwrap();
        }
        for (index, (id, handoff, reply, cause, from, to, requested, subject, body)) in [
            ("images", "images", None, "turn-one", "frontend", "researcher", true, "Find imagery", "Private request details stay out of the projection"),
            ("image-return", "images", Some("images"), "images", "researcher", "frontend", false, "Image sources", "Published image source map"),
            ("first-review", "first-review", None, "images", "frontend", "coder", true, "Check interactions", "First review"),
            ("second-review", "second-review", None, "turn-two", "frontend", "coder", true, "Check interactions", "Repeated review"),
            ("failure-delivery", "failure-delivery", Some("second-review"), "second-review", "coder", "frontend", false, "coder turn failed", "The `coder` agent could not complete its turn: usage limit reached"),
        ].into_iter().enumerate() {
            db.execute("INSERT INTO company_messages VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'injected',?10,'conversation','group-design','group-design')",
                rusqlite::params![id,handoff,reply,cause,from,to,subject,body,requested as i64,index as i64]).unwrap();
        }
        std::fs::create_dir_all(home.join("sessions")).unwrap();
        let messages = serde_json::json!({"messages":[
            {"type":"User","content":"Continue"},
            {"type":"ToolResult","tool_name":"__phoenix_group_user_boundary","input":"turn-one"},
            {"type":"User","content":"Continue"},
            {"type":"ToolResult","tool_name":"__phoenix_group_user_boundary","input":"turn-two"},
            {"type":"GroupContribution","turn_id":"turn-two","message_id":"owner-result","group_id":"design",
             "agent_id":"frontend","internal_role":"frontend","display_name":"Iris Before Rename","subject":"frontend turn failed",
             "body":"The provider became unavailable after saved work."}
        ]});
        std::fs::write(home.join("sessions/group-design.json"), messages.to_string()).unwrap();
        for file in ["agent-frontend.json", "agent-researcher.json", "group-design__frontend.json", "group-design__researcher.json"] {
            std::fs::write(home.join("sessions").join(file), "PRIVATE PEER HISTORY: deliberately invalid JSON").unwrap();
        }
        (home, db)
    }

    fn read_canvas_group_history(home: &std::path::Path) -> Vec<serde_json::Value> {
        session_context_get_blocking_at(home, "group-design".into(),
            &canvas_owner(CanvasConversationOwnerKind::Group, "design")).unwrap()
    }

    #[test]
    fn group_history_recovers_published_handoffs_and_failure_after_disk_reopen() {
        let (home, db) = seed_canvas_group_history("group-history-reopen");
        assert!(!home.join("company.sqlite").exists(), "fixture must use the actual runtime database path");
        let before = std::fs::read(home.join("sessions/group-design.json")).unwrap();
        let rows = read_canvas_group_history(&home);
        assert_eq!(rows.iter().filter(|r| r["role"] == "user").count(), 2);
        assert_eq!(rows.iter().filter(|r| r["role"] == "handoff").count(), 3);
        assert_eq!(rows.iter().filter(|r| r["role"] == "return").count(), 2);
        assert_eq!(rows.iter().filter(|r| r["role"] == "group_member_status").count(), 2);
        assert_eq!(rows.iter().filter(|r| r["role"] == "group_message").count(), 1);
        let delivery = |id: &str| rows.iter().find(|r| r["message_id"] == id).unwrap();
        assert_eq!(delivery("first-review")["turn_id"], "turn-one", "nested causation follows the original image request");
        assert_eq!(delivery("first-review")["status"], "inactive", "injected is not successful work or live execution");
        assert!(delivery("images")["body"].as_str().unwrap().is_empty());
        assert_eq!(delivery("image-return")["status"], "returned");
        assert!(delivery("image-return")["ok"].is_null(), "a returned body is not proof of success");
        assert_eq!(delivery("failure-delivery")["handoff_id"], "failure-delivery");
        assert_eq!(delivery("failure-delivery")["reply_to"], "second-review");
        assert_eq!(delivery("failure-delivery")["turn_id"], "turn-two");
        assert_eq!(delivery("failure-delivery")["ok"], false);
        assert_eq!(delivery("failure-delivery")["status"], "blocked");
        let mut turn = "";
        for row in &rows {
            if row["role"] == "user" { turn = row["turn_id"].as_str().unwrap(); }
            assert_eq!(row["turn_id"], turn, "all rows remain inside their exact repeated-prompt boundary");
            assert!(row.get("execution").is_none() && row.get("event_sequence").is_none());
        }
        assert!(!serde_json::to_string(&rows).unwrap().contains("PRIVATE PEER HISTORY"));
        assert!(!rows.iter().any(|r| r["role"] == "tool" || r["role"] == "notice"));
        drop(db);
        assert_eq!(read_canvas_group_history(&home), rows, "reopening the actual disk database is idempotent");
        assert_eq!(std::fs::read(home.join("sessions/group-design.json")).unwrap(), before);
        assert!(!home.join("canvas-feeds").exists());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn group_history_failed_receipt_overrides_legacy_done_activation() {
        let (home, db) = seed_canvas_group_history("group-history-legacy-failure");
        db.execute("UPDATE company_group_turn_members SET state='done',status_detail='Contribution saved' WHERE turn_id='turn-two'", []).unwrap();
        let original = std::fs::read(home.join("sessions/group-design.json")).unwrap();
        let rows = read_canvas_group_history(&home);
        let status = rows.iter().find(|row| row["role"] == "group_member_status" && row["turn_id"] == "turn-two").unwrap();
        assert_eq!(status["state"], "blocked", "the exact saved failure overrides a stale successful ledger state");
        assert_eq!(status["recorded_state"], "done", "read-only recovery must retain the original state for diagnosis");
        assert_eq!(std::fs::read(home.join("sessions/group-design.json")).unwrap(), original);
        let recorded: String = db.query_row("SELECT state FROM company_group_turn_members WHERE turn_id='turn-two'", [], |row| row.get(0)).unwrap();
        assert_eq!(recorded, "done", "display recovery must not mutate the ledger");
        drop(db); std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn group_history_rejects_foreign_or_ambiguous_lineage_and_wrong_return_routes() {
        let (home, db) = seed_canvas_group_history("group-history-privacy");
        for (id, reply, cause, from, to, requested, session) in [
            ("foreign", None, "turn-one", "frontend", "researcher", 1, "another-group"),
            ("unactivated", None, "turn-one", "researcher", "coder", 1, "group-design"),
            ("wrong-sender", Some("images"), "images", "coder", "frontend", 0, "group-design"),
            ("wrong-turn", Some("second-review"), "images", "coder", "frontend", 0, "group-design"),
            ("cycle-a", None, "cycle-b", "frontend", "coder", 1, "group-design"),
            ("cycle-b", None, "cycle-a", "coder", "frontend", 1, "group-design"),
            ("orphan", None, "missing-turn", "frontend", "coder", 1, "group-design"),
            ("private-peer", None, "turn-one", "finance", "frontend", 1, "group-design"),
            ("malformed-flag", None, "turn-one", "frontend", "coder", 2, "group-design"),
        ] {
            db.execute("INSERT INTO company_messages VALUES(?1,?1,?2,?3,?4,?5,'UNVERIFIED SUBJECT','DO NOT EXPOSE THIS BODY',?6,'injected',90,'conversation',?7,?7)",
                rusqlite::params![id,reply,cause,from,to,requested,session]).unwrap();
        }
        let rows = read_canvas_group_history(&home);
        assert_eq!(rows.iter().filter(|r| r["role"] == "handoff").count(), 3);
        assert_eq!(rows.iter().filter(|r| r["role"] == "return").count(), 2);
        assert_eq!(rows.iter().filter(|r| r["role"] == "notice").count(), 1);
        let text = serde_json::to_string(&rows).unwrap();
        assert!(!text.contains("DO NOT EXPOSE") && !text.contains("UNVERIFIED SUBJECT"));
        let error = session_context_get_blocking_at(&home, "agent-researcher".into(),
            &canvas_owner(CanvasConversationOwnerKind::Group, "design")).unwrap_err();
        assert!(error.contains("owner mismatch") && !error.contains("PRIVATE"));
        db.execute("UPDATE company_messages SET handoff_id='images' WHERE message_id='second-review'", []).unwrap();
        let ambiguous = read_canvas_group_history(&home);
        assert!(!ambiguous.iter().any(|r| ["images", "second-review", "image-return", "first-review", "failure-delivery"]
            .iter().any(|id| r["message_id"] == *id)), "ambiguous handoff aliases cannot be resolved by arrival order");
        assert_eq!(ambiguous.iter().filter(|r| r["role"] == "user").count(), 2);
        drop(db); std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn group_history_validates_declared_phoenix_and_custom_agent_aliases() {
        let (home, db) = seed_canvas_group_history("group-history-aliases");
        db.execute_batch("INSERT INTO company_agents VALUES('phoenix','agent-phoenix','phoenix');
            INSERT INTO company_agents VALUES('custom-reviewer','agent-custom','reviewer');
            INSERT INTO company_group_members VALUES('design','phoenix'),('design','custom-reviewer');
            UPDATE company_messages SET from_agent='orchestrator' WHERE from_agent='frontend';
            UPDATE company_messages SET to_agent='orchestrator' WHERE to_agent='frontend';
            UPDATE company_messages SET to_agent='reviewer' WHERE to_agent='researcher';
            UPDATE company_messages SET from_agent='reviewer' WHERE from_agent='researcher';").unwrap();
        let snapshot = serde_json::json!({"agent_id":"phoenix","internal_role":"phoenix","display_name":"Phoenix"});
        db.execute("UPDATE company_group_turn_members SET agent_id='phoenix',participant_json=?1", [snapshot.to_string()]).unwrap();
        let rows = read_canvas_group_history(&home);
        let request = rows.iter().find(|r| r["message_id"] == "images").unwrap();
        assert_eq!(request["from"], "phoenix");
        assert_eq!(request["from_internal_role"], "orchestrator");
        assert_eq!(request["to"], "custom-reviewer");
        assert_eq!(rows.iter().filter(|r| r["role"] == "return").count(), 2);
        db.execute_batch("INSERT INTO company_agents VALUES('reviewer','agent-unrelated','finance');
            INSERT INTO company_group_members VALUES('design','reviewer');").unwrap();
        let ambiguous = read_canvas_group_history(&home);
        assert!(!ambiguous.iter().any(|r| r["message_id"] == "images" || r["message_id"] == "image-return"));
        assert!(ambiguous.iter().any(|r| r["role"] == "notice"));
        drop(db); std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn group_history_bounds_untrusted_results_and_never_promotes_pending_delivery() {
        let (home, db) = seed_canvas_group_history("group-history-bounds");
        let snapshot = serde_json::json!({"agent_id":"frontend","internal_role":"frontend","display_name":"Iris",
            "future_private_field":"PRIVATE PEER DATA", "metadata":{"not_public":"PRIVATE"}});
        db.execute("UPDATE company_group_turn_members SET participant_json=?1", [snapshot.to_string()]).unwrap();
        db.execute("UPDATE company_messages SET state='accepted' WHERE message_id='image-return'", []).unwrap();
        db.execute("UPDATE company_messages SET body=?1 WHERE message_id='failure-delivery'", ["PRIVATE".repeat(12000)]).unwrap();
        db.execute("UPDATE company_group_turn_members SET state='done',receipt_id='missing'", []).unwrap();
        let rows = read_canvas_group_history(&home);
        assert!(!rows.iter().any(|r| r["role"] == "return"));
        assert!(rows.iter().filter(|r| r["role"] == "group_member_status").all(|r| r["state"] == "inactive"));
        assert!(!serde_json::to_string(&rows).unwrap().contains("PRIVATE"));
        for index in 0..24 {
            db.execute("INSERT INTO company_messages VALUES(?1,?1,'images','images','researcher','frontend','Source map',?2,0,'injected',100+?3,'conversation','group-design','group-design')",
                rusqlite::params![format!("large-result-{index}"),"a".repeat(52_000),index]).unwrap();
        }
        let limited = read_canvas_group_history(&home);
        assert!(limited.iter().any(|r| r["role"] == "return"), "preserve verified returns within the byte budget");
        assert!(limited.iter().any(|r| r["role"] == "notice"));
        assert!(limited.iter().filter(|r| r["historical"] == true).map(|r| r.to_string().len()).sum::<usize>() <= 1024 * 1024);
        assert_eq!(limited.iter().filter(|r| r["role"] == "user").count(), 2);
        db.execute("DELETE FROM company_messages WHERE message_id LIKE 'large-result-%'", []).unwrap();
        for index in 0..270 {
            let id = format!("request-{index}");
            db.execute("INSERT INTO company_messages VALUES(?1,?1,NULL,'turn-two','frontend','coder','Review','',1,'injected',1000+?2,'conversation','group-design','group-design')",
                rusqlite::params![id,index]).unwrap();
        }
        let bounded = read_canvas_group_history(&home);
        assert!(bounded.iter().filter(|r| r["role"] == "handoff").count() <= 256);
        assert_eq!(bounded.iter().filter(|r| r["role"] == "user").count(), 2);
        assert!(bounded.iter().any(|r| r["role"] == "notice"));
        db.execute("UPDATE company_group_turns SET group_id='other' WHERE turn_id='turn-one'", []).unwrap();
        assert!(read_canvas_group_history(&home).iter().filter(|r| r["turn_id"] == "turn-one").all(|r| r["role"] == "user"));
        drop(db); std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn canonical_talk_rows_project_stable_handoff_and_return_causality() {
        let home = isolated_dir("canonical-handoff-causality");
        seed_canvas_owner_directory(&home, &[("iris", Some("agent-iris"))], &[]);
        let sessions = home.join("sessions");
        std::fs::create_dir_all(&sessions).expect("session fixture directory");
        std::fs::write(
            sessions.join("agent-iris.json"),
            serde_json::to_vec(&serde_json::json!({
                "messages": [
                    {
                        "type":"Talk", "from":"iris", "to":"scribe",
                        "subject":"Draft copy", "body":"Make it concise.",
                        "reply_expected":true, "handoff_id":"message_draft",
                        "causation_id":"turn_7", "status":"queued"
                    },
                    {
                        "type":"Talk", "from":"scribe", "to":"iris",
                        "subject":"Draft copy", "body":"Draft is ready.",
                        "reply_expected":false, "handoff_id":"message_return",
                        "reply_to":"message_draft", "causation_id":"message_draft",
                        "status":"done"
                    }
                ]
            }))
            .expect("talk causality fixture JSON"),
        )
        .expect("talk causality fixture");

        let rows = session_context_get_blocking_at(
            &home,
            "agent-iris".to_string(),
            &canvas_owner(CanvasConversationOwnerKind::Agent, "iris"),
        )
        .expect("direct owner reads canonical handoffs");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["handoff_id"], "message_draft");
        assert_eq!(rows[0]["causation_id"], "turn_7");
        assert_eq!(rows[0]["status"], "queued");
        assert_eq!(rows[1]["handoff_id"], "message_return");
        assert_eq!(rows[1]["reply_to"], "message_draft");
        assert_eq!(rows[1]["status"], "done");
        std::fs::remove_dir_all(home).expect("cleanup handoff causality fixture");
    }

    #[test]
    fn routine_prompt_recovers_typed_occurrence_metadata() {
        let text = "[cron morning-check | scheduled daily 05:00 | occurrence 2026-08-26T11:00:00.000Z | turn routine:morning-check:1787742000000] Check school.";
        let (turn_id, origin) = routine_turn_metadata(text).expect("typed routine marker");
        assert_eq!(turn_id, "routine:morning-check:1787742000000");
        assert_eq!(origin["kind"], "routine");
        assert_eq!(origin["routine_id"], "morning-check");
        assert_eq!(origin["scheduled_for"], "2026-08-26T11:00:00.000Z");
        assert_eq!(origin["schedule"], "daily 05:00");
        assert!(
            routine_turn_metadata("[cron old | scheduled daily 05:00] Check school.").is_none()
        );
    }

    #[test]
    fn context_recovery_never_splits_a_long_authored_turn() {
        let mut rows = vec![serde_json::json!({"role":"user","text":"older"})];
        rows.extend((0..25).map(|index| serde_json::json!({"role":"tool","index":index})));
        rows.push(serde_json::json!({
            "role":"user", "text":"routine",
            "turn_id":"routine:morning-check:1787742000000",
            "origin":{"kind":"routine"}
        }));
        rows.extend((0..100).map(|index| serde_json::json!({"role":"tool","index":index})));

        let kept = keep_recent_complete_context_turns(rows, 80);
        assert_eq!(
            kept.len(),
            101,
            "the complete latest turn may exceed the soft row target"
        );
        assert_eq!(kept[0]["role"], "user");
        assert_eq!(kept[0]["turn_id"], "routine:morning-check:1787742000000");
    }

    #[cfg(unix)]
    #[test]
    fn feed_patch_merges_sets_and_deletes_without_erasing_bad_data() {
        let dir = isolated_dir("feed-patch");
        let path = dir.join("session.json");
        feeds_set_at(
            &path,
            serde_json::json!({
                "orchestrator": "old",
                "coder": "keep",
                "tester": "remove"
            }),
        )
        .expect("seed feed");

        feeds_patch_at(
            &path,
            serde_json::json!({
                "orchestrator": "new",
                "tester": null,
                "browser": "added"
            }),
        )
        .expect("patch feed");
        assert_eq!(
            feeds_get_at(&path).expect("read patched feed"),
            serde_json::json!({
                "orchestrator": "new",
                "coder": "keep",
                "browser": "added"
            })
        );

        let preserved = std::fs::read(&path).expect("read feed before invalid patch");
        assert!(feeds_patch_at(&path, serde_json::json!([])).is_err());
        assert!(feeds_patch_at(&path, serde_json::json!({"coder": 7})).is_err());
        assert_eq!(
            std::fs::read(&path).expect("read feed after invalid patch"),
            preserved
        );

        std::fs::write(&path, b"not json").expect("seed corrupt feed");
        assert!(feeds_patch_at(&path, serde_json::json!({"coder": "new"})).is_err());
        assert_eq!(std::fs::read(&path).expect("preserved feed"), b"not json");
        std::fs::remove_dir_all(dir).expect("cleanup isolated test directory");
    }

    #[cfg(unix)]
    #[test]
    fn image_and_text_ingress_rejects_limits_and_type_spoofing_before_decode() {
        use base64::Engine;
        use std::os::unix::fs::PermissionsExt;

        let dir = isolated_dir("bounded-file-ingress");
        let workspace = dir.join("workspace");
        let home = dir.join("home");
        std::fs::create_dir_all(&workspace).expect("workspace fixture");
        std::fs::create_dir_all(&home).expect("home fixture");
        std::fs::set_permissions(&workspace, std::fs::Permissions::from_mode(0o700))
            .expect("private workspace fixture");
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))
            .expect("private home fixture");

        let png = minimal_png_fixture();
        let image = workspace.join("tiny.png");
        std::fs::write(&image, &png).expect("PNG fixture");
        assert!(image_data_url_at(&image, &workspace, &home)
            .expect("bounded image data URL")
            .starts_with("data:image/png;base64,"));

        let spoofed_extension = workspace.join("spoofed.jpg");
        std::fs::write(&spoofed_extension, &png).expect("spoofed image fixture");
        assert!(image_data_url_at(&spoofed_extension, &workspace, &home).is_err());
        let fake_image = workspace.join("not-really.png");
        std::fs::write(&fake_image, b"this is not an image").expect("fake image fixture");
        assert!(image_data_url_at(&fake_image, &workspace, &home).is_err());

        let oversized_image = workspace.join("oversized.png");
        let oversized_image_file = std::fs::File::create(&oversized_image).expect("sparse image");
        oversized_image_file
            .set_len(INLINE_IMAGE_MAX_BYTES as u64 + 1)
            .expect("oversized sparse image");
        drop(oversized_image_file);
        assert!(image_data_url_at(&oversized_image, &workspace, &home).is_err());

        let text = workspace.join("context.txt");
        std::fs::write(&text, "bounded context").expect("text fixture");
        assert_eq!(
            read_file_at(&text, &workspace, &home).expect("bounded text read"),
            "bounded context"
        );
        let oversized_text = workspace.join("oversized.txt");
        let oversized_text_file = std::fs::File::create(&oversized_text).expect("sparse text");
        oversized_text_file
            .set_len(TEXT_FILE_MAX_BYTES as u64 + 1)
            .expect("oversized sparse text");
        drop(oversized_text_file);
        assert!(read_file_at(&oversized_text, &workspace, &home).is_err());
        let binary_text = workspace.join("binary.txt");
        std::fs::write(&binary_text, [0xff, 0xfe]).expect("binary text fixture");
        assert!(read_file_at(&binary_text, &workspace, &home).is_err());

        assert_eq!(
            bounded_base64_decode("QUJD", 3).expect("three-byte base64"),
            b"ABC".to_vec()
        );
        assert!(
            bounded_base64_decode("QUJDREU=", 4)
                .expect_err("decoded-size preflight")
                .contains("decoded image exceeds"),
            "a payload within the encoded-length ceiling must still be rejected from its exact decoded length before allocation"
        );
        assert!(bounded_base64_decode("QUJDRA==", 3).is_err());
        assert!(bounded_base64_decode("!!!!", 16).is_err());

        let encoded = base64::engine::general_purpose::STANDARD.encode(&png);
        let stored = save_attachment_at(&home, &format!("data:image/png;base64,{encoded}"))
            .expect("private attachment");
        let stored_again = save_attachment_at(&home, &format!("data:image/png;base64,{encoded}"))
            .expect("deduplicated private attachment");
        assert_eq!(stored_again, stored);
        assert!(stored
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("sha256-") && name.ends_with(".png")));
        assert_eq!(
            std::fs::read_dir(home.join("attachments"))
                .expect("list attachments")
                .count(),
            1,
            "identical pasted bytes must occupy one content-addressed blob"
        );
        assert_eq!(std::fs::read(&stored).expect("stored attachment"), png);
        assert_eq!(
            std::fs::metadata(home.join("attachments"))
                .expect("attachment directory")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&stored)
                .expect("attachment metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(save_attachment_at(&home, &format!("data:image/jpeg;base64,{encoded}")).is_err());
        assert!(
            save_attachment_at(&home, &format!("data:application/x-png;base64,{encoded}")).is_err()
        );
        assert!(save_attachment_at(&home, "data:image/png;base64,!!!!").is_err());
        let avatar_id = avatar_import_at(&home, &format!("data:image/png;base64,{encoded}"))
            .expect("private avatar");
        assert!(avatar_id.starts_with("avatar-"));
        assert!(avatar_id.ends_with(".png"));
        assert_eq!(
            std::fs::read(home.join("avatars").join(&avatar_id)).expect("stored avatar"),
            png
        );
        assert_eq!(
            std::fs::metadata(home.join("avatars"))
                .expect("avatar directory")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert!(avatar_import_at(&home, &format!("data:image/jpeg;base64,{encoded}")).is_err());
        let picked_avatar = workspace.join("picked-avatar.bin");
        std::fs::write(&picked_avatar, &png).expect("picked avatar fixture");
        assert_eq!(
            avatar_import_path_at(&home, &picked_avatar).expect("native-picked avatar"),
            avatar_id,
            "the native picker and data URL ingress must deduplicate by content"
        );
        let picked_fake = workspace.join("picked-fake.png");
        std::fs::write(&picked_fake, b"not an image").expect("fake picked avatar fixture");
        assert!(avatar_import_path_at(&home, &picked_fake).is_err());
        assert_eq!(
            std::fs::read_dir(home.join("attachments"))
                .expect("attachment directory entries")
                .flatten()
                .filter(|entry| entry.path().is_file())
                .count(),
            1,
            "rejected payloads must not leave partial attachment files"
        );
        std::fs::remove_dir_all(dir).expect("cleanup isolated test directory");
    }

    #[cfg(unix)]
    #[test]
    fn note_walk_read_and_delete_never_follow_external_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = isolated_dir("note-boundaries");
        let root = dir.join("memory");
        let tier = root.join("HOT").join("projects");
        std::fs::create_dir_all(&tier).expect("note tier");
        let note = tier.join("owned.md");
        std::fs::write(&note, b"owned note").expect("owned note");

        let external = dir.join("external.md");
        std::fs::write(&external, b"external secret").expect("external note");
        let linked_file = tier.join("linked.md");
        symlink(&external, &linked_file).expect("linked external note");
        let external_dir = dir.join("external-dir");
        std::fs::create_dir_all(&external_dir).expect("external directory");
        std::fs::write(external_dir.join("nested.md"), b"nested secret")
            .expect("nested external note");
        symlink(&external_dir, root.join("linked-tier")).expect("linked external directory");

        assert_eq!(
            list_note_files_at(&root).expect("list owned notes"),
            vec![note.clone()]
        );
        assert_eq!(
            read_note_file_at(&root, &note).expect("bounded owned note"),
            "owned note"
        );
        assert!(read_note_file_at(&root, &linked_file).is_err());
        assert!(remove_note_file_at(&root, &linked_file).is_err());
        assert_eq!(
            std::fs::read(&external).expect("external note preserved"),
            b"external secret"
        );

        let oversized = tier.join("oversized.md");
        let oversized_file = std::fs::File::create(&oversized).expect("oversized note");
        oversized_file
            .set_len(MEMORY_NOTE_MAX_BYTES as u64 + 1)
            .expect("sparse oversized note");
        drop(oversized_file);
        assert!(read_note_file_at(&root, &oversized).is_err());

        let linked_root = dir.join("linked-memory");
        symlink(&root, &linked_root).expect("linked memory root");
        assert!(list_note_files_at(&linked_root).is_err());

        remove_note_file_at(&root, &note).expect("remove owned note");
        assert!(!note.exists());
        std::fs::remove_dir_all(dir).expect("cleanup isolated test directory");
    }

    #[cfg(unix)]
    #[test]
    fn wallpaper_import_is_bounded_typed_private_and_never_replaces() {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::{symlink, FileTypeExt, PermissionsExt};

        let dir = isolated_dir("wallpaper-ingress");
        let home = dir.join("state");
        let source = dir.join("source.png");
        let png = minimal_png_fixture();
        std::fs::write(&source, &png).expect("wallpaper source");
        let destination = home.join("wallpapers").join("fixed.png");
        copy_wallpaper_to_at(&home, &source, &destination).expect("private wallpaper copy");
        assert_eq!(std::fs::read(&destination).expect("wallpaper bytes"), png);
        assert_eq!(
            std::fs::metadata(&destination)
                .expect("wallpaper metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        assert!(copy_wallpaper_to_at(&home, &source, &destination).is_err());
        assert_eq!(
            std::fs::read(&destination).expect("existing destination preserved"),
            png
        );
        let spoofed_destination = home.join("wallpapers").join("spoofed.jpg");
        assert!(copy_wallpaper_to_at(&home, &source, &spoofed_destination).is_err());
        assert!(!spoofed_destination.exists());

        let linked_source = dir.join("linked.png");
        symlink(&source, &linked_source).expect("linked wallpaper source");
        assert!(copy_wallpaper_to_at(
            &home,
            &linked_source,
            &home.join("wallpapers").join("linked.png"),
        )
        .is_err());

        let fifo = dir.join("source-fifo.png");
        let fifo_name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("fifo path");
        assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
        assert!(std::fs::symlink_metadata(&fifo)
            .expect("fifo metadata")
            .file_type()
            .is_fifo());
        assert!(
            copy_wallpaper_to_at(&home, &fifo, &home.join("wallpapers").join("fifo.png"),).is_err()
        );

        let oversized = dir.join("oversized.png");
        std::fs::write(&oversized, minimal_png_fixture()).expect("oversized wallpaper header");
        let oversized_file = std::fs::OpenOptions::new()
            .write(true)
            .open(&oversized)
            .expect("open oversized wallpaper");
        oversized_file
            .set_len(WALLPAPER_MAX_BYTES + 1)
            .expect("sparse oversized wallpaper");
        drop(oversized_file);
        assert!(copy_wallpaper_to_at(
            &home,
            &oversized,
            &home.join("wallpapers").join("oversized.png"),
        )
        .is_err());

        assert_eq!(
            std::fs::read_dir(home.join("wallpapers"))
                .expect("wallpaper directory")
                .flatten()
                .filter(|entry| entry.path().is_file())
                .count(),
            1,
            "rejected imports leave no partial destinations"
        );
        std::fs::remove_dir_all(dir).expect("cleanup isolated test directory");
    }

    #[test]
    fn broad_or_relative_custom_state_roots_are_rejected() {
        let default_root = std::path::PathBuf::from("/home/example/.phoenix");
        for unsafe_root in [
            std::path::PathBuf::from("/"),
            std::env::temp_dir(),
            std::path::PathBuf::from("/home/example"),
            std::env::current_dir().expect("current directory"),
            std::path::PathBuf::from("relative/state"),
            std::path::PathBuf::from("/tmp/state/../other"),
        ] {
            assert!(
                validate_state_root_for_mutation(&unsafe_root, &default_root, true).is_err(),
                "unsafe custom root should be rejected: {}",
                unsafe_root.display()
            );
        }

        let parent = isolated_dir("dedicated-state-shape");
        let dedicated = parent.join("state-not-yet-created");
        validate_state_root_for_mutation(&dedicated, &default_root, true)
            .expect("a dedicated absolute custom root is valid");
        validate_state_root_for_mutation(&default_root, &default_root, true)
            .expect("an explicit default root remains valid");
        std::fs::remove_dir_all(parent).expect("cleanup isolated test directory");
    }

    #[cfg(unix)]
    #[test]
    fn sensitive_replacement_is_private_atomic_and_conflict_safe() {
        use std::os::unix::fs::PermissionsExt;

        let dir = isolated_dir("private-write");
        let path = dir.join("config.toml");
        replace_private_atomic(&path, None, b"old secret", false, 1024, "test state")
            .expect("seed private file");
        replace_private_atomic(
            &path,
            Some(b"old secret"),
            b"new secret",
            true,
            1024,
            "test state",
        )
        .expect("replace private file");
        assert_eq!(std::fs::read(&path).expect("read final"), b"new secret");
        assert_eq!(
            std::fs::metadata(&path)
                .expect("final metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(replace_private_atomic(
            &path,
            Some(b"stale"),
            b"lost update",
            true,
            1024,
            "test state",
        )
        .is_err());
        assert_eq!(std::fs::read(&path).expect("read preserved"), b"new secret");
        let backups: Vec<_> = std::fs::read_dir(&dir)
            .expect("read dir")
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("config.toml.bak-")
            })
            .collect();
        assert_eq!(backups.len(), 1);
        assert!(backups.iter().all(|entry| {
            entry
                .metadata()
                .expect("backup metadata")
                .permissions()
                .mode()
                & 0o777
                == 0o600
        }));
        std::fs::remove_dir_all(dir).expect("cleanup isolated test directory");
    }

    #[cfg(unix)]
    #[test]
    fn private_cas_rejects_symlink_fifo_and_oversize_without_replacement() {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::{symlink, FileTypeExt};

        let dir = isolated_dir("private-cas-special-files");

        let target = dir.join("symlink-target");
        std::fs::write(&target, b"target bytes").expect("seed symlink target");
        let linked = dir.join("linked-state");
        symlink(&target, &linked).expect("state symlink");
        assert!(
            replace_private_atomic(&linked, None, b"replacement", false, 64, "test state",)
                .is_err()
        );
        assert_eq!(
            std::fs::read(&target).expect("target remains"),
            b"target bytes"
        );
        assert!(std::fs::symlink_metadata(&linked)
            .expect("symlink remains")
            .file_type()
            .is_symlink());

        let fifo = dir.join("state-fifo");
        let fifo_name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("fifo path");
        assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
        assert!(
            replace_private_atomic(&fifo, None, b"replacement", false, 64, "test state",).is_err()
        );
        assert!(std::fs::symlink_metadata(&fifo)
            .expect("fifo remains")
            .file_type()
            .is_fifo());

        let oversized = dir.join("oversized-state");
        let oversized_file = std::fs::File::create(&oversized).expect("create sparse state");
        oversized_file.set_len(65).expect("size sparse state");
        drop(oversized_file);
        assert!(
            replace_private_atomic(&oversized, None, b"replacement", false, 64, "test state",)
                .is_err()
        );
        assert_eq!(
            std::fs::metadata(&oversized)
                .expect("oversized state remains")
                .len(),
            65
        );

        std::fs::remove_dir_all(dir).expect("cleanup isolated test directory");
    }

    #[test]
    fn config_save_compares_the_document_that_was_read() {
        let dir = isolated_dir("config-cas");
        let path = dir.join("config.toml");
        let original = "[cli]\nyolo = false\n";
        let cli_edit = "[cli]\nyolo = true\n";
        let stale_canvas_edit = "[cli]\nyolo = false\nactions = false\n";
        replace_private_atomic(&path, None, original.as_bytes(), false, 1024, "test config")
            .expect("seed config");
        config_write_at(&path, cli_edit, Some(original)).expect("first compare-and-swap");
        assert!(
            config_write_at(&path, stale_canvas_edit, Some(original)).is_err(),
            "a Phoenix edit based on stale content must not erase a newer CLI edit"
        );
        assert!(
            config_write_at(&path, stale_canvas_edit, None).is_err(),
            "legacy writes without a loaded revision fail closed"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("preserved config"),
            cli_edit
        );
        std::fs::remove_dir_all(dir).expect("cleanup isolated test directory");
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_cron_updates_preserve_every_entry_privately() {
        use std::os::unix::fs::PermissionsExt;

        let dir = isolated_dir("cron-rmw");
        let path = dir.join("crons.json");
        std::thread::scope(|scope| {
            for index in 0..12 {
                let path = path.clone();
                scope.spawn(move || {
                    mutate_crons_at(&path, |entries| {
                        entries.push(serde_json::json!({
                            "id": format!("cron-{index}"),
                            "session_id": "session",
                            "prompt": "tick",
                            "schedule": { "kind": "once" },
                            "next_run": "2026-08-10T12:00:00Z",
                            "enabled": true,
                            "created_at": "2026-08-10T11:00:00Z"
                        }));
                        Ok(())
                    })
                    .expect("locked cron append");
                });
            }
        });
        let entries: Vec<serde_json::Value> =
            serde_json::from_slice(&std::fs::read(&path).expect("read cron store"))
                .expect("valid cron store");
        assert_eq!(entries.len(), 12);
        assert_eq!(
            std::fs::metadata(&path)
                .expect("cron metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        let baseline = std::fs::read(&path).expect("baseline cron bytes");
        assert!(cron_remove_at(&path, "does-not-exist").is_err());
        assert_eq!(
            std::fs::read(&path).expect("missing remove preserves cron store"),
            baseline
        );
        assert!(cron_add_at(
            &path,
            1_786_363_200,
            CronDraft {
                session_id: "session".into(),
                canvas: Some("canvas".into()),
                prompt: "oversized".into(),
                schedule_kind: "every".into(),
                seconds: Some(MAX_SCHEDULE_SECONDS + 1),
                hour: None,
                minute: None,
                days: None,
            },
        )
        .is_err());
        assert_eq!(
            std::fs::read(&path).expect("oversized input preserves cron store"),
            baseline
        );
        assert!(cron_add_at(
            &path,
            i64::MAX,
            CronDraft {
                session_id: "session".into(),
                canvas: None,
                prompt: "date overflow".into(),
                schedule_kind: "once".into(),
                seconds: Some(1),
                hour: None,
                minute: None,
                days: None,
            },
        )
        .is_err());
        assert_eq!(
            std::fs::read(&path).expect("date overflow preserves cron store"),
            baseline
        );
        assert!(mutate_crons_at(&path, |entries| {
            entries.push(serde_json::json!({
                "id": "invalid-replacement",
                "session_id": "session",
                "schedule": { "kind": "every", "seconds": 0 }
            }));
            Ok(())
        })
        .is_err());
        assert_eq!(
            std::fs::read(&path).expect("invalid replacement preserves cron store"),
            baseline
        );
        assert!(mutate_crons_at(&path, |entries| {
            entries.push(entries[0].clone());
            Ok(())
        })
        .is_err());
        assert_eq!(
            std::fs::read(&path).expect("duplicate id preserves cron store"),
            baseline
        );

        let invalid = serde_json::to_vec_pretty(&serde_json::json!([{
            "id": "invalid-existing",
            "session_id": "session",
            "prompt": "tick",
            "schedule": { "kind": "every", "seconds": 0 },
            "next_run": "2026-08-10T12:00:00Z",
            "enabled": true,
            "created_at": "2026-08-10T11:00:00Z"
        }]))
        .expect("invalid structured fixture");
        std::fs::write(&path, &invalid).expect("seed invalid structured store");
        assert!(mutate_crons_at(&path, |_| Ok(())).is_err());
        assert_eq!(
            std::fs::read(&path).expect("preserve invalid structured store"),
            invalid
        );

        std::fs::write(&path, b"{not json").expect("seed corrupt store");
        assert!(mutate_crons_at(&path, |_| Ok(())).is_err());
        assert_eq!(
            std::fs::read(&path).expect("preserve corrupt store"),
            b"{not json"
        );
        std::fs::remove_dir_all(dir).expect("cleanup isolated test directory");
    }

    #[test]
    fn cron_pause_and_resume_move_stale_recurring_deadline_forward() {
        let dir = isolated_dir("cron-pause-resume");
        let path = dir.join("crons.json");
        let now = 1_786_363_200;
        cron_add_at(
            &path,
            now - 7_200,
            CronDraft {
                session_id: "session".into(),
                canvas: None,
                prompt: "tick".into(),
                schedule_kind: "every".into(),
                seconds: Some(600),
                hour: None,
                minute: None,
                days: None,
            },
        )
        .expect("seed recurring cron");

        let paused = cron_set_enabled_at(&path, "cron-", false, now).expect("pause cron");
        assert_eq!(paused["enabled"], false);
        let resumed = cron_set_enabled_at(&path, "cron-", true, now).expect("resume cron");
        assert_eq!(resumed["enabled"], true);
        assert_eq!(resumed["next_run"], chrono_free_iso(now + 600));

        std::fs::remove_dir_all(dir).expect("cleanup isolated test directory");
    }

    #[test]
    fn restart_log_rotation_keeps_one_bounded_predecessor() {
        let dir = isolated_dir("log");
        let log = dir.join("gateway-restart.log");
        std::fs::write(&log, b"1234").expect("seed log");
        drop(open_bounded_restart_log_at_with_limit(&dir, 4).expect("rotate and reopen"));
        assert_eq!(std::fs::read(&log).expect("fresh current log"), b"");
        assert_eq!(
            std::fs::read(rotated_log_path(&log)).expect("rotated log"),
            b"1234"
        );
        assert!(!dir.join("gateway-restart.log.2").exists());
        std::fs::remove_dir_all(dir).expect("cleanup isolated test directory");
    }

    #[cfg(unix)]
    #[test]
    fn restart_log_rejects_current_and_rotated_symlinks_without_touching_targets() {
        use std::os::unix::fs::symlink;

        let dir = isolated_dir("log-symlinks");
        let log = dir.join("gateway-restart.log");
        let target = dir.join("outside-log-target");
        std::fs::write(&target, b"preserve me").expect("seed target");

        symlink(&target, &log).expect("symlink current log");
        assert!(open_bounded_restart_log_at_with_limit(&dir, 4).is_err());
        assert_eq!(
            std::fs::read(&target).expect("current target"),
            b"preserve me"
        );

        std::fs::remove_file(&log).expect("remove current symlink");
        std::fs::write(&log, b"1234").expect("seed oversized current");
        let rotated = rotated_log_path(&log);
        symlink(&target, &rotated).expect("symlink rotated log");
        assert!(open_bounded_restart_log_at_with_limit(&dir, 4).is_err());
        assert_eq!(
            std::fs::read(&target).expect("rotated target"),
            b"preserve me"
        );
        assert_eq!(std::fs::read(&log).expect("current preserved"), b"1234");

        std::fs::remove_dir_all(dir).expect("cleanup isolated test directory");
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_restart_log_writers_share_rotation_and_private_modes() {
        use std::os::unix::fs::PermissionsExt;

        let dir = isolated_dir("concurrent-log");
        let log = dir.join("gateway-restart.log");
        std::fs::write(&log, b"12345678").expect("seed full generation");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(16));
        std::thread::scope(|scope| {
            for _ in 0..16 {
                let barrier = std::sync::Arc::clone(&barrier);
                let dir = &dir;
                scope.spawn(move || {
                    barrier.wait();
                    append_bounded_restart_log_at_with_limit(dir, 8, b"x").expect("locked append");
                });
            }
        });

        let rotated = rotated_log_path(&log);
        assert_eq!(
            std::fs::read(&log).expect("current generation"),
            b"xxxxxxxx"
        );
        assert_eq!(
            std::fs::read(&rotated).expect("previous generation"),
            b"xxxxxxxx"
        );
        assert!(!dir.join("gateway-restart.log.2").exists());
        let lock = dir.join(".gateway-restart.log.lock");
        for path in [&log, &rotated, &lock] {
            assert_eq!(
                std::fs::metadata(path)
                    .expect("private restart-log path")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600,
                "{}",
                path.display()
            );
        }
        std::fs::remove_dir_all(dir).expect("cleanup isolated test directory");
    }

    #[cfg(unix)]
    #[test]
    fn fresh_restart_log_creates_a_private_phoenix_home() {
        use std::os::unix::fs::PermissionsExt;

        let parent = isolated_dir("fresh-log-home");
        let home = parent.join("not-created-yet");
        let file = open_bounded_restart_log_at(&home).expect("create fresh restart log");
        drop(file);
        assert_eq!(
            std::fs::metadata(&home)
                .expect("home metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(home.join("gateway-restart.log"))
                .expect("log metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        std::fs::remove_dir_all(parent).expect("cleanup isolated test directory");
    }

    #[cfg(unix)]
    #[test]
    fn existing_custom_state_root_is_never_repermissioned() {
        use std::os::unix::fs::PermissionsExt;

        let parent = isolated_dir("custom-home-mode");
        let custom = parent.join("shared-existing-root");
        std::fs::create_dir_all(&custom).expect("create custom root");
        std::fs::set_permissions(&custom, std::fs::Permissions::from_mode(0o755))
            .expect("make custom root intentionally shared");
        let default_root = parent.join("default-home");

        assert!(secure_private_directory_at(&custom, &custom, &default_root, true).is_err());
        assert_eq!(
            std::fs::metadata(&custom)
                .expect("custom root metadata")
                .permissions()
                .mode()
                & 0o777,
            0o755,
            "Phoenix must reject, not chmod, an existing custom root"
        );

        std::fs::set_permissions(&custom, std::fs::Permissions::from_mode(0o700))
            .expect("make custom root private");
        secure_private_directory_at(&custom, &custom, &default_root, true)
            .expect("private custom root is accepted");
        std::fs::remove_dir_all(parent).expect("cleanup isolated test directory");
    }

    #[cfg(unix)]
    #[test]
    fn shared_custom_root_is_rejected_even_when_requested_child_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let parent = isolated_dir("custom-root-child");
        let custom = parent.join("shared-existing-root");
        let child = custom.join("private-child");
        std::fs::create_dir_all(&child).expect("create custom child");
        std::fs::set_permissions(&custom, std::fs::Permissions::from_mode(0o755))
            .expect("make custom root shared");
        std::fs::set_permissions(&child, std::fs::Permissions::from_mode(0o700))
            .expect("make requested child private");

        assert!(
            secure_private_directory_at(&child, &custom, &parent.join("default-home"), true)
                .is_err(),
            "Phoenix must validate the custom root inode, not only the child"
        );
        assert_eq!(
            std::fs::metadata(&custom)
                .expect("custom root metadata")
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        std::fs::remove_dir_all(parent).expect("cleanup isolated test directory");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_custom_state_root_is_rejected() {
        let parent = isolated_dir("custom-home-symlink");
        let destination = parent.join("destination");
        std::fs::create_dir_all(&destination).expect("create destination");
        let custom = parent.join("custom-root");
        std::os::unix::fs::symlink(&destination, &custom).expect("create state symlink");

        assert!(
            secure_private_directory_at(&custom, &custom, &parent.join("default-home"), true)
                .is_err()
        );
        std::fs::remove_dir_all(parent).expect("cleanup isolated test directory");
    }

    #[test]
    fn remote_runner_requires_pinned_host_and_clean_workspace() {
        let mut runner = DesktopRemoteRunner {
            id: "BUILD-1".into(),
            label: " Build machine ".into(),
            host: "BUILD.EXAMPLE.COM".into(),
            port: 22,
            user: "phoenix".into(),
            workspace_root: "/srv/phoenix/project".into(),
            host_key:
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIG4p5lO8wI6FhL9G5D8Rz4uHn8yY9eB1cQ2xV3zW4a5b"
                    .into(),
            identity_file: None,
            enabled: true,
        };
        validate_desktop_remote_runner(&mut runner).expect("valid pinned runner");
        assert_eq!(runner.id, "build-1");
        assert_eq!(runner.host, "build.example.com");
        assert_eq!(runner.label, "Build machine");

        let mut unpinned = runner.clone();
        unpinned.host_key = "ssh-ed25519 trust-on-first-use".into();
        assert!(validate_desktop_remote_runner(&mut unpinned).is_err());

        let mut escaping = runner.clone();
        escaping.workspace_root = "/srv/phoenix/../elsewhere".into();
        assert!(validate_desktop_remote_runner(&mut escaping).is_err());

        let mut shell_host = runner;
        shell_host.host = "build.example.com;touch-pwned".into();
        assert!(validate_desktop_remote_runner(&mut shell_host).is_err());
    }
}

#[cfg(test)]
mod renderer_selection_tests {
    use super::{
        automatic_chromium_gpu_mode, dri_prime_selector_from_pci_address,
        drm_prime_selector_for_vendor, gdk_backend_for_renderer, render_gpu_preference,
        resolve_webview_renderer, visible_chromium_ozone_platform, webview_renderer_env_plan,
        RenderGpuPreference, ResolvedWebviewRenderer, INTEL_VENDOR_ID, NVIDIA_VENDOR_ID,
    };

    #[cfg(unix)]
    fn drm_fixture(intel: bool, nvidia: bool) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "phoenix-renderer-selection-{}-{nonce}",
            std::process::id()
        ));
        let drm_root = root.join("class/drm");
        std::fs::create_dir_all(&drm_root).expect("drm root");
        if intel {
            let intel_device = root.join("devices/pci0000:00/0000:00:02.0");
            std::fs::create_dir_all(drm_root.join("renderD128")).expect("Intel render node");
            std::fs::create_dir_all(&intel_device).expect("Intel PCI device");
            std::fs::write(intel_device.join("vendor"), "0x8086\n").expect("Intel vendor");
            std::os::unix::fs::symlink(&intel_device, drm_root.join("renderD128/device"))
                .expect("Intel device link");
        }
        if nvidia {
            let nvidia_device = root.join("devices/pci0000:00/0000:01:00.0");
            let node = if intel { "renderD129" } else { "renderD128" };
            std::fs::create_dir_all(drm_root.join(node)).expect("NVIDIA render node");
            std::fs::create_dir_all(&nvidia_device).expect("NVIDIA PCI device");
            std::fs::write(nvidia_device.join("vendor"), "0x10de\n").expect("NVIDIA vendor");
            std::os::unix::fs::symlink(&nvidia_device, drm_root.join(node).join("device"))
                .expect("NVIDIA device link");
        }
        drm_root
    }

    #[test]
    fn renderer_preference_preserves_integrated_default_and_system_escape_hatch() {
        assert_eq!(render_gpu_preference(None), RenderGpuPreference::Integrated);
        assert_eq!(
            render_gpu_preference(Some(" dedicated ")),
            RenderGpuPreference::Dedicated
        );
        assert_eq!(
            render_gpu_preference(Some("SYSTEM")),
            RenderGpuPreference::System
        );
        assert_eq!(
            render_gpu_preference(Some("unexpected")),
            RenderGpuPreference::Integrated
        );
    }

    #[test]
    fn visible_chromium_follows_wayland_even_when_hidden_webkit_uses_x11() {
        assert_eq!(
            visible_chromium_ozone_platform(None, true, true),
            Some("wayland")
        );
        assert_eq!(
            visible_chromium_ozone_platform(None, false, true),
            Some("x11")
        );
        assert_eq!(visible_chromium_ozone_platform(None, false, false), None);
        assert_eq!(
            visible_chromium_ozone_platform(Some("x11"), true, true),
            Some("x11")
        );
        assert_eq!(
            visible_chromium_ozone_platform(Some("WAYLAND"), false, true),
            Some("wayland")
        );
    }

    #[test]
    fn automatic_vulkan_is_never_forced_on_wayland() {
        assert_eq!(automatic_chromium_gpu_mode(true, Some("wayland")), None);
        assert_eq!(
            automatic_chromium_gpu_mode(true, Some("x11")),
            Some("vulkan")
        );
        assert_eq!(automatic_chromium_gpu_mode(false, Some("x11")), None);
        assert_eq!(automatic_chromium_gpu_mode(false, Some("wayland")), None);
    }

    #[test]
    fn pci_addresses_become_exact_dri_prime_selectors() {
        assert_eq!(
            dri_prime_selector_from_pci_address("0000:00:02.0").as_deref(),
            Some("pci-0000_00_02_0")
        );
        assert_eq!(
            dri_prime_selector_from_pci_address("000A:0B:1C.7").as_deref(),
            Some("pci-000a_0b_1c_7")
        );
        assert_eq!(dri_prime_selector_from_pci_address("00:02.0"), None);
        assert_eq!(dri_prime_selector_from_pci_address("renderD128"), None);
    }

    #[test]
    fn integrated_renderer_prefers_x11_for_native_browser_surfaces() {
        assert_eq!(
            gdk_backend_for_renderer(RenderGpuPreference::Integrated, true, true),
            Some("x11")
        );
        assert_eq!(
            gdk_backend_for_renderer(RenderGpuPreference::Integrated, true, false),
            Some("wayland")
        );
        assert_eq!(
            gdk_backend_for_renderer(RenderGpuPreference::Integrated, false, true),
            Some("x11")
        );
        assert_eq!(
            gdk_backend_for_renderer(RenderGpuPreference::Integrated, false, false),
            None
        );
    }

    #[test]
    fn dedicated_renderer_uses_x11_only_when_available() {
        assert_eq!(
            gdk_backend_for_renderer(RenderGpuPreference::Dedicated, true, true),
            Some("x11")
        );
        assert_eq!(
            gdk_backend_for_renderer(RenderGpuPreference::Dedicated, false, true),
            Some("x11")
        );
        assert_eq!(
            gdk_backend_for_renderer(RenderGpuPreference::Dedicated, true, false),
            None
        );
        assert_eq!(
            gdk_backend_for_renderer(RenderGpuPreference::Dedicated, false, false),
            None
        );
    }

    #[test]
    fn system_renderer_uses_x11_when_the_native_surface_can_run() {
        assert_eq!(
            gdk_backend_for_renderer(RenderGpuPreference::System, true, true),
            Some("x11")
        );
        assert_eq!(
            gdk_backend_for_renderer(RenderGpuPreference::System, false, true),
            Some("x11")
        );
        assert_eq!(
            gdk_backend_for_renderer(RenderGpuPreference::System, true, false),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn renderer_discovery_uses_render_node_vendor_and_canonical_pci_path() {
        let drm_root = drm_fixture(true, true);
        assert_eq!(
            drm_prime_selector_for_vendor(&drm_root, "0x8086").as_deref(),
            Some("pci-0000_00_02_0")
        );
        assert_eq!(
            drm_prime_selector_for_vendor(&drm_root, "0x10DE").as_deref(),
            Some("pci-0000_01_00_0")
        );
        assert_eq!(drm_prime_selector_for_vendor(&drm_root, "0x1002"), None);
        std::fs::remove_dir_all(drm_root.ancestors().nth(2).unwrap()).expect("clean fixture");
    }

    #[cfg(unix)]
    #[test]
    fn nvidia_only_host_selects_nvidia_for_default_and_dedicated() {
        let drm_root = drm_fixture(false, true);
        let expected = ResolvedWebviewRenderer {
            vendor_id: NVIDIA_VENDOR_ID,
            dri_prime: "pci-0000_01_00_0".into(),
            backend_preference: RenderGpuPreference::Dedicated,
            nvidia_ui: true,
        };
        assert_eq!(
            resolve_webview_renderer(RenderGpuPreference::Integrated, &drm_root),
            Some(expected.clone())
        );
        assert_eq!(
            resolve_webview_renderer(RenderGpuPreference::Dedicated, &drm_root),
            Some(expected)
        );
        assert_eq!(
            resolve_webview_renderer(RenderGpuPreference::System, &drm_root),
            None
        );
        std::fs::remove_dir_all(drm_root.ancestors().nth(2).unwrap()).expect("clean fixture");
    }

    #[cfg(unix)]
    #[test]
    fn nvidia_selection_applies_nvidia_ui_env_and_never_forces_dmabuf() {
        let drm_root = drm_fixture(false, true);
        let resolved = resolve_webview_renderer(RenderGpuPreference::Integrated, &drm_root)
            .expect("NVIDIA-only host must resolve");
        let plan = webview_renderer_env_plan(&resolved);
        assert_eq!(plan.dri_prime, "pci-0000_01_00_0");
        assert_eq!(plan.backend_preference, RenderGpuPreference::Dedicated);
        assert!(plan.nvidia_offload);
        assert_eq!(plan.glx_vendor, Some("nvidia"));
        assert!(!plan.force_dmabuf);
        let dedicated = resolve_webview_renderer(RenderGpuPreference::Dedicated, &drm_root)
            .expect("dedicated NVIDIA");
        assert_eq!(webview_renderer_env_plan(&dedicated), plan);
        std::fs::remove_dir_all(drm_root.ancestors().nth(2).unwrap()).expect("clean fixture");
    }

    #[cfg(unix)]
    #[test]
    fn hybrid_host_keeps_intel_default_and_honors_dedicated_nvidia() {
        let drm_root = drm_fixture(true, true);
        assert_eq!(
            resolve_webview_renderer(RenderGpuPreference::Integrated, &drm_root),
            Some(ResolvedWebviewRenderer {
                vendor_id: INTEL_VENDOR_ID,
                dri_prime: "pci-0000_00_02_0".into(),
                backend_preference: RenderGpuPreference::Integrated,
                nvidia_ui: false,
            })
        );
        assert_eq!(
            resolve_webview_renderer(RenderGpuPreference::Dedicated, &drm_root),
            Some(ResolvedWebviewRenderer {
                vendor_id: NVIDIA_VENDOR_ID,
                dri_prime: "pci-0000_01_00_0".into(),
                backend_preference: RenderGpuPreference::Dedicated,
                nvidia_ui: true,
            })
        );
        std::fs::remove_dir_all(drm_root.ancestors().nth(2).unwrap()).expect("clean fixture");
    }

    #[cfg(unix)]
    #[test]
    fn live_sysfs_resolution_matches_discovered_vendors() {
        let drm = std::path::Path::new("/sys/class/drm");
        let intel = drm_prime_selector_for_vendor(drm, INTEL_VENDOR_ID);
        let nvidia = drm_prime_selector_for_vendor(drm, NVIDIA_VENDOR_ID);
        let resolved = resolve_webview_renderer(RenderGpuPreference::Integrated, drm);
        let dedicated = resolve_webview_renderer(RenderGpuPreference::Dedicated, drm);
        match (intel.as_deref(), nvidia.as_deref()) {
            (Some(selector), _) => {
                let resolved = resolved.expect("Intel node must select Intel");
                assert_eq!(resolved.vendor_id, INTEL_VENDOR_ID);
                assert_eq!(resolved.dri_prime, selector);
                assert!(!resolved.nvidia_ui);
            }
            (None, Some(selector)) => {
                let resolved = resolved.expect("NVIDIA-only host must select NVIDIA");
                assert_eq!(resolved.vendor_id, NVIDIA_VENDOR_ID);
                assert_eq!(resolved.dri_prime, selector);
                assert!(resolved.nvidia_ui);
                assert_eq!(resolved.backend_preference, RenderGpuPreference::Dedicated);
                let dedicated = dedicated.expect("dedicated must also select NVIDIA");
                assert_eq!(dedicated.vendor_id, NVIDIA_VENDOR_ID);
                assert_eq!(dedicated.dri_prime, selector);
                assert!(dedicated.nvidia_ui);
            }
            (None, None) => {
                assert!(resolved.is_none());
                assert!(dedicated.is_none());
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn missing_gpus_leave_the_process_unconfigured() {
        let drm_root = drm_fixture(false, false);
        assert_eq!(
            resolve_webview_renderer(RenderGpuPreference::Integrated, &drm_root),
            None
        );
        assert_eq!(
            resolve_webview_renderer(RenderGpuPreference::Dedicated, &drm_root),
            None
        );
        std::fs::remove_dir_all(drm_root.ancestors().nth(2).unwrap()).expect("clean fixture");
    }
}

const CHROMIUM_DEBUG_PORT: u16 = 17_442;
const CHROMIUM_BRIDGE_PORT: u16 = 17_443;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChromiumPorts { debug: u16, bridge: u16 }

fn chromium_ports(debug: Option<&str>, bridge: Option<&str>) -> Result<ChromiumPorts, String> {
    fn port(value: Option<&str>, default: u16, name: &str) -> Result<u16, String> {
        let Some(value) = value else { return Ok(default); };
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(format!("{name} must be a decimal port from 1024 to 65535"));
        }
        value.parse::<u16>().ok().filter(|port| *port >= 1024)
            .ok_or_else(|| format!("{name} must be a decimal port from 1024 to 65535"))
    }
    let ports = ChromiumPorts {
        debug: port(debug, CHROMIUM_DEBUG_PORT, "PHOENIX_CHROMIUM_DEBUG_PORT")?,
        bridge: port(bridge, CHROMIUM_BRIDGE_PORT, "PHOENIX_CHROMIUM_BRIDGE_PORT")?,
    };
    if ports.debug == ports.bridge { return Err("Chromium debug and bridge ports must differ".into()); }
    Ok(ports)
}

#[cfg(test)]
mod chromium_port_tests {
    use super::*;
    #[test]
    fn desktop_port_configuration_preserves_defaults_and_fences_invalid_overrides() {
        assert_eq!(chromium_ports(None,None).unwrap(),ChromiumPorts{debug:17442,bridge:17443});
        assert_eq!(chromium_ports(Some("28442"),Some("28443")).unwrap(),ChromiumPorts{debug:28442,bridge:28443});
        assert_eq!(chromium_ports(Some("65535"),None).unwrap().bridge,17443);
        for value in ["", "0", "1023", "65536", "-1", "+17442", "17442 ", "localhost:17442"] {
            assert!(chromium_ports(Some(value),None).is_err(),"{value}");
            assert!(chromium_ports(None,Some(value)).is_err(),"{value}");
        }
        assert!(chromium_ports(Some("17443"),None).is_err());
        assert!(chromium_ports(None,Some("17442")).is_err());
    }
}

fn chromium_bridge_token() -> Result<String, String> {
    use std::io::Read;

    let path = phoenix_home().join("chromium-bridge.token");
    if let Some(token) = read_private_text(&path, 256, "Chromium bridge token")? {
        let token = token.trim();
        if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!(
                "Chromium bridge token {} is invalid",
                path.display()
            ));
        }
        return Ok(token.to_ascii_lowercase());
    }
    let mut bytes = [0_u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut bytes))
        .map_err(|error| format!("could not generate Chromium bridge credentials: {error}"))?;
    let token = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    replace_private_atomic(
        &path,
        None,
        token.as_bytes(),
        false,
        256,
        "Chromium bridge token",
    )?;
    Ok(token)
}

fn launch_chromium_shell(app: &tauri::AppHandle, token: &str, ports: ChromiumPorts) -> Result<(), String> {
    let shell_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("chromium-shell");
    let electron = shell_root
        .join("node_modules")
        .join("electron")
        .join("dist")
        .join("electron");
    if !electron.is_file() {
        return Err(format!(
            "Chromium runtime is missing at {}; run npm install in {}",
            electron.display(),
            shell_root.display()
        ));
    }

    let log_dir = phoenix_home().join("logs");
    std::fs::create_dir_all(&log_dir)
        .map_err(|error| format!("could not create Chromium log directory: {error}"))?;
    let log_path = log_dir.join("chromium-shell.log");
    let output = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|error| format!("could not open {}: {error}", log_path.display()))?;
    let errors = output
        .try_clone()
        .map_err(|error| format!("could not clone Chromium log handle: {error}"))?;

    let bridge_url = format!("http://127.0.0.1:{}",ports.bridge);
    // The detached gateway is spawned after this function and inherits only
    // the authenticated local bridge coordinates, never the visible WebKit
    // renderer's X11/DMA-BUF workarounds.
    std::env::set_var("PHOENIX_CHROMIUM_BRIDGE_URL", &bridge_url);
    std::env::set_var("PHOENIX_CHROMIUM_BRIDGE_TOKEN", token);
    std::env::set_var(
        "PHOENIX_CHROMIUM_DEBUG_PORT",
        ports.debug.to_string(),
    );

    let mut command = std::process::Command::new(&electron);
    // `configure_webview_renderer` may have selected X11 for the hidden GTK
    // relay. The visible shell must follow the user's actual desktop session.
    let chromium_ozone = visible_chromium_ozone_platform_from_env();
    command
        .arg(&shell_root)
        .env(
            "PHOENIX_CHROMIUM_BRIDGE_PORT",
            ports.bridge.to_string(),
        )
        .env("PHOENIX_CHROMIUM_BRIDGE_TOKEN", token)
        .env(
            "PHOENIX_CHROMIUM_DEBUG_PORT",
            ports.debug.to_string(),
        )
        .env("PHOENIX_RUST_PARENT_PID", std::process::id().to_string())
        // `configure_webview_renderer` exists primarily for the hidden
        // WebKitGTK relay. Do not copy its DRI/PRIME selection wholesale into
        // Chromium; the child receives only its own native NVIDIA GBM hint.
        .env_remove("DRI_PRIME")
        .env_remove("__NV_PRIME_RENDER_OFFLOAD")
        .env_remove("__NV_PRIME_RENDER_OFFLOAD_PROVIDER")
        .env_remove("__GLX_VENDOR_LIBRARY_NAME")
        .env_remove("__VK_LAYER_NV_optimus")
        .env_remove("GBM_BACKEND")
        .env_remove("GDK_BACKEND")
        .env_remove("WEBKIT_FORCE_DMABUF_RENDERER")
        .env_remove("WEBKIT_DISABLE_DMABUF_RENDERER")
        .stdin(std::process::Stdio::null())
        .stdout(output)
        .stderr(errors);
    if let Some(backend) = chromium_ozone {
        command.env("PHOENIX_CHROMIUM_OZONE_PLATFORM", backend);
    }
    if std::env::var_os("PHOENIX_CHROMIUM_NVIDIA").is_some() {
        command
            .env("GBM_BACKEND", "nvidia-drm")
            .env("__GLX_VENDOR_LIBRARY_NAME", "nvidia");
    }
    let handle = app.clone();
    std::thread::Builder::new()
        .name("phoenix-chromium-shell".to_string())
        .spawn(move || {
            // `.setup` runs while GTK is still constructing its hidden relay
            // window. Starting another X11 client inside that callback can
            // leave Chromium waiting forever on GTK's provisional drawable.
            // Let setup return to the GTK event loop before creating Electron.
            std::thread::sleep(std::time::Duration::from_millis(500));
            let mut child = match command.spawn() {
                Ok(child) => child,
                Err(error) => {
                    eprintln!("phoenix: could not launch Chromium shell: {error}");
                    CHROMIUM_EXIT_CODE.store(3, std::sync::atomic::Ordering::Relaxed);
                    handle.exit(3);
                    return;
                }
            };
            let exit_code = match child.wait() {
                Ok(status) if status.success() => 0,
                Ok(status) => {
                    eprintln!("phoenix: Chromium shell exited unexpectedly: {status}; see {}", log_path.display());
                    status.code().filter(|code| *code != 0).unwrap_or(3)
                }
                Err(error) => {
                    eprintln!("phoenix: could not wait for Chromium shell: {error}");
                    3
                }
            };
            CHROMIUM_EXIT_CODE.store(exit_code, std::sync::atomic::Ordering::Relaxed);
            handle.exit(exit_code);
        })
        .map_err(|error| format!("could not schedule Chromium shell: {error}"))?;
    Ok(())
}

fn main() {
    // RENDERER: configure only the hidden WebKit native-services relay here.
    // It may use X11 on an NVIDIA Wayland desktop to avoid WebKitGTK's DMA-BUF
    // crash; the visible Electron shell independently follows WAYLAND_DISPLAY.
    // PHOENIX_RENDER_GPU=dedicated always means NVIDIA; =system leaves GPU
    // selection untouched. Gateway/model processes are unaffected.
    configure_webview_renderer();

    let exit_code = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(native_browser::BrowserSurfaceRegistry::default())
        .manage(term::TermRegistry::default())
        .setup(|app| {
            // Debug hatch: PHOENIX_SHOT=<mode> opens a supported shell preview
            // fixture in the packaged renderer. It is inert in normal launches.
            // A boot stamp busts WebKit's custom-protocol cache so a freshly
            // embedded UI is not hidden behind yesterday's index.html.
            use tauri::Manager;
            let shot = std::env::var("PHOENIX_SHOT")
                .or_else(|_| std::env::var("PHOENIX_CANVAS_SHOT"))
                .ok()
                .filter(|value| !value.trim().is_empty());
            let replay_onboarding =
                std::env::var("PHOENIX_ONBOARDING_REPLAY")
                    .ok()
                    .is_some_and(|value| {
                        matches!(
                            value.trim().to_ascii_lowercase().as_str(),
                            "1" | "true" | "yes" | "on"
                        )
                    });
            if let Some(win) = app.get_webview_window("main") {
                let chromium_enabled = shot.is_none()
                    && !replay_onboarding
                    && !std::env::var("PHOENIX_WEBKIT_SHELL")
                        .ok()
                        .is_some_and(|value| matches!(value.trim(), "1" | "true" | "yes"));
                let chromium = if chromium_enabled {
                    chromium_ports(std::env::var("PHOENIX_CHROMIUM_DEBUG_PORT").ok().as_deref(),
                        std::env::var("PHOENIX_CHROMIUM_BRIDGE_PORT").ok().as_deref())
                        .and_then(|ports| chromium_bridge_token().and_then(|token|
                            launch_chromium_shell(app.handle(), &token, ports).map(|()| (token,ports))))
                } else {
                    Err("Chromium shell disabled for renderer fixture".to_string())
                };

                let _ = win.with_webview(|webview| {
                    #[cfg(any(
                        target_os = "linux",
                        target_os = "dragonfly",
                        target_os = "freebsd",
                        target_os = "netbsd",
                        target_os = "openbsd"
                    ))]
                    {
                        use webkit2gtk::{SettingsExt, WebContextExt, WebViewExt};
                        // Tauri's packaged assets keep stable custom-protocol URLs. WebKitGTK
                        // may otherwise reuse an older settings.js/settings.css response after
                        // an application update, leaving the visible settings shell paired with
                        // stale (or missing) interaction handlers. Clear only the WebKit HTTP/
                        // resource cache at desktop startup; cookies and Phoenix browser profiles
                        // live in their own stores and are not touched.
                        if let Some(context) = webview.inner().context() {
                            context.clear_cache();
                        }
                        if let Some(settings) = webview.inner().settings() {
                            settings.set_enable_smooth_scrolling(true);
                        }
                    }
                });
                let boot = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_millis())
                    .unwrap_or(0);
                let target = match chromium.as_ref() {
                    Ok((token,ports)) => format!(
                        "tauri://localhost/relay.html?port={}&token={token}&boot={boot}",ports.bridge
                    ),
                    Err(_) => match shot {
                        Some(shot) => format!(
                        "tauri://localhost/index.html?shot={}&boot={boot}",
                        shot.trim()
                    ),
                        None if replay_onboarding => {
                            format!("tauri://localhost/index.html?onboarding=replay&boot={boot}")
                        }
                        None => format!("tauri://localhost/index.html?boot={boot}"),
                    },
                };
                match target.parse() {
                    Ok(url) => {
                        if let Err(e) = win.navigate(url) {
                            eprintln!("phoenix: boot navigation failed: {e}");
                        }
                    }
                    Err(e) => eprintln!("phoenix: bad boot url {target:?}: {e}"),
                }
                if let Err(error) = chromium {
                    eprintln!("phoenix: Chromium shell unavailable; using WebKit fallback: {error}");
                    let _ = win.show();
                }
            }

            // Keep a gateway alive for the whole session. This starts only
            // after Chromium bridge coordinates enter the environment, so a
            // freshly spawned gateway controls the exact embedded targets.
            start_gateway_supervisor();
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "main"
                && matches!(
                    event,
                    tauri::WindowEvent::CloseRequested { .. } | tauri::WindowEvent::Destroyed
                )
            {
                channels::shutdown();
                native_browser::browser_surface_shutdown(window.app_handle());
            }
        })
        .invoke_handler(tauri::generate_handler![
            channels::channels_command,
            gateway_status,
            gateway_start,
            gateway_ensure,
            gateway_stop,
            gateway_token,
            gateway_log_tail,
            action_audit_recent,
            usage_dashboard,
            remote_runners_list,
            remote_runner_save,
            remote_runner_remove,
            list_sessions,
            config_summary,
            onboarding_reach::onboarding_status,
            onboarding_reach::agent_reach_snapshot,
            config_read,
            config_write,
            artifacts_list,
            memory_maintenance,
            provider_logos,
            llm_chains,
            assignment_set,
            agent_model_set,
            lane_pin_set,
            composio_key_set,
            composio_status,
            powers::skills_list,
            powers::skill_remove,
            powers::skill_search,
            powers::skill_install,
            powers::mcp_list,
            powers::mcp_upsert,
            powers::mcp_toggle,
            powers::mcp_remove,
            powers::composio_apps,
            powers::composio_key_clear,
            llm_chain_set,
            fallback_set,
            providers_catalog,
            auth_set_key,
            provider_initialize_config,
            prompt_overlays_list,
            prompt_overlay_read,
            prompt_overlay_write,
            prompt_overlay_reset,
            data_snapshot_create,
            data_snapshots_list,
            auth_remove,
            auth_probe,
            auth_rename,
            auth_provider_order,
            oauth_login,
            lanes_get,
            lane_set,
            voice_capture_prepare_owned,
            voice_capture_start_owned,
            voice_capture_stop_owned,
            voice_capture_cancel,
            voice_speak,
            web_lane_set,
            effort_set,
            crons_list,
            cron_add,
            cron_set_enabled,
            cron_remove,
            vitals_list,
            vitals_write,
            vitals_delete,
            memory_status,
            memory_search,
            memory_add,
            memory_delete,
            memory_graph,
            wallpaper_import,
            wallpapers_list,
            wallpaper_previews,
            prefs_get,
            prefs_set,
            workspace_default,
            workspace_pick,
            workspace_review,
            workspace_environment,
            workspace_git_commit,
            workspace_git_push,
            workspace_git_branches,
            workspace_git_switch_branch,
            workspace_git_create_pull_request,
            attachment_pick_images,
            attachment_pick_file,
            attachment_pick_folder,
            term::term_open,
            term::term_write,
            term::term_resize,
            term::term_close,
            feeds_get,
            session_context_get,
            feeds_set,
            feeds_patch,
            feeds_delete,
            image_data_url,
            open_external,
            browser_downloads_list,
            browser_download_open,
            open_workspace_file,
            context_usage_get,
            native_browser::browser_surface_attach,
            native_browser::browser_surface_set_bounds,
            native_browser::browser_surface_show,
            native_browser::browser_surface_hide,
            native_browser::browser_surface_detach,
            read_file,
            save_attachment,
            avatar_import,
            avatar_pick,
            avatar_data_url,
            perf_report
        ])
        .build(tauri::generate_context!())
        .expect("error while building Phoenix desktop")
        .run_return(|_, _| {});
    let shell_code = CHROMIUM_EXIT_CODE.load(std::sync::atomic::Ordering::Relaxed);
    std::process::exit(if shell_code != 0 { shell_code } else { exit_code });
}

#[cfg(test)]
mod provider_pool_priority_tests {
    use super::provider_account_order;
    #[test]
    fn saves_exact_provider_order_without_changing_credentials() {
        let mut doc=serde_json::json!({"profiles":{"codex:a":{"provider":"codex","access":"private-a"},"codex:b":{"provider":"codex","access":"private-b"},"other:c":{"provider":"other"}}});
        let before=doc["profiles"].clone();
        assert!(provider_account_order(&mut doc,"codex",Some(vec!["codex:a".into(),"codex:a".into()])).is_err());
        assert!(provider_account_order(&mut doc,"codex",Some(vec!["codex:a".into(),"other:c".into()])).is_err());
        let expected=vec!["codex:b".to_string(),"codex:a".to_string()];
        provider_account_order(&mut doc,"codex",Some(expected.clone())).unwrap();
        let mut reloaded=serde_json::from_str(&serde_json::to_string(&doc).unwrap()).unwrap();
        assert_eq!(provider_account_order(&mut reloaded,"codex",None).unwrap(),expected);
        assert_eq!(doc["profiles"],before);
    }
}

#[cfg(test)]
mod history_edit_diff_tests {
    #[test]
    fn history_edit_diff_counts_only_changed_lines() {
        let input = serde_json::json!({"path":"a.css","old_str":"a\nb\nc\n","new_str":"a\nB\nnew\nc\n"}).to_string();
        assert_eq!(super::history_edit_diff("str_replace", &input), "-b\n+B\n+new\n");
        let write = serde_json::json!({"path":"x.txt","content":"one\ntwo"}).to_string();
        assert_eq!(super::history_edit_diff("write", &write), "+one\n+two\n");
        assert_eq!(super::history_edit_diff("str_replace", "not json"), "");
    }
}
