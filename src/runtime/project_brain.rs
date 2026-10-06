//! The project brain — tiered cross-thread memory.
//!
//! Phoenix work happens across several threads inside one project: a main
//! (orchestrator) thread and its specialist side-threads (coder, browser, …),
//! all sharing a mesh key (`main-<uuid>`; a specialist is `main-<uuid>__coder`).
//! By default each thread sees ONLY its own transcript, so the main thread
//! forgot what a specialist did the moment its window scrolled off, and a
//! specialist never knew the project's spine.
//!
//! This module assembles, per turn, a bounded digest of the OTHER threads so a
//! thread never loses their work — without re-feeding their full transcripts
//! (which would bloat every turn). It is TIERED so it stays fresh and small:
//!
//!   * The **main thread** sees each side-thread, most-recently-active first:
//!     the top few in detail, the rest collapsed to one line.
//!   * A **side-thread** sees the MAIN thread in full detail (the spine to align
//!     with) plus its siblings as one-liners (awareness, not noise).
//!
//! Digests reuse compaction's own outputs: a thread's folded-history summary
//! (the `[AUTO-COMPACTED HISTORY …]` continuation) plus a deterministic
//! `receipt_digest` of its recent tail. So the brain rides on top of the same
//! compaction that keeps each thread lean — never forgets, never bloats.

use std::time::SystemTime;

use crate::session::{Message, Session, SessionKind, SessionStore};

/// The first line of a compaction continuation summary (kept in sync with
/// `compaction::CONTINUATION_MARKER`, which is private there).
const ANCHOR_MARKER: &str = "[AUTO-COMPACTED HISTORY";

/// Char caps: a full digest, a one-liner, and the whole block (so the brain can
/// never itself become the bloat it exists to prevent).
const FULL_CAP: usize = 1400;
const LINE_CAP: usize = 200;
const BLOCK_CAP: usize = 6000;
/// Side-threads shown in FULL (most-recent first) before the rest collapse to
/// one line. The main thread is always full for a side-thread regardless.
const FULL_SIBLINGS: usize = 2;

/// The mesh/project key for a session id: everything before the `__specialist`
/// suffix. `main-abc__coder` and `main-abc` share key `main-abc`.
pub fn project_key(session_id: &str) -> &str {
    session_id.split("__").next().unwrap_or(session_id)
}

/// True for the main (orchestrator) thread — no `__specialist` suffix.
pub fn is_main_thread(session_id: &str) -> bool {
    !session_id.contains("__")
}

/// The PROJECT a session belongs to. A project spans several mesh conversations
/// ("threads") over time, so grouping by the mesh key alone would wall each
/// conversation off. Instead: the mesh main's recorded folder (so every
/// specialist of a mesh joins its main's project), else the session's own
/// folder, else the mesh key (folder-less legacy stays isolated — never merged
/// into one giant bucket). This is what lets every coder thread see all coder
/// threads and every main see all mains within a project.
fn project_id_of(session: &Session, store: &SessionStore) -> String {
    let mesh_main = project_key(&session.id);
    store
        .get(mesh_main)
        .and_then(|m| m.workspace.clone())
        .or_else(|| session.workspace.clone())
        .unwrap_or_else(|| mesh_main.to_string())
}

fn truncate(text: &str, cap: usize) -> String {
    if text.chars().count() <= cap {
        text.to_string()
    } else {
        let head: String = text.chars().take(cap).collect();
        format!("{head}…")
    }
}

fn role_of(session: &Session) -> String {
    match &session.kind {
        SessionKind::Main => "main".to_string(),
        SessionKind::SubAgent(agent) => {
            crate::runtime::delegation::specialist_label(agent.clone()).to_string()
        }
    }
}

/// The folded-history summary a thread carries, if compaction has run on it.
fn anchor_summary(session: &Session) -> Option<&str> {
    match session.messages.first() {
        Some(Message::Assistant { content }) if content.starts_with(ANCHOR_MARKER) => {
            Some(content.as_str())
        }
        _ => None,
    }
}

/// The most recent real assistant answer (not the compaction continuation).
fn last_final(session: &Session) -> Option<String> {
    session.messages.iter().rev().find_map(|m| match m {
        Message::Assistant { content }
            if !content.trim().is_empty() && !content.starts_with(ANCHOR_MARKER) =>
        {
            Some(content.clone())
        }
        _ => None,
    })
}

