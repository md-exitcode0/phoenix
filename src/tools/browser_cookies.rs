//! Login porting — bring a Firefox-family browser's logged-in sessions into the
//! Chrome session Phoenix drives, by reading the source's cookies and injecting
//! them over CDP. "Always fresh": the read happens at browser-session start, so
//! Phoenix is exactly as logged-in as the source browser right now — no daily
//! timer to drift.
//!
//! Why this and not a Firefox driver: Firefox/Zen dropped Chrome's CDP, so
//! driving them needs a whole WebDriver backend. But their `cookies.sqlite`
//! stores values in PLAINTEXT, so the sessions transplant cleanly into Chrome —
//! Phoenix keeps its mature CDP engine and just borrows the logins.
//!
//! Limits (honest): this carries session COOKIES (what keeps you logged in on
//! most sites), not saved passwords or localStorage/IndexedDB auth tokens; a few
//! fingerprint-bound sites may force a re-login because the cookie now arrives in
//! Chrome instead of the source. Adding more source browsers later is just a new
//! profile-locator (Firefox-family share this reader; Chromium sources would
//! need a keyring decryptor).

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};

const MAX_PROFILES_INI_BYTES: u64 = 1024 * 1024;
const MAX_PROFILE_PATHS: usize = 256;
const MAX_PROFILE_DIR_ENTRIES: usize = 4_096;
const MAX_COOKIE_DB_BYTES: u64 = 256 * 1024 * 1024;
const MAX_COOKIE_WAL_BYTES: u64 = 256 * 1024 * 1024;
const MAX_COOKIE_SHM_BYTES: u64 = 64 * 1024 * 1024;
const MAX_COOKIES: usize = 50_000;
const MAX_COOKIE_VALUE_BYTES: usize = 64 * 1024;
const MAX_COOKIE_FIELD_BYTES: usize = 4 * 1024;

/// Sources Phoenix can discover automatically. The winner is selected by the
/// newest real cookie-store write, not profiles.ini order or a hard-coded
/// browser preference.
pub const AUTO_COOKIE_SOURCES: [&str; 11] = [
    "zen",
    "firefox",
    "librewolf",
    "floorp",
    "waterfox",
    "chrome",
    "chromium",
    "brave",
    "edge",
    "vivaldi",
    "opera",
];

/// One cookie lifted from the source browser, in CDP-injection shape.
#[derive(Debug, Clone, PartialEq)]
pub struct PortedCookie {
    pub domain: String,
    pub name: String,
    pub value: String,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    /// CDP sameSite spelling: "Strict" | "Lax" | "None" (None = omit at call site).
    pub same_site: Option<String>,
    /// Unix seconds; None = session cookie.
    pub expires: Option<f64>,
}

/// Is this a source we know how to read? Firefox-family share one plaintext
/// reader here; Chromium-family (Chrome/Brave/Edge/…) route to
/// `chromium_cookies` which decrypts their OS-Crypt store.
pub fn is_supported_source(source: &str) -> bool {
    is_firefox_source(source) || crate::tools::chromium_cookies::is_chromium_source(source)
}

/// Read one supported browser's complete cookie inventory through the same
/// bounded, read-only path used by live imports. Callers decide which domains
/// are portable before persisting any sharing authority.
pub fn read_source_cookies(source: &str) -> Result<Vec<PortedCookie>> {
    let source = source.trim();
    anyhow::ensure!(
        is_supported_source(source),
        "unsupported cookie source `{source}`"
    );
    if crate::tools::chromium_cookies::is_chromium_source(source) {
        crate::tools::chromium_cookies::read_cookies(source)
    } else {
        let profile = locate_profile(source)?;
        tracing::info!(
            "login port ({source}): reading profile {}",
            profile.display()
        );
        read_cookies(&profile)
    }
}

/// Firefox-family sources read here (plaintext `cookies.sqlite`).
pub fn is_firefox_source(source: &str) -> bool {
    matches!(
        source.trim().to_lowercase().as_str(),
        "zen" | "firefox" | "librewolf" | "floorp" | "waterfox"
    )
}

/// Find the installed browser whose cookie store was written most recently.
/// This is deliberately metadata-only: detection never reads cookie values or
/// asks the keyring to decrypt anything.
pub fn detect_active_source() -> Result<String> {
    let mut activity = Vec::new();
    let mut errors = Vec::new();
    for source in AUTO_COOKIE_SOURCES {
        match source_activity_mtime(source) {
            Ok(modified) => activity.push((source.to_string(), modified)),
            Err(error) => errors.push(format!("{source}: {error:#}")),
        }
    }
    if let Some(source) = select_newest_source(&activity) {
        return Ok(source);
    }
    tracing::debug!(failures = ?errors, "automatic browser-profile detection found no source");
    anyhow::bail!(
        "Phoenix couldn't find a browser profile with portable logins. Open Zen, Firefox, Chrome, Brave, Edge, Vivaldi, or Opera once, then try again."
    )
}

