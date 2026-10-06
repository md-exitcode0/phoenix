//! Unified Phoenix home paths and legacy cwd-relative state migration.
//!
//! All Phoenix-owned state lives under [`phoenix_home()`] (default `~/.phoenix`),
//! independent of the process cwd. The cwd is the target repo for workspace tools.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use super::phoenix_home;

pub fn phoenix_memory_root() -> PathBuf {
    phoenix_home().join("memory")
}

/// Root for the Cognee memory engine's storage (graph + vectors + sqlite +
/// cache + logs). Lives under Phoenix home so memory stays with the user's
/// config, next to prompts and VITALS.
pub fn phoenix_cognee_root() -> PathBuf {
    phoenix_home().join("cognee")
}

/// Phoenix's always-on "vital memory" file. Deliberately at the Phoenix home
/// ROOT, NOT under [`phoenix_memory_root`] — this is outside the librarian's
/// tiered memory and is injected into every turn's context verbatim.
pub fn phoenix_vitals_path() -> PathBuf {
    phoenix_home().join("VITALS.md")
}

pub fn phoenix_sessions_root() -> PathBuf {
    phoenix_home().join("sessions")
}

pub fn phoenix_session_cache_root() -> PathBuf {
    phoenix_home().join("session_cache")
}

pub fn phoenix_runs_root() -> PathBuf {
    phoenix_home().join("runs")
}

pub fn phoenix_history_path() -> PathBuf {
    phoenix_home().join("history")
}

/// Shared default working directory for every desktop coworker. Conversation
/// identity and memory remain private, while ordinary relative file work
/// starts from this one user-visible company workspace.
pub fn phoenix_workspace_root() -> PathBuf {
    phoenix_home().join("workspace")
}

/// Shared credential for the loopback Chromium shell bridge. The desktop may
/// restart while the detached gateway stays alive, so this cannot be a fresh
/// process-local random value on every app launch.
pub fn phoenix_chromium_bridge_token_path() -> PathBuf {
    phoenix_home().join("chromium-bridge.token")
}

pub fn read_chromium_bridge_token() -> Result<Option<String>> {
    let path = phoenix_chromium_bridge_token_path();
    let Some(bytes) = super::private_io::read_private_file_limited(&path, 256)? else {
        return Ok(None);
    };
    let token = std::str::from_utf8(&bytes)
        .with_context(|| format!("{} is not UTF-8", path.display()))?
        .trim();
    anyhow::ensure!(
        token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{} does not contain a valid Chromium bridge token",
        path.display()
    );
    Ok(Some(token.to_ascii_lowercase()))
}

/// Detached gateways can start before the desktop, without inheriting its
/// bridge environment. Discover the standard local bridge once this same
/// Phoenix home has a private desktop credential. Read it on every request.
pub fn chromium_bridge_url() -> Result<Option<String>> {
    let configured = std::env::var("PHOENIX_CHROMIUM_BRIDGE_URL").ok();
    if let Some(url) = configured.filter(|value| !value.trim().is_empty()) {
        return Ok(Some(url));
    }
    Ok(read_chromium_bridge_token()?.map(|_| "http://127.0.0.1:17443".to_string()))
}

/// Create the unified Phoenix home layout if missing.
pub fn ensure_phoenix_home() -> Result<PathBuf> {
    let home = phoenix_home();
    // A new app-owned leaf is private from its first inode. Existing custom
    // roots must already be same-owner and private; they are rejected rather
    // than chmodded. Only the known default ~/.phoenix tree is repaired.
    super::private_io::prepare_phoenix_home(&home)?;
    super::private_io::repair_known_default_top_level_state(&home)?;
    for sub in [
        "sessions",
        "session_cache",
        "runs",
        "workspace",
        "skills",
        "prompts",
        "checkpoints",
    ] {
        super::private_io::prepare_phoenix_directory(&home.join(sub))?;
    }
    let memory = home.join("memory");
    for tier in ["HOT", "WARM", "COLD", "knowledge", "curation"] {
        super::private_io::prepare_phoenix_directory(&memory.join(tier))?;
    }
    // The map of the house — kept current on every startup.
    let readme = home.join("README.md");
    super::private_io::atomic_write_private(&readme, PHOENIX_HOME_README.as_bytes())
        .with_context(|| format!("Failed to write {}", readme.display()))?;
    seed_prompt_overlays(&home)?;
    Ok(home)
}

