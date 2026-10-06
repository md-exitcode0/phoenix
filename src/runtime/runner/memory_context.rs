use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::librarian::SessionCache;
use crate::runtime::{LibrarianPassRecord, MemoryBundle, SessionScope};
use crate::session::SessionStore;

/// A pinned memory is prompt material, not a bulk-document store. Bounding
/// refreshes keeps a replaced file from consuming unbounded memory while the
/// durable last-known copy remains available to the session.
const MAX_PINNED_MEMORY_BYTES: u64 = 1024 * 1024;

fn read_pinned_memory(path: &Path) -> Result<String> {
    crate::config::private_io::reject_symlink_components(path)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .with_context(|| format!("failed to open pinned memory {}", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect pinned memory {}", path.display()))?;
    if !metadata.is_file() {
        anyhow::bail!("pinned memory is not a regular file: {}", path.display());
    }
    if metadata.len() > MAX_PINNED_MEMORY_BYTES {
        anyhow::bail!(
            "pinned memory {} is {} bytes; maximum is {MAX_PINNED_MEMORY_BYTES}",
            path.display(),
            metadata.len()
        );
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_PINNED_MEMORY_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read pinned memory {}", path.display()))?;
    if bytes.len() as u64 > MAX_PINNED_MEMORY_BYTES {
        anyhow::bail!(
            "pinned memory {} grew beyond {MAX_PINNED_MEMORY_BYTES} bytes while reading",
            path.display()
        );
    }
    String::from_utf8(bytes)
        .with_context(|| format!("pinned memory is not UTF-8: {}", path.display()))
}

/// Short content hash for change-detection on pinned memory files.
pub(crate) fn short_memory_hash(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hasher
        .finalize()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Pin the librarian's freshly-loaded memories into the durable orchestrator
/// transcript so they ride forward across turns (the agent stops re-reading the
/// same files every message), refresh any already-pinned memory whose source
/// file changed on disk, record the pinned set for the next preload's
/// "already-in-context" hint, and clear the ephemeral copies so the prompt does
/// not double-render them.
pub(crate) fn pin_loaded_memories_into_session(
    store: &mut SessionStore,
    session_id: &str,
    memory_root: &std::path::Path,
    loaded: &mut crate::librarian::LoadedMemories,
    cache: &mut SessionCache,
) {
    let Some(session) = store.get_mut(session_id) else {
        return;
    };
    // Re-verify each already-pinned memory against disk: refresh on edit, keep
    // the last-known copy if the file can't be re-read (never silently lose it).
    let mut refreshed = Vec::new();
    for pinned in std::mem::take(&mut session.pinned_memory) {
        let rel = pinned.path.strip_prefix("memory/").unwrap_or(&pinned.path);
        let source = memory_root.join(rel);
        match read_pinned_memory(&source) {
            Ok(content) => {
                let hash = short_memory_hash(&content);
                refreshed.push(crate::session::PinnedMemory {
                    path: pinned.path,
                    hash,
                    content,
                });
            }
            Err(error) => {
                tracing::warn!(
                    path = %source.display(),
                    error = %format_args!("{error:#}"),
                    "keeping last-known pinned memory after refresh failure"
                );
                refreshed.push(pinned);
            }
        }
    }
    session.pinned_memory = refreshed;
    // Pin the freshly-loaded memories (pin_memory refreshes a changed one).
    for memory in &loaded.memories {
        let hash = short_memory_hash(&memory.content);
        session.pin_memory(&memory.path, &hash, &memory.content);
    }
    cache.persisted_memory = session
        .pinned_memory
        .iter()
        .map(|memory| memory.path.clone())
        .collect();
    loaded.memories.clear();
}

pub(crate) fn default_state_root(memory_root: &std::path::Path) -> PathBuf {
    // The configured home need not be named `.phoenix` (isolated gateways,
    // portable installs). Match its actual memory path before legacy layout
    // inference, or the runner and daemon persist into different databases.
    if memory_root == crate::config::paths::phoenix_memory_root() {
        return crate::config::phoenix_home();
    }
    if memory_root.file_name().and_then(|name| name.to_str()) == Some("memory") {
        if let Some(parent) = memory_root.parent() {
            // memory_root is `<state>/memory`; the Phoenix state root is its
            // parent. When the parent is already the `.phoenix` home (e.g.
            // `~/.phoenix/memory`), use it directly instead of appending a
            // second `.phoenix` segment.
            if parent.file_name().and_then(|name| name.to_str()) == Some(".phoenix") {
                return parent.to_path_buf();
            }
            return parent.join(".phoenix");
        }
    }
    memory_root.join(".phoenix")
}

#[cfg(test)]
#[test]
fn configured_state_home_does_not_gain_an_extra_phoenix_directory() {
    let home = tempfile::tempdir().unwrap();
    let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
    assert_eq!(default_state_root(&home.path().join("memory")), home.path());
    assert_eq!(default_state_root(std::path::Path::new("/legacy/project/memory")),
        std::path::Path::new("/legacy/project/.phoenix"));
}

/// True for pure pleasantry/acknowledgment messages — every word is phatic
/// vocabulary and the message is short. Conservative on purpose: one
/// non-phatic word ("thanks, now fix the tests") and the gate stays open.
pub(crate) fn is_phatic_message(message: &str) -> bool {
    let normalized: String = message
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '\'' {
                c
            } else {
                ' '
            }
        })
        .collect();
    let words: Vec<&str> = normalized.split_whitespace().collect();
    if words.is_empty() || words.len() > 8 {
        return false;
    }
    const PHATIC: &[&str] = &[
        "thanks", "thank", "you", "ty", "thx", "tysm", "ok", "okay", "k", "kk", "cool", "nice",
        "great", "awesome", "amazing", "perfect", "dope", "sweet", "good", "job", "well", "done",
        "bro", "man", "wow", "lol", "haha", "bye", "goodbye", "night", "gn", "hi", "hello", "hey",
        "yo", "sup", "morning", "gm", "gg", "wp", "love", "it", "that's", "that", "this", "is",
        "was", "really", "super", "so", "very", "much", "a", "lot", "works", "worked", "work",
    ];
    words.iter().all(|w| PHATIC.contains(w))
}

pub(crate) fn with_preload_context(
    mut save_record: LibrarianPassRecord,
    bundle: &MemoryBundle,
) -> LibrarianPassRecord {
    save_record.memory_paths = bundle.loaded_memory_paths.clone();
    save_record.knowledge_paths = bundle.loaded_knowledge_paths.clone();
    save_record.summary = honest_save_summary(&save_record);
    save_record
}

/// Build a truthful save-phase status line. The librarian falls back to a no-op
/// when its model returns unparseable JSON; in that case nothing is persisted, so
/// we must not claim "save completed".
pub(crate) fn honest_save_summary(save_record: &LibrarianPassRecord) -> String {
    let scope = match &save_record.session_scope {
        SessionScope::Main => "Main-session",
        SessionScope::Specialist(_) => "Specialist-session",
    };
    let unparseable = save_record.receipts.iter().any(|receipt| {
        receipt.contains("did not return parseable JSON") || receipt.contains("safe no-op")
    });
    if !save_record.saved_memory_paths.is_empty() {
        format!(
            "{scope} librarian saved {} memory file(s).",
            save_record.saved_memory_paths.len()
        )
    } else if unparseable {
        format!("{scope} librarian save skipped — model output unparseable; nothing was written.")
    } else {
        format!("{scope} librarian save: nothing durable to persist.")
    }
}

#[cfg(all(test, unix))]
mod bounded_refresh_tests {
    use super::*;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn pinned_memory_refresh_rejects_symlink_and_keeps_last_known_copy() {
        let dir = tempfile::tempdir().unwrap();
        let memory_root = dir.path().join("memory");
        std::fs::create_dir(&memory_root).unwrap();
        let outside = dir.path().join("outside.md");
        std::fs::write(&outside, "replacement").unwrap();
        std::os::unix::fs::symlink(&outside, memory_root.join("note.md")).unwrap();

        let mut store = SessionStore::new(dir.path().join("sessions"));
        store
            .load_or_create_main("session", "model", "prompt")
            .unwrap();
        store.get_mut("session").unwrap().pinned_memory = vec![crate::session::PinnedMemory {
            path: "memory/note.md".into(),
            hash: short_memory_hash("last known"),
            content: "last known".into(),
        }];
        let mut loaded = crate::librarian::LoadedMemories::default();
        let mut cache = SessionCache::new("session");

        pin_loaded_memories_into_session(
            &mut store,
            "session",
            &memory_root,
            &mut loaded,
            &mut cache,
        );

        let pinned = &store.get("session").unwrap().pinned_memory[0];
        assert_eq!(pinned.content, "last known");
        assert_eq!(pinned.hash, short_memory_hash("last known"));
    }

    #[test]
    fn pinned_memory_refresh_rejects_fifo_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("memory.pipe");
        let fifo_c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);

        let started = std::time::Instant::now();
        assert!(read_pinned_memory(&fifo).is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn pinned_memory_refresh_rejects_oversize_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oversize.md");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_PINNED_MEMORY_BYTES + 1).unwrap();

        assert!(read_pinned_memory(&path).is_err());
    }
}