fn select_newest_source(activity: &[(String, SystemTime)]) -> Option<String> {
    activity
        .iter()
        .max_by_key(|(_, modified)| *modified)
        .map(|(source, _)| source.clone())
}

fn source_activity_mtime(source: &str) -> Result<SystemTime> {
    if crate::tools::chromium_cookies::is_chromium_source(source) {
        return crate::tools::chromium_cookies::source_activity_mtime(source);
    }
    let profile = locate_active_profile(source)?;
    profile_activity_mtime(&profile)?.context("active browser profile has no activity marker")
}

/// Domains whose sessions are DEVICE- or TOKEN-BOUND. Copying their cookies into
/// a different browser makes the server treat it as session theft and INVALIDATE
/// the session server-side — which logs the user out EVERYWHERE, including their
/// real daily-driver browser. Confirmed live: porting Zen's Google cookies into
/// Phoenix's Chrome logged the user out of all Google accounts in Zen (Google's
/// Device Bound Session Credentials). We NEVER port these — the user signs into
/// such a site inside Phoenix's own browser if it's ever needed. A re-login in
/// Phoenix is a fine cost; nuking the user's real sessions is not.
pub fn is_session_bound_domain(domain: &str) -> bool {
    let d = domain.trim_start_matches('.').to_lowercase();
    const BOUND_ROOTS: [&str; 15] = [
        "google.com",  // accounts./mail./docs./… all DBSC-bound
        "youtube.com", // Google auth
        "googleusercontent.com",
        "googleapis.com",
        "microsoftonline.com", // Microsoft token binding
        "microsoftonline-p.com",
        "microsoft.com",
        "office.com",
        "office365.com",
        "outlook.com",
        "sharepoint.com",
        "onmicrosoft.com",
        "windows.net",
        "live.com",
        "xboxlive.com",
    ];
    BOUND_ROOTS
        .iter()
        .any(|root| d == *root || d.ends_with(&format!(".{root}")))
}

/// Per-OS data root for a Firefox-family browser. Only Linux is wired today;
/// the match is the single place a Windows/macOS port adds its paths.
fn source_data_root(source: &str) -> Option<PathBuf> {
    let home = dirs_home()?;
    let s = source.trim().to_lowercase();
    // Linux conventions. (Windows: %APPDATA%\<vendor>; macOS: ~/Library/...)
    let rel = match s.as_str() {
        "zen" => ".config/zen",
        "firefox" => ".mozilla/firefox",
        "librewolf" => ".librewolf",
        "floorp" => ".floorp",
        "waterfox" => ".waterfox",
        _ => return None,
    };
    Some(home.join(rel))
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn read_profiles_ini(path: &Path) -> Result<String> {
    let Some(bytes) = read_regular_file_limited(path, MAX_PROFILES_INI_BYTES)? else {
        return Ok(String::new());
    };
    String::from_utf8(bytes).with_context(|| format!("{} is not UTF-8", path.display()))
}

fn activity_file_limit(relative: &str) -> u64 {
    match relative {
        "cookies.sqlite" => MAX_COOKIE_DB_BYTES,
        "cookies.sqlite-wal" | "places.sqlite-wal" => MAX_COOKIE_WAL_BYTES,
        _ => 128 * 1024 * 1024,
    }
}

fn regular_file_metadata(path: &Path, max_bytes: u64) -> Result<Option<std::fs::Metadata>> {
    crate::config::private_io::reject_symlink_components(path)?;
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("inspecting {}", path.display()));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        anyhow::bail!("refusing non-regular browser state {}", path.display());
    }
    if metadata.len() > max_bytes {
        anyhow::bail!(
            "browser state {} is too large ({} bytes; max {max_bytes})",
            path.display(),
            metadata.len()
        );
    }
    Ok(Some(metadata))
}

fn cookie_store_metadata(profile: &Path) -> Result<Option<std::fs::Metadata>> {
    regular_file_metadata(&profile.join("cookies.sqlite"), MAX_COOKIE_DB_BYTES)
}