fn seed_prompt_overlays(home: &Path) -> Result<()> {
    let prompts = home.join("prompts");
    super::private_io::prepare_phoenix_directory(&prompts)?;

    for (name, body) in DEFAULT_PROMPT_OVERLAYS {
        let path = prompts.join(name);
        let existing = super::private_io::read_private_file(&path)?;
        let should_write = match existing.as_deref() {
            Some(existing) => {
                let text = std::str::from_utf8(existing)
                    .with_context(|| format!("{} is not UTF-8", path.display()))?;
                if text.trim().is_empty() {
                    true
                } else if is_historical_seeded_prompt(name, existing) && existing != body.as_bytes()
                {
                    archive_seeded_prompt(home, name, existing)?;
                    true
                } else {
                    false
                }
            }
            None => true,
        };
        if should_write {
            super::private_io::atomic_write_private(&path, body.as_bytes())
                .with_context(|| format!("Failed to write {}", path.display()))?;
        }
    }
    Ok(())
}

/// Exact hashes of Phoenix-shipped prompt defaults from earlier releases.
/// This is deliberately a byte-exact allowlist: a user edit, even one that
/// kept the historical first line, is never rewritten by the migration.
fn is_historical_seeded_prompt(name: &str, bytes: &[u8]) -> bool {
    let digest = format!("{:x}", Sha256::digest(bytes));
    HISTORICAL_PROMPT_SEEDS
        .iter()
        .any(|(seed_name, seed_digest)| *seed_name == name && *seed_digest == digest)
}

fn archive_seeded_prompt(home: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let digest = format!("{:x}", Sha256::digest(bytes));
    let archive = home
        .join("archive")
        .join("prompt-overlays")
        .join(&digest[..16]);
    super::private_io::prepare_phoenix_directory(&archive)?;
    super::private_io::atomic_write_private_if_missing(&archive.join(name), bytes)?;
    Ok(())
}

const HISTORICAL_PROMPT_SEEDS: &[(&str, &str)] = &[
    // Older defaults currently found in long-lived Phoenix homes.
    (
        "browser_system.md",
        "f27b1dd27a494280f555e033d9d679b5b5317317b5b4595b7cb3d9f263949526",
    ),
    (
        "orchestrator_system.md",
        "5abd9296301b44f23f3b472fa5849ea93fe5a8dce7acce7b3603d56187e907a5",
    ),
    (
        "planner_system.md",
        "1cf6b7a39c31d4d2b194464f30cfd4e28f6e5b20abf3d6e0b7d7e0e2b6c21da3",
    ),
    (
        "researcher_system.md",
        "bf85f473619d6042eab127ef99d91bd3a17da2293a51b2004a1f75e44647d880",
    ),
    (
        "presentation_system.md",
        "5863201c66c57261e4c9d7cbb45eb2c73c25c0c428d1c52dc3c9cbe30e7701f5",
    ),
    (
        "critic_system.md",
        "ace07245e52b2c7466738805ea05e219ff5c11120eee5a615fc81bbb3914f317",
    ),
    (
        "scribe_system.md",
        "1ce8eb90709dcc83b626a0e29189f2265a05ecfdc7dfcc42c9af1836e1d01054",
    ),
    (
        "frontend_system.md",
        "a6e1082588b0b1e18efb52f77a8f495b29ef648e26118c48aff74bb505a09b8a",
    ),
    (
        "coder_system.md",
        "8a1a6b0b0edc916f2cd29f154a14db223f76c4ddd9c364a11fdce18bc921e824",
    ),
    // The immediately preceding responsibility-company defaults.
    (
        "orchestrator_system.md",
        "d934acaf36d088cc2b1effca1f5ae877e794e2b9ad3bf9673a7392ee5fe0bbd8",
    ),
    (
        "planner_system.md",
        "51e9e781df6ed31b459d5b1d559ff88791f19e1657f96c384f10ceccbc1e95e5",
    ),
    (
        "researcher_system.md",
        "03985fa654e6d8f4cb1048328dcbc0a47e6f70f7eaab725f16794053eed2a1ce",
    ),
    (
        "presentation_system.md",
        "69ff01701aa87f1d2d45e80fa8ff000ddd298857bba5204957bf72fd43f03c11",
    ),
    (
        "critic_system.md",
        "18aa09a06918fba73f6e5325f6ee90097164b9f9e4cb06998dd072dc00486124",
    ),
    (
        "scribe_system.md",
        "e4a598a536b20f03384edfd736f5a18cfed1272bfc620d25da66a4c6a2fc4584",
    ),
    (
        "finance_system.md",
        "57b2cf416d3b1f5e58f093f0ff9622604f4caf3b6b9e296166a99b5dfcbacde4",
    ),
    (
        "frontend_system.md",
        "ea56a54827cc2f603616f51fd1da3a519f2871aec4145c0fe0b94c79d2899c34",
    ),
    (
        "coder_system.md",
        "6510f2b4adf97d30da379d569f695dde2009eb4227e026ac8e9987da365756d6",
    ),
    (
        "marketing_system.md",
        "9bacacbe95a723b325eef5d63cbd4acf9137eac3482801916f64a0870d12a836",
    ),
    (
        "sales_system.md",
        "37e7057e288a441a3cd9c705b1578872f80d46c855010443abaca1bb8eab2cea",
    ),
    (
        "personal_logistics_system.md",
        "630c8ff0452bad8a6df533e878002d9eaa8093e9d2a07b9a4d63979bb361eb99",
    ),
];

