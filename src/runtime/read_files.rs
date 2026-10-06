//! Per-session "read before edit" guard (Claude Code safety feature).
//!
//! Tracks the exact content version read through the `read` tool in each
//! session. The `write` (overwrite) and `str_replace` tools consult this store
//! before editing an EXISTING file: a path-only receipt is not enough, because
//! another agent or the user may have changed the file since the read. New file
//! creation is always allowed — only existing files need the read guard.
//!
//! Storage is one small JSON map at `~/.phoenix/read_files.json`, keyed by
//! session id. Each session maps canonical paths to bounded SHA-256/length
//! receipts. The map is pruned by last-updated time past MAX_SESSIONS entries,
//! matching the turn_anchor pattern.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Keep the map from growing without bound: past this many sessions the
/// oldest are dropped on save.
const MAX_SESSIONS: usize = 512;
/// Keep each session's file set from growing without bound: past this many
/// entries the oldest are dropped on save.
const MAX_FILES_PER_SESSION: usize = 4096;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SessionReads {
    /// Canonical path strings → the exact content version that was read.
    files: HashMap<String, StoredReadReceipt>,
    /// Unix seconds of the last read that touched this session (prune key).
    updated: i64,
}

/// Content identity authorized by a successful `read`. The hash is prefixed
/// (`sha256:<hex>`) so a future schema can add algorithms without ambiguity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadReceipt {
    pub sha256: String,
    pub bytes: u64,
    pub recorded_at: i64,
}

/// Old stores used a bare unix timestamp. Preserve their JSON during load, but
/// never let a path-only legacy receipt authorize an edit: the content version
/// it referred to is unknowable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum StoredReadReceipt {
    Current(ReadReceipt),
    Legacy(i64),
}

impl StoredReadReceipt {
    fn recorded_at(&self) -> i64 {
        match self {
            Self::Current(receipt) => receipt.recorded_at,
            Self::Legacy(recorded_at) => *recorded_at,
        }
    }

    fn current(&self) -> Option<&ReadReceipt> {
        match self {
            Self::Current(receipt) if valid_sha256(&receipt.sha256) => Some(receipt),
            Self::Current(_) | Self::Legacy(_) => None,
        }
    }
}

fn store_path() -> std::path::PathBuf {
    crate::config::phoenix_home().join("read_files.json")
}

fn load_at(path: &Path) -> anyhow::Result<HashMap<String, SessionReads>> {
    match crate::config::private_io::read_private_file(path)? {
        None => Ok(HashMap::new()),
        Some(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
            anyhow::anyhow!(
                "read-before-edit store {} is invalid; refusing to treat it as empty: {error}",
                path.display()
            )
        }),
    }
}