fn open_regular_file_limited(path: &Path, max_bytes: u64) -> Result<Option<std::fs::File>> {
    if regular_file_metadata(path, max_bytes)?.is_none() {
        return Ok(None);
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > max_bytes {
        anyhow::bail!("browser state {} changed while opening", path.display());
    }
    Ok(Some(file))
}

fn read_regular_file_limited(path: &Path, max_bytes: u64) -> Result<Option<Vec<u8>>> {
    let Some(mut file) = open_regular_file_limited(path, max_bytes)? else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    file.by_ref()
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading {}", path.display()))?;
    if bytes.len() as u64 > max_bytes {
        anyhow::bail!(
            "browser state {} grew beyond its size limit",
            path.display()
        );
    }
    Ok(Some(bytes))
}

/// Locate the DEFAULT profile directory of a source browser by parsing its
/// `profiles.ini` (the `Default=` install pointer, else the profile flagged
/// `Default=1`, else the first profile). Returns the absolute profile dir.
pub fn locate_profile(source: &str) -> Result<PathBuf> {
    let root = source_data_root(source)
        .with_context(|| format!("unknown/unsupported login source: {source}"))?;
    let ini = root.join("profiles.ini");
    // A browser update or interrupted profile-manager run can temporarily
    // replace/delete profiles.ini while the real profile is still live.  Do
    // not make that tiny registry file a single point of failure: the cookie
    // stores below are authoritative enough to recover the active directory.
    let text = read_profiles_ini(&ini)?;
    let candidate = default_profile_path(&text)
        .map(|rel| resolve_profile_path(&root, &rel))
        .transpose()?;
    // Prefer the pointed-at profile ONLY if it is the one actually being used.
    //
    // Existence alone is not enough. A real case: profiles.ini pointed at a
    // profile whose cookies.sqlite was three months stale and held ONE cookie,
    // while the user's live profile had 2122 including the ChatGPT session.
    // The old check saw a cookies.sqlite, returned immediately, and the agent
    // launched signed out of everything — with no error, because from here
    // everything had "succeeded". Firefox leaves stale profiles behind on
    // upgrade and re-point, so a pointer going stale is normal, not exotic.
    //
    // So: if another profile's cookie store is materially newer, that is the
    // one in use. Compare mtimes rather than trusting the pointer.
    let newest = newest_cookie_profile(&root, &text)?;
    if let Some(candidate) = candidate {
        if cookie_store_metadata(&candidate)?.is_none() {
            if let Some(active) = newest {
                return Ok(active);
            }
            anyhow::bail!(
                "{source} has no profile with a cookies.sqlite (looked under {}) — open {source} once and sign in first",
                root.display()
            );
        }
        match &newest {
            // A clearly fresher store elsewhere means the pointer is stale.
            // The day of slack keeps a browser that is merely idle right now
            // from being treated as abandoned.
            Some(active) if *active != candidate && is_newer_by(active, &candidate, 86_400)? => {
                return Ok(active.clone());
            }
            _ => return Ok(candidate),
        }
    }
    // No usable registry pointer. This is the recovery path for a missing,
    // truncated, or mid-rewrite profiles.ini.
    if let Some(active) = newest {
        return Ok(active);
    }
    anyhow::bail!(
        "{source} has no profile with a cookies.sqlite (looked under {}) — open {source} once and sign in first",
        root.display()
    )
}

/// Locate the profile with the freshest live browser activity, ignoring a
/// potentially stale/mis-pointed install default. OAuth URL launch uses this:
/// opening the authorization tab in the profile the user is actively browsing
/// is more important than preserving profiles.ini's preference.
pub fn locate_active_profile(source: &str) -> Result<PathBuf> {
    let root = source_data_root(source)
        .with_context(|| format!("unknown/unsupported login source: {source}"))?;
    let text = read_profiles_ini(&root.join("profiles.ini"))?;
    newest_cookie_profile(&root, &text)?
        .with_context(|| format!("{source} has no active profile under {}", root.display()))
}

/// Is `a`'s cookies.sqlite newer than `b`'s by more than `slack` seconds?
///
/// Used to decide that a profiles.ini pointer has gone stale. Missing
/// timestamps answer `false`; unsafe or unreadable state is surfaced instead
/// of silently redirecting the login source.
fn is_newer_by(a: &Path, b: &Path, slack: u64) -> Result<bool> {
    let mtime = |p: &Path| -> Result<Option<SystemTime>> {
        let Some(metadata) = cookie_store_metadata(p)? else {
            return Ok(None);
        };
        Ok(Some(metadata.modified()?))
    };
    Ok(newer_by(mtime(a)?, mtime(b)?, slack))
}

/// Pure half of [`is_newer_by`], so the staleness rule is testable without
/// having to forge file mtimes.
fn newer_by(a: Option<SystemTime>, b: Option<SystemTime>, slack: u64) -> bool {
    match (a, b) {
        (Some(ta), Some(tb)) => ta
            .duration_since(tb)
            .map(|d| d.as_secs() > slack)
            .unwrap_or(false),
        _ => false,
    }
}

/// All `[Profile*]` `Path=` values in a profiles.ini (relative or absolute).
fn all_profile_paths(ini: &str) -> Result<Vec<String>> {
    let mut paths = Vec::new();
    let mut in_profile = false;
    for line in ini.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_profile = line.to_lowercase().starts_with("[profile");
            continue;
        }
        if in_profile {
            if let Some((key, val)) = line.split_once('=') {
                if key.trim().eq_ignore_ascii_case("path") {
                    if paths.len() >= MAX_PROFILE_PATHS {
                        anyhow::bail!(
                            "profiles.ini has more than {MAX_PROFILE_PATHS} profile paths"
                        );
                    }
                    paths.push(val.trim().to_string());
                }
            }
        }
    }
    Ok(paths)
}

