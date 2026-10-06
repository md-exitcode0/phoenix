//! Durable writes for Phoenix-owned configuration and credential files.
//!
//! These files are mutated by the CLI, Canvas, and the gateway. A shared
//! `*.tmp` name lets two processes truncate each other's staging file, while a
//! plain write exposes partial TOML/JSON to concurrent readers. Use a unique
//! owner-only staging file, serialize replacements with an advisory lock, and
//! rename only after the bytes are durable.

use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
/// A Phoenix control/state document must never be large enough to exhaust the
/// process while a cross-process lock is held. Large payloads belong in their
/// own bounded stores, not config/auth/receipt JSON or TOML files.
const PRIVATE_FILE_MAX_BYTES: u64 = 64 * 1024 * 1024;

fn validate_private_write_size(path: &Path, contents: &[u8]) -> Result<()> {
    let size = u64::try_from(contents.len()).unwrap_or(u64::MAX);
    if size > PRIVATE_FILE_MAX_BYTES {
        anyhow::bail!(
            "refusing to write {} bytes to {} (max {PRIVATE_FILE_MAX_BYTES})",
            contents.len(),
            path.display()
        );
    }
    Ok(())
}

fn known_default_home() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".phoenix"))
}

fn is_known_private_top_level_file(name: &str) -> bool {
    name == "history"
        || name == "last_session"
        || name.starts_with('.')
        || name.ends_with(".toml")
        || name.contains(".toml.bak")
        || name.ends_with(".json")
        || name.contains(".json.bak")
        || name.ends_with(".jsonl")
        || name.contains(".jsonl.")
        || name.ends_with(".md")
        || name.ends_with(".sqlite")
        || name.ends_with(".out")
        || name.ends_with(".key")
        || name.ends_with(".token")
        || name.contains(".log")
}

/// Repair known sensitive files left by pre-hardening Phoenix versions. This
/// is deliberately shallow and only runs for the canonical `~/.phoenix`:
/// recursively chmodding a custom root could alter a workspace, while walking
/// skills/browser profiles could remove required executable bits.
pub(crate) fn repair_known_default_top_level_state(home: &Path) -> Result<()> {
    if known_default_home().as_deref() != Some(home) {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let entries = std::fs::read_dir(home).with_context(|| {
            format!("failed to inspect default Phoenix home {}", home.display())
        })?;
        for (index, entry) in entries.enumerate() {
            if index >= 4_096 {
                anyhow::bail!(
                    "default Phoenix home {} has too many top-level entries",
                    home.display()
                );
            }
            let entry = entry
                .with_context(|| format!("failed to inspect an entry below {}", home.display()))?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !is_known_private_top_level_file(&name) {
                continue;
            }
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)
                .with_context(|| format!("failed to inspect sensitive state {}", path.display()))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                anyhow::bail!("refusing unsafe sensitive state target {}", path.display());
            }
            if metadata.uid() != unsafe { libc::geteuid() } || metadata.nlink() != 1 {
                anyhow::bail!(
                    "refusing sensitive state {} with unsafe owner/link count",
                    path.display()
                );
            }
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .with_context(|| format!("failed to secure sensitive state {}", path.display()))?;
        }
    }
    Ok(())
}

fn validate_path_shape(path: &Path, label: &str) -> Result<()> {
    if path.as_os_str().is_empty() {
        anyhow::bail!("{label} cannot be empty");
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        anyhow::bail!("{label} must not contain '.' or '..': {}", path.display());
    }
    Ok(())
}

pub(crate) fn reject_symlink_components(path: &Path) -> Result<()> {
    validate_path_shape(path, "private path")?;
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                anyhow::bail!(
                    "refusing to follow symlink component in private path: {}",
                    current.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {}", current.display()));
            }
        }
    }
    Ok(())
}

fn custom_home_is_dangerously_broad(home: &Path) -> Result<bool> {
    if !home.is_absolute() {
        return Ok(true);
    }
    if home.parent().is_none() || home == Path::new("/") {
        return Ok(true);
    }
    if home == std::env::temp_dir() {
        return Ok(true);
    }
    if let Some(base) = directories::BaseDirs::new() {
        if home == base.home_dir() {
            return Ok(true);
        }
    }
    if let Ok(current) = std::env::current_dir() {
        if home == current {
            return Ok(true);
        }
    }
    Ok(false)
}