const DEFAULT_PROMPT_OVERLAYS: &[(&str, &str)] = &[
    (
        "orchestrator_system.md",
        include_str!("../../prompts/orchestrator_system.md"),
    ),
    (
        "coder_system.md",
        include_str!("../../prompts/coder_system.md"),
    ),
    (
        "browser_system.md",
        include_str!("../../prompts/browser_system.md"),
    ),
    (
        "researcher_system.md",
        include_str!("../../prompts/researcher_system.md"),
    ),
    (
        "frontend_system.md",
        include_str!("../../prompts/frontend_system.md"),
    ),
    (
        "presentation_system.md",
        include_str!("../../prompts/presentation_system.md"),
    ),
    (
        "computer_use_system.md",
        include_str!("../../prompts/computer_use_system.md"),
    ),
    (
        "database_system.md",
        include_str!("../../prompts/database_system.md"),
    ),
    (
        "hacker_system.md",
        include_str!("../../prompts/hacker_system.md"),
    ),
    (
        "critic_system.md",
        include_str!("../../prompts/critic_system.md"),
    ),
    (
        "tester_system.md",
        include_str!("../../prompts/tester_system.md"),
    ),
    (
        "planner_system.md",
        include_str!("../../prompts/planner_system.md"),
    ),
    (
        "scribe_system.md",
        include_str!("../../prompts/scribe_system.md"),
    ),
    (
        "finance_system.md",
        include_str!("../../prompts/finance_system.md"),
    ),
    (
        "marketing_system.md",
        include_str!("../../prompts/marketing_system.md"),
    ),
    (
        "sales_system.md",
        include_str!("../../prompts/sales_system.md"),
    ),
    (
        "personal_logistics_system.md",
        include_str!("../../prompts/personal_logistics_system.md"),
    ),
];

/// Written to `~/.phoenix/README.md` so the state directory explains itself.
const PHOENIX_HOME_README: &str = "\
# ~/.phoenix — Phoenix's home

Everything Phoenix knows and does lives here, per directory:

