//! Bounded, race-aware workspace file I/O shared by the editing tools.
//!
//! `std::fs::read`/`write` follow final symlinks, may block on a FIFO, allocate
//! directly from attacker-controlled sizes, and expose a truncate/write
//! window. This module keeps those properties out of every caller.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use super::resolve_workspace_path;

pub(crate) const MAX_TOOL_PATH_BYTES: usize = 4 * 1024;
pub(crate) const MAX_WORKSPACE_TEXT_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_PUBLISH_ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DestinationIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    len: u64,
    #[cfg(unix)]
    modified_seconds: i64,
    #[cfg(unix)]
    modified_nanoseconds: i64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
}

fn metadata_identity(metadata: &fs::Metadata) -> DestinationIdentity {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        DestinationIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
            len: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
    #[cfg(not(unix))]
    {
        DestinationIdentity {
            len: metadata.len(),
        }
    }
}

pub(crate) struct Utf8Snapshot {
    pub(crate) text: String,
    pub(crate) sha256: String,
    pub(crate) bytes: u64,
}

pub(crate) fn validate_path_input(path: &str) -> Result<()> {
    if path.is_empty() {
        bail!("workspace path cannot be empty");
    }
    if path.len() > MAX_TOOL_PATH_BYTES {
        bail!(
            "workspace path is too long ({} bytes; max {})",
            path.len(),
            MAX_TOOL_PATH_BYTES
        );
    }
    if path.as_bytes().contains(&0) {
        bail!("workspace path contains a NUL byte");
    }
    Ok(())
}

fn requested_path(workspace_root: &Path, input: &str) -> Result<PathBuf> {
    validate_path_input(input)?;
    let root = workspace_root
        .canonicalize()
        .context("failed to canonicalize workspace root")?;
    let raw = Path::new(input);
    Ok(if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        root.join(raw)
    })
}

fn reject_final_symlink(path: &Path, label: &str) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("refusing {label} through symlink: {}", path.display())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {}", path.display())),
    }
}

#[cfg(target_os = "linux")]
fn component_cstring(component: &std::ffi::OsStr) -> Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(component.as_bytes()).context("workspace path component contains NUL")
}

#[cfg(target_os = "linux")]
fn open_directory_path(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    use std::os::unix::fs::OpenOptionsExt;
    options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_DIRECTORY);
    let directory = options
        .open(path)
        .with_context(|| format!("failed to open workspace directory {}", path.display()))?;
    if !directory.metadata()?.is_dir() {
        bail!("workspace parent is not a directory: {}", path.display());
    }
    Ok(directory)
}

#[cfg(target_os = "linux")]
fn open_directory_at(parent: &File, name: &std::ffi::OsStr) -> std::io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = component_cstring(name).map_err(|error| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, error.to_string())
    })?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY
                | libc::O_CLOEXEC
                | libc::O_NOFOLLOW
                | libc::O_NONBLOCK
                | libc::O_DIRECTORY,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// Resolve a canonical workspace-relative directory one component at a time
/// from an already-open root. `openat(O_NOFOLLOW)` makes an interposed parent
/// symlink fail instead of redirecting directory creation or publication.
#[cfg(target_os = "linux")]
fn directory_beneath(
    workspace_root: &Path,
    directory: &Path,
    create_missing: bool,
) -> Result<File> {
    use std::os::fd::AsRawFd;

    let root = workspace_root
        .canonicalize()
        .context("failed to canonicalize workspace root")?;
    let relative = directory.strip_prefix(&root).with_context(|| {
        format!(
            "workspace directory escaped root ({} outside {})",
            directory.display(),
            root.display()
        )
    })?;
    let mut current = open_directory_path(&root)?;
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            if matches!(component, std::path::Component::CurDir) {
                continue;
            }
            bail!(
                "workspace directory contains a non-normal component: {}",
                directory.display()
            );
        };
        match open_directory_at(&current, name) {
            Ok(next) => current = next,
            Err(error) if create_missing && error.kind() == std::io::ErrorKind::NotFound => {
                let name_c = component_cstring(name)?;
                let created = unsafe {
                    libc::mkdirat(current.as_raw_fd(), name_c.as_ptr(), 0o777 as libc::mode_t)
                };
                if created != 0 {
                    let mkdir_error = std::io::Error::last_os_error();
                    if mkdir_error.kind() != std::io::ErrorKind::AlreadyExists {
                        return Err(mkdir_error).with_context(|| {
                            format!("failed to create workspace directory component {name:?}")
                        });
                    }
                }
                current = open_directory_at(&current, name).with_context(|| {
                    format!("failed to open workspace directory component {name:?}")
                })?;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to open workspace directory component {name:?}")
                });
            }
        }
    }
    Ok(current)
}