/// The profile dir whose `cookies.sqlite` was modified most recently — a robust
/// "which profile is actually active" signal when the install pointer is stale.
fn newest_cookie_profile(root: &Path, ini: &str) -> Result<Option<PathBuf>> {
    crate::config::private_io::reject_symlink_components(root)?;
    let mut candidates: Vec<PathBuf> = all_profile_paths(ini)?
        .into_iter()
        .map(|rel| resolve_profile_path(root, &rel))
        .collect::<Result<_>>()?;
    // Also inspect unregistered directories. This is what lets Phoenix recover
    // when profiles.ini was damaged but Zen/Firefox is still writing its real
    // profile on disk.
    match std::fs::read_dir(root) {
        Ok(entries) => {
            for (index, entry) in entries.enumerate() {
                if index >= MAX_PROFILE_DIR_ENTRIES {
                    anyhow::bail!(
                        "browser profile root {} has more than {MAX_PROFILE_DIR_ENTRIES} entries",
                        root.display()
                    );
                }
                let entry = entry.context("reading browser profile directory")?;
                if !entry.file_type()?.is_dir() {
                    continue;
                }
                let dir = entry.path();
                if !candidates.contains(&dir) {
                    candidates.push(dir);
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("reading browser profile root"),
    }

    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for dir in candidates {
        if cookie_store_metadata(&dir)?.is_none() {
            continue;
        }
        let Some(mtime) = profile_activity_mtime(&dir)? else {
            continue;
        };
        if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
            best = Some((mtime, dir));
        }
    }
    Ok(best.map(|(_, dir)| dir))
}

fn resolve_profile_path(root: &Path, value: &str) -> Result<PathBuf> {
    if value.is_empty() || value.len() > 4_096 {
        anyhow::bail!("profile path must be 1..=4096 bytes");
    }
    let path = Path::new(value);
    if path.is_absolute() {
        crate::config::private_io::reject_symlink_components(path)?;
        Ok(path.to_path_buf())
    } else {
        if path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        {
            anyhow::bail!("relative profile path is unsafe: {value:?}");
        }
        validate_profile_path_below_root(root, path)?;
        Ok(root.join(path))
    }
}

fn validate_profile_path_below_root(root: &Path, relative: &Path) -> Result<()> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            anyhow::bail!("unsafe relative profile path");
        };
        current.push(name);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                anyhow::bail!(
                    "refusing symlinked browser profile path {}",
                    current.display()
                );
            }
            Ok(metadata) if current != root.join(relative) && !metadata.is_dir() => {
                anyhow::bail!(
                    "browser profile parent is not a directory: {}",
                    current.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Session recovery and WAL files advance while the browser is being used even
/// when cookies.sqlite itself has not checkpointed recently.
fn profile_activity_mtime(dir: &Path) -> Result<Option<SystemTime>> {
    let mut newest = None;
    for relative in [
        "sessionstore-backups/recovery.jsonlz4",
        "sessionstore-backups/recovery.baklz4",
        "cookies.sqlite-wal",
        "places.sqlite-wal",
        "cookies.sqlite",
        "sessionstore.jsonlz4",
    ] {
        let path = dir.join(relative);
        match regular_file_metadata(&path, activity_file_limit(relative))? {
            Some(metadata) => {
                let modified = metadata.modified()?;
                if newest.map_or(true, |current| modified > current) {
                    newest = Some(modified);
                }
            }
            None => {}
        }
    }
    Ok(newest)
}

/// Parse profiles.ini text → the chosen profile's `Path` value. Prefers the
/// `[Install*]` section's `Default=`, then a `[Profile*]` with `Default=1`, then
/// the first `[Profile*]` Path.
fn default_profile_path(ini: &str) -> Option<String> {
    let mut install_default: Option<String> = None;
    let mut flagged_default: Option<String> = None;
    let mut first_profile: Option<String> = None;
    let mut cur_path: Option<String> = None;
    let mut cur_is_default = false;
    let mut in_profile = false;

    let flush = |path: &mut Option<String>,
                 is_default: &mut bool,
                 flagged: &mut Option<String>,
                 first: &mut Option<String>| {
        if let Some(p) = path.take() {
            if first.is_none() {
                *first = Some(p.clone());
            }
            if *is_default {
                *flagged = Some(p);
            }
        }
        *is_default = false;
    };

    for line in ini.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            // section boundary — flush any pending profile
            if in_profile {
                flush(
                    &mut cur_path,
                    &mut cur_is_default,
                    &mut flagged_default,
                    &mut first_profile,
                );
            }
            in_profile = line.to_lowercase().starts_with("[profile");
            continue;
        }
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        let (key, val) = (key.trim(), val.trim());
        if in_profile {
            match key.to_lowercase().as_str() {
                "path" => cur_path = Some(val.to_string()),
                "default" => cur_is_default = val == "1",
                _ => {}
            }
        } else if key.eq_ignore_ascii_case("Default") {
            // [Install*] Default=<path>
            install_default = Some(val.to_string());
        }
    }
    if in_profile {
        flush(
            &mut cur_path,
            &mut cur_is_default,
            &mut flagged_default,
            &mut first_profile,
        );
    }
    install_default.or(flagged_default).or(first_profile)
}

