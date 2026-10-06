//! Per-session turn anchors: the workspace + permission mode a session's
//! turns run under.
//!
//! A client turn (TUI/CLI) arrives with the client's cwd and yolo flag. The
//! daemon's INTERNAL wake turns — background-return wakes, goal heartbeats,
//! cron fires — used to hard-code `PermissionMode::Workspace` with the
//! daemon's own cwd as the root. Live failure 2026-07-08 (main-60e78831): a
//! YOLO session running in `evals/.../runs/e2/phoenix` had a specialist finish
//! in the background; the wake turn reverted to daemon-cwd Workspace
//! confinement, so the orchestrator AND the woken computer_use lane got
//! `path escapes workspace` on files inside the session's own folder and
//! verified blind for eight minutes.
//!
//! Fix: every client turn REMEMBERS its anchor here; every internal wake
//! REPLAYS it. Storage is one small JSON map at
//! `~/.phoenix/turn_anchors.json` — deliberately NOT inside `sessions/`
//! (sidecar files in that directory once broke session loading).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::tools::PermissionMode;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TurnAnchor {
    pub workspace: PathBuf,
    /// New three-tier posture. Missing in old files, where `yolo` below is
    /// the migration source.
    #[serde(default)]
    pub mode: Option<PermissionMode>,
    /// Legacy compatibility for Phoenix builds that only understood safe/yolo.
    #[serde(default)]
    pub yolo: bool,
    /// Unix seconds of the last client turn that set this anchor (prune key).
    pub updated: i64,
}

impl TurnAnchor {
    pub fn permission_mode(&self) -> PermissionMode {
        if let Some(mode) = self.mode {
            mode
        } else if self.yolo {
            PermissionMode::FullAccess
        } else {
            PermissionMode::Workspace
        }
    }
}

/// Keep the map from growing without bound: past this many entries the
/// oldest anchors are dropped on save. Sessions past a few hundred are
/// resumable but their wakes just fall back to pre-anchor behavior.
const MAX_ANCHORS: usize = 512;

fn store_path() -> PathBuf {
    crate::config::phoenix_home().join("turn_anchors.json")
}

fn parse_store(bytes: Option<&[u8]>) -> Result<HashMap<String, TurnAnchor>> {
    let Some(bytes) = bytes else {
        return Ok(HashMap::new());
    };
    let map: HashMap<String, TurnAnchor> =
        serde_json::from_slice(bytes).context("turn-anchor store is corrupt")?;
    if map.len() > MAX_ANCHORS.saturating_mul(4) {
        anyhow::bail!(
            "turn-anchor store has {} entries; refusing more than {}",
            map.len(),
            MAX_ANCHORS * 4
        );
    }
    for session_id in map.keys() {
        crate::session::SessionStore::validate_session_id(session_id)
            .with_context(|| format!("turn-anchor store contains invalid id {session_id:?}"))?;
    }
    Ok(map)
}

fn load() -> Result<HashMap<String, TurnAnchor>> {
    let path = store_path();
    let bytes = crate::config::private_io::read_private_file(&path)?;
    parse_store(bytes.as_deref())
}

/// Record the anchor of a CLIENT turn. Call on every user-initiated turn —
/// last write wins, so a session moved to a new directory (or toggled out of
/// yolo) re-anchors on its next real turn.
pub fn remember(session_id: &str, workspace: &Path, yolo: bool) -> Result<()> {
    remember_mode(
        session_id,
        workspace,
        if yolo {
            PermissionMode::FullAccess
        } else {
            PermissionMode::Workspace
        },
    )
}

/// Record a client turn using the current Talk / Workspace / Full Access
/// vocabulary while also writing the old boolean for downgrade compatibility.
pub fn remember_mode(session_id: &str, workspace: &Path, mode: PermissionMode) -> Result<()> {
    if crate::config::test_isolated_from_live_home() {
        return Ok(());
    }
    crate::session::SessionStore::validate_session_id(session_id)
        .with_context(|| format!("turn anchors: invalid session id {session_id:?}"))?;
    let path = store_path();
    let result = crate::config::private_io::read_modify_write_private(&path, |current| {
        let mut map = parse_store(current)?;
        map.insert(
            session_id.to_string(),
            TurnAnchor {
                workspace: workspace.to_path_buf(),
                mode: Some(mode),
                yolo: mode == PermissionMode::FullAccess,
                updated: chrono::Utc::now().timestamp(),
            },
        );
        if map.len() > MAX_ANCHORS {
            let mut by_age: Vec<(String, i64)> = map
                .iter()
                .map(|(id, anchor)| (id.clone(), anchor.updated))
                .collect();
            by_age.sort_by_key(|(_, updated)| *updated);
            for (id, _) in by_age.into_iter().take(map.len() - MAX_ANCHORS) {
                map.remove(&id);
            }
        }
        let replacement = serde_json::to_vec_pretty(&map)?;
        Ok(((), replacement))
    });
    result.with_context(|| format!("turn anchor for {session_id:?} was not saved"))
}