fn prepare_write_parent(workspace_root: &Path, target: &Path, confined: bool) -> Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("workspace path has no parent: {}", target.display()))?;
    #[cfg(target_os = "linux")]
    if confined {
        let directory = directory_beneath(workspace_root, parent, true)?;
        directory
            .sync_all()
            .with_context(|| format!("failed to sync created parent {}", parent.display()))?;
        return Ok(());
    }
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))
}

pub(crate) fn reject_final_symlink_input(
    workspace_root: &Path,
    input: &str,
    label: &str,
) -> Result<()> {
    let requested = requested_path(workspace_root, input)?;
    reject_final_symlink(&requested, label)
}

fn resolve_existing(workspace_root: &Path, input: &str, confined: bool) -> Result<PathBuf> {
    let requested = requested_path(workspace_root, input)?;
    reject_final_symlink(&requested, "workspace file access")?;
    resolve_workspace_path(workspace_root, input, true, confined)
}

fn resolve_for_write(
    workspace_root: &Path,
    input: &str,
    confined: bool,
    create_parents: bool,
) -> Result<PathBuf> {
    let requested = requested_path(workspace_root, input)?;
    reject_final_symlink(&requested, "workspace write")?;

    // Resolve once before creating anything, then again afterwards. A newly
    // created/interposed parent symlink must not turn a path that passed the
    // first confinement check into a write somewhere else.
    let before = resolve_workspace_path(workspace_root, input, false, confined)?;
    if create_parents {
        prepare_write_parent(workspace_root, &before, confined)?;
    }
    reject_final_symlink(&requested, "workspace write")?;
    let after = resolve_workspace_path(workspace_root, input, false, confined)?;
    if after != before {
        bail!(
            "workspace path changed while preparing its parent ({} -> {}); refusing write",
            before.display(),
            after.display()
        );
    }
    Ok(after)
}

fn open_regular(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // O_NONBLOCK makes opening a FIFO/device prompt; the metadata check
        // below then rejects everything except a regular file.
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect opened file {}", path.display()))?;
    if !metadata.is_file() {
        bail!("workspace path is not a regular file: {}", path.display());
    }
    Ok(file)
}

#[cfg(target_os = "linux")]
fn open_regular_at(parent: &File, name: &std::ffi::OsStr, display: &Path) -> Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = component_cstring(name)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to open {}", display.display()));
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect opened file {}", display.display()))?;
    if !metadata.is_file() {
        bail!(
            "workspace path is not a regular file: {}",
            display.display()
        );
    }
    Ok(file)
}

fn validate_open_file_confinement(
    workspace_root: &Path,
    file: &File,
    confined: bool,
) -> Result<()> {
    if !confined {
        return Ok(());
    }
    let root = workspace_root
        .canonicalize()
        .context("failed to canonicalize workspace root")?;

    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let fd_path = PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()));
        let opened = fd_path
            .canonicalize()
            .context("failed to revalidate opened workspace file")?;
        if !opened.starts_with(&root) {
            bail!(
                "opened file escaped workspace during access: {}",
                opened.display()
            );
        }
    }

    #[cfg(not(target_os = "linux"))]
    let _ = root;
    Ok(())
}

fn read_open_file_bounded(mut file: File, path: &Path, max_bytes: usize) -> Result<Vec<u8>> {
    if max_bytes == 0 {
        bail!("workspace read limit must be greater than zero");
    }
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if metadata.len() > max_bytes as u64 {
        bail!(
            "workspace file is too large: {} bytes (max {}) at {}",
            metadata.len(),
            max_bytes,
            path.display()
        );
    }
    let before = metadata_identity(&metadata);
    let mut bytes = Vec::with_capacity((metadata.len() as usize).min(max_bytes));
    std::io::Read::by_ref(&mut file)
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {}", path.display()))?;
    if bytes.len() > max_bytes {
        bail!(
            "workspace file grew beyond the {max_bytes}-byte limit while reading: {}",
            path.display()
        );
    }
    let after_metadata = file
        .metadata()
        .with_context(|| format!("failed to re-inspect {}", path.display()))?;
    let after = metadata_identity(&after_metadata);
    if before != after || bytes.len() as u64 != after.len {
        bail!(
            "workspace file changed while it was being read: {}",
            path.display()
        );
    }
    Ok(bytes)
}

