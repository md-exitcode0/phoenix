//! Feed snapshots — resume shows EXACTLY what the user left.
//!
//! The old resume path re-derived the journal from session transcripts
//! (splicing specialist chunks under hand-off rows), which could never be
//! pixel-faithful: message order in a transcript is the orchestrator's
//! context order, not the display order, and chunks whose hand-off fell
//! outside the replay window got appended at the END of the feed — the
//! "why do I see specialist returns below my messages" bug.
//!
//! This module removes the reconstruction problem instead of patching it:
//! the rendered feed itself is persisted (debounced, plus at turn end and
//! quit) to `sessions/<id>.feed.json`, and resume reloads it verbatim.
//! Transcript reconstruction remains only as the fallback for sessions
//! that predate snapshots, and for messages that landed AFTER the snapshot
//! (background returns while the TUI was closed), which append at the
//! bottom — chronologically where they belong.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::support::state_root;
use super::FeedItem;

/// Bump when `PersistedFeedItem` changes shape incompatibly; a mismatched
/// snapshot is ignored (falls back to transcript reconstruction), never
/// half-parsed.
const VERSION: u32 = 1;

/// Keep the persisted journal bounded: the newest rows win. 2000 rows is
/// far deeper than any terminal scrollback session actually revisits.
const MAX_ITEMS: usize = 2000;
/// A row count alone is not a byte bound: one diff or answer can contain an
/// arbitrarily large tool transcript. Oversized snapshots are skipped while
/// preserving the last good file; the canonical session transcript remains
/// the resume fallback.
const MAX_FEED_BYTES: usize = 16 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
pub(super) struct PersistedFeed {
    pub version: u32,
    /// Orders concurrent TUI writers for the same session. Older clients may
    /// finish a debounced save after a newer client; the lock-scoped write
    /// below must not roll the visible feed backward.
    #[serde(default)]
    saved_at_unix_micros: i64,
    /// Number of main-transcript messages already represented in `items` —
    /// resume replays only messages beyond this, so work that finished while
    /// the TUI was closed still shows up (at the bottom, where it happened).
    pub main_watermark: usize,
    pub items: Vec<PersistedFeedItem>,
}

/// Owned mirror of `FeedItem` (whose activity symbol is `&'static str`).
#[derive(Serialize, Deserialize)]
pub(super) enum PersistedFeedItem {
    User(String),
    Activity {
        agent: String,
        symbol: String,
        text: String,
        ok: Option<bool>,
    },
    Answer(String),
    Diff {
        agent: String,
        diff: String,
    },
    AgentReturn {
        agent: String,
        subject: String,
        body: String,
        ok: bool,
    },
    Notice(String),
    Error(String),
    Blank,
}

impl From<&FeedItem> for PersistedFeedItem {
    fn from(item: &FeedItem) -> Self {
        match item {
            FeedItem::User(text) => Self::User(text.clone()),
            FeedItem::Activity {
                agent,
                symbol,
                text,
                ok,
            } => Self::Activity {
                agent: agent.clone(),
                symbol: (*symbol).to_string(),
                text: text.clone(),
                ok: *ok,
            },
            FeedItem::Answer(text) => Self::Answer(text.clone()),
            FeedItem::Diff { agent, diff } => Self::Diff {
                agent: agent.clone(),
                diff: diff.clone(),
            },
            FeedItem::AgentReturn {
                agent,
                subject,
                body,
                ok,
            } => Self::AgentReturn {
                agent: agent.clone(),
                subject: subject.clone(),
                body: body.clone(),
                ok: *ok,
            },
            FeedItem::Notice(text) => Self::Notice(text.clone()),
            FeedItem::Error(text) => Self::Error(text.clone()),
            FeedItem::Blank => Self::Blank,
        }
    }
}

impl From<PersistedFeedItem> for FeedItem {
    fn from(item: PersistedFeedItem) -> Self {
        match item {
            PersistedFeedItem::User(text) => Self::User(text),
            PersistedFeedItem::Activity {
                agent,
                symbol,
                text,
                ok,
            } => Self::Activity {
                agent,
                symbol: intern_symbol(&symbol),
                text,
                ok,
            },
            PersistedFeedItem::Answer(text) => Self::Answer(text),
            PersistedFeedItem::Diff { agent, diff } => Self::Diff { agent, diff },
            PersistedFeedItem::AgentReturn {
                agent,
                subject,
                body,
                ok,
            } => Self::AgentReturn {
                agent,
                subject,
                body,
                ok,
            },
            PersistedFeedItem::Notice(text) => Self::Notice(text),
            PersistedFeedItem::Error(text) => Self::Error(text),
            PersistedFeedItem::Blank => Self::Blank,
        }
    }
}

/// Map a persisted symbol back to the static set the renderer draws.
/// Unknown symbols (from a future version) degrade to the neutral dot.
fn intern_symbol(symbol: &str) -> &'static str {
    for known in ["·", "∴", "⧉", "✘", "✖", "⚿", "≡", "◆"] {
        if symbol == known {
            return known;
        }
    }
    "·"
}