/// Read all cookies from a source profile. Copies the DB first (Firefox holds
/// `cookies.sqlite` WAL-locked while running) so the live browser is untouched
/// and we read a consistent snapshot. Read-only; values are plaintext in
/// Firefox-family stores.
pub fn read_cookies(profile_dir: &Path) -> Result<Vec<PortedCookie>> {
    crate::config::private_io::reject_symlink_components(profile_dir)?;
    let profile_metadata = std::fs::symlink_metadata(profile_dir)
        .with_context(|| format!("inspecting browser profile {}", profile_dir.display()))?;
    if profile_metadata.file_type().is_symlink() || !profile_metadata.is_dir() {
        anyhow::bail!(
            "browser profile is not a safe directory: {}",
            profile_dir.display()
        );
    }
    let mut temp = tempfile::Builder::new()
        .prefix("phoenix-cookies-")
        .suffix(".sqlite")
        .tempfile()
        .context("creating private cookie snapshot")?;
    copy_regular_file_to(
        &profile_dir.join("cookies.sqlite"),
        temp.as_file_mut(),
        MAX_COOKIE_DB_BYTES,
    )?
    .context("source profile has no cookies.sqlite")?;
    temp.as_file_mut().sync_all()?;

    // Copy WAL/SHM only when present. The random create-new base makes the
    // exact SQLite sidecar names unguessable before this call; each sidecar is
    // also opened with O_EXCL/O_NOFOLLOW.
    let mut sidecars = SidecarCleanup::default();
    for (suffix, limit) in [("wal", MAX_COOKIE_WAL_BYTES), ("shm", MAX_COOKIE_SHM_BYTES)] {
        let src = profile_dir.join(format!("cookies.sqlite-{suffix}"));
        let mut name = temp.path().as_os_str().to_os_string();
        name.push(format!("-{suffix}"));
        let dst = PathBuf::from(name);
        if copy_regular_file_to_new(&src, &dst, limit)? {
            sidecars.0.push(dst);
        }
    }
    read_cookies_from_db(temp.path())
}

#[derive(Default)]
struct SidecarCleanup(Vec<PathBuf>);

