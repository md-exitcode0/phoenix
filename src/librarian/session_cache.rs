//! Session cache management

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const MAX_SESSION_CACHE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCache {
    pub session_id: String,
    pub created: chrono::DateTime<chrono::Utc>,
    pub last_updated: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub loaded: Vec<String>,
    #[serde(default)]
    pub new_this_session: Vec<String>,
    #[serde(default)]
    pub skipped_memory_titles: Vec<String>,
    #[serde(default)]
    pub receipt_log: Vec<String>,
    #[serde(default)]
    pub last_injected_paths: Vec<String>,
    #[serde(default)]
    pub last_omitted_items: Vec<String>,
    #[serde(default)]
    pub last_grounding_receipts: Vec<String>,
    #[serde(default)]
    pub last_completion_state: String,
    #[serde(default)]
    pub last_recommended_next: Option<String>,
    #[serde(default)]
    pub last_pruned_message_indices: Vec<usize>,
    #[serde(default)]
    pub last_pruned_receipts: Vec<String>,
    /// Memory paths PINNED into the orchestrator transcript (durable, carried
    /// across turns). The librarian is told these are already in the agent's
    /// context so it stops re-reading the same files every turn.
    #[serde(default)]
    pub persisted_memory: Vec<String>,
}

impl SessionCache {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            created: chrono::Utc::now(),
            last_updated: chrono::Utc::now(),
            loaded: vec![],
            new_this_session: vec![],
            skipped_memory_titles: vec![],
            receipt_log: vec![],
            last_injected_paths: vec![],
            last_omitted_items: vec![],
            last_grounding_receipts: vec![],
            last_completion_state: String::new(),
            last_recommended_next: None,
            last_pruned_message_indices: vec![],
            last_pruned_receipts: vec![],
            persisted_memory: vec![],
        }
    }

    pub fn load_or_create(root: &Path, session_id: &str) -> Result<Self> {
        crate::session::SessionStore::validate_session_id(session_id)
            .context("invalid session-cache id")?;
        let path = Self::path_for(root, session_id);
        let Some(content) = crate::config::private_io::read_private_file(&path)? else {
            return Ok(Self::new(session_id));
        };
        if content.len() > MAX_SESSION_CACHE_BYTES {
            anyhow::bail!(
                "session cache {} is {} bytes; maximum is {MAX_SESSION_CACHE_BYTES}",
                path.display(),
                content.len()
            );
        }
        let cache: Self = serde_json::from_slice(&content)
            .with_context(|| format!("invalid session cache {}", path.display()))?;
        if cache.session_id != session_id {
            anyhow::bail!(
                "session cache {} belongs to `{}`, not `{session_id}`",
                path.display(),
                cache.session_id
            );
        }
        Ok(cache)
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        crate::session::SessionStore::validate_session_id(&self.session_id)
            .context("invalid session-cache id")?;
        let path = Self::path_for(root, &self.session_id);
        let proposed = serde_json::to_vec_pretty(self)?;
        if proposed.len() > MAX_SESSION_CACHE_BYTES {
            anyhow::bail!(
                "session cache {} serializes to {} bytes; maximum is {MAX_SESSION_CACHE_BYTES}",
                self.session_id,
                proposed.len()
            );
        }
        // Refuse to erase a corrupt or mismatched cache. The lock spans the
        // check and replacement, so a concurrent process cannot swap the
        // inode between them.
        crate::config::private_io::read_modify_write_private(&path, |current| {
            let replacement = if let Some(current) = current {
                let stored: SessionCache = serde_json::from_slice(current)
                    .with_context(|| format!("invalid session cache {}", path.display()))?;
                if stored.session_id != self.session_id {
                    anyhow::bail!(
                        "session cache {} belongs to `{}`, not `{}`",
                        path.display(),
                        stored.session_id,
                        self.session_id
                    );
                }
                merge_concurrent_cache(stored, self.clone())
            } else {
                self.clone()
            };
            let json = serde_json::to_vec_pretty(&replacement)?;
            if json.len() > MAX_SESSION_CACHE_BYTES {
                anyhow::bail!(
                    "merged session cache {} is {} bytes; maximum is {MAX_SESSION_CACHE_BYTES}",
                    self.session_id,
                    json.len()
                );
            }
            Ok(((), json))
        })
        .with_context(|| format!("failed to save session cache {}", path.display()))
    }

    pub fn path_for(root: &Path, session_id: &str) -> PathBuf {
        root.join(format!("{session_id}.json"))
    }
}

/// A delayed writer must not roll a cache back to an older turn snapshot.
/// Most fields describe the latest pass, so the greater `last_updated` wins.
/// `receipt_log` is append history; merge divergent suffixes after their
/// common prefix so concurrent pass receipts both survive.
fn merge_concurrent_cache(stored: SessionCache, proposed: SessionCache) -> SessionCache {
    let (older, newer) = if stored.last_updated <= proposed.last_updated {
        (&stored, &proposed)
    } else {
        (&proposed, &stored)
    };
    let common = older
        .receipt_log
        .iter()
        .zip(&newer.receipt_log)
        .take_while(|(left, right)| left == right)
        .count();
    let mut merged_receipts = older.receipt_log.clone();
    merged_receipts.extend(newer.receipt_log[common..].iter().cloned());

    let mut merged = newer.clone();
    merged.created = stored.created.min(proposed.created);
    merged.receipt_log = merged_receipts;
    merged
}

impl Default for SessionCache {
    fn default() -> Self {
        Self::new(uuid::Uuid::new_v4().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_ids_and_corrupt_existing_cache_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(SessionCache::load_or_create(dir.path(), "../escape").is_err());

        let path = dir.path().join("main-safe.json");
        crate::config::private_io::atomic_write_private(&path, b"{not-json").unwrap();
        let cache = SessionCache::new("main-safe");
        assert!(cache.save(dir.path()).is_err());
        assert_eq!(
            crate::config::private_io::read_private_file(&path)
                .unwrap()
                .unwrap(),
            b"{not-json"
        );
    }

    #[test]
    fn delayed_save_cannot_roll_back_newer_snapshot_and_receipts_merge() {
        let dir = tempfile::tempdir().unwrap();
        let mut older = SessionCache::new("main-race");
        older.loaded = vec!["old".into()];
        older.receipt_log = vec!["older receipt".into()];
        let mut newer = older.clone();
        newer.last_updated = older.last_updated + chrono::Duration::seconds(1);
        newer.loaded = vec!["new".into()];
        newer.receipt_log = vec!["newer receipt".into()];

        newer.save(dir.path()).unwrap();
        older.save(dir.path()).unwrap();
        let loaded = SessionCache::load_or_create(dir.path(), "main-race").unwrap();
        assert_eq!(loaded.loaded, vec!["new".to_string()]);
        assert_eq!(
            loaded.receipt_log,
            vec!["older receipt".to_string(), "newer receipt".to_string()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn cache_is_owner_only_and_symlinks_are_rejected() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let dir = tempfile::tempdir().unwrap();
        let cache = SessionCache::new("main-private");
        cache.save(dir.path()).unwrap();
        let path = SessionCache::path_for(dir.path(), "main-private");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let outside = dir.path().join("outside.json");
        std::fs::write(&outside, "untouched").unwrap();
        let linked = SessionCache::path_for(dir.path(), "main-linked");
        symlink(&outside, &linked).unwrap();
        assert!(SessionCache::new("main-linked").save(dir.path()).is_err());
        assert!(SessionCache::load_or_create(dir.path(), "main-linked").is_err());
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "untouched");
    }
}