/// Feeds live in their OWN directory, never in `sessions/`: the session
/// store parses every `.json` there as a `Session`, and a feed snapshot in
/// that directory killed every gateway turn with `missing field id`
/// (live incident 2026-07-08, the file was byte-for-byte the error column).
pub(super) fn feed_file(session_id: &str) -> std::path::PathBuf {
    state_root()
        .join("feeds")
        .join(format!("{session_id}.feed.json"))
}

/// Persist the rendered feed for `session_id`. Atomic (tmp + rename) so a
/// crash mid-write can only ever lose this save, not corrupt the last one.
/// Snapshot failure never aborts the live turn, but it is returned to the TUI
/// so the user is not told/led to believe the visible journal is durable.
pub(super) fn save(session_id: &str, main_watermark: usize, feed: &[FeedItem]) -> Result<()> {
    // The receipts guard's twin: test builds must never write snapshots into
    // the live ~/.phoenix (see config::test_isolated_from_live_home).
    if crate::config::test_isolated_from_live_home() {
        return Ok(());
    }
    crate::session::SessionStore::validate_session_id(session_id)
        .with_context(|| format!("feed snapshot has invalid session id {session_id:?}"))?;
    save_at(&feed_file(session_id), main_watermark, feed)
        .with_context(|| format!("feed snapshot for {session_id:?} was not saved"))
}

fn save_at(path: &std::path::Path, main_watermark: usize, feed: &[FeedItem]) -> Result<()> {
    let skip = feed.len().saturating_sub(MAX_ITEMS);
    let snapshot = PersistedFeed {
        version: VERSION,
        saved_at_unix_micros: chrono::Utc::now().timestamp_micros(),
        main_watermark,
        items: feed[skip..].iter().map(PersistedFeedItem::from).collect(),
    };
    let json = serde_json::to_vec(&snapshot).context("serializing feed snapshot")?;
    if json.len() > MAX_FEED_BYTES {
        anyhow::bail!(
            "feed snapshot is {} bytes; maximum is {MAX_FEED_BYTES}",
            json.len()
        );
    }
    crate::config::private_io::read_modify_write_private(path, |current| {
        if let Some(current) = current {
            if current.len() > MAX_FEED_BYTES {
                anyhow::bail!(
                    "existing feed snapshot {} is {} bytes; maximum is {MAX_FEED_BYTES}",
                    path.display(),
                    current.len()
                );
            }
            let stored: PersistedFeed = serde_json::from_slice(current)
                .with_context(|| format!("existing feed snapshot {} is corrupt", path.display()))?;
            if stored.version != VERSION {
                anyhow::bail!(
                    "refusing to overwrite feed snapshot {} with incompatible version {}",
                    path.display(),
                    stored.version
                );
            }
            if stored.saved_at_unix_micros > snapshot.saved_at_unix_micros {
                return Ok(((), current.to_vec()));
            }
        }
        Ok(((), json.clone()))
    })
    .with_context(|| format!("writing feed snapshot {}", path.display()))
}

/// Load the feed snapshot for `session_id`; None means "no usable snapshot"
/// (missing, corrupt, or from an incompatible version) and the caller falls
/// back to transcript reconstruction.
pub(super) fn load(session_id: &str) -> Option<PersistedFeed> {
    if crate::config::test_isolated_from_live_home() {
        return None;
    }
    if let Err(error) = crate::session::SessionStore::validate_session_id(session_id) {
        tracing::warn!("feed snapshot: refusing invalid session id {session_id:?} ({error})");
        return None;
    }
    match load_at(&feed_file(session_id)) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            tracing::warn!("feed snapshot for {session_id:?} is unusable: {error:#}");
            None
        }
    }
}