| Path | What lives there |
|---|---|
| `config.toml` | Provider, models, temperature, reasoning effort, context window |
| `auth-profiles.json` | Stored auth (API keys / OAuth profiles) |
| `memory/` | Persistent memory: `HOT/` `WARM/` `COLD/` tiers + `knowledge/`. `.access.json` is the usage ledger that drives tier promotion |
| `skills/` | Installed skills — one portable `SKILL.md` package per directory |
| `prompts/` | Editable system-prompt overlays seeded from Phoenix defaults; edit `<agent>_system.md` here to override without recompiling |
| `archive/prompt-overlays/` | Byte-exact older Phoenix defaults retained before an automatic prompt refresh; user-edited overlays are never refreshed |
| `checkpoints/` | Shadow copies of files the agent edited, per turn — `phoenix rewind` restores them |
| `sessions/` | Durable conversation transcripts (+ `.archive.jsonl` of auto-compacted history) |
| `session_cache/` | Per-session librarian working state |
| `runs/` | Per-turn traces (`trace.json`) for debugging |
| `workspace/` | Shared default working directory for all coworkers; Workspace mode is confined here |
| `history` | CLI input history |
| `gateway.log` / `.pid` / `.sock` | The running gateway daemon |
| `crons.json` | Scheduled wake-ups created via `/cron` or the cron tool |
| `composio-mcp.key` | Composio For You consumer key (`ck_`) — your connected apps over MCP (Gmail, GitHub, Tavily, Reddit…); or set COMPOSIO_MCP_KEY |
| `composio.key` | Composio Platform API key (`ak_`) — legacy REST fallback; or set COMPOSIO_API_KEY |
| `browser/` `bridge/` | Browser automation profile + desktop-control bridge |
| `codegraph.sqlite` | Code symbol index |

Safe to edit by hand: `config.toml`, `prompts/`, `skills/`, `memory/`.
Managed by Phoenix (don't hand-edit): everything else.
";

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LegacyMigrationReport {
    pub files_copied: usize,
    pub legacy_roots_scanned: usize,
}

const LEGACY_MIGRATION_MAX_DEPTH: usize = 32;
const LEGACY_MIGRATION_MAX_NODES: usize = 50_000;
const LEGACY_MIGRATION_MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
const LEGACY_MIGRATION_MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Default)]
struct LegacyMigrationBudget {
    nodes: usize,
    bytes: u64,
}

impl LegacyMigrationBudget {
    fn visit(&mut self) -> Result<()> {
        self.nodes = self.nodes.saturating_add(1);
        if self.nodes > LEGACY_MIGRATION_MAX_NODES {
            anyhow::bail!(
                "legacy state migration exceeded {LEGACY_MIGRATION_MAX_NODES} filesystem nodes"
            );
        }
        Ok(())
    }

    fn add_file(&mut self, path: &Path, bytes: u64) -> Result<()> {
        if bytes > LEGACY_MIGRATION_MAX_FILE_BYTES {
            anyhow::bail!(
                "legacy state file {} is too large ({bytes} bytes; max {LEGACY_MIGRATION_MAX_FILE_BYTES})",
                path.display()
            );
        }
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .context("legacy migration byte counter overflow")?;
        if self.bytes > LEGACY_MIGRATION_MAX_TOTAL_BYTES {
            anyhow::bail!(
                "legacy state migration exceeded {LEGACY_MIGRATION_MAX_TOTAL_BYTES} total bytes"
            );
        }
        Ok(())
    }
}

/// Merge legacy cwd-relative Phoenix state into the unified home directory.
///
/// Older builds stored `.phoenix/` and `memory/` under the launch directory.
/// This copies missing files into `~/.phoenix` without overwriting existing data.
pub fn migrate_legacy_state(workspace: &Path) -> Result<LegacyMigrationReport> {
    let home = phoenix_home();
    let mut report = LegacyMigrationReport::default();
    let mut budget = LegacyMigrationBudget::default();

    for legacy_root in legacy_state_roots(workspace)? {
        report.legacy_roots_scanned += 1;
        let dot_phoenix = legacy_root.join(".phoenix");
        if optional_legacy_directory(&dot_phoenix)? {
            for (src_name, dest) in [
                ("sessions", phoenix_sessions_root()),
                ("session_cache", phoenix_session_cache_root()),
                ("runs", phoenix_runs_root()),
            ] {
                report.files_copied += copy_tree_if_missing(
                    &dot_phoenix.join(src_name),
                    &dest,
                    &home,
                    &mut budget,
                    0,
                    false,
                )?;
            }
            report.files_copied += copy_file_if_missing(
                &dot_phoenix.join("history"),
                &phoenix_history_path(),
                &home,
                &mut budget,
                false,
            )?;
        }

        report.files_copied += copy_tree_if_missing(
            &legacy_root.join("memory"),
            &phoenix_memory_root(),
            &home,
            &mut budget,
            0,
            false,
        )?;
    }

    Ok(report)
}