pub(crate) fn read_regular_bytes(
    workspace_root: &Path,
    input: &str,
    confined: bool,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    let path = resolve_existing(workspace_root, input, confined)?;
    let file = open_regular(&path)?;
    validate_open_file_confinement(workspace_root, &file, confined)?;
    read_open_file_bounded(file, &path, max_bytes)
}

pub(crate) fn read_regular_bytes_at(
    workspace_root: &Path,
    path: &Path,
    confined: bool,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    let root = workspace_root
        .canonicalize()
        .context("failed to canonicalize workspace root")?;
    reject_final_symlink(path, "workspace file access")?;
    let resolved = path
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", path.display()))?;
    if confined && !resolved.starts_with(&root) {
        bail!("path escapes workspace: {}", path.display());
    }
    let file = open_regular(&resolved)?;
    validate_open_file_confinement(&root, &file, confined)?;
    read_open_file_bounded(file, &resolved, max_bytes)
}

pub(crate) fn read_regular_utf8_snapshot(
    workspace_root: &Path,
    input: &str,
    confined: bool,
    max_bytes: usize,
) -> Result<Utf8Snapshot> {
    let bytes = read_regular_bytes(workspace_root, input, confined, max_bytes)?;
    let byte_count = bytes.len() as u64;
    let sha256 = format!("sha256:{:x}", Sha256::digest(&bytes));
    let text = String::from_utf8(bytes)
        .with_context(|| format!("workspace file is not UTF-8: {input}"))?;
    Ok(Utf8Snapshot {
        text,
        sha256,
        bytes: byte_count,
    })
}

struct TargetLock(File);

impl TargetLock {
    fn acquire(target: &Path) -> Result<Self> {
        let lock_root = prepare_lock_root()?;
        let mut hasher = Sha256::new();
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            hasher.update(target.as_os_str().as_bytes());
        }
        #[cfg(not(unix))]
        hasher.update(target.to_string_lossy().as_bytes());
        let lock_path = lock_root.join(format!("{:x}.lock", hasher.finalize()));

        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options
            .open(&lock_path)
            .with_context(|| format!("failed to open workspace lock {}", lock_path.display()))?;
        let metadata = file
            .metadata()
            .with_context(|| format!("failed to inspect workspace lock {}", lock_path.display()))?;
        if !metadata.is_file() {
            bail!(
                "workspace lock is not a regular file: {}",
                lock_path.display()
            );
        }
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            use std::os::unix::fs::MetadataExt;
            if metadata.uid() != unsafe { libc::geteuid() } || metadata.nlink() != 1 {
                bail!(
                    "workspace lock owner/link count is unsafe: {}",
                    lock_path.display()
                );
            }
            let started = std::time::Instant::now();
            loop {
                if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                    break;
                }
                let error = std::io::Error::last_os_error();
                if !error.raw_os_error().is_some_and(|code| {
                    code == libc::EAGAIN || code == libc::EWOULDBLOCK || code == libc::EINTR
                }) {
                    return Err(error)
                        .with_context(|| format!("failed to lock {}", lock_path.display()));
                }
                if started.elapsed() >= std::time::Duration::from_secs(5) {
                    bail!(
                        "timed out after 5s waiting for concurrent workspace edit lock {}",
                        lock_path.display()
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
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

fn prepare_lock_root() -> Result<PathBuf> {
    // Unit tests that did not explicitly isolate PHOENIX_HOME must never
    // mutate the live profile. They still need a process-shared lock root so
    // concurrency tests exercise the same flock path.
    if crate::config::test_isolated_from_live_home() {
        let root = std::env::temp_dir().join(format!(
            "phoenix-workspace-locks-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root)
            .with_context(|| format!("failed to create test lock root {}", root.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        }
        return Ok(root);
    }

    let root = crate::config::phoenix_home().join("workspace-locks");
    crate::config::private_io::prepare_phoenix_directory(&root)?;
    Ok(root)
}

fn destination_identity(path: &Path) -> Result<Option<(DestinationIdentity, fs::Permissions)>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    };
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        bail!(
            "workspace write target is not a regular file: {}",
            path.display()
        );
    }
    let identity = metadata_identity(&metadata);
    Ok(Some((identity, metadata.permissions())))
}

#[cfg(target_os = "linux")]
fn destination_identity_at(
    parent: &File,
    name: &std::ffi::OsStr,
    display: &Path,
) -> Result<Option<(DestinationIdentity, fs::Permissions)>> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::PermissionsExt;

    let name = component_cstring(name)?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let result = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(error).with_context(|| format!("failed to inspect {}", display.display()));
    }
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
        bail!(
            "workspace write target is not a regular file: {}",
            display.display()
        );
    }
    let identity = DestinationIdentity {
        device: stat.st_dev as u64,
        inode: stat.st_ino as u64,
        len: stat.st_size.max(0) as u64,
        modified_seconds: stat.st_mtime as i64,
        modified_nanoseconds: stat.st_mtime_nsec as i64,
        changed_seconds: stat.st_ctime as i64,
        changed_nanoseconds: stat.st_ctime_nsec as i64,
    };
    Ok(Some((
        identity,
        fs::Permissions::from_mode(stat.st_mode as u32),
    )))
}