fn record_at(
    path: &Path,
    session_id: &str,
    key: String,
    sha256: &str,
    bytes: u64,
    now: i64,
) -> anyhow::Result<()> {
    if !valid_sha256(sha256) {
        anyhow::bail!("invalid read-before-edit SHA-256 receipt");
    }
    crate::config::private_io::read_modify_write_private(path, |current| {
        let mut map: HashMap<String, SessionReads> = match current {
            None | Some([]) => HashMap::new(),
            Some(bytes) => serde_json::from_slice(bytes).map_err(|error| {
                anyhow::anyhow!(
                    "read-before-edit store {} is invalid; refusing to erase it: {error}",
                    path.display()
                )
            })?,
        };
        let entry = map.entry(session_id.to_string()).or_default();
        entry.files.insert(
            key,
            StoredReadReceipt::Current(ReadReceipt {
                sha256: sha256.to_string(),
                bytes,
                recorded_at: now,
            }),
        );
        entry.updated = now;
        if entry.files.len() > MAX_FILES_PER_SESSION {
            let mut by_age: Vec<(String, i64)> = entry
                .files
                .iter()
                .map(|(file, receipt)| (file.clone(), receipt.recorded_at()))
                .collect();
            by_age.sort_by_key(|(_, time)| *time);
            for (file, _) in by_age
                .into_iter()
                .take(entry.files.len() - MAX_FILES_PER_SESSION)
            {
                entry.files.remove(&file);
            }
        }
        if map.len() > MAX_SESSIONS {
            let mut by_age: Vec<(String, i64)> = map
                .iter()
                .map(|(id, session)| (id.clone(), session.updated))
                .collect();
            by_age.sort_by_key(|(_, time)| *time);
            for (id, _) in by_age.into_iter().take(map.len() - MAX_SESSIONS) {
                map.remove(&id);
            }
        }
        Ok(((), serde_json::to_vec_pretty(&map)?))
    })
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Record that `path` was read in `session_id`. The path should already be
/// canonical (use `std::fs::canonicalize` before calling). No-op in tests
/// isolated from the live home.
pub fn record(session_id: &str, canonical_path: &Path, sha256: &str, bytes: u64) {
    if crate::config::test_isolated_from_live_home() {
        return;
    }
    let key = canonical_path.to_string_lossy().to_string();
    let now = chrono::Utc::now().timestamp();
    if let Err(error) = record_at(&store_path(), session_id, key, sha256, bytes, now) {
        tracing::warn!("could not persist read-before-edit receipt: {error:#}");
    }
}

/// Return the content version recorded for `canonical_path`. Legacy path-only
/// receipts intentionally return `None`, forcing one fresh read after upgrade.
pub fn receipt(session_id: &str, canonical_path: &Path) -> Option<ReadReceipt> {
    let key = canonical_path.to_string_lossy().to_string();
    match load_at(&store_path()) {
        Ok(map) => map
            .get(session_id)
            .and_then(|session| session.files.get(&key))
            .and_then(StoredReadReceipt::current)
            .cloned(),
        Err(error) => {
            tracing::warn!("could not read read-before-edit receipts: {error:#}");
            None
        }
    }
}

/// Was `canonical_path` read in `session_id` this session? Returns true if
/// the session has no tracking (None session id) — the guard is skipped for
/// executors without a session, matching the "fresh start" behavior.
pub fn was_read(session_id: Option<&str>, canonical_path: &Path) -> bool {
    let Some(sid) = session_id else {
        return true; // no session tracking → no guard
    };
    receipt(sid, canonical_path).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt_at(path: &Path, session_id: &str, file: &Path) -> Option<ReadReceipt> {
        let key = file.to_string_lossy().to_string();
        load_at(path)
            .unwrap()
            .get(session_id)
            .and_then(|session| session.files.get(&key))
            .and_then(StoredReadReceipt::current)
            .cloned()
    }

    #[test]
    fn record_then_check_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("read_files.json");
        let file = Path::new("/tmp/some/file.rs");
        let hash = format!("sha256:{}", "a".repeat(64));
        record_at(
            &store,
            "main-abc",
            file.to_string_lossy().to_string(),
            &hash,
            7,
            1,
        )
        .unwrap();
        assert_eq!(
            receipt_at(&store, "main-abc", file),
            Some(ReadReceipt {
                sha256: hash,
                bytes: 7,
                recorded_at: 1,
            })
        );
        assert!(receipt_at(&store, "main-abc", Path::new("/tmp/other.rs")).is_none());
    }

    #[test]
    fn unknown_session_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("read_files.json");
        assert!(receipt_at(&store, "main-never", Path::new("/tmp/some/file.rs")).is_none());
    }

    #[test]
    fn none_session_id_skips_guard() {
        assert!(was_read(None, Path::new("/tmp/some/file.rs")));
    }

    #[test]
    fn reads_are_per_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("read_files.json");
        let file = Path::new("/tmp/shared.rs");
        let hash = format!("sha256:{}", "b".repeat(64));
        record_at(
            &store,
            "main-a",
            file.to_string_lossy().to_string(),
            &hash,
            1,
            1,
        )
        .unwrap();
        assert!(receipt_at(&store, "main-a", file).is_some());
        assert!(receipt_at(&store, "main-b", file).is_none());
    }

    #[test]
    fn concurrent_sessions_preserve_both_read_receipts() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("read_files.json");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        std::thread::scope(|scope| {
            for (session, file) in [("main-a", "/tmp/a.rs"), ("main-b", "/tmp/b.rs")] {
                let store = store.clone();
                let barrier = barrier.clone();
                scope.spawn(move || {
                    barrier.wait();
                    let hash = format!("sha256:{}", "c".repeat(64));
                    record_at(&store, session, file.to_string(), &hash, 1, 1).unwrap();
                });
            }
            barrier.wait();
        });
        assert!(receipt_at(&store, "main-a", Path::new("/tmp/a.rs")).is_some());
        assert!(receipt_at(&store, "main-b", Path::new("/tmp/b.rs")).is_some());
    }

    #[test]
    fn corrupt_store_is_preserved_instead_of_reset() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("read_files.json");
        std::fs::write(&store, b"{not json").unwrap();
        let hash = format!("sha256:{}", "d".repeat(64));
        assert!(record_at(&store, "main-a", "/tmp/a.rs".to_string(), &hash, 1, 1).is_err());
        assert_eq!(std::fs::read(store).unwrap(), b"{not json");
    }

    #[test]
    fn legacy_path_only_receipt_never_authorizes_an_edit() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("read_files.json");
        std::fs::write(
            &store,
            br#"{"main":{"files":{"/tmp/a.rs":123},"updated":123}}"#,
        )
        .unwrap();
        assert!(receipt_at(&store, "main", Path::new("/tmp/a.rs")).is_none());
    }
}
