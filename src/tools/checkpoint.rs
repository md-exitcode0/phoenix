//! Checkpoints/rewind — shadow snapshots of files the agent edits.
//!
//! The first time a turn touches a file through `write`/`str_replace`, the
//! pre-edit bytes are copied to `.phoenix/checkpoints/<session>/<scope>/`
//! (first-write-wins, so a scope always holds the turn-START state). New-file
//! creations are recorded too, so a rewind deletes them. `phoenix rewind`
//! restores the most recent scope and consumes it — repeated rewinds walk
//! further back, exactly like an undo stack.
//!
//! Known hole, by design: file changes made through `bash` are not tracked.
//! Snapshotting cannot see inside a shell command; the structured edit tools
//! are the rewindable path.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

const MAX_CHECKPOINT_FILE_BYTES: usize = 32 * 1024 * 1024;
const MAX_MANIFEST_BYTES: usize = 4 * 1024 * 1024;
const MAX_MANIFEST_ENTRIES: usize = 10_000;
const MAX_SESSIONS: usize = 4_096;
const MAX_SCOPES: usize = 20_000;

/// One file recorded in a checkpoint scope.
#[derive(Debug, Serialize, Deserialize)]
struct ManifestEntry {
    /// Path relative to the workspace root.
    rel: String,
    /// False when the tool CREATED the file — rewind deletes it.
    existed: bool,
}

#[derive(Debug)]
pub struct ScopeInfo {
    pub session: String,
    pub scope: String,
    pub files: usize,
}

#[derive(Debug, Default)]
pub struct RewindReport {
    pub restored: Vec<String>,
    pub deleted: Vec<String>,
    pub scope: String,
    pub session: String,
}

pub struct CheckpointStore {
    root: PathBuf,
}

/// Opaque proof that one call attempted to add a file to a checkpoint scope.
/// The executor hands it back after a failed edit so the store can remove only
/// a newly-added, provably unchanged snapshot. Existing first-write-wins state
/// is never discarded by a later failed call.
#[derive(Debug)]
pub struct SnapshotReceipt {
    session: String,
    scope: String,
    rel: String,
    inserted: bool,
    existed: bool,
}

impl CheckpointStore {
    /// `state_root` is the Phoenix state dir (`~/.phoenix` or a test root).
    pub fn new(state_root: &Path) -> Self {
        Self {
            root: state_root.join("checkpoints"),
        }
    }

    fn scope_dir(&self, session: &str, scope: &str) -> PathBuf {
        self.root.join(session).join(scope)
    }

    fn guard_path(&self) -> PathBuf {
        self.root.join("checkpoint-operations")
    }