/// A detailed digest: folded-history summary (if any) + a deterministic receipt
/// of the recent tail. This is "everything this thread knows" in bounded form.
fn full_digest(session: &Session) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(anchor) = anchor_summary(session) {
        let body = anchor.splitn(2, ']').nth(1).unwrap_or(anchor).trim();
        parts.push(format!("folded history: {}", truncate(body, 700)));
    }
    let tail_start = session.messages.len().saturating_sub(24);
    let recent = crate::runtime::compaction::receipt_digest(&session.messages[tail_start..]);
    if !recent.trim().is_empty() {
        parts.push(format!("recent steps:\n{}", truncate(recent.trim(), 900)));
    }
    if parts.is_empty() {
        return String::new();
    }
    truncate(&parts.join("\n"), FULL_CAP)
}

/// A one-line digest: the thread's latest answer, else a receipt of its tail.
fn line_digest(session: &Session) -> String {
    if let Some(final_answer) = last_final(session) {
        return truncate(&final_answer.replace('\n', " "), LINE_CAP);
    }
    let tail_start = session.messages.len().saturating_sub(4);
    let recent = crate::runtime::compaction::receipt_digest(&session.messages[tail_start..]);
    truncate(recent.replace('\n', " ").trim(), LINE_CAP)
}

/// Recency of a session = its file's mtime (the honest "last touched", the same
/// signal the canvas session list uses). Missing file → epoch (sorts last).
fn mtime(store: &SessionStore, id: &str) -> SystemTime {
    std::fs::metadata(store.root().join(format!("{id}.json")))
        .and_then(|meta| meta.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// Build the tiered cross-thread block for `current`, or None when there are no
/// sibling threads worth showing (a fresh solo thread).
pub fn project_context_block(
    current: &Session,
    store: &SessionStore,
    main_session_id: &str,
) -> Option<String> {
    let key = project_key(main_session_id);
    // Every thread of THIS project — across separate mesh conversations, so a
    // coder sees all coder threads and a main sees all main threads.
    let proj = project_id_of(current, store);
    let mut siblings: Vec<&Session> = store
        .all()
        .filter(|s| s.id != current.id && !s.messages.is_empty() && project_id_of(s, store) == proj)
        .collect();
    if siblings.is_empty() {
        return None;
    }
    // Most-recently-active first.
    siblings.sort_by(|a, b| mtime(store, &b.id).cmp(&mtime(store, &a.id)));

    let mut out = String::new();
    if is_main_thread(&current.id) {
        out.push_str(
            "=== YOUR PROJECT THREADS (their work is yours — do not lose or redo it) ===\n",
        );
        out.push_str(
            "Side-threads in this project. You retain what they did without re-reading their \
             transcripts. Most-recent shown in detail, older as one-liners. To act on one again, \
             `talk` to it — never redo work already done here.\n\n",
        );
        for (index, session) in siblings.iter().enumerate() {
            let role = role_of(session);
            let title = session.title.as_deref().unwrap_or("");
            if index < FULL_SIBLINGS {
                let digest = full_digest(session);
                if digest.trim().is_empty() {
                    continue;
                }
                out.push_str(&format!("[{role}] {title}\n{digest}\n\n"));
            } else {
                out.push_str(&format!("[{role}] {title} — {}\n", line_digest(session)));
            }
        }
    } else {
        // A side-thread: the main thread in full detail, siblings light.
        if let Some(main) = store.get(key).filter(|m| !m.messages.is_empty()) {
            let digest = full_digest(main);
            if !digest.trim().is_empty() {
                out.push_str("=== MAIN PROJECT THREAD (the spine — align with it) ===\n");
                out.push_str(&digest);
                out.push_str("\n\n");
            }
        }
        let others: Vec<&&Session> = siblings.iter().filter(|s| s.id != key).collect();
        if !others.is_empty() {
            out.push_str("=== SIBLING THREADS (light — for awareness) ===\n");
            for session in others {
                out.push_str(&format!(
                    "[{}] {} — {}\n",
                    role_of(session),
                    session.title.as_deref().unwrap_or(""),
                    line_digest(session)
                ));
            }
        }
    }

    let out = truncate(out.trim_end(), BLOCK_CAP);
    if out.trim().is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Session, SubAgentType};

    fn main_session(id: &str) -> Session {
        let mut s = Session::new_main_with_id(id, "m", "sys");
        s.push_message(Message::User {
            content: "build the phoenix canvas project brain".to_string(),
        });
        s.push_message(Message::Assistant {
            content: "Decided: threads share a tiered digest keyed by mesh id.".to_string(),
        });
        s
    }

    fn coder_session(main_id: &str) -> Session {
        let id = format!("{main_id}__coder");
        let mut s = Session::new_sub_agent_with_id(id, SubAgentType::Coder, "m", "sys");
        s.push_message(Message::User {
            content: "implement project_brain.rs".to_string(),
        });
        s.push_message(Message::ToolResult {
            tool_name: "write".to_string(),
            input: "project_brain.rs".to_string(),
            success: true,
            output: "wrote 210 lines".to_string(),
        });
        s.push_message(Message::Assistant {
            content: "Implemented the tiered block builder and tests.".to_string(),
        });
        s
    }

    #[test]
    fn project_key_strips_specialist_suffix() {
        assert_eq!(project_key("main-abc"), "main-abc");
        assert_eq!(project_key("main-abc__coder"), "main-abc");
        assert_eq!(project_key("main-abc__coder--2"), "main-abc");
        assert!(is_main_thread("main-abc"));
        assert!(!is_main_thread("main-abc__coder"));
    }

    #[test]
    fn main_thread_sees_its_specialists() {
        let dir = std::env::temp_dir().join(format!("pb-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = SessionStore::new(&dir);
        let main = main_session("main-abc");
        let coder = coder_session("main-abc");
        store.upsert(main.clone());
        store.upsert(coder.clone());

        let block = project_context_block(&main, &store, "main-abc").expect("main sees coder");
        assert!(block.contains("YOUR PROJECT THREADS"));
        assert!(block.contains("coder"));
        // The specialist's actual work is carried (receipt of its steps).
        assert!(block.contains("project_brain.rs") || block.contains("Implemented"));
        // It does NOT re-list the main thread as one of its own siblings.
        assert!(!block.contains("MAIN PROJECT THREAD"));
    }

    #[test]
    fn specialist_sees_the_main_thread_in_detail() {
        let dir = std::env::temp_dir().join(format!("pb-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = SessionStore::new(&dir);
        let main = main_session("main-abc");
        let coder = coder_session("main-abc");
        let browser_id = "main-abc__browser".to_string();
        let mut browser =
            Session::new_sub_agent_with_id(browser_id, SubAgentType::Browser, "m", "sys");
        browser.push_message(Message::Assistant {
            content: "Logged into the dashboard and captured the screenshot.".to_string(),
        });
        store.upsert(main.clone());
        store.upsert(coder.clone());
        store.upsert(browser.clone());

        let block = project_context_block(&coder, &store, "main-abc").expect("coder sees the mesh");
        // The main spine is present, in detail.
        assert!(block.contains("MAIN PROJECT THREAD"));
        assert!(block.contains("tiered digest") || block.contains("phoenix canvas"));
        // The sibling browser thread is present, but only as a light line.
        assert!(block.contains("SIBLING THREADS"));
        assert!(block.contains("browser"));
    }

    #[test]
    fn project_spans_threads_by_folder() {
        // Two separate mesh conversations (threads) in the SAME project folder.
        let dir = std::env::temp_dir().join(format!("pb-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = SessionStore::new(&dir);
        let folder = "/proj/phoenix".to_string();

        let mut main_a = main_session("main-A");
        main_a.workspace = Some(folder.clone());
        let mut coder_a = coder_session("main-A");
        coder_a.workspace = Some(folder.clone());
        let mut main_b = main_session("main-B");
        main_b.workspace = Some(folder.clone());
        let mut coder_b = coder_session("main-B");
        coder_b.push_message(Message::Assistant {
            content: "Thread-B coder refactored the parser.".to_string(),
        });
        coder_b.workspace = Some(folder.clone());
        for s in [&main_a, &coder_a, &main_b, &coder_b] {
            store.upsert((*s).clone());
        }

        // Thread-A's coder must see Thread-B's coder (all coder threads) AND the
        // other main thread — because they share the project folder.
        let block = project_context_block(&coder_a, &store, "main-A").expect("cross-thread block");
        assert!(block.contains("Thread-B coder refactored") || block.contains("main-B"));
        assert!(block.contains("MAIN PROJECT THREAD")); // its own spine still shown
    }

    #[test]
    fn solo_thread_has_no_block() {
        let dir = std::env::temp_dir().join(format!("pb-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = SessionStore::new(&dir);
        let main = main_session("main-solo");
        store.upsert(main.clone());
        assert!(project_context_block(&main, &store, "main-solo").is_none());
    }
}