impl Drop for SidecarCleanup {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn copy_regular_file_to(
    source: &Path,
    destination: &mut std::fs::File,
    max_bytes: u64,
) -> Result<Option<()>> {
    let Some(mut source_file) = open_regular_file_limited(source, max_bytes)? else {
        return Ok(None);
    };
    destination.set_len(0)?;
    let copied = std::io::copy(&mut source_file.by_ref().take(max_bytes + 1), destination)
        .with_context(|| format!("copying {}", source.display()))?;
    if copied > max_bytes {
        anyhow::bail!(
            "{} grew beyond its size limit while copying",
            source.display()
        );
    }
    Ok(Some(()))
}

fn copy_regular_file_to_new(source: &Path, destination: &Path, max_bytes: u64) -> Result<bool> {
    let Some(mut source_file) = open_regular_file_limited(source, max_bytes)? else {
        return Ok(false);
    };
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    let mut destination_file = options
        .open(destination)
        .with_context(|| format!("creating cookie sidecar snapshot {}", destination.display()))?;
    let copy_result = (|| -> Result<()> {
        let copied = std::io::copy(
            &mut source_file.by_ref().take(max_bytes + 1),
            &mut destination_file,
        )?;
        if copied > max_bytes {
            anyhow::bail!(
                "{} grew beyond its size limit while copying",
                source.display()
            );
        }
        destination_file.sync_all()?;
        Ok(())
    })();
    if copy_result.is_err() {
        drop(destination_file);
        let _ = std::fs::remove_file(destination);
    }
    copy_result.map(|()| true)
}

/// Normalize a Firefox `moz_cookies.expiry` to Unix SECONDS (what CDP
/// `CookieParam.expires` wants). Upstream Firefox stores seconds (10-digit), but
/// some builds — including this user's Zen — store MILLISECONDS (13-digit), and
/// a few stores use microseconds (16-digit). A genuine seconds-expiry can't
/// exceed ~year 5138 (1e11), so anything larger is ms or µs. Passing raw ms
/// through made every cookie's expiry ~1000× too far out (Chrome can reject an
/// absurd timestamp), and broke the session-vs-persistent distinction.
fn firefox_expiry_to_unix_secs(raw: i64) -> Option<f64> {
    if raw <= 0 {
        return None; // session cookie
    }
    let secs = if raw > 100_000_000_000_000 {
        raw as f64 / 1_000_000.0 // microseconds
    } else if raw > 100_000_000_000 {
        raw as f64 / 1_000.0 // milliseconds
    } else {
        raw as f64 // already seconds
    };
    Some(secs)
}

/// Prefer the user's ACTIVE container when the same (domain,name,path) cookie
/// exists in several Firefox containers/partitions. Zen's default workspace is
/// `^userContextId=1` (where the real logins live); the no-container default
/// (`^`/empty) is next; everything else last. Without this, flattening all
/// containers into one Chrome session let an arbitrary (rowid-ordered) duplicate
/// win — e.g. a stale `.claude.ai sessionKey` or `github user_session` from the
/// wrong container, injecting the wrong/expired session.
fn container_rank(origin_attributes: &str) -> u8 {
    if origin_attributes.contains("userContextId=1") {
        2
    } else if origin_attributes.is_empty() || origin_attributes == "^" {
        1
    } else {
        0
    }
}

fn read_cookies_from_db(db: &Path) -> Result<Vec<PortedCookie>> {
    let conn = rusqlite::Connection::open_with_flags(
        db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .context("failed to open cookies snapshot")?;
    let row_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM moz_cookies", [], |row| row.get(0))
        .context("cookies.sqlite has no moz_cookies table (not a Firefox-family store?)")?;
    if row_count < 0 || row_count as u64 > MAX_COOKIES as u64 {
        anyhow::bail!("cookies.sqlite has more than {MAX_COOKIES} cookie rows");
    }
    let oversized: i64 = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM moz_cookies
            WHERE length(CAST(host AS BLOB))>?1 OR length(CAST(name AS BLOB))>?1
               OR length(CAST(path AS BLOB))>?1
               OR length(CAST(originAttributes AS BLOB))>?1
               OR length(CAST(value AS BLOB))>?2
            LIMIT 1
         )",
        rusqlite::params![MAX_COOKIE_FIELD_BYTES as i64, MAX_COOKIE_VALUE_BYTES as i64],
        |row| row.get(0),
    )?;
    if oversized != 0 {
        anyhow::bail!("cookies.sqlite contains an oversized cookie field");
    }
    let mut stmt = conn
        .prepare(
            "SELECT host, name, value, path, expiry, isSecure, isHttpOnly, sameSite, \
             originAttributes, lastAccessed \
             FROM moz_cookies LIMIT ?1",
        )
        .context("cookies.sqlite has no moz_cookies table (not a Firefox-family store?)")?;
    let rows = stmt
        .query_map([MAX_COOKIES as i64 + 1], |row| {
            let host: String = row.get(0)?;
            let expiry: i64 = row.get(4)?;
            let same_site_code: i64 = row.get(7)?;
            let origin_attributes: String = row.get(8)?;
            let last_accessed: i64 = row.get(9)?;
            let cookie = PortedCookie {
                domain: host,
                name: row.get(1)?,
                value: row.get(2)?,
                path: row.get(3)?,
                secure: row.get::<_, i64>(5)? != 0,
                http_only: row.get::<_, i64>(6)? != 0,
                same_site: match same_site_code {
                    1 => Some("Lax".to_string()),
                    2 => Some("Strict".to_string()),
                    _ => None, // 0 / 256-unset / unknown → no same-site constraint
                },
                expires: firefox_expiry_to_unix_secs(expiry),
            };
            Ok((cookie, origin_attributes, last_accessed))
        })
        .context("failed to read moz_cookies")?;
    // Deterministic de-dup across containers: keep the highest-ranked container,
    // breaking ties by most-recently-accessed.
    use std::collections::HashMap;
    let mut best: HashMap<(String, String, String), (u8, i64, PortedCookie)> = HashMap::new();
    let mut row_count = 0usize;
    for row in rows {
        let (cookie, origin_attributes, last_accessed) = row.context("invalid cookie row")?;
        row_count += 1;
        if row_count > MAX_COOKIES {
            anyhow::bail!("cookies.sqlite has more than {MAX_COOKIES} cookie rows");
        }
        if cookie.name.is_empty() {
            continue;
        }
        if cookie.domain.len() > MAX_COOKIE_FIELD_BYTES
            || cookie.name.len() > MAX_COOKIE_FIELD_BYTES
            || cookie.path.len() > MAX_COOKIE_FIELD_BYTES
            || origin_attributes.len() > MAX_COOKIE_FIELD_BYTES
            || cookie.value.len() > MAX_COOKIE_VALUE_BYTES
        {
            anyhow::bail!("cookies.sqlite contains an oversized cookie field");
        }
        let rank = container_rank(&origin_attributes);
        let key = (
            cookie.domain.clone(),
            cookie.name.clone(),
            cookie.path.clone(),
        );
        let better = match best.get(&key) {
            None => true,
            Some((r, l, _)) => rank > *r || (rank == *r && last_accessed > *l),
        };
        if better {
            best.insert(key, (rank, last_accessed, cookie));
        }
    }
    Ok(best.into_values().map(|(_, _, cookie)| cookie).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_install_default_pointer() {
        let ini = "[Install ABC]\nDefault=abcd.Default (release)\nLocked=1\n\n\
                   [Profile0]\nName=default\nIsRelative=1\nPath=abcd.Default (release)\n";
        assert_eq!(
            default_profile_path(ini).as_deref(),
            Some("abcd.Default (release)")
        );
    }

    #[test]
    fn falls_back_to_default_flagged_profile() {
        let ini =
            "[Profile0]\nPath=aaa.dev\nDefault=0\n\n[Profile1]\nPath=bbb.release\nDefault=1\n";
        assert_eq!(default_profile_path(ini).as_deref(), Some("bbb.release"));
    }

    #[test]
    fn falls_back_to_first_profile() {
        let ini = "[Profile0]\nPath=only.one\n";
        assert_eq!(default_profile_path(ini).as_deref(), Some("only.one"));
    }

    #[test]
    fn discovers_an_unregistered_profile_when_the_registry_is_empty() {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("live.Default");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(profile.join("cookies.sqlite"), b"live").unwrap();
        assert_eq!(
            newest_cookie_profile(root.path(), "").unwrap(),
            Some(profile)
        );
    }

    #[test]
    fn live_session_activity_beats_a_mispointed_registry() {
        use std::fs::{File, FileTimes};
        use std::time::{Duration, UNIX_EPOCH};

        let root = tempfile::tempdir().unwrap();
        let wrong = root.path().join("new.empty");
        let active = root.path().join("real.active");
        std::fs::create_dir_all(&wrong).unwrap();
        std::fs::create_dir_all(active.join("sessionstore-backups")).unwrap();
        std::fs::write(wrong.join("cookies.sqlite"), b"new").unwrap();
        std::fs::write(active.join("cookies.sqlite"), b"real").unwrap();
        let recovery = active.join("sessionstore-backups/recovery.jsonlz4");
        std::fs::write(&recovery, b"tabs").unwrap();
        File::options()
            .write(true)
            .open(&recovery)
            .unwrap()
            .set_times(
                FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(2_000_000_000)),
            )
            .unwrap();

        let ini = "[Profile0]\nPath=new.empty\nDefault=1\n";
        assert_eq!(
            newest_cookie_profile(root.path(), ini).unwrap(),
            Some(active)
        );
    }

