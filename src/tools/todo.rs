//! Durable per-conversation task lists for `todo_write` and desktop progress.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::ToolOutput;

const MAX_TODOS: usize = 20;
const MAX_TASK_BYTES: usize = 2_000;
const MAX_DOCUMENT_BYTES: usize = 128 * 1024;
const TODO_DOCUMENT_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TodoItem {
    pub task: String,
    #[serde(default)]
    pub completed: bool,
    /// `pending`, `in_progress`, `completed` or `cancelled`. Omitted means
    /// pending, or completed when `completed` is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// The agent's own estimate (0–100) of how far the in-progress step is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    /// A few words shown beside the step, e.g. "3 of 5 pages".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

const TODO_STATUSES: &[&str] = &["pending", "in_progress", "completed", "cancelled"];
const MAX_DETAIL_CHARS: usize = 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoWriteInput {
    pub todos: Vec<TodoItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TodoDocument {
    version: u32,
    session_id: String,
    updated_at: String,
    todos: Vec<TodoItem>,
}

pub fn execute(input: TodoWriteInput, session_id: &str) -> Result<ToolOutput> {
    let mut todos = input.todos;
    if todos.is_empty() {
        bail!("todo_write requires at least one todo item");
    }
    if todos.len() > MAX_TODOS {
        bail!("todo_write limit is {MAX_TODOS} items, got {}", todos.len());
    }
    for item in &mut todos {
        item.task = item.task.trim().to_string();
        anyhow::ensure!(
            !item.task.is_empty() && item.task.len() <= MAX_TASK_BYTES,
            "todo task must be 1..={MAX_TASK_BYTES} bytes"
        );
        anyhow::ensure!(
            !item.task.chars().any(char::is_control),
            "todo task must be one printable line"
        );
        if let Some(status) = item.status.as_mut() {
            *status = status.trim().to_ascii_lowercase().replace(['-', ' '], "_");
            anyhow::ensure!(
                TODO_STATUSES.contains(&status.as_str()),
                "todo status must be pending, in_progress, completed or cancelled"
            );
            // `completed` and `status` never disagree.
            item.completed = status == "completed";
        } else if item.completed {
            item.status = Some("completed".into());
        }
        if let Some(progress) = item.progress {
            anyhow::ensure!(progress <= 100, "todo progress must be 0..=100");
        }
        if let Some(detail) = item.detail.as_mut() {
            *detail = detail.trim().chars().filter(|ch| !ch.is_control()).take(MAX_DETAIL_CHARS).collect();
            if detail.is_empty() {
                item.detail = None;
            }
        }
    }
    let path = todo_path(session_id)?;
    let completed = todos.iter().filter(|t| t.completed).count();
    let total = todos.len();
    let document = TodoDocument {
        version: TODO_DOCUMENT_VERSION,
        session_id: session_id.to_string(),
        updated_at: chrono::Utc::now().to_rfc3339(),
        todos,
    };
    let bytes = serde_json::to_vec_pretty(&document)?;
    anyhow::ensure!(
        bytes.len() <= MAX_DOCUMENT_BYTES,
        "todo document exceeds its safe bound"
    );
    crate::config::private_io::atomic_write_private(&path, &bytes)?;

    // Format a compact summary
    let mut lines = vec![format!("Todo list ({total} items, {completed} done):")];
    for (i, item) in document.todos.iter().enumerate() {
        let status = match item.status.as_deref() {
            Some("completed") => "✓",
            Some("in_progress") => "◐",
            Some("cancelled") => "✗",
            _ if item.completed => "✓",
            _ => "○",
        };
        let progress = item.progress.filter(|_| item.status.as_deref() == Some("in_progress")).map(|p| format!(" {p}%")).unwrap_or_default();
        lines.push(format!("  {status} [{i}] {}{progress}", item.task));
    }

    Ok(ToolOutput {
        summary: format!("Updated todo list: {completed}/{total} completed."),
        content: lines.join("\n"),
    })
}

/// Snapshot the durable structured list for one canonical conversation.
/// Missing means no plan; corrupt or mismatched state is an error so a client
/// cannot silently render lost work as an intentionally empty list.
pub fn snapshot(session_id: &str) -> Result<Vec<TodoItem>> {
    Ok(snapshot_with_update_time(session_id)?.0)
}

/// The plan plus when it was last written (RFC 3339; empty when no plan).
pub fn snapshot_with_update_time(session_id: &str) -> Result<(Vec<TodoItem>, String)> {
    let path = todo_path(session_id)?;
    let Some(raw) =
        crate::config::private_io::read_private_file_limited(&path, MAX_DOCUMENT_BYTES)?
    else {
        return Ok((Vec::new(), String::new()));
    };
    let document: TodoDocument =
        serde_json::from_slice(&raw).context("todo document is invalid")?;
    anyhow::ensure!(
        document.version == TODO_DOCUMENT_VERSION,
        "unsupported todo document version"
    );
    anyhow::ensure!(
        document.session_id == session_id,
        "todo document belongs to another conversation"
    );
    anyhow::ensure!(
        document.todos.len() <= MAX_TODOS,
        "todo document exceeds its item bound"
    );
    Ok((document.todos, document.updated_at))
}

/// Explicitly clear a conversation plan. Normal turn/session completion does
/// not call this: canonical coworker threads retain their current plan across
/// gateway and desktop restarts.
pub fn clear_session(session_id: &str) -> Result<()> {
    let path = todo_path(session_id)?;
    let _ = crate::config::private_io::remove_private_file(&path)?;
    Ok(())
}

fn todo_path(session_id: &str) -> Result<PathBuf> {
    crate::session::SessionStore::validate_session_id(session_id)?;
    Ok(crate::config::phoenix_home()
        .join("todos")
        .join(format!("{session_id}.json")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manages_todo_list() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let result = execute(
            TodoWriteInput {
                todos: vec![
                    TodoItem {
                        task: "Add web_search tool".to_string(),
                        completed: true, ..Default::default()
                    },
                    TodoItem {
                        task: "Add web_fetch tool".to_string(),
                        completed: false, ..Default::default()
                    },
                    TodoItem {
                        task: "Update prompts".to_string(),
                        completed: false, ..Default::default()
                    },
                ],
            },
            "test-session",
        )
        .unwrap();

        assert!(result.summary.contains("1/3"));
        assert!(result.content.contains("✓"));
        assert!(result.content.contains("○"));

        // Update the list
        let result2 = execute(
            TodoWriteInput {
                todos: vec![
                    TodoItem {
                        task: "Add web_search tool".to_string(),
                        completed: true, ..Default::default()
                    },
                    TodoItem {
                        task: "Add web_fetch tool".to_string(),
                        completed: true, ..Default::default()
                    },
                    TodoItem {
                        task: "Update prompts".to_string(),
                        completed: false, ..Default::default()
                    },
                ],
            },
            "test-session",
        )
        .unwrap();

        assert!(result2.summary.contains("2/3"));
    }

    #[test]
    fn rejects_empty_todo_list() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let result = execute(TodoWriteInput { todos: vec![] }, "test-session");
        assert!(result.is_err());
    }

    #[test]
    fn rejects_too_many_todos() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let todos: Vec<TodoItem> = (0..25)
            .map(|i| TodoItem {
                task: format!("Task {i}"),
                completed: false, ..Default::default()
            })
            .collect();
        let result = execute(TodoWriteInput { todos }, "test-session");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("limit"));
    }

    #[test]
    fn clears_session_todos() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        execute(
            TodoWriteInput {
                todos: vec![TodoItem {
                    task: "Test task".to_string(),
                    completed: false, ..Default::default()
                }],
            },
            "clear-test",
        )
        .unwrap();

        clear_session("clear-test").unwrap();
        assert!(snapshot("clear-test").unwrap().is_empty());
    }

    #[test]
    fn status_progress_and_detail_round_trip_and_validate() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let input: TodoWriteInput = serde_json::from_value(serde_json::json!({"todos":[
            {"task":"Build hero","status":"completed"},
            {"task":"Write copy","status":"in-progress","progress":40,"detail":"3 of 5 sections"},
            {"task":"Old idea","status":"cancelled"},
            {"task":"Ship","completed":false}
        ]})).unwrap();
        let output = execute(input, "status-test").unwrap();
        assert!(output.summary.contains("1/4"), "{}", output.summary);
        assert!(output.content.contains("◐ [1] Write copy 40%"), "{}", output.content);
        let saved = snapshot("status-test").unwrap();
        assert!(saved[0].completed);
        assert_eq!(saved[1].status.as_deref(), Some("in_progress"));
        assert_eq!(saved[1].progress, Some(40));
        assert_eq!(saved[1].detail.as_deref(), Some("3 of 5 sections"));
        assert_eq!(saved[3].status, None);

        for bad in [
            serde_json::json!({"todos":[{"task":"x","status":"doing"}]}),
            serde_json::json!({"todos":[{"task":"x","progress":101}]}),
        ] {
            let parsed: TodoWriteInput = serde_json::from_value(bad).unwrap();
            assert!(execute(parsed, "status-test").is_err());
        }
    }

    #[test]
    fn snapshots_each_canonical_thread_independently() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        execute(
            TodoWriteInput {
                todos: vec![TodoItem {
                    task: "Ship the sidebar".into(),
                    completed: false, ..Default::default()
                }],
            },
            "company-phoenix",
        )
        .unwrap();
        execute(
            TodoWriteInput {
                todos: vec![TodoItem {
                    task: "Triage the inbox".into(),
                    completed: true, ..Default::default()
                }],
            },
            "company-nico",
        )
        .unwrap();

        assert_eq!(
            snapshot("company-phoenix").unwrap()[0].task,
            "Ship the sidebar"
        );
        assert!(snapshot("company-nico").unwrap()[0].completed);
        assert!(snapshot("company-missing").unwrap().is_empty());
    }

    #[test]
    fn todos_survive_an_in_memory_boundary_and_corruption_is_not_erased() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        execute(
            TodoWriteInput {
                todos: vec![TodoItem {
                    task: "Resume after restart".into(),
                    completed: false, ..Default::default()
                }],
            },
            "company-phoenix",
        )
        .unwrap();
        assert_eq!(
            snapshot("company-phoenix").unwrap()[0].task,
            "Resume after restart"
        );

        let path = todo_path("company-phoenix").unwrap();
        crate::config::private_io::atomic_write_private(&path, b"not json").unwrap();
        assert!(snapshot("company-phoenix").is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"not json");
    }
}