fn load_at(path: &std::path::Path) -> Result<Option<PersistedFeed>> {
    let Some(content) = crate::config::private_io::read_private_file(path)? else {
        return Ok(None);
    };
    if content.len() > MAX_FEED_BYTES {
        anyhow::bail!(
            "feed snapshot {} is {} bytes; maximum is {MAX_FEED_BYTES}",
            path.display(),
            content.len()
        );
    }
    let snapshot: PersistedFeed = serde_json::from_slice(&content)
        .with_context(|| format!("parsing feed snapshot {}", path.display()))?;
    Ok((snapshot.version == VERSION).then_some(snapshot))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_feed() -> Vec<FeedItem> {
        vec![
            FeedItem::Notice("Phoenix — your whole-computer agent inbox.".into()),
            FeedItem::User("fix the countdown page".into()),
            FeedItem::Activity {
                agent: "tester".into(),
                symbol: "⧉",
                text: "orchestrator → tester: verify".into(),
                ok: None,
            },
            FeedItem::Diff {
                agent: "tester".into(),
                diff: "@@ verify.test.mjs\n- old\n+ new".into(),
            },
            FeedItem::AgentReturn {
                agent: "tester".into(),
                subject: "verified".into(),
                body: "all green".into(),
                ok: true,
            },
            FeedItem::Blank,
            FeedItem::Answer("Done — verified green.".into()),
        ]
    }

    /// Round-trip through the persisted form preserves every row in order —
    /// the entire point of the feature.
    #[test]
    fn round_trip_preserves_rows_verbatim() {
        let feed = sample_feed();
        let persisted: Vec<PersistedFeedItem> = feed.iter().map(PersistedFeedItem::from).collect();
        let json = serde_json::to_string(&persisted).unwrap();
        let back: Vec<PersistedFeedItem> = serde_json::from_str(&json).unwrap();
        let restored: Vec<FeedItem> = back.into_iter().map(FeedItem::from).collect();
        assert_eq!(feed.len(), restored.len());
        for (a, b) in feed.iter().zip(&restored) {
            assert_eq!(
                std::mem::discriminant(a),
                std::mem::discriminant(b),
                "row kind changed in round-trip"
            );
        }
        match (&feed[3], &restored[3]) {
            (FeedItem::Diff { diff: a, .. }, FeedItem::Diff { diff: b, .. }) => assert_eq!(a, b),
            _ => panic!("diff row lost"),
        }
        match (&feed[2], &restored[2]) {
            (FeedItem::Activity { symbol: a, .. }, FeedItem::Activity { symbol: b, .. }) => {
                assert_eq!(a, b, "symbol must intern back to the same glyph")
            }
            _ => panic!("activity row lost"),
        }
    }

    #[test]
    fn unknown_symbols_degrade_to_neutral_dot() {
        assert_eq!(intern_symbol("☄"), "·");
        assert_eq!(intern_symbol("⧉"), "⧉");
    }

    /// Full disk round-trip through an explicit path (no env involved):
    /// watermark survives, the journal is tail-capped, newest rows win.
    #[test]
    fn save_and_load_round_trip_with_tail_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.feed.json");
        let mut feed = sample_feed();
        for i in 0..(MAX_ITEMS + 50) {
            feed.push(FeedItem::Notice(format!("row {i}")));
        }
        save_at(&path, 7, &feed).unwrap();
        let snapshot = load_at(&path)
            .expect("snapshot read should succeed")
            .expect("snapshot should load");
        assert_eq!(snapshot.main_watermark, 7);
        assert_eq!(snapshot.items.len(), MAX_ITEMS, "tail-capped");
        match snapshot.items.last() {
            Some(PersistedFeedItem::Notice(text)) => {
                assert_eq!(text, &format!("row {}", MAX_ITEMS + 49))
            }
            _ => panic!("newest row must survive the cap"),
        }
        // Overwrite is atomic-in-effect: a second save fully replaces.
        save_at(&path, 9, &feed[..3]).unwrap();
        let second = load_at(&path)
            .expect("snapshot read should succeed")
            .expect("second snapshot should load");
        assert_eq!(second.main_watermark, 9);
        assert_eq!(second.items.len(), 3);
    }

    #[test]
    fn corrupt_or_mismatched_snapshot_loads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        // Missing file.
        assert!(load_at(&dir.path().join("missing.feed.json"))
            .unwrap()
            .is_none());
        // Corrupt JSON.
        let corrupt = dir.path().join("corrupt.feed.json");
        std::fs::write(&corrupt, "{not json").unwrap();
        assert!(load_at(&corrupt).is_err());
        // Version from the future: parses but is rejected whole.
        let future = dir.path().join("future.feed.json");
        let snapshot = PersistedFeed {
            version: VERSION + 1,
            saved_at_unix_micros: 0,
            main_watermark: 0,
            items: Vec::new(),
        };
        std::fs::write(&future, serde_json::to_string(&snapshot).unwrap()).unwrap();
        assert!(load_at(&future).unwrap().is_none());
    }

    #[test]
    fn corrupt_snapshot_is_preserved_instead_of_silently_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.feed.json");
        crate::config::private_io::atomic_write_private(&path, b"{broken").unwrap();
        assert!(save_at(&path, 1, &sample_feed()).is_err());
        assert_eq!(
            crate::config::private_io::read_private_file(&path)
                .unwrap()
                .unwrap(),
            b"{broken"
        );
    }

    #[cfg(unix)]
    #[test]
    fn snapshots_are_private_and_symlinks_are_rejected() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private.feed.json");
        save_at(&path, 1, &sample_feed()).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let outside = dir.path().join("outside");
        std::fs::write(&outside, "untouched").unwrap();
        let link = dir.path().join("linked.feed.json");
        symlink(&outside, &link).unwrap();
        assert!(save_at(&link, 1, &sample_feed()).is_err());
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "untouched");
        assert!(load_at(&link).is_err());
    }
}