fn validate_phoenix_home_for_mutation(home: &Path) -> Result<bool> {
    validate_path_shape(home, "PHOENIX_HOME")?;
    let known_default = known_default_home().is_some_and(|default| home == default);
    if !known_default && custom_home_is_dangerously_broad(home)? {
        anyhow::bail!(
            "refusing to mutate dangerously broad PHOENIX_HOME '{}'; choose a dedicated absolute directory",
            home.display()
        );
    }
    reject_symlink_components(home)?;
    if let Ok(metadata) = std::fs::symlink_metadata(home) {
        if !metadata.is_dir() {
            anyhow::bail!("PHOENIX_HOME is not a directory: {}", home.display());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};

            if metadata.uid() != unsafe { libc::geteuid() } {
                anyhow::bail!(
                    "refusing PHOENIX_HOME not owned by the current user: {}",
                    home.display()
                );
            }
            if !known_default && metadata.permissions().mode() & 0o077 != 0 {
                anyhow::bail!(
                    "custom PHOENIX_HOME must already be owner-only (0700): {}",
                    home.display()
                );
            }
        }
    }
    Ok(known_default)
}

/// Create a directory chain one component at a time. Newly-created
/// components are private from their first inode; existing directories are
/// never chmodded unless they belong to the known default `~/.phoenix` tree.
fn prepare_directory_chain(path: &Path, repair_existing_from: Option<&Path>) -> Result<()> {
    validate_path_shape(path, "private directory")?;
    reject_symlink_components(path)?;

    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if matches!(component, Component::RootDir | Component::Prefix(_)) {
            continue;
        }

        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    anyhow::bail!(
                        "refusing to follow symlink component in private directory: {}",
                        current.display()
                    );
                }
                if !metadata.is_dir() {
                    anyhow::bail!("{} is not a directory", current.display());
                }
                if repair_existing_from.is_some_and(|root| current.starts_with(root)) {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        std::fs::set_permissions(&current, std::fs::Permissions::from_mode(0o700))
                            .with_context(|| format!("failed to secure {}", current.display()))?;
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                #[cfg(unix)]
                let create_result = {
                    use std::os::unix::fs::DirBuilderExt;
                    let mut builder = std::fs::DirBuilder::new();
                    builder.mode(0o700);
                    builder.create(&current)
                };
                #[cfg(not(unix))]
                let create_result = std::fs::create_dir(&current);

                if let Err(error) = create_result {
                    if error.kind() != std::io::ErrorKind::AlreadyExists {
                        return Err(error)
                            .with_context(|| format!("failed to create {}", current.display()));
                    }
                    // Another Phoenix process may have won the mkdir race.
                    // Re-validate the inode instead of accepting a symlink or
                    // permissive attacker-created directory at this point.
                    let metadata = std::fs::symlink_metadata(&current)
                        .with_context(|| format!("failed to inspect {}", current.display()))?;
                    if metadata.file_type().is_symlink() || !metadata.is_dir() {
                        anyhow::bail!(
                            "unsafe directory appeared while creating {}",
                            current.display()
                        );
                    }
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::{MetadataExt, PermissionsExt};
                        if metadata.uid() != unsafe { libc::geteuid() }
                            || metadata.permissions().mode() & 0o077 != 0
                        {
                            anyhow::bail!(
                                "non-private directory appeared while creating {}",
                                current.display()
                            );
                        }
                    }
                }
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {}", current.display()));
            }
        }
    }
    Ok(())
}

/// Prepare the selected Phoenix root itself. The default `~/.phoenix` is
/// known to be application-owned and may be repaired to 0700. An existing
/// custom root must already be same-owner and private; its mode is never
/// changed as a side effect.
pub(crate) fn prepare_phoenix_home(home: &Path) -> Result<()> {
    let known_default = validate_phoenix_home_for_mutation(home)?;
    prepare_directory_chain(home, known_default.then_some(home))
}

/// Prepare a known Phoenix-owned directory below the selected home.
pub(crate) fn prepare_phoenix_directory(path: &Path) -> Result<()> {
    let home = crate::config::phoenix_home();
    let known_default = validate_phoenix_home_for_mutation(&home)?;
    validate_path_shape(path, "Phoenix directory")?;
    if !path.starts_with(&home) {
        anyhow::bail!(
            "refusing to create Phoenix directory outside PHOENIX_HOME: {}",
            path.display()
        );
    }
    prepare_directory_chain(path, known_default.then_some(home.as_path()))
}