    #[test]
    fn supported_sources() {
        // Firefox-family read here (plaintext).
        assert!(is_supported_source("zen"));
        assert!(is_supported_source("Firefox"));
        assert!(is_firefox_source("zen"));
        assert!(!is_firefox_source("chrome"));
        // Chromium-family are now supported too (decrypted via chromium_cookies).
        assert!(is_supported_source("chrome"));
        assert!(is_supported_source("brave"));
        assert!(!is_supported_source("safari"));
    }

    #[test]
    fn device_bound_domains_are_never_ported() {
        // The confirmed harm: porting these logged the user out of their real browser.
        assert!(is_session_bound_domain(".google.com"));
        assert!(is_session_bound_domain("accounts.google.com"));
        assert!(is_session_bound_domain("mail.google.com"));
        assert!(is_session_bound_domain(".youtube.com"));
        assert!(is_session_bound_domain("login.microsoftonline.com"));
        assert!(is_session_bound_domain("outlook.office.com"));
        assert!(is_session_bound_domain("tenant.sharepoint.com"));
        assert!(is_session_bound_domain("graph.microsoft.com"));
        // Sites where cookie transplant is fine stay portable.
        assert!(!is_session_bound_domain(".reddit.com"));
        assert!(!is_session_bound_domain("x.com"));
        assert!(!is_session_bound_domain("github.com"));
        // No over-broad substring match (a domain merely containing "google").
        assert!(!is_session_bound_domain("notgoogle.com.evil.test"));
    }

    #[test]
    fn automatic_source_uses_the_most_recent_browser_activity() {
        use std::time::{Duration, UNIX_EPOCH};

        let candidates = vec![
            (
                "firefox".to_string(),
                UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            ),
            (
                "zen".to_string(),
                UNIX_EPOCH + Duration::from_secs(1_800_000_000),
            ),
            (
                "chrome".to_string(),
                UNIX_EPOCH + Duration::from_secs(1_750_000_000),
            ),
        ];
        assert_eq!(select_newest_source(&candidates).as_deref(), Some("zen"));
        assert_eq!(select_newest_source(&[]), None);
    }

    #[test]
    fn firefox_expiry_units_normalize_to_seconds() {
        assert_eq!(firefox_expiry_to_unix_secs(0), None); // session cookie
        assert_eq!(firefox_expiry_to_unix_secs(1893456000), Some(1893456000.0)); // seconds stay
        assert_eq!(
            firefox_expiry_to_unix_secs(1781197309107),
            Some(1781197309.107)
        ); // 13-digit ms → s
        assert_eq!(
            firefox_expiry_to_unix_secs(1781197309107000),
            Some(1781197309.107)
        ); // 16-digit µs → s
    }

    #[test]
    fn container_rank_prefers_active_workspace() {
        assert_eq!(container_rank("^userContextId=1"), 2);
        assert_eq!(container_rank(""), 1);
        assert_eq!(container_rank("^"), 1);
        assert_eq!(container_rank("^userContextId=3"), 0);
    }