fn optional_legacy_directory(path: &Path) -> Result<bool> {
    crate::config::private_io::reject_symlink_components(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_dir()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("Failed to inspect {}", path.display())),
    }
}

fn legacy_state_roots(workspace: &Path) -> Result<Vec<PathBuf>> {
    let mut roots = Vec::new();
    if optional_legacy_directory(workspace)? {
        roots.push(workspace.to_path_buf());
    }
    let nested = workspace.join("phoenix_agent");
    if nested != workspace && optional_legacy_directory(&nested)? {
        roots.push(nested);
    }
    Ok(roots)
}

fn open_legacy_directory(src: &Path, source_is_stable: bool) -> Result<Option<fs::File>> {
    if !source_is_stable {
        crate::config::private_io::reject_symlink_components(src)?;
    }
    let expected = match fs::symlink_metadata(src) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("Failed to inspect {}", src.display()))
        }
    };
    if expected.file_type().is_symlink() || !expected.file_type().is_dir() {
        return Ok(None);
    }

    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(
            libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_DIRECTORY,
        );
    }
    let directory = options
        .open(src)
        .with_context(|| format!("Failed to open legacy directory {}", src.display()))?;
    let opened = directory
        .metadata()
        .with_context(|| format!("Failed to inspect open directory {}", src.display()))?;
    if !opened.is_dir() || !same_file_identity(&expected, &opened) {
        anyhow::bail!(
            "legacy source directory changed while opening it: {}",
            src.display()
        );
    }
    Ok(Some(directory))
}

#[cfg(unix)]
fn same_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_file_identity(_left: &fs::Metadata, _right: &fs::Metadata) -> bool {
    true
}

/// Enumerate through an already-open directory on Linux. `/proc/self/fd/N`
/// is a kernel-owned reference to that exact inode, so renaming or replacing
/// an ancestor after validation cannot redirect the migration walk.
#[cfg(target_os = "linux")]
fn stable_directory_path(_src: &Path, directory: &fs::File) -> (PathBuf, bool) {
    use std::os::fd::AsRawFd;
    (
        PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())),
        true,
    )
}

#[cfg(not(target_os = "linux"))]
fn stable_directory_path(src: &Path, _directory: &fs::File) -> (PathBuf, bool) {
    (src.to_path_buf(), false)
}

fn copy_tree_if_missing(
    src: &Path,
    dest: &Path,
    _home: &Path,
    budget: &mut LegacyMigrationBudget,
    depth: usize,
    source_is_stable: bool,
) -> Result<usize> {
    if depth > LEGACY_MIGRATION_MAX_DEPTH {
        anyhow::bail!(
            "legacy state migration exceeded depth {LEGACY_MIGRATION_MAX_DEPTH} at {}",
            src.display()
        );
    }
    let Some(directory) = open_legacy_directory(src, source_is_stable)? else {
        return Ok(0);
    };
    budget.visit()?;
    crate::config::private_io::prepare_phoenix_directory(dest)
        .with_context(|| format!("Failed to create private directory {}", dest.display()))?;

    let mut copied = 0;
    let (read_path, children_are_stable) = stable_directory_path(src, &directory);
    for entry in
        fs::read_dir(&read_path).with_context(|| format!("Failed to read {}", src.display()))?
    {
        let entry = entry.with_context(|| format!("Failed to enumerate {}", src.display()))?;
        // Count every directory entry, including symlinks/FIFOs we safely
        // skip, so an attacker cannot bypass the traversal cap with millions
        // of non-regular nodes.
        budget.visit()?;
        let path = entry.path();
        let Some(name) = path.file_name() else {
            continue;
        };
        let target = dest.join(name);
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("Failed to inspect {}", path.display()))?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.file_type().is_dir() {
            copied += copy_tree_if_missing(
                &path,
                &target,
                _home,
                budget,
                depth + 1,
                children_are_stable,
            )?;
        } else if metadata.file_type().is_file() {
            copied += copy_file_if_missing(&path, &target, _home, budget, children_are_stable)?;
        }
    }
    Ok(copied)
}