    fn validate_root(&self) -> Result<()> {
        if !self.root.is_absolute()
            || self.root.parent().is_none()
            || self.root.parent() == Some(Path::new("/"))
        {
            bail!("refusing unsafe checkpoint root {}", self.root.display());
        }
        crate::config::private_io::reject_symlink_components(&self.root)?;
        match std::fs::symlink_metadata(&self.root) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                bail!("refusing unsafe checkpoint root {}", self.root.display());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    /// Sortable timestamp plus a random suffix, so concurrent turns cannot
    /// accidentally share one first-write-wins scope.
    pub fn new_scope_id() -> String {
        format!(
            "{:013}-{}",
            chrono::Utc::now().timestamp_millis(),
            uuid::Uuid::new_v4().simple()
        )
    }

    /// Record `abs_path`'s current state into the scope, once. Later snapshots
    /// of the same file in the same scope are no-ops (turn-start state wins).
    pub fn snapshot(
        &self,
        session: &str,
        scope: &str,
        workspace: &Path,
        abs_path: &Path,
    ) -> Result<SnapshotReceipt> {
        self.validate_root()?;
        crate::session::SessionStore::validate_session_id(session)
            .context("invalid checkpoint session id")?;
        validate_scope(scope)?;
        let rel_path = abs_path.strip_prefix(workspace).with_context(|| {
            format!(
                "refusing to checkpoint {} outside workspace {}",
                abs_path.display(),
                workspace.display()
            )
        })?;
        validate_relative_path(rel_path)?;
        // Missing parents are valid for a not-yet-created file; every existing
        // prefix must still be a real directory rather than a symlink.
        let _ = workspace_parent_is_safe_and_present(workspace, rel_path)?;
        let rel = rel_path
            .to_str()
            .context("checkpoint paths must be valid UTF-8")?
            .to_string();
        let manifest = self.scope_dir(session, scope).join("manifest.jsonl");
        let guard = self.guard_path();
        crate::config::private_io::with_private_lock(&guard, || {
            crate::config::private_io::read_modify_write_private(&manifest, |current| {
                let mut entries = parse_manifest(current.unwrap_or_default(), &manifest)?;
                if entries.iter().any(|entry| entry.rel == rel) {
                    return Ok((
                        SnapshotReceipt {
                            session: session.to_string(),
                            scope: scope.to_string(),
                            rel: rel.clone(),
                            inserted: false,
                            existed: false,
                        },
                        current.unwrap_or_default().to_vec(),
                    ));
                }
                if entries.len() >= MAX_MANIFEST_ENTRIES {
                    bail!("checkpoint scope has reached its {MAX_MANIFEST_ENTRIES}-file limit");
                }

                let bytes = read_workspace_snapshot(abs_path)?;
                let existed = bytes.is_some();
                if let Some(bytes) = bytes {
                    let shadow = self
                        .scope_dir(session, scope)
                        .join("files")
                        .join(shadow_name(&rel));
                    crate::config::private_io::atomic_write_private(&shadow, &bytes)
                        .context("writing checkpoint shadow")?;
                }
                entries.push(ManifestEntry {
                    rel: rel.clone(),
                    existed,
                });
                let encoded = encode_manifest(&entries)?;
                Ok((
                    SnapshotReceipt {
                        session: session.to_string(),
                        scope: scope.to_string(),
                        rel: rel.clone(),
                        inserted: true,
                        existed,
                    },
                    encoded,
                ))
            })
        })
    }

    /// Remove a failed edit's snapshot only when this call inserted it and the
    /// target still exactly matches the pre-edit state. A changed/missing
    /// original may mean publication happened before a later durability error,
    /// so ambiguity always keeps the checkpoint.
    pub(crate) fn discard_failed_if_unchanged(
        &self,
        receipt: &SnapshotReceipt,
        workspace: &Path,
    ) -> Result<bool> {
        self.validate_root()?;
        if !receipt.inserted {
            return Ok(false);
        }
        crate::session::SessionStore::validate_session_id(&receipt.session)
            .context("invalid checkpoint receipt session id")?;
        validate_scope(&receipt.scope)?;
        let rel = Path::new(&receipt.rel);
        validate_relative_path(rel)?;
        let workspace = std::fs::canonicalize(workspace)
            .with_context(|| format!("resolving workspace {}", workspace.display()))?;
        if !std::fs::metadata(&workspace)?.is_dir() {
            bail!("workspace is not a directory: {}", workspace.display());
        }
        let target = workspace.join(rel);
        let manifest = self
            .scope_dir(&receipt.session, &receipt.scope)
            .join("manifest.jsonl");
        let shadow = self
            .scope_dir(&receipt.session, &receipt.scope)
            .join("files")
            .join(shadow_name(&receipt.rel));
        let guard = self.guard_path();
        let discarded = crate::config::private_io::with_private_lock(&guard, || {
            crate::config::private_io::read_modify_write_private(&manifest, |current| {
                let current = current.with_context(|| {
                    format!(
                        "checkpoint receipt manifest disappeared: {}",
                        manifest.display()
                    )
                })?;
                let mut entries = parse_manifest(current, &manifest)?;
                let Some(index) = entries.iter().position(|entry| entry.rel == receipt.rel) else {
                    return Ok((false, current.to_vec()));
                };
                if entries[index].existed != receipt.existed {
                    bail!("checkpoint receipt no longer matches its manifest entry");
                }

                let parent_present = workspace_parent_is_safe_and_present(&workspace, rel)?;
                let unchanged = if receipt.existed && parent_present {
                    let original = crate::config::private_io::read_private_file_limited(
                        &shadow,
                        MAX_CHECKPOINT_FILE_BYTES,
                    )?
                    .with_context(|| format!("checkpoint shadow is missing for {}", receipt.rel))?;
                    read_workspace_snapshot(&target)?.is_some_and(|bytes| bytes == original)
                } else if receipt.existed {
                    false
                } else if !parent_present {
                    true
                } else {
                    read_workspace_snapshot(&target)?.is_none()
                };
                if !unchanged {
                    return Ok((false, current.to_vec()));
                }

                entries.remove(index);
                Ok((true, encode_manifest(&entries)?))
            })
        })?;
        if discarded && receipt.existed {
            // The manifest is already the durable source of truth. Failure to
            // remove this now-unreferenced shadow is an orphan-cleanup warning,
            // never a reason to resurrect a misleading checkpoint entry.
            if let Err(error) = crate::config::private_io::remove_private_file(&shadow) {
                tracing::warn!(
                    "discarded failed checkpoint entry but could not remove orphan shadow {}: {error:#}",
                    shadow.display()
                );
            }
        }
        Ok(discarded)
    }

    /// Every scope across all sessions, newest first.
    pub fn scopes(&self) -> Vec<ScopeInfo> {
        match self.scopes_result() {
            Ok(scopes) => scopes,
            Err(error) => {
                tracing::warn!("checkpoint scopes are unreadable: {error:#}");
                Vec::new()
            }
        }
    }

    /// Truthful scope listing for rewind and callers that can surface errors.
    pub fn scopes_result(&self) -> Result<Vec<ScopeInfo>> {
        self.validate_root()?;
        let mut out: Vec<ScopeInfo> = Vec::new();
        let sessions = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(error) => return Err(error).context("reading checkpoint sessions"),
        };
        let mut session_count = 0usize;
        let mut scope_count = 0usize;
        for session_entry in sessions {
            let session_entry = session_entry.context("reading checkpoint session entry")?;
            if !session_entry
                .file_type()
                .context("inspecting checkpoint session entry")?
                .is_dir()
            {
                continue;
            }
            session_count += 1;
            if session_count > MAX_SESSIONS {
                bail!("checkpoint store has more than {MAX_SESSIONS} sessions");
            }
            let session = session_entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("checkpoint session name is not UTF-8"))?;
            crate::session::SessionStore::validate_session_id(&session)
                .context("checkpoint store contains an invalid session id")?;
            let scopes = std::fs::read_dir(session_entry.path())
                .with_context(|| format!("reading checkpoint scopes for {session}"))?;
            for scope_entry in scopes {
                let scope_entry = scope_entry.context("reading checkpoint scope entry")?;
                if !scope_entry
                    .file_type()
                    .context("inspecting checkpoint scope entry")?
                    .is_dir()
                {
                    continue;
                }
                scope_count += 1;
                if scope_count > MAX_SCOPES {
                    bail!("checkpoint store has more than {MAX_SCOPES} scopes");
                }
                let scope = scope_entry
                    .file_name()
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("checkpoint scope name is not UTF-8"))?;
                validate_scope(&scope)?;
                let files =
                    read_manifest_required(&scope_entry.path().join("manifest.jsonl"))?.len();
                if files == 0 {
                    // A failed edit can leave an empty reusable scope for the
                    // rest of its turn. It is not a rewindable checkpoint and
                    // must not be presented as one.
                    continue;
                }
                out.push(ScopeInfo {
                    session: session.clone(),
                    scope,
                    files,
                });
            }
        }
        // Scope ids are millisecond timestamps — lexicographic length-aware sort.
        out.sort_by(|a, b| (b.scope.len(), &b.scope).cmp(&(a.scope.len(), &a.scope)));
        Ok(out)
    }

    /// Restore the newest scope (across sessions) and consume it.
    pub fn rewind_latest(&self, workspace: &Path) -> Result<Option<RewindReport>> {
        self.validate_root()?;
        let workspace = std::fs::canonicalize(workspace)
            .with_context(|| format!("resolving workspace {}", workspace.display()))?;
        if !std::fs::metadata(&workspace)?.is_dir() {
            bail!("workspace is not a directory: {}", workspace.display());
        }
        let guard = self.guard_path();
        crate::config::private_io::with_private_lock(&guard, || {
            let Some(latest) = self.scopes_result()?.into_iter().next() else {
                return Ok(None);
            };
            let dir = self.scope_dir(&latest.session, &latest.scope);
            let entries = read_manifest_required(&dir.join("manifest.jsonl"))?;
            let mut report = RewindReport {
                scope: latest.scope.clone(),
                session: latest.session.clone(),
                ..Default::default()
            };
            for entry in entries {
                let rel = Path::new(&entry.rel);
                validate_relative_path(rel)?;
                let target = workspace.join(rel);
                if entry.existed {
                    let files = dir.join("files");
                    let current_shadow = files.join(shadow_name(&entry.rel));
                    let legacy_shadow = files.join(legacy_shadow_name(&entry.rel));
                    let bytes = match crate::config::private_io::read_private_file_limited(
                        &current_shadow,
                        MAX_CHECKPOINT_FILE_BYTES,
                    )? {
                        Some(bytes) => bytes,
                        None => crate::config::private_io::read_private_file_limited(
                            &legacy_shadow,
                            MAX_CHECKPOINT_FILE_BYTES,
                        )?
                        .with_context(|| {
                            format!("checkpoint shadow is missing for {}", entry.rel)
                        })?,
                    };
                    atomic_replace_workspace_file(&workspace, rel, &target, &bytes)
                        .with_context(|| format!("restoring {}", entry.rel))?;
                    report.restored.push(entry.rel);
                } else {
                    if !workspace_parent_is_safe_and_present(&workspace, rel)? {
                        continue;
                    }
                    match std::fs::symlink_metadata(&target) {
                        Ok(metadata) if metadata.is_file() || metadata.file_type().is_symlink() => {
                            std::fs::remove_file(&target)
                                .with_context(|| format!("deleting created file {}", entry.rel))?;
                            report.deleted.push(entry.rel);
                        }
                        Ok(_) => bail!(
                            "refusing to delete non-file checkpoint target {}",
                            target.display()
                        ),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            return Err(error).with_context(|| {
                                format!("inspecting checkpoint target {}", target.display())
                            });
                        }
                    }
                }
            }
            if let Err(error) = std::fs::remove_dir_all(&dir) {
                bail!(
                    "rewind restored {} and deleted {} file(s), but failed to consume checkpoint scope {}: {error}",
                    report.restored.len(),
                    report.deleted.len(),
                    dir.display()
                );
            }
            // Drop the session dir too when it just emptied.
            let session_dir = self.root.join(&latest.session);
            match std::fs::read_dir(&session_dir) {
                Ok(mut remaining) => {
                    if remaining.next().is_none() {
                        if let Err(error) = std::fs::remove_dir(&session_dir) {
                            tracing::warn!(
                                "rewind completed but could not remove empty checkpoint session {}: {error}",
                                session_dir.display()
                            );
                        }
                    }
                }
                Err(error) => tracing::warn!(
                    "rewind completed but could not inspect checkpoint session {}: {error}",
                    session_dir.display()
                ),
            }
            Ok(Some(report))
        })
    }
}