    #[test]
    fn reads_plaintext_cookies_from_a_moz_store() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("cookies.sqlite");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE moz_cookies (host TEXT, name TEXT, value TEXT, path TEXT, \
             expiry INTEGER, isSecure INTEGER, isHttpOnly INTEGER, sameSite INTEGER, \
             originAttributes TEXT, lastAccessed INTEGER);\
             INSERT INTO moz_cookies VALUES \
             ('.reddit.com','session','tok123','/',1893456000,1,1,1,'',1),\
             ('x.com','auth','zz','/',0,1,0,2,'',1);",
        )
        .unwrap();
        drop(conn);
        let cookies = read_cookies(dir.path()).unwrap();
        assert_eq!(cookies.len(), 2);
        let reddit = cookies.iter().find(|c| c.domain == ".reddit.com").unwrap();
        assert_eq!(reddit.value, "tok123");
        assert!(reddit.secure && reddit.http_only);
        assert_eq!(reddit.same_site.as_deref(), Some("Lax"));
        assert_eq!(reddit.expires, Some(1893456000.0));
        let x = cookies.iter().find(|c| c.domain == "x.com").unwrap();
        assert_eq!(x.same_site.as_deref(), Some("Strict"));
        assert_eq!(x.expires, None); // session cookie
    }

    #[test]
    fn dedups_containers_preferring_active_workspace_and_normalizes_ms_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("cookies.sqlite");
        let conn = rusqlite::Connection::open(&db).unwrap();
        // Same (host,name,path) in two containers: the no-container '' (older,
        // stale value) and userContextId=1 (the active workspace, newer value).
        // Expiry stored in MILLISECONDS (Zen build). The active-workspace value
        // must win and the expiry must normalize to seconds.
        conn.execute_batch(
            "CREATE TABLE moz_cookies (host TEXT, name TEXT, value TEXT, path TEXT, \
             expiry INTEGER, isSecure INTEGER, isHttpOnly INTEGER, sameSite INTEGER, \
             originAttributes TEXT, lastAccessed INTEGER);\
             INSERT INTO moz_cookies VALUES \
             ('.claude.ai','sessionKey','STALE','/',1781197309107,1,1,0,'',100),\
             ('.claude.ai','sessionKey','ACTIVE','/',1781197309107,1,1,0,'^userContextId=1',50);",
        )
        .unwrap();
        drop(conn);
        let cookies = read_cookies(dir.path()).unwrap();
        assert_eq!(cookies.len(), 1, "the duplicate must collapse to one");
        let c = &cookies[0];
        assert_eq!(c.value, "ACTIVE", "active workspace (userContextId=1) wins");
        assert_eq!(
            c.expires,
            Some(1781197309.107),
            "ms expiry normalized to seconds"
        );
    }

    #[test]
    fn profile_registry_rejects_traversal_and_oversized_cookie_values() {
        let root = tempfile::tempdir().unwrap();
        assert!(newest_cookie_profile(root.path(), "[Profile0]\nPath=../escape\n").is_err());

        let db = root.path().join("cookies.sqlite");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE moz_cookies (host TEXT, name TEXT, value TEXT, path TEXT, \
             expiry INTEGER, isSecure INTEGER, isHttpOnly INTEGER, sameSite INTEGER, \
             originAttributes TEXT, lastAccessed INTEGER);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO moz_cookies VALUES (?1,?2,?3,'/',0,0,0,0,'',0)",
            rusqlite::params![
                "example.com",
                "session",
                "x".repeat(MAX_COOKIE_VALUE_BYTES + 1)
            ],
        )
        .unwrap();
        drop(conn);
        assert!(read_cookies(root.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn browser_state_readers_reject_symlinks_and_oversized_databases() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::write(&outside, b"secret").unwrap();
        let ini = dir.path().join("profiles.ini");
        symlink(&outside, &ini).unwrap();
        assert!(read_profiles_ini(&ini).is_err());

        let profile = dir.path().join("profile");
        std::fs::create_dir(&profile).unwrap();
        symlink(&outside, profile.join("cookies.sqlite")).unwrap();
        assert!(read_cookies(&profile).is_err());
        std::fs::remove_file(profile.join("cookies.sqlite")).unwrap();
        let large = std::fs::File::create(profile.join("cookies.sqlite")).unwrap();
        large.set_len(MAX_COOKIE_DB_BYTES + 1).unwrap();
        assert!(read_cookies(&profile).is_err());
    }
}

#[cfg(test)]
mod stale_pointer_tests {
    use super::*;
    use std::time::Duration;

    fn ago(secs: u64) -> Option<SystemTime> {
        Some(SystemTime::now() - Duration::from_secs(secs))
    }

    #[test]
    fn a_clearly_fresher_store_beats_a_stale_pointer() {
        // The live case: profiles.ini pointed at a three-month-old profile
        // holding one cookie while the user browsed in another that held the
        // real ChatGPT session. The agent launched signed out, with no error.
        assert!(newer_by(ago(60), ago(90 * 86_400), 86_400));
    }

    #[test]
    fn a_browser_merely_idle_today_is_not_treated_as_abandoned() {
        // Both used recently: the pointer wins, so ordinary multi-profile
        // setups are never silently redirected.
        assert!(!newer_by(ago(60), ago(7_200), 86_400));
    }

    #[test]
    fn an_unreadable_mtime_never_redirects() {
        // An mtime we cannot trust must not move the login source.
        assert!(!newer_by(None, ago(60), 86_400));
        assert!(!newer_by(ago(60), None, 86_400));
    }
}