struct TempPath {
    path: PathBuf,
    armed: bool,
    #[cfg(target_os = "linux")]
    parent: File,
    #[cfg(target_os = "linux")]
    name: std::ffi::OsString,
}

impl TempPath {
    fn disarm(&mut self) {
        self.armed = false;
    }

    fn remove_now(&mut self) -> Result<()> {
        if !self.armed {
            return Ok(());
        }
        #[cfg(target_os = "linux")]
        {
            unlink_child(&self.parent, &self.name, &self.path)?;
        }
        #[cfg(not(target_os = "linux"))]
        {
            fs::remove_file(&self.path)
                .with_context(|| format!("failed to remove staged {}", self.path.display()))?;
        }
        self.disarm();
        Ok(())
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        if self.armed {
            #[cfg(target_os = "linux")]
            let _ = unlink_child(&self.parent, &self.name, &self.path);
            #[cfg(not(target_os = "linux"))]
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn create_staging_file(
    target: &Path,
    #[cfg(target_os = "linux")] parent_directory: &File,
) -> Result<(TempPath, File)> {
    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("workspace target has no parent: {}", target.display()))?;
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    for _ in 0..128 {
        let temp_name = std::ffi::OsString::from(format!(
            ".{name}.phoenix-tmp-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let temp_path = parent.join(&temp_name);
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::{AsRawFd, FromRawFd};
            let temp_name_c = component_cstring(&temp_name)?;
            let fd = unsafe {
                libc::openat(
                    parent_directory.as_raw_fd(),
                    temp_name_c.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_CLOEXEC
                        | libc::O_NOFOLLOW
                        | libc::O_NONBLOCK,
                    0o666 as libc::mode_t,
                )
            };
            if fd >= 0 {
                let file = unsafe { File::from_raw_fd(fd) };
                return Ok((
                    TempPath {
                        path: temp_path,
                        armed: true,
                        parent: parent_directory.try_clone()?,
                        name: temp_name,
                    },
                    file,
                ));
            }
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                continue;
            }
            return Err(error).with_context(|| format!("failed to stage {}", target.display()));
        }
        #[cfg(not(target_os = "linux"))]
        {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options
                    .mode(0o666)
                    .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
            }
            match options.open(&temp_path) {
                Ok(file) => {
                    return Ok((
                        TempPath {
                            path: temp_path,
                            armed: true,
                        },
                        file,
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to stage {}", target.display()));
                }
            }
        }
    }
    bail!(
        "could not allocate a staging file beside {}",
        target.display()
    )
}

#[cfg(target_os = "linux")]
fn unlink_child(parent: &File, name: &std::ffi::OsStr, display: &Path) -> Result<()> {
    use std::os::fd::AsRawFd;
    let name = component_cstring(name)?;
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to remove staged {}", display.display()));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn rename_child(
    parent: &File,
    source: &std::ffi::OsStr,
    destination: &std::ffi::OsStr,
    display: &Path,
) -> Result<()> {
    use std::os::fd::AsRawFd;
    let source = component_cstring(source)?;
    let destination = component_cstring(destination)?;
    if unsafe {
        libc::renameat(
            parent.as_raw_fd(),
            source.as_ptr(),
            parent.as_raw_fd(),
            destination.as_ptr(),
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to atomically replace {}", display.display()));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn link_child(
    parent: &File,
    source: &std::ffi::OsStr,
    destination: &std::ffi::OsStr,
    display: &Path,
) -> Result<()> {
    use std::os::fd::AsRawFd;
    let source = component_cstring(source)?;
    let destination = component_cstring(destination)?;
    if unsafe {
        libc::linkat(
            parent.as_raw_fd(),
            source.as_ptr(),
            parent.as_raw_fd(),
            destination.as_ptr(),
            0,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "failed to atomically create {} without clobbering",
                display.display()
            )
        });
    }
    Ok(())
}

fn sync_parent(workspace_root: &Path, path: &Path, confined: bool) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("workspace target has no parent: {}", path.display()))?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(
            libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_DIRECTORY,
        );
    }
    let directory = options
        .open(parent)
        .with_context(|| format!("failed to open directory {} for sync", parent.display()))?;
    if !directory.metadata()?.is_dir() {
        bail!("workspace parent is not a directory: {}", parent.display());
    }
    validate_open_file_confinement(workspace_root, &directory, confined)?;
    directory
        .sync_all()
        .with_context(|| format!("failed to sync directory {}", parent.display()))
}