/// Legacy pre-hardening shadow name, retained only for rewinding existing
/// checkpoints. It is collision-prone (`a/b` and `a__b`), so new snapshots use
/// a SHA-256 address below.
fn legacy_shadow_name(rel: &str) -> String {
    rel.replace(['/', '\\'], "__")
}

fn shadow_name(rel: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(rel.as_bytes()))
}

fn read_manifest_required(manifest: &Path) -> Result<Vec<ManifestEntry>> {
    let bytes = crate::config::private_io::read_private_file_limited(manifest, MAX_MANIFEST_BYTES)?
        .with_context(|| format!("checkpoint scope is missing {}", manifest.display()))?;
    parse_manifest(&bytes, manifest)
}

fn parse_manifest(bytes: &[u8], manifest: &Path) -> Result<Vec<ManifestEntry>> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        bail!(
            "checkpoint manifest {} is too large ({} bytes; max {MAX_MANIFEST_BYTES})",
            manifest.display(),
            bytes.len()
        );
    }
    let content = std::str::from_utf8(bytes)
        .with_context(|| format!("checkpoint manifest {} is not UTF-8", manifest.display()))?;
    let mut entries = Vec::new();
    for (line_no, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        if entries.len() >= MAX_MANIFEST_ENTRIES {
            bail!(
                "checkpoint manifest {} has more than {MAX_MANIFEST_ENTRIES} entries",
                manifest.display()
            );
        }
        let entry: ManifestEntry = serde_json::from_str(line).with_context(|| {
            format!(
                "checkpoint manifest {} is corrupt at line {}",
                manifest.display(),
                line_no + 1
            )
        })?;
        validate_relative_path(Path::new(&entry.rel)).with_context(|| {
            format!(
                "checkpoint manifest {} has an unsafe path at line {}",
                manifest.display(),
                line_no + 1
            )
        })?;
        if entries
            .iter()
            .any(|existing: &ManifestEntry| existing.rel == entry.rel)
        {
            bail!(
                "checkpoint manifest {} repeats path {}",
                manifest.display(),
                entry.rel
            );
        }
        entries.push(entry);
    }
    Ok(entries)
}