/// The anchor a session's INTERNAL wake turns should replay. None = the
/// session never had a client turn recorded (fresh store, very old session)
/// — callers fall back to the daemon's own cwd + Workspace, the pre-anchor
/// behavior.
pub fn recall(session_id: &str) -> Option<TurnAnchor> {
    if let Err(error) = crate::session::SessionStore::validate_session_id(session_id) {
        tracing::warn!("turn anchors: refusing invalid session id {session_id:?} ({error})");
        return None;
    }
    let anchor = match load() {
        Ok(mut map) => map.remove(session_id)?,
        Err(error) => {
            tracing::warn!("turn anchors: cannot load anchor for {session_id:?}: {error:#}");
            return None;
        }
    };
    // A vanished workspace (deleted checkout, unmounted drive) must not
    // anchor tools to a dead path — better the old fallback than a root
    // every canonicalize() rejects.
    if !anchor.workspace.is_dir() {
        return None;
    }
    Some(anchor)
}

/// `recall` unpacked for the daemon's wake sites: (workspace override,
/// permission mode) with the shared company-workspace default already applied.
pub fn wake_context(session_id: &str) -> (Option<PathBuf>, PermissionMode) {
    let _ = crate::config::ensure_phoenix_home();
    let remembered = recall(session_id).map(|anchor| anchor.permission_mode());
    let configured = configured_mode(session_id);
    let rank = |mode: &PermissionMode| match mode {
        PermissionMode::Talk => 0,
        PermissionMode::Workspace => 1,
        PermissionMode::FullAccess => 2,
    };
    // Scheduled runs, coworker returns and chat-app turns use the access the
    // user gave this coworker (its composer choice, saved as its setting) or
    // the access its conversation last ran with, whichever is higher.
    let mode = [remembered, configured]
        .into_iter()
        .flatten()
        .max_by_key(rank)
        .unwrap_or(PermissionMode::Workspace);
    // Coworkers always work in Phoenix's own workspace.
    (Some(crate::config::phoenix_workspace_root()), mode)
}

/// The access explicitly configured for the coworker (or group) that owns
/// this conversation, if any.
fn configured_mode(session_id: &str) -> Option<PermissionMode> {
    // Inside the gateway the company store is already open; reuse that
    // connection. When the owner cannot be resolved, the company default
    // (Settings → Composer → Default access) still applies.
    let owner_scope = crate::runtime::company::global()
        .ok()
        .and_then(|store| store.directory_snapshot().ok())
        .and_then(|snapshot| owner_scope(&snapshot, session_id));
    let scope = owner_scope.unwrap_or(crate::settings::SettingsScope::Global);
    match crate::settings::effective_string("composer.default_permission", &scope).as_deref() {
        Some("full_access") => Some(PermissionMode::FullAccess),
        Some("talk") => Some(PermissionMode::Talk),
        Some("workspace") => Some(PermissionMode::Workspace),
        _ => None,
    }
}