pub(crate) struct LockedWorkspaceTarget<'a> {
    workspace_root: &'a Path,
    requested: &'a str,
    confined: bool,
    path: PathBuf,
    #[cfg(target_os = "linux")]
    parent_directory: File,
    #[cfg(target_os = "linux")]
    target_name: std::ffi::OsString,
    _lock: TargetLock,
}

impl<'a> LockedWorkspaceTarget<'a> {
    pub(crate) fn acquire(
        workspace_root: &'a Path,
        requested: &'a str,
        confined: bool,
        create_parents: bool,
    ) -> Result<Self> {
        let initial = resolve_for_write(workspace_root, requested, confined, create_parents)?;
        let lock = TargetLock::acquire(&initial)?;
        let after_lock = resolve_for_write(workspace_root, requested, confined, false)?;
        if after_lock != initial {
            bail!(
                "workspace path changed while waiting for its edit lock ({} -> {})",
                initial.display(),
                after_lock.display()
            );
        }
        #[cfg(target_os = "linux")]
        let (parent_directory, target_name) = {
            let parent = after_lock.parent().ok_or_else(|| {
                anyhow::anyhow!("workspace target has no parent: {}", after_lock.display())
            })?;
            let target_name = after_lock
                .file_name()
                .ok_or_else(|| {
                    anyhow::anyhow!("workspace target has no filename: {}", after_lock.display())
                })?
                .to_os_string();
            let directory = if confined {
                directory_beneath(workspace_root, parent, false)?
            } else {
                open_directory_path(parent)?
            };
            validate_open_file_confinement(workspace_root, &directory, confined)?;
            (directory, target_name)
        };
        Ok(Self {
            workspace_root,
            requested,
            confined,
            path: after_lock,
            #[cfg(target_os = "linux")]
            parent_directory,
            #[cfg(target_os = "linux")]
            target_name,
            _lock: lock,
        })
    }

    pub(crate) fn canonical_path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn exists(&self) -> Result<bool> {
        Ok(self.destination_identity()?.is_some())
    }

    fn destination_identity(&self) -> Result<Option<(DestinationIdentity, fs::Permissions)>> {
        #[cfg(target_os = "linux")]
        {
            destination_identity_at(&self.parent_directory, &self.target_name, &self.path)
        }
        #[cfg(not(target_os = "linux"))]
        {
            destination_identity(&self.path)
        }
    }

    fn open_current(&self) -> Result<File> {
        #[cfg(target_os = "linux")]
        {
            open_regular_at(&self.parent_directory, &self.target_name, &self.path)
        }
        #[cfg(not(target_os = "linux"))]
        {
            open_regular(&self.path)
        }
    }