/// Create the target's parent without following symlinks. For a target under
/// PHOENIX_HOME, this also validates that the selected root is not dangerously
/// broad. Existing custom roots must already be owner-only and retain their
/// caller-chosen mode.
pub(crate) fn prepare_private_parent_for(path: &Path, home: &Path) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    validate_path_shape(path, "private file")?;
    let known_default = validate_phoenix_home_for_mutation(home)?;
    if !path.starts_with(home) {
        anyhow::bail!(
            "refusing to prepare private file outside selected PHOENIX_HOME: {}",
            path.display()
        );
    }
    prepare_directory_chain(parent, known_default.then_some(home))
}

pub(crate) fn prepare_private_parent(path: &Path) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    validate_path_shape(path, "private file")?;

    let home = crate::config::phoenix_home();
    validate_path_shape(&home, "PHOENIX_HOME")?;
    if path.starts_with(&home) {
        prepare_private_parent_for(path, &home)
    } else {
        // Private I/O also serves explicit config paths in tests and tooling.
        // Secure only newly-created directories; never chmod caller-owned
        // existing directories outside Phoenix's selected state tree.
        prepare_directory_chain(parent, None)
    }
}

fn open_private(path: &Path, create_new: bool) -> Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true);
    if create_new {
        options.create_new(true);
    } else {
        options.create(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .with_context(|| format!("failed to open {} for private writing", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if !metadata.is_file() {
        anyhow::bail!("{} is not a regular file", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.nlink() != 1 {
            anyhow::bail!(
                "refusing unsafe private write target {} (owner/link count mismatch)",
                path.display()
            );
        }
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to secure {}", path.display()))?;
    }
    Ok(file)
}

fn read_nofollow_limited(path: &Path, max_bytes: u64) -> Result<Option<Vec<u8>>> {
    let Some(mut file) = open_private_read_stream(path)? else { return Ok(None); };
    let metadata = file.metadata()?;
    if metadata.len() > max_bytes {
        anyhow::bail!("{} is too large ({} bytes; max {max_bytes})", path.display(), metadata.len());
    }
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file).take(max_bytes.saturating_add(1)).read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {}", path.display()))?;
    if bytes.len() as u64 > max_bytes {
        anyhow::bail!("{} grew beyond the {max_bytes}-byte private-file limit while reading", path.display());
    }
    Ok(Some(bytes))
}

/// Open an existing regular private-state file without following symlinks or
/// blocking on a FIFO. Streaming callers must bound individual allocations.
pub(crate) fn open_private_read_stream(path: &Path) -> Result<Option<std::fs::File>> {
    reject_symlink_components(path)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // O_NONBLOCK is essential for a hostile/stale FIFO at a state path:
        // opening it must fail promptly instead of wedging every config RMW.
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if !metadata.is_file() {
        anyhow::bail!("{} is not a regular file", path.display());
    }
    Ok(Some(file))
}

fn read_nofollow(path: &Path) -> Result<Option<Vec<u8>>> {
    read_nofollow_limited(path, PRIVATE_FILE_MAX_BYTES)
}

fn regular_file_exists_nofollow(path: &Path) -> Result<bool> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    };
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if !metadata.is_file() {
        anyhow::bail!("{} is not a regular file", path.display());
    }
    Ok(true)
}

pub(crate) fn read_private_file(path: &Path) -> Result<Option<Vec<u8>>> {
    reject_symlink_components(path)?;
    read_nofollow(path)
}

/// Read a Phoenix-owned private file with a caller-specific ceiling. The
/// global 64 MiB safety net is intentionally generous for sessions; tiny
/// config/key documents should not have to allocate anywhere near it.
pub(crate) fn read_private_file_limited(path: &Path, max_bytes: usize) -> Result<Option<Vec<u8>>> {
    if max_bytes == 0 || max_bytes as u64 > PRIVATE_FILE_MAX_BYTES {
        anyhow::bail!(
            "invalid private-file read limit {max_bytes}; expected 1..={PRIVATE_FILE_MAX_BYTES}"
        );
    }
    reject_symlink_components(path)?;
    read_nofollow_limited(path, max_bytes as u64)
}

pub(crate) fn write_private_file(path: &Path, contents: &[u8]) -> Result<()> {
    // Reject before opening/truncating so an oversized replacement preserves
    // the last readable state exactly.
    validate_private_write_size(path, contents)?;
    prepare_private_parent(path)?;
    let mut file = open_private(path, false)?;
    file.set_len(0)
        .with_context(|| format!("failed to truncate {}", path.display()))?;
    file.write_all(contents)
        .with_context(|| format!("failed to write {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync {}", path.display()))
}

struct TargetLock(std::fs::File);

/// An execution claim, released by the OS even if its process is killed.
/// Keep its lock inode in place: unlinking it could admit two concurrent owners.
pub(crate) struct PrivateExecutionClaim {
    _file: std::fs::File,
}

pub(crate) fn try_execution_claim(path: &Path) -> Result<Option<PrivateExecutionClaim>> {
    prepare_private_parent(path)?;
    let file = open_private(path, false)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(PrivateExecutionClaim { _file: file })),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => Err(error).context("execution claim lock failed"),
    }
}

