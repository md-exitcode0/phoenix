//! list_directory tool - list files and directories with metadata
//!
//! Compact directory listing patterns for Phoenix tools.
//! Better than glob for structured directory exploration —
//! returns separate arrays of files and directories.

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::{format_bytes, relative_display, resolve_workspace_path, ToolOutput};

const MAX_LIST_ENTRIES: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListDirectoryInput {
    pub path: String,
}

pub fn execute(
    workspace_root: &Path,
    confined: bool,
    input: ListDirectoryInput,
) -> Result<ToolOutput> {
    let path = resolve_workspace_path(workspace_root, &input.path, true, confined)?;
    if !path.is_dir() {
        bail!("list_directory path is not a directory: {}", input.path);
    }

    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let mut total = 0usize;

    for entry in fs::read_dir(&path)
        .with_context(|| format!("failed to read directory {}", path.display()))?
    {
        if total >= MAX_LIST_ENTRIES {
            break;
        }

        let entry = entry.with_context(|| format!("error reading entry in {}", path.display()))?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to read file type for {}", entry.path().display()))?;

        let _name = entry.file_name().to_string_lossy().to_string();
        let entry_path = entry.path();
        let display = relative_display(workspace_root, &entry_path);

        if file_type.is_dir() {
            dirs.push(display);
        } else if file_type.is_file() {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            files.push(format!("{} ({})", display, format_bytes(size)));
        } else {
            files.push(format!("{} (symlink)", display));
        }
        total += 1;
    }

    files.sort();
    dirs.sort();

    let mut lines = Vec::new();
    if !dirs.is_empty() {
        lines.push("Directories:".to_string());
        for dir in &dirs {
            lines.push(format!("  {}/", dir));
        }
    }
    if !files.is_empty() {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push("Files:".to_string());
        for file in &files {
            lines.push(format!("  {}", file));
        }
    }

    let truncated_note = if total >= MAX_LIST_ENTRIES {
        format!(" (capped at {MAX_LIST_ENTRIES})")
    } else {
        String::new()
    };

    if lines.is_empty() {
        return Ok(ToolOutput {
            summary: format!("Directory {} is empty.", input.path),
            content: String::new(),
        });
    }

    Ok(ToolOutput {
        summary: format!(
            "{} ({} files, {} dirs{})",
            input.path,
            files.len(),
            dirs.len(),
            truncated_note
        ),
        content: lines.join("\n"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn lists_files_and_directories() {
        let root_dir = tempdir().unwrap();
        let root = root_dir.path().to_path_buf();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join("README.md"), "hello").unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();

        let result = execute(
            &root,
            true,
            ListDirectoryInput {
                path: ".".to_string(),
            },
        )
        .unwrap();

        assert!(result.summary.contains("files"));
        assert!(result.content.contains("src/"));
        assert!(result.content.contains("README.md"));
    }

    #[test]
    fn rejects_non_directory_path() {
        let root_dir = tempdir().unwrap();
        let root = root_dir.path().to_path_buf();
        std::fs::write(root.join("file.txt"), "content").unwrap();

        let err = execute(
            &root,
            true,
            ListDirectoryInput {
                path: "file.txt".to_string(),
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("not a directory"));
    }

    #[test]
    fn empty_directory_returns_clear_message() {
        let root_dir = tempdir().unwrap();
        let root = root_dir.path().to_path_buf();
        std::fs::create_dir(root.join("empty")).unwrap();

        let result = execute(
            &root,
            true,
            ListDirectoryInput {
                path: "empty".to_string(),
            },
        )
        .unwrap();

        assert!(result.summary.contains("empty"));
    }
}