    fn revalidate_path(&self) -> Result<()> {
        let current = resolve_for_write(self.workspace_root, self.requested, self.confined, false)?;
        if current != self.path {
            bail!("workspace path changed while locked: {}", self.requested);
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::MetadataExt;
            let parent = current.parent().ok_or_else(|| {
                anyhow::anyhow!("workspace target has no parent: {}", current.display())
            })?;
            let current_parent = if self.confined {
                directory_beneath(self.workspace_root, parent, false)?
            } else {
                open_directory_path(parent)?
            };
            let anchored = self.parent_directory.metadata()?;
            let reopened = current_parent.metadata()?;
            if anchored.dev() != reopened.dev() || anchored.ino() != reopened.ino() {
                bail!(
                    "workspace parent directory changed while locked: {}",
                    self.requested
                );
            }
        }
        Ok(())
    }

    pub(crate) fn read_utf8_snapshot(&self, max_bytes: usize) -> Result<Utf8Snapshot> {
        self.revalidate_path()?;
        let file = self.open_current()?;
        validate_open_file_confinement(self.workspace_root, &file, self.confined)?;
        let bytes = read_open_file_bounded(file, &self.path, max_bytes)?;
        let byte_count = bytes.len() as u64;
        let sha256 = format!("sha256:{:x}", Sha256::digest(&bytes));
        let text = String::from_utf8(bytes)
            .with_context(|| format!("workspace file is not UTF-8: {}", self.requested))?;
        Ok(Utf8Snapshot {
            text,
            sha256,
            bytes: byte_count,
        })
    }

    pub(crate) fn publish(&self, contents: &[u8], overwrite: bool) -> Result<bool> {
        self.publish_checked(contents, overwrite, None)
    }

    pub(crate) fn publish_checked(
        &self,
        contents: &[u8],
        overwrite: bool,
        expected: Option<(&str, u64)>,
    ) -> Result<bool> {
        if contents.len() > MAX_WORKSPACE_TEXT_BYTES {
            bail!(
                "workspace write content is too large ({} bytes; max {})",
                contents.len(),
                MAX_WORKSPACE_TEXT_BYTES
            );
        }
        self.revalidate_path()?;

        let original = self.destination_identity()?;
        let existed = original.is_some();
        if existed && !overwrite {
            bail!(
                "refusing to overwrite {} without overwrite=true",
                self.requested
            );
        }
        if let Some((expected_sha256, expected_bytes)) = expected {
            if !existed {
                bail!(
                    "{} was removed after it was read; refusing a stale overwrite",
                    self.requested
                );
            }
            let file = self.open_current()?;
            validate_open_file_confinement(self.workspace_root, &file, self.confined)?;
            let current_bytes = read_open_file_bounded(file, &self.path, MAX_WORKSPACE_TEXT_BYTES)?;
            let current_sha256 = format!("sha256:{:x}", Sha256::digest(&current_bytes));
            if current_bytes.len() as u64 != expected_bytes || current_sha256 != expected_sha256 {
                bail!(
                    "{} changed since it was read; read the current file before editing it",
                    self.requested
                );
            }
        }
        let permissions = original.as_ref().map(|(_, permissions)| permissions);
        #[cfg(target_os = "linux")]
        let (mut temp, mut file) = create_staging_file(&self.path, &self.parent_directory)?;
        #[cfg(not(target_os = "linux"))]
        let (mut temp, mut file) = create_staging_file(&self.path)?;
        validate_open_file_confinement(self.workspace_root, &file, self.confined)?;
        file.write_all(contents)
            .with_context(|| format!("failed to stage {}", self.path.display()))?;
        if let Some(permissions) = permissions {
            // Apply the old mode after writing: on Unix, writing can clear
            // setuid/setgid bits that were set before the write.
            file.set_permissions(permissions.clone())
                .with_context(|| format!("failed to preserve mode for {}", self.path.display()))?;
        }
        file.sync_all()
            .with_context(|| format!("failed to sync staged {}", self.path.display()))?;
        drop(file);

        self.revalidate_path().with_context(|| {
            format!(
                "workspace path changed while staging publication: {}",
                self.requested
            )
        })?;

        // Do not silently overwrite an inode that appeared or changed while
        // the staged bytes were being written. Phoenix peers cooperate via the
        // lock; this catches non-cooperating interference.
        let now = self.destination_identity()?;
        if now.as_ref().map(|(id, _)| id) != original.as_ref().map(|(id, _)| id) {
            bail!(
                "workspace destination changed during publication: {}",
                self.path.display()
            );
        }

        if existed {
            #[cfg(target_os = "linux")]
            rename_child(
                &self.parent_directory,
                &temp.name,
                &self.target_name,
                &self.path,
            )?;
            #[cfg(not(target_os = "linux"))]
            fs::rename(&temp.path, &self.path)
                .with_context(|| format!("failed to atomically replace {}", self.path.display()))?;
            temp.disarm();
        } else {
            // `hard_link` is an atomic create-if-missing operation. Unlike a
            // check followed by rename, it cannot clobber a destination
            // created between those two steps.
            #[cfg(target_os = "linux")]
            link_child(
                &self.parent_directory,
                &temp.name,
                &self.target_name,
                &self.path,
            )?;
            #[cfg(not(target_os = "linux"))]
            fs::hard_link(&temp.path, &self.path).with_context(|| {
                format!(
                    "failed to atomically create {} without clobbering",
                    self.path.display()
                )
            })?;
            temp.remove_now().with_context(|| {
                format!(
                    "{} was published, but its staging link could not be removed",
                    self.path.display()
                )
            })?;
        }
        #[cfg(target_os = "linux")]
        let sync_result = validate_open_file_confinement(
            self.workspace_root,
            &self.parent_directory,
            self.confined,
        )
        .and_then(|()| {
            self.parent_directory
                .sync_all()
                .context("failed to sync anchored workspace directory")
        });
        #[cfg(not(target_os = "linux"))]
        let sync_result = sync_parent(self.workspace_root, &self.path, self.confined);
        sync_result.with_context(|| {
            format!(
                "{} was published atomically, but its directory durability sync failed",
                self.path.display()
            )
        })?;
        Ok(existed)
    }
}