impl TargetLock {
    fn acquire(target: &Path) -> Result<Self> {
        let parent = target.parent().unwrap_or_else(|| Path::new("."));
        prepare_private_parent(target)?;
        let name = target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("phoenix-state");
        let lock_path = parent.join(format!(".{name}.lock"));
        let file = open_private(&lock_path, false)?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == -1 {
                return Err(std::io::Error::last_os_error())
                    .with_context(|| format!("failed to lock {}", lock_path.display()));
            }
        }
        Ok(Self(file))
    }
}

impl Drop for TargetLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

struct TempPath(PathBuf);

impl Drop for TempPath {
    fn drop(&mut self) {
        if !self.0.as_os_str().is_empty() {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

fn create_unique_temp(target: &Path) -> Result<(TempPath, std::fs::File)> {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("phoenix-state");
    for _ in 0..128 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(".{name}.tmp.{}.{}", std::process::id(), sequence));
        match open_private(&path, true) {
            Ok(file) => return Ok((TempPath(path), file)),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|io| io.kind() == std::io::ErrorKind::AlreadyExists) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        }
    }
    anyhow::bail!(
        "could not allocate a unique staging file beside {}",
        target.display()
    )
}

fn atomic_write_locked(path: &Path, contents: &[u8]) -> Result<()> {
    validate_private_write_size(path, contents)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let (mut temp, mut file) = create_unique_temp(path)?;
    file.write_all(contents)
        .with_context(|| format!("failed to stage {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync staged {}", path.display()))?;
    drop(file);
    #[cfg(test)]
    if std::env::var_os("PHOENIX_TEST_ATOMIC_PAUSE_TARGET").as_deref() == Some(path.as_os_str()) {
        // Subprocess-only crash probe. No hook or environment check is
        // compiled into production; the parent kills this exact child.
        let marker = path.with_extension("publication-ready");
        std::fs::write(&marker, b"staged and synced")?;
        let started = std::time::Instant::now();
        while started.elapsed() < std::time::Duration::from_secs(15) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        anyhow::bail!("test publication pause expired without process termination");
    }
    std::fs::rename(&temp.0, path)
        .with_context(|| format!("failed to atomically replace {}", path.display()))?;
    temp.0 = PathBuf::new();
    // `create_unique_temp` opens the staging inode through `open_private`,
    // which verifies same-owner/single-link regular-file semantics and applies
    // mode 0600 before returning it. `sync_all` above makes both those metadata
    // and the contents durable before publication. Do not perform a fallible
    // chmod after `rename`: once the new inode is visible, returning `Err` would
    // falsely tell transaction callers that the old file was still on disk.
    if let Ok(directory) = std::fs::File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

/// Publish a fully-synced staging inode without ever replacing `path`.
///
/// A check followed by `rename` is not a create-if-missing operation: a
/// non-cooperating process can create a FIFO, symlink, or regular file after
/// the check and have it silently replaced. A hard link is an atomic
/// no-clobber publication because the kernel returns `AlreadyExists` whenever
/// the destination name is occupied. The staging inode is in the same
/// directory, so cross-filesystem links are impossible.
fn atomic_create_locked(path: &Path, contents: &[u8]) -> Result<bool> {
    validate_private_write_size(path, contents)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let (temp, mut file) = create_unique_temp(path)?;
    file.write_all(contents)
        .with_context(|| format!("failed to stage {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync staged {}", path.display()))?;
    drop(file);

    // A destination can disappear between an `AlreadyExists` result and the
    // safety check below. Retry the same already-synced inode a bounded number
    // of times rather than misreporting that transient race as preservation.
    for _ in 0..128 {
        match std::fs::hard_link(&temp.0, path) {
            Ok(()) => {
                if let Ok(directory) = std::fs::File::open(parent) {
                    let _ = directory.sync_all();
                }
                return Ok(true);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                // Preserve only an existing regular file. A symlink, FIFO, or
                // other unsafe inode is an error, just as it is when present
                // before this function starts.
                if regular_file_exists_nofollow(path)? {
                    return Ok(false);
                }
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to atomically create {}", path.display()));
            }
        }
        std::thread::yield_now();
    }
    anyhow::bail!(
        "destination {} changed repeatedly while creating it",
        path.display()
    )
}

pub(crate) fn atomic_write_private(path: &Path, contents: &[u8]) -> Result<()> {
    prepare_private_parent(path)?;
    let _lock = TargetLock::acquire(path)?;
    // An atomic rename would replace (not follow) an existing symlink/FIFO,
    // but silently consuming an unsafe inode is still the wrong contract for
    // Phoenix state: callers must learn that the target was tampered with or
    // corrupted. Existing regular files remain replaceable so permissions and
    // contents can be repaired transactionally.
    let _existing_regular = regular_file_exists_nofollow(path)?;
    atomic_write_locked(path, contents)
}

/// Replace a private file while the caller already holds this target's lock
/// through [`with_private_lock`]. This exists for read/validate/write
/// transactions such as typed Settings updates; using `atomic_write_private`
/// from inside that transaction would try to acquire the same `flock` again.
pub(crate) fn atomic_write_private_under_lock(path: &Path, contents: &[u8]) -> Result<()> {
    validate_private_write_size(path, contents)?;
    prepare_private_parent(path)?;
    let _existing_regular = regular_file_exists_nofollow(path)?;
    atomic_write_locked(path, contents)
}

/// Atomically create a private file only when no target exists. The existence
/// check and rename share the target's cross-process lock, which preserves the
/// "copy missing legacy state, never overwrite current state" contract.
pub(crate) fn atomic_write_private_if_missing(path: &Path, contents: &[u8]) -> Result<bool> {
    prepare_private_parent(path)?;
    let _lock = TargetLock::acquire(path)?;
    // Existence, not content, is the contract. Do not allocate/read a large
    // pre-existing destination merely to preserve it during migration.
    if regular_file_exists_nofollow(path)? {
        return Ok(false);
    }
    atomic_create_locked(path, contents)
}

/// Hold a stable cross-process lock derived from `target` for the duration of
/// `action`. The target itself need not exist. Callers use this when the
/// protected operation includes work outside a single file replacement (for
/// example, OAuth network refresh followed by a credential-store update).
pub(crate) fn with_private_lock<T>(target: &Path, action: impl FnOnce() -> Result<T>) -> Result<T> {
    prepare_private_parent(target)?;
    let _lock = TargetLock::acquire(target)?;
    action()
}

/// Remove a private regular file while holding the same target lock used by
/// writers. Missing files are a successful no-op; symlinks, FIFOs, sockets,
/// and directories are rejected without being followed.
pub(crate) fn remove_private_file(path: &Path) -> Result<bool> {
    prepare_private_parent(path)?;
    let _lock = TargetLock::acquire(path)?;
    remove_private_file_under_lock(path)
}

/// Remove a private regular file while the caller already holds this target's
/// lock through [`with_private_lock`].
pub(crate) fn remove_private_file_under_lock(path: &Path) -> Result<bool> {
    prepare_private_parent(path)?;
    if !regular_file_exists_nofollow(path)? {
        return Ok(false);
    }
    match std::fs::remove_file(path) {
        Ok(()) => {
            if let Some(parent) = path.parent() {
                if let Ok(directory) = std::fs::File::open(parent) {
                    let _ = directory.sync_all();
                }
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => {
            Err(error).with_context(|| format!("failed to remove private file {}", path.display()))
        }
    }
}

/// Lock-scoped read/modify/write. The callback always sees the bytes read
/// after the target lock was acquired, and its replacement is renamed before
/// the lock is released. This is the primitive for shared JSON stores.
pub(crate) fn read_modify_write_private<T>(
    path: &Path,
    update: impl FnOnce(Option<&[u8]>) -> Result<(T, Vec<u8>)>,
) -> Result<T> {
    prepare_private_parent(path)?;
    let _lock = TargetLock::acquire(path)?;
    let current = read_nofollow(path)?;
    let (result, replacement) = update(current.as_deref())?;
    validate_private_write_size(path, &replacement)?;
    atomic_write_locked(path, &replacement)?;
    Ok(result)
}

/// Compare-and-swap under the same target lock used by all Phoenix private
/// writers. `None` means the caller expects the file not to exist.
pub(crate) fn compare_and_swap_private(
    path: &Path,
    expected: Option<&[u8]>,
    replacement: &[u8],
) -> Result<()> {
    read_modify_write_private(path, |current| {
        if current != expected {
            anyhow::bail!("{} changed in another process; retry", path.display());
        }
        Ok(((), replacement.to_vec()))
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    #[test]
    fn default_top_level_repair_matcher_covers_legacy_sensitive_state() {
        for name in [
            "config.toml.bak-2026-08-04",
            "auth-profiles.json.bak",
            "canvas-prefs.json.bak-perf",
            "goals.json.bak-zombie-close",
            "receipts.jsonl.pre-scrub",
            "codegraph.sqlite",
            "gateway-restart.out",
            "gateway-console.log",
            ".legacy-state-migrated",
        ] {
            assert!(is_known_private_top_level_file(name), "{name}");
        }
        assert!(!is_known_private_top_level_file("some-directory"));
    }

    #[test]
    fn concurrent_private_replacements_are_complete_and_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();

        std::thread::scope(|scope| {
            for value in ["alpha", "bravo", "charlie", "delta"] {
                let path = path.clone();
                scope.spawn(move || atomic_write_private(&path, value.as_bytes()).unwrap());
            }
        });

        let value = std::fs::read_to_string(&path).unwrap();
        assert!(["alpha", "bravo", "charlie", "delta"].contains(&value.as_str()));
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let staging_files = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp."))
            .count();
        assert_eq!(staging_files, 0);
    }

    #[test]
    fn existing_private_custom_root_retains_mode_while_new_children_are_private() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("shared");
        std::fs::create_dir(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();

        prepare_phoenix_home(&home).unwrap();
        let child = home.join("credentials");
        prepare_directory_chain(&child, None).unwrap();

        assert_eq!(
            std::fs::metadata(&home).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&child).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn existing_shared_custom_root_is_rejected_without_chmod() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("shared");
        std::fs::create_dir(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(prepare_phoenix_home(&home).is_err());
        assert_eq!(
            std::fs::metadata(&home).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0);
    }

    #[test]
    fn symlinked_home_is_rejected_without_touching_external_directory() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let external = dir.path().join("external");
        let linked = dir.path().join("linked-home");
        std::fs::create_dir(&external).unwrap();
        symlink(&external, &linked).unwrap();

        assert!(prepare_phoenix_home(&linked).is_err());
        assert_eq!(std::fs::read_dir(&external).unwrap().count(), 0);
    }

    #[test]
    fn dangerously_broad_custom_roots_are_rejected() {
        assert!(validate_phoenix_home_for_mutation(Path::new("/")).is_err());
        assert!(validate_phoenix_home_for_mutation(&std::env::temp_dir()).is_err());
    }

    #[test]
    fn compare_and_swap_rejects_a_stale_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        atomic_write_private(&path, b"one").unwrap();
        compare_and_swap_private(&path, Some(b"one"), b"two").unwrap();
        assert!(compare_and_swap_private(&path, Some(b"one"), b"stale").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
    }

    #[test]
    fn create_if_missing_never_replaces_an_existing_target() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        assert!(atomic_write_private_if_missing(&path, b"first").unwrap());
        assert!(!atomic_write_private_if_missing(&path, b"second").unwrap());
        assert_eq!(std::fs::read(path).unwrap(), b"first");
    }

    #[test]
    fn create_if_missing_preserves_an_oversized_existing_target_without_reading_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large-state.json");
        std::fs::File::create(&path)
            .unwrap()
            .set_len(PRIVATE_FILE_MAX_BYTES + 1)
            .unwrap();

        assert!(!atomic_write_private_if_missing(&path, b"replacement").unwrap());
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            PRIVATE_FILE_MAX_BYTES + 1
        );
    }

    #[test]
    fn create_if_missing_rejects_unsafe_existing_targets_without_touching_them() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let external = dir.path().join("external");
        let linked = dir.path().join("linked-state");
        std::fs::write(&external, b"outside").unwrap();
        symlink(&external, &linked).unwrap();
        assert!(atomic_write_private_if_missing(&linked, b"replacement").is_err());
        assert_eq!(std::fs::read(&external).unwrap(), b"outside");
        assert!(std::fs::symlink_metadata(&linked)
            .unwrap()
            .file_type()
            .is_symlink());

        let fifo = dir.path().join("state.fifo");
        let raw = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(raw.as_ptr(), 0o600) }, 0);
        assert!(atomic_write_private_if_missing(&fifo, b"replacement").is_err());
        assert!(std::fs::symlink_metadata(&fifo)
            .unwrap()
            .file_type()
            .is_fifo());
    }

    #[test]
    fn atomic_replacement_rejects_a_symlink_without_touching_either_inode() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let external = dir.path().join("external");
        let linked = dir.path().join("linked-state");
        std::fs::write(&external, b"outside").unwrap();
        symlink(&external, &linked).unwrap();

        assert!(atomic_write_private(&linked, b"replacement").is_err());
        assert_eq!(std::fs::read(&external).unwrap(), b"outside");
        assert!(std::fs::symlink_metadata(&linked)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn private_read_rejects_fifo_without_waiting_for_a_writer() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.fifo");
        let raw = CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(raw.as_ptr(), 0o600) }, 0);

        let started = std::time::Instant::now();
        let error = read_private_file(&path).unwrap_err().to_string();
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(error.contains("not a regular file"), "{error}");
    }

    #[test]
    fn private_rmw_rejects_a_fifo_lock_without_waiting_for_a_reader() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let lock_path = dir.path().join(".config.json.lock");
        let raw = CString::new(lock_path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(raw.as_ptr(), 0o600) }, 0);

        let started = std::time::Instant::now();
        assert!(read_modify_write_private(&path, |_| Ok(((), b"new".to_vec()))).is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(!path.exists());
    }

    #[test]
    fn caller_specific_read_limit_rejects_before_allocating_the_global_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small-config.toml");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(4_097).unwrap();
        drop(file);

        let error = read_private_file_limited(&path, 4_096)
            .unwrap_err()
            .to_string();
        assert!(error.contains("max 4096"), "{error}");
        assert!(read_private_file_limited(&path, 0).is_err());
    }

    #[test]
    fn oversized_private_target_is_rejected_and_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(PRIVATE_FILE_MAX_BYTES + 1).unwrap();
        drop(file);

        assert!(
            read_modify_write_private(&path, |_| { Ok(((), b"replacement".to_vec())) }).is_err()
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            PRIVATE_FILE_MAX_BYTES + 1
        );
    }

    #[test]
    fn every_private_writer_rejects_oversized_output_before_publication() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        atomic_write_private(&path, b"original").unwrap();

        let replacement = vec![b'x'; (PRIVATE_FILE_MAX_BYTES + 1) as usize];
        assert!(write_private_file(&path, &replacement).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"original");

        assert!(atomic_write_private(&path, &replacement).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"original");

        let missing = dir.path().join("missing.json");
        assert!(atomic_write_private_if_missing(&missing, &replacement).is_err());
        assert!(!missing.exists());

        assert!(read_modify_write_private(&path, |_| Ok(((), replacement))).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
    }

    #[test]
    fn private_remove_is_idempotent_and_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("marker");
        atomic_write_private(&path, b"pending").unwrap();
        assert!(remove_private_file(&path).unwrap());
        assert!(!remove_private_file(&path).unwrap());

        atomic_write_private(&path, b"pending-again").unwrap();
        assert!(with_private_lock(&path, || remove_private_file_under_lock(&path)).unwrap());
        assert!(!path.exists());

        let external = dir.path().join("external");
        std::fs::write(&external, b"outside").unwrap();
        symlink(&external, &path).unwrap();
        assert!(remove_private_file(&path).is_err());
        assert_eq!(std::fs::read(external).unwrap(), b"outside");
    }
}