fn copy_file_if_missing(
    src: &Path,
    dest: &Path,
    _home: &Path,
    budget: &mut LegacyMigrationBudget,
    source_is_stable: bool,
) -> Result<usize> {
    if !source_is_stable {
        crate::config::private_io::reject_symlink_components(src)?;
    }
    let source_metadata = match fs::symlink_metadata(src) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(error).with_context(|| format!("Failed to inspect {}", src.display()))
        }
    };
    if source_metadata.file_type().is_symlink() || !source_metadata.file_type().is_file() {
        return Ok(0);
    }
    budget.visit()?;
    match fs::symlink_metadata(dest) {
        Ok(metadata) if metadata.file_type().is_file() => return Ok(0),
        Ok(_) => anyhow::bail!(
            "legacy migration destination is not a regular file: {}",
            dest.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("Failed to inspect {}", dest.display()));
        }
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(src)
        .with_context(|| format!("Failed to open legacy file {}", src.display()))?;
    let opened = file
        .metadata()
        .with_context(|| format!("Failed to inspect open legacy file {}", src.display()))?;
    if !opened.is_file()
        || !same_file_identity(&source_metadata, &opened)
        || opened.len() > LEGACY_MIGRATION_MAX_FILE_BYTES
    {
        anyhow::bail!("legacy source changed or is unsafe: {}", src.display());
    }
    budget.add_file(src, opened.len())?;
    use std::io::Read;
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    file.take(LEGACY_MIGRATION_MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("Failed to read legacy file {}", src.display()))?;
    if bytes.len() as u64 > LEGACY_MIGRATION_MAX_FILE_BYTES {
        anyhow::bail!(
            "legacy source grew beyond the file limit: {}",
            src.display()
        );
    }
    let copied = crate::config::private_io::atomic_write_private_if_missing(dest, &bytes)
        .with_context(|| format!("Failed to copy {} -> {}", src.display(), dest.display()))?;
    Ok(usize::from(copied))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn with_temp_home<F: FnOnce()>(f: F) {
        let dir = tempdir().unwrap();
        let home = dir.path().join("phoenix-home");
        let _guard = crate::config::test_env::PhoenixHomeGuard::set(&home);
        f();
    }

    #[test]
    fn detached_gateway_discovers_desktop_bridge() {
        with_temp_home(|| {
            // No inherited desktop address, just the private durable token.
            let old = std::env::var_os("PHOENIX_CHROMIUM_BRIDGE_URL");
            std::env::remove_var("PHOENIX_CHROMIUM_BRIDGE_URL");
            assert!(chromium_bridge_url().unwrap().is_none());
            ensure_phoenix_home().unwrap();
            super::super::private_io::atomic_write_private(
                &phoenix_chromium_bridge_token_path(), "a".repeat(64).as_bytes(),
            ).unwrap();
            assert_eq!(chromium_bridge_url().unwrap().as_deref(), Some("http://127.0.0.1:17443"));
            std::env::set_var("PHOENIX_CHROMIUM_BRIDGE_URL", "http://127.0.0.1:17444");
            assert_eq!(chromium_bridge_url().unwrap().as_deref(), Some("http://127.0.0.1:17444"));
            match old { Some(value) => std::env::set_var("PHOENIX_CHROMIUM_BRIDGE_URL", value), None => std::env::remove_var("PHOENIX_CHROMIUM_BRIDGE_URL") }
        });
    }

    #[test]
    fn ensure_phoenix_home_creates_layout() {
        with_temp_home(|| {
            let home = ensure_phoenix_home().unwrap();
            assert!(home.join("memory").join("WARM").is_dir());
            assert!(home.join("sessions").is_dir());
            assert!(home.join("runs").is_dir());
            assert!(home.join("workspace").is_dir());
            for name in [
                "orchestrator_system.md",
                "scribe_system.md",
                "planner_system.md",
                "finance_system.md",
                "coder_system.md",
                "frontend_system.md",
                "researcher_system.md",
                "presentation_system.md",
                "critic_system.md",
                "sales_system.md",
                "marketing_system.md",
                "personal_logistics_system.md",
            ] {
                assert!(home.join("prompts").join(name).is_file(), "missing {name}");
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(&home).unwrap().permissions().mode() & 0o777,
                    0o700
                );
                assert_eq!(
                    fs::metadata(home.join("sessions"))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o700
                );
                assert_eq!(
                    fs::metadata(home.join("workspace"))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o700
                );
                assert_eq!(
                    fs::metadata(home.join("memory").join("WARM"))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o700
                );
            }
        });
    }

    #[test]
    fn shared_workspace_root_honors_phoenix_home_and_is_private() {
        with_temp_home(|| {
            let home = phoenix_home();
            let workspace = phoenix_workspace_root();

            assert_eq!(workspace, home.join("workspace"));
            ensure_phoenix_home().unwrap();
            assert!(workspace.is_dir());

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(workspace).unwrap().permissions().mode() & 0o777,
                    0o700
                );
            }
        });
    }

    #[test]
    fn prompt_seed_refresh_support_is_exact_and_custom_overlays_survive() {
        with_temp_home(|| {
            let home = ensure_phoenix_home().unwrap();
            let planner = home.join("prompts/planner_system.md");
            crate::config::private_io::atomic_write_private(&planner, b"my custom planner")
                .unwrap();
            seed_prompt_overlays(&home).unwrap();
            assert_eq!(fs::read_to_string(planner).unwrap(), "my custom planner");

            archive_seeded_prompt(&home, "old_system.md", b"old shipped prompt").unwrap();
            let digest = format!("{:x}", Sha256::digest(b"old shipped prompt"));
            assert_eq!(
                fs::read(
                    home.join("archive/prompt-overlays")
                        .join(&digest[..16])
                        .join("old_system.md")
                )
                .unwrap(),
                b"old shipped prompt"
            );
            assert!(HISTORICAL_PROMPT_SEEDS
                .iter()
                .all(|(_, digest)| digest.len() == 64
                    && digest.chars().all(|c| c.is_ascii_hexdigit())));
        });
    }

    #[cfg(unix)]
    #[test]
    fn existing_shared_custom_home_is_rejected_without_chmod() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let home = dir.path().join("shared-root");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o755)).unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set(&home);

        let result = ensure_phoenix_home();
        assert!(result.is_err());

        assert_eq!(
            fs::metadata(&home).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(!home.join("sessions").exists());
    }

    #[test]
    fn migrate_legacy_dot_phoenix_and_memory() {
        with_temp_home(|| {
            let workspace = tempdir().unwrap();
            let legacy = workspace.path().join(".phoenix");
            fs::create_dir_all(legacy.join("sessions")).unwrap();
            fs::create_dir_all(legacy.join("runs").join("run_old")).unwrap();
            fs::write(
                legacy.join("sessions").join("main.json"),
                r#"{"id":"main"}"#,
            )
            .unwrap();
            fs::write(legacy.join("runs").join("run_old").join("trace.json"), "{}").unwrap();

            let memory = workspace
                .path()
                .join("memory")
                .join("WARM")
                .join("projects");
            fs::create_dir_all(&memory).unwrap();
            fs::write(memory.join("prefs.md"), "name: Mik").unwrap();

            let report = migrate_legacy_state(workspace.path()).unwrap();
            assert!(report.files_copied >= 3);

            let home = phoenix_home();
            assert!(home.join("sessions").join("main.json").exists());
            assert!(home
                .join("runs")
                .join("run_old")
                .join("trace.json")
                .exists());
            assert!(home
                .join("memory")
                .join("WARM")
                .join("projects")
                .join("prefs.md")
                .exists());
        });
    }

    #[test]
    fn migrate_does_not_overwrite_existing_home_files() {
        with_temp_home(|| {
            ensure_phoenix_home().unwrap();
            fs::write(
                phoenix_home().join("sessions").join("main.json"),
                "existing",
            )
            .unwrap();

            let workspace = tempdir().unwrap();
            let legacy = workspace.path().join(".phoenix").join("sessions");
            fs::create_dir_all(&legacy).unwrap();
            fs::write(legacy.join("main.json"), "legacy").unwrap();

            migrate_legacy_state(workspace.path()).unwrap();
            let contents =
                fs::read_to_string(phoenix_home().join("sessions").join("main.json")).unwrap();
            assert_eq!(contents, "existing");
        });
    }

    #[cfg(unix)]
    #[test]
    fn migration_skips_symlink_loops_and_outside_targets() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        with_temp_home(|| {
            let workspace = tempdir().unwrap();
            let sessions = workspace.path().join(".phoenix/sessions");
            fs::create_dir_all(&sessions).unwrap();
            let outside = tempdir().unwrap();
            fs::write(outside.path().join("secret.json"), "outside").unwrap();
            symlink("..", sessions.join("loop")).unwrap();
            symlink(outside.path(), sessions.join("outside")).unwrap();
            fs::write(sessions.join("safe.json"), "safe").unwrap();

            let report = migrate_legacy_state(workspace.path()).unwrap();
            assert_eq!(report.files_copied, 1);
            let destination = phoenix_sessions_root();
            assert_eq!(
                fs::read_to_string(destination.join("safe.json")).unwrap(),
                "safe"
            );
            assert!(!destination.join("loop").exists());
            assert!(!destination.join("outside").exists());
            assert_eq!(
                fs::metadata(destination.join("safe.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn migration_rejects_a_symlinked_workspace_root() {
        use std::os::unix::fs::symlink;

        with_temp_home(|| {
            let container = tempdir().unwrap();
            let actual = container.path().join("actual");
            let linked = container.path().join("linked");
            fs::create_dir_all(actual.join(".phoenix/sessions")).unwrap();
            fs::write(actual.join(".phoenix/sessions/private.json"), "secret").unwrap();
            symlink(&actual, &linked).unwrap();

            assert!(migrate_legacy_state(&linked).is_err());
            assert!(!phoenix_sessions_root().join("private.json").exists());
        });
    }

    #[cfg(unix)]
    #[test]
    fn migration_skips_fifo_sources_and_rejects_fifo_destinations_promptly() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::FileTypeExt;

        with_temp_home(|| {
            let workspace = tempdir().unwrap();
            let sessions = workspace.path().join(".phoenix/sessions");
            fs::create_dir_all(&sessions).unwrap();
            let source_fifo = sessions.join("ignored.fifo");
            let raw = CString::new(source_fifo.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(raw.as_ptr(), 0o600) }, 0);

            let started = std::time::Instant::now();
            let report = migrate_legacy_state(workspace.path()).unwrap();
            assert!(started.elapsed() < std::time::Duration::from_secs(1));
            assert_eq!(report.files_copied, 0);

            fs::write(sessions.join("blocked.json"), "legacy").unwrap();
            crate::config::private_io::prepare_phoenix_directory(&phoenix_sessions_root()).unwrap();
            let destination_fifo = phoenix_sessions_root().join("blocked.json");
            let raw = CString::new(destination_fifo.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(raw.as_ptr(), 0o600) }, 0);

            let started = std::time::Instant::now();
            assert!(migrate_legacy_state(workspace.path()).is_err());
            assert!(started.elapsed() < std::time::Duration::from_secs(1));
            assert!(fs::symlink_metadata(destination_fifo)
                .unwrap()
                .file_type()
                .is_fifo());
        });
    }

    #[test]
    fn oversized_legacy_file_fails_without_partial_destination() {
        with_temp_home(|| {
            let workspace = tempdir().unwrap();
            let sessions = workspace.path().join(".phoenix/sessions");
            fs::create_dir_all(&sessions).unwrap();
            let huge = sessions.join("huge.json");
            fs::File::create(&huge)
                .unwrap()
                .set_len(LEGACY_MIGRATION_MAX_FILE_BYTES + 1)
                .unwrap();

            assert!(migrate_legacy_state(workspace.path()).is_err());
            assert!(!phoenix_sessions_root().join("huge.json").exists());
        });
    }
}