pub(crate) fn sha256_regular_file(
    workspace_root: &Path,
    input: &str,
    confined: bool,
) -> Result<(String, u64)> {
    let path = resolve_existing(workspace_root, input, confined)?;
    let mut file = open_regular(&path)?;
    validate_open_file_confinement(workspace_root, &file, confined)?;
    let before = file
        .metadata()
        .with_context(|| format!("failed to inspect artifact {}", path.display()))?;
    if before.len() > MAX_PUBLISH_ARTIFACT_BYTES {
        bail!(
            "artifact is too large to publish safely ({} bytes; max {})",
            before.len(),
            MAX_PUBLISH_ARTIFACT_BYTES
        );
    }

    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("failed to hash artifact {}", path.display()))?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .context("artifact byte count overflowed")?;
        if total > MAX_PUBLISH_ARTIFACT_BYTES {
            bail!(
                "artifact grew beyond the {}-byte publication limit while hashing",
                MAX_PUBLISH_ARTIFACT_BYTES
            );
        }
        hasher.update(&buffer[..read]);
    }

    let after = file
        .metadata()
        .with_context(|| format!("failed to re-inspect artifact {}", path.display()))?;
    if metadata_identity(&before) != metadata_identity(&after) || total != after.len() {
        bail!(
            "artifact changed while it was being hashed: {}",
            path.display()
        );
    }

    Ok((format!("sha256:{:x}", hasher.finalize()), total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_read_rejects_sparse_file_before_allocating_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sparse.txt");
        File::create(&path)
            .unwrap()
            .set_len(MAX_WORKSPACE_TEXT_BYTES as u64 + 1)
            .unwrap();
        let error = read_regular_bytes(dir.path(), "sparse.txt", true, MAX_WORKSPACE_TEXT_BYTES)
            .unwrap_err();
        assert!(error.to_string().contains("too large"), "{error:#}");
    }

    #[cfg(unix)]
    #[test]
    fn bounded_read_rejects_fifo_without_blocking() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe.txt");
        let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let started = std::time::Instant::now();
        let error = read_regular_bytes(dir.path(), "pipe.txt", true, 1024).unwrap_err();
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(error.to_string().contains("regular file"), "{error:#}");
    }

    #[cfg(unix)]
    #[test]
    fn reads_and_writes_reject_final_symlinks() {
        let state_home = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(state_home.path());
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("real.txt"), "secret").unwrap();
        symlink("real.txt", dir.path().join("alias.txt")).unwrap();
        assert!(read_regular_bytes(dir.path(), "alias.txt", true, 1024).is_err());
        assert!(LockedWorkspaceTarget::acquire(dir.path(), "alias.txt", true, false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn parent_symlink_cannot_redirect_a_confined_write() {
        let state_home = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(state_home.path());
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), dir.path().join("redirect")).unwrap();
        let error = LockedWorkspaceTarget::acquire(dir.path(), "redirect/escaped.txt", true, true)
            .err()
            .expect("confined write through outside symlink must fail");
        assert!(error.to_string().contains("escapes workspace"), "{error:#}");
        assert!(!outside.path().join("escaped.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn atomic_overwrite_preserves_existing_mode() {
        let state_home = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(state_home.path());
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("script.sh");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o751)).unwrap();
        let target = LockedWorkspaceTarget::acquire(dir.path(), "script.sh", true, false).unwrap();
        target.publish(b"new", true).unwrap();
        assert_eq!(fs::metadata(path).unwrap().mode() & 0o7777, 0o751);
    }

    #[test]
    fn atomic_no_clobber_has_exactly_one_winner() {
        let state_home = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(state_home.path());
        let dir = tempfile::tempdir().unwrap();
        let root = std::sync::Arc::new(dir.path().to_path_buf());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let mut threads = Vec::new();
        for index in 0..8 {
            let root = std::sync::Arc::clone(&root);
            let barrier = std::sync::Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                let target =
                    LockedWorkspaceTarget::acquire(&root, "winner.txt", true, true).unwrap();
                target.publish(index.to_string().as_bytes(), false).is_ok()
            }));
        }
        let wins = threads
            .into_iter()
            .map(|thread| thread.join().unwrap() as usize)
            .sum::<usize>();
        assert_eq!(wins, 1);
        assert!(dir.path().join("winner.txt").is_file());
    }

    #[test]
    fn checked_publish_rejects_a_noncooperating_in_place_edit() {
        let state_home = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(state_home.path());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared.txt");
        fs::write(&path, "version one").unwrap();
        let target = LockedWorkspaceTarget::acquire(dir.path(), "shared.txt", true, false).unwrap();
        let snapshot = target.read_utf8_snapshot(MAX_WORKSPACE_TEXT_BYTES).unwrap();

        // This writer intentionally ignores Phoenix's flock.
        fs::write(&path, "version two").unwrap();
        let error = target
            .publish_checked(
                b"stale replacement",
                true,
                Some((&snapshot.sha256, snapshot.bytes)),
            )
            .unwrap_err();
        assert!(error.to_string().contains("changed since it was read"));
        assert_eq!(fs::read_to_string(path).unwrap(), "version two");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn anchored_staging_never_follows_a_replaced_parent_path() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("parent")).unwrap();
        let anchor = directory_beneath(dir.path(), &dir.path().join("parent"), false).unwrap();
        fs::rename(dir.path().join("parent"), dir.path().join("moved")).unwrap();
        symlink(outside.path(), dir.path().join("parent")).unwrap();

        let display = dir.path().join("parent/result.txt");
        let (mut temp, mut file) = create_staging_file(&display, &anchor).unwrap();
        file.write_all(b"anchored").unwrap();
        file.sync_all().unwrap();
        drop(file);
        link_child(
            &anchor,
            &temp.name,
            std::ffi::OsStr::new("result.txt"),
            &display,
        )
        .unwrap();
        temp.remove_now().unwrap();

        assert_eq!(
            fs::read_to_string(dir.path().join("moved/result.txt")).unwrap(),
            "anchored"
        );
        assert!(!outside.path().join("result.txt").exists());
    }

    #[test]
    fn artifact_hash_is_streamed_and_exact() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = vec![0x5a; 2 * 1024 * 1024 + 17];
        fs::write(dir.path().join("artifact.bin"), &bytes).unwrap();
        let (actual, count) = sha256_regular_file(dir.path(), "artifact.bin", true).unwrap();
        assert_eq!(count, bytes.len() as u64);
        assert_eq!(actual, format!("sha256:{:x}", Sha256::digest(&bytes)));
    }
}