fn encode_manifest(entries: &[ManifestEntry]) -> Result<Vec<u8>> {
    let mut encoded = Vec::new();
    for entry in entries {
        serde_json::to_writer(&mut encoded, entry)?;
        encoded.push(b'\n');
        if encoded.len() > MAX_MANIFEST_BYTES {
            bail!("checkpoint manifest exceeds {MAX_MANIFEST_BYTES} bytes");
        }
    }
    Ok(encoded)
}

fn validate_scope(scope: &str) -> Result<()> {
    if scope.is_empty()
        || scope.len() > 64
        || !scope
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        bail!("invalid checkpoint scope id {scope:?}");
    }
    Ok(())
}

fn validate_relative_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        bail!("checkpoint path must be a non-empty relative path");
    }
    if path.as_os_str().len() > 4_096 {
        bail!("checkpoint path is too long");
    }
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("checkpoint path must not contain '.', '..', or a root prefix");
    }
    Ok(())
}

fn read_workspace_snapshot(path: &Path) -> Result<Option<Vec<u8>>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspecting checkpoint source {}", path.display()));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("refusing to checkpoint non-regular file {}", path.display());
    }
    if metadata.len() > MAX_CHECKPOINT_FILE_BYTES as u64 {
        bail!(
            "checkpoint source {} is too large ({} bytes; max {MAX_CHECKPOINT_FILE_BYTES})",
            path.display(),
            metadata.len()
        );
    }

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("opening checkpoint source {}", path.display()))?;
    if !file.metadata()?.is_file() {
        bail!("checkpoint source changed to a non-regular file");
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    std::io::Read::by_ref(&mut file)
        .take(MAX_CHECKPOINT_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading checkpoint source {}", path.display()))?;
    if bytes.len() > MAX_CHECKPOINT_FILE_BYTES {
        bail!("checkpoint source grew beyond {MAX_CHECKPOINT_FILE_BYTES} bytes while reading");
    }
    Ok(Some(bytes))
}

fn prepare_workspace_parent(workspace: &Path, rel: &Path) -> Result<PathBuf> {
    let parent_rel = rel.parent().unwrap_or_else(|| Path::new(""));
    let mut current = workspace.to_path_buf();
    for component in parent_rel.components() {
        let Component::Normal(name) = component else {
            bail!("unsafe checkpoint target path");
        };
        current.push(name);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                bail!(
                    "refusing unsafe checkpoint target directory {}",
                    current.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::create_dir(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        let metadata = std::fs::symlink_metadata(&current)?;
                        if metadata.file_type().is_symlink() || !metadata.is_dir() {
                            bail!(
                                "unsafe checkpoint target directory appeared at {}",
                                current.display()
                            );
                        }
                    }
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!("creating checkpoint target directory {}", current.display())
                        });
                    }
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(current)
}