fn owner_scope(
    snapshot: &crate::runtime::company_directory::DirectorySnapshot,
    session_id: &str,
) -> Option<crate::settings::SettingsScope> {
    snapshot
        .agents
        .iter()
        .find(|agent| agent.profile.canonical_session_id.as_deref() == Some(session_id))
        .map(|agent| crate::settings::SettingsScope::Agent { id: agent.profile.agent_id.clone() })
        .or_else(|| {
            snapshot
                .groups
                .iter()
                .find(|group| group.profile.canonical_session_id.as_deref() == Some(session_id))
                .map(|group| crate::settings::SettingsScope::Group { id: group.profile.group_id.clone() })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_isolated_home<T>(test: impl FnOnce() -> T) -> T {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(dir.path());
        test()
    }

    #[test]
    fn remember_then_recall_roundtrips_workspace_and_mode() {
        with_isolated_home(|| {
            let workspace = std::env::temp_dir();
            remember("main-abc", &workspace, true).unwrap();
            let anchor = recall("main-abc").expect("anchor should be recorded");
            assert_eq!(anchor.workspace, workspace);
            assert!(anchor.yolo);
            assert_eq!(anchor.permission_mode(), PermissionMode::FullAccess);

            // Wakes keep the remembered access but always use Phoenix's own
            // workspace (the user's rule: coworkers always work there).
            let (ws, mode) = wake_context("main-abc");
            assert_eq!(ws, Some(crate::config::phoenix_workspace_root()));
            assert_eq!(mode, PermissionMode::FullAccess);
        });
    }

    #[test]
    fn talk_mode_roundtrips_and_legacy_yolo_files_still_migrate() {
        with_isolated_home(|| {
            let workspace = std::env::temp_dir();
            remember_mode("main-talk", &workspace, PermissionMode::Talk).unwrap();
            let talk = recall("main-talk").expect("talk anchor should be recorded");
            assert_eq!(talk.permission_mode(), PermissionMode::Talk);
            assert!(!talk.yolo);

            let legacy: TurnAnchor = serde_json::from_value(serde_json::json!({
                "workspace": workspace,
                "yolo": true,
                "updated": 1
            }))
            .unwrap();
            assert_eq!(legacy.permission_mode(), PermissionMode::FullAccess);
        });
    }

    #[test]
    fn unknown_session_falls_back_to_shared_workspace() {
        with_isolated_home(|| {
            assert!(recall("main-never-seen").is_none());
            let (ws, mode) = wake_context("main-never-seen");
            assert_eq!(
                ws.as_deref(),
                Some(crate::config::phoenix_workspace_root().as_path())
            );
            assert_eq!(mode, PermissionMode::Workspace);
        });
    }

    #[test]
    fn vanished_workspace_falls_back_to_shared_workspace() {
        with_isolated_home(|| {
            let gone = std::env::temp_dir().join("phx-anchor-gone-dir");
            std::fs::create_dir_all(&gone).unwrap();
            remember("main-gone", &gone, true).unwrap();
            std::fs::remove_dir_all(&gone).unwrap();
            assert!(recall("main-gone").is_none());
            let (ws, mode) = wake_context("main-gone");
            assert_eq!(
                ws.as_deref(),
                Some(crate::config::phoenix_workspace_root().as_path())
            );
            assert_eq!(mode, PermissionMode::Workspace);
        });
    }

    #[test]
    fn last_client_turn_wins() {
        with_isolated_home(|| {
            let first = std::env::temp_dir();
            remember("main-move", &first, true).unwrap();
            let second = std::env::temp_dir().join("phx-anchor-second");
            std::fs::create_dir_all(&second).unwrap();
            remember("main-move", &second, false).unwrap();
            let anchor = recall("main-move").expect("anchor present");
            assert_eq!(anchor.workspace, second);
            assert!(!anchor.yolo);
            std::fs::remove_dir_all(&second).ok();
        });
    }

    #[test]
    fn concurrent_sessions_do_not_lose_anchors() {
        with_isolated_home(|| {
            let workspace = std::env::temp_dir();
            std::thread::scope(|scope| {
                for index in 0..24 {
                    let workspace = workspace.clone();
                    scope.spawn(move || {
                        remember(
                            &format!("main-concurrent-{index}"),
                            &workspace,
                            index % 2 == 0,
                        )
                        .unwrap();
                    });
                }
            });
            let map = load().expect("valid anchor store");
            assert_eq!(map.len(), 24);
        });
    }

    #[test]
    fn corrupt_store_is_preserved_instead_of_replaced() {
        with_isolated_home(|| {
            let path = store_path();
            crate::config::private_io::atomic_write_private(&path, b"{not-json").unwrap();
            assert!(remember("main-safe", &std::env::temp_dir(), false).is_err());
            assert_eq!(
                crate::config::private_io::read_private_file(&path)
                    .unwrap()
                    .unwrap(),
                b"{not-json"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn store_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        with_isolated_home(|| {
            remember("main-private", &std::env::temp_dir(), false).unwrap();
            let mode = std::fs::metadata(store_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        });
    }
}