fn workspace_parent_is_safe_and_present(workspace: &Path, rel: &Path) -> Result<bool> {
    let parent_rel = rel.parent().unwrap_or_else(|| Path::new(""));
    let mut current = workspace.to_path_buf();
    for component in parent_rel.components() {
        let Component::Normal(name) = component else {
            bail!("unsafe checkpoint target path");
        };
        current.push(name);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                bail!(
                    "refusing unsafe checkpoint target directory {}",
                    current.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(true)
}

fn atomic_replace_workspace_file(
    workspace: &Path,
    rel: &Path,
    target: &Path,
    bytes: &[u8],
) -> Result<()> {
    if bytes.len() > MAX_CHECKPOINT_FILE_BYTES {
        bail!("checkpoint shadow exceeds {MAX_CHECKPOINT_FILE_BYTES} bytes");
    }
    let parent = prepare_workspace_parent(workspace, rel)?;
    let prior_permissions = match std::fs::symlink_metadata(target) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            bail!(
                "refusing unsafe checkpoint restore target {}",
                target.display()
            );
        }
        Ok(metadata) => Some(metadata.permissions()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let base = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("checkpoint-restore");
    for _ in 0..8 {
        let temp = parent.join(format!(
            ".{base}.phoenix-rewind.{}",
            uuid::Uuid::new_v4().simple()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        }
        let mut file = match options.open(&temp) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        };
        let result = (|| -> Result<()> {
            file.write_all(bytes)?;
            if let Some(permissions) = prior_permissions.clone() {
                file.set_permissions(permissions)?;
            }
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temp, target)?;
            if let Ok(directory) = std::fs::File::open(&parent) {
                let _ = directory.sync_all();
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        return result;
    }
    bail!("could not allocate a checkpoint restore staging file")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_then_rewind_restores_edits_and_deletes_creations() {
        let state = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(state.path());

        let edited = workspace.path().join("src/lib.rs");
        std::fs::create_dir_all(edited.parent().unwrap()).unwrap();
        std::fs::write(&edited, "original").unwrap();
        let created = workspace.path().join("new.txt");

        let scope = CheckpointStore::new_scope_id();
        store
            .snapshot("sess", &scope, workspace.path(), &edited)
            .unwrap();
        store
            .snapshot("sess", &scope, workspace.path(), &created)
            .unwrap();

        // Simulate the agent's edits.
        std::fs::write(&edited, "mutated").unwrap();
        std::fs::write(&created, "brand new").unwrap();

        let report = store
            .rewind_latest(workspace.path())
            .unwrap()
            .expect("scope to rewind");
        assert_eq!(report.restored, vec!["src/lib.rs".to_string()]);
        assert_eq!(report.deleted, vec!["new.txt".to_string()]);
        assert_eq!(std::fs::read_to_string(&edited).unwrap(), "original");
        assert!(!created.exists());
        // Consumed: nothing left to rewind.
        assert!(store.rewind_latest(workspace.path()).unwrap().is_none());
    }

    #[test]
    fn first_snapshot_wins_within_a_scope() {
        let state = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(state.path());
        let file = workspace.path().join("f.txt");
        std::fs::write(&file, "turn start").unwrap();

        let scope = CheckpointStore::new_scope_id();
        store
            .snapshot("sess", &scope, workspace.path(), &file)
            .unwrap();
        std::fs::write(&file, "edit one").unwrap();
        // Second snapshot of the same file must NOT overwrite the shadow.
        store
            .snapshot("sess", &scope, workspace.path(), &file)
            .unwrap();
        std::fs::write(&file, "edit two").unwrap();

        let report = store.rewind_latest(workspace.path()).unwrap().unwrap();
        assert_eq!(report.restored.len(), 1);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "turn start");
    }

    #[test]
    fn scopes_lists_newest_first() {
        let state = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(state.path());
        let file = workspace.path().join("a.txt");
        std::fs::write(&file, "v").unwrap();
        store
            .snapshot("sess", "100", workspace.path(), &file)
            .unwrap();
        store
            .snapshot("sess", "200", workspace.path(), &file)
            .unwrap();
        let scopes = store.scopes();
        assert_eq!(scopes.len(), 2);
        assert_eq!(scopes[0].scope, "200");
        assert_eq!(scopes[0].files, 1);
    }

    #[test]
    fn snapshot_rejects_paths_outside_workspace_and_corrupt_manifests() {
        let state = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let outside = state.path().join("outside.txt");
        std::fs::write(&outside, "secret").unwrap();
        let store = CheckpointStore::new(state.path());
        assert!(store
            .snapshot("sess", "100", workspace.path(), &outside)
            .is_err());

        let manifest = state.path().join("checkpoints/sess/100/manifest.jsonl");
        crate::config::private_io::atomic_write_private(&manifest, b"{not-json}\n").unwrap();
        assert!(store.scopes_result().is_err());
        assert!(store.rewind_latest(workspace.path()).is_err());
    }

    #[test]
    fn shadow_addresses_do_not_collide_for_flattening_lookalikes() {
        assert_ne!(shadow_name("a/b"), shadow_name("a__b"));
        assert_eq!(legacy_shadow_name("a/b"), legacy_shadow_name("a__b"));
    }

    #[test]
    fn failed_unchanged_edits_discard_only_their_new_snapshot() {
        let state = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(state.path());

        let missing = workspace.path().join("new.txt");
        let receipt = store
            .snapshot("sess", "100", workspace.path(), &missing)
            .unwrap();
        assert!(store
            .discard_failed_if_unchanged(&receipt, workspace.path())
            .unwrap());
        assert!(store.scopes_result().unwrap().is_empty());

        let existing = workspace.path().join("existing.txt");
        std::fs::write(&existing, "original").unwrap();
        let receipt = store
            .snapshot("sess", "200", workspace.path(), &existing)
            .unwrap();
        assert!(store
            .discard_failed_if_unchanged(&receipt, workspace.path())
            .unwrap());
        assert!(store.scopes_result().unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(existing).unwrap(), "original");
    }

    #[test]
    fn failed_edit_cleanup_keeps_changed_and_prior_snapshots() {
        let state = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(state.path());
        let file = workspace.path().join("file.txt");
        std::fs::write(&file, "original").unwrap();

        let first = store
            .snapshot("sess", "100", workspace.path(), &file)
            .unwrap();
        std::fs::write(&file, "possibly published").unwrap();
        assert!(!store
            .discard_failed_if_unchanged(&first, workspace.path())
            .unwrap());

        let duplicate = store
            .snapshot("sess", "100", workspace.path(), &file)
            .unwrap();
        assert!(!store
            .discard_failed_if_unchanged(&duplicate, workspace.path())
            .unwrap());
        store.rewind_latest(workspace.path()).unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(file).unwrap(), "original");
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_rejects_symlinks_and_rewind_will_not_follow_parent_symlinks() {
        use std::os::unix::fs::symlink;

        let state = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(state.path());

        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "secret").unwrap();
        let linked = workspace.path().join("linked.txt");
        symlink(&secret, &linked).unwrap();
        assert!(store
            .snapshot("sess", "100", workspace.path(), &linked)
            .is_err());

        let nested = workspace.path().join("dir/file.txt");
        std::fs::create_dir(workspace.path().join("dir")).unwrap();
        std::fs::write(&nested, "original").unwrap();
        store
            .snapshot("sess", "200", workspace.path(), &nested)
            .unwrap();
        std::fs::remove_file(&nested).unwrap();
        std::fs::remove_dir(workspace.path().join("dir")).unwrap();
        symlink(outside.path(), workspace.path().join("dir")).unwrap();
        assert!(store.rewind_latest(workspace.path()).is_err());
        assert_eq!(std::fs::read_to_string(&secret).unwrap(), "secret");
    }

    #[cfg(unix)]
    #[test]
    fn checkpoint_root_symlinks_are_rejected_even_when_empty() {
        use std::os::unix::fs::symlink;

        let state = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), state.path().join("checkpoints")).unwrap();
        let store = CheckpointStore::new(state.path());
        assert!(store.scopes_result().is_err());
        assert!(store.rewind_latest(state.path()).is_err());
    }
}
