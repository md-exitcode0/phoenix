//! str_replace tool - targeted text replacement in files
//!
//! Precise, bounded string replacement for Phoenix coding workflows.
//! This avoids reading and rewriting an entire file for a small edit.
//! str_replace enables precise, minimal, token-efficient edits.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{format_bytes, workspace_io, ToolOutput};

const MAX_REPLACE_FRAGMENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_REPLACE_OCCURRENCES: usize = 100_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrReplaceInput {
    pub path: String,
    /// Exact string to find and replace. Must match exactly including whitespace.
    pub old_str: String,
    /// Replacement string. Can be empty to delete old_str.
    pub new_str: String,
    /// Whether to replace ALL occurrences of old_str. Default false (single replacement only).
    #[serde(default)]
    pub allow_multiple: bool,
}

pub fn execute(
    workspace_root: &Path,
    confined: bool,
    input: StrReplaceInput,
) -> Result<ToolOutput> {
    execute_for_session(workspace_root, confined, input, None)
}

pub(crate) fn execute_for_session(
    workspace_root: &Path,
    confined: bool,
    input: StrReplaceInput,
    session_id: Option<&str>,
) -> Result<ToolOutput> {
    workspace_io::validate_path_input(&input.path)?;
    if input.old_str.is_empty() {
        bail!("str_replace old_str cannot be empty");
    }
    if input.old_str.len() > MAX_REPLACE_FRAGMENT_BYTES
        || input.new_str.len() > MAX_REPLACE_FRAGMENT_BYTES
    {
        bail!(
            "str_replace old_str/new_str exceeds the {}-byte fragment limit",
            MAX_REPLACE_FRAGMENT_BYTES
        );
    }

    // The lock covers read, match, and publication. Without it two Phoenix
    // agents can both read the same old bytes and silently lose one edit.
    let target =
        workspace_io::LockedWorkspaceTarget::acquire(workspace_root, &input.path, confined, false)?;
    let snapshot = target.read_utf8_snapshot(workspace_io::MAX_WORKSPACE_TEXT_BYTES)?;
    if confined {
        if let Some(session_id) = session_id {
            let receipt = crate::runtime::read_files::receipt(session_id, target.canonical_path())
                .context(
                    "file has no content-version read receipt; read it again before editing",
                )?;
            if receipt.sha256 != snapshot.sha256 || receipt.bytes != snapshot.bytes {
                bail!(
                    "{} changed since it was read; read the current file before editing it",
                    input.path
                );
            }
        }
    }
    let original = &snapshot.text;

    let occurrences = original
        .match_indices(&input.old_str)
        .take(MAX_REPLACE_OCCURRENCES + 1)
        .count();
    if occurrences > MAX_REPLACE_OCCURRENCES {
        bail!(
            "str_replace old_str occurs more than {MAX_REPLACE_OCCURRENCES} times; refusing unbounded replacement"
        );
    }

    if occurrences == 0 {
        // Anchored match missed — the drift case the fast-apply lane exists
        // for (donor: morphllm Fast Apply). Only when configured; the merge
        // must pass the sanity gate or the original error stands.
        if let Ok(merged) = crate::tools::fast_apply::rescue_replace(
            original.as_str(),
            &input.old_str,
            &input.new_str,
        ) {
            if merged.as_str() == original.as_str() {
                bail!("fast-apply rescue produced no change in {}", input.path);
            }
            if merged.len() > workspace_io::MAX_WORKSPACE_TEXT_BYTES {
                bail!(
                    "fast-apply rescue exceeds the {}-byte workspace file limit",
                    workspace_io::MAX_WORKSPACE_TEXT_BYTES
                );
            }
            target.publish_checked(
                merged.as_bytes(),
                true,
                Some((&snapshot.sha256, snapshot.bytes)),
            )?;
            record_edit_receipt(session_id, confined, &target, merged.as_bytes());
            return Ok(ToolOutput {
                summary: format!("fast-apply rescue in {}", input.path),
                content: format!(
                    "str_replace: old_str did not match (file drifted), so the configured \
                     fast-apply model merged the intended edit into {}. The file changed in \
                     the intended way but not via exact anchor — READ the edited region \
                     before building on it.",
                    input.path
                ),
            });
        }
        bail!(
            "str_replace: old_str not found in {}. The string must match exactly including whitespace and punctuation.{}",
            input.path,
            mismatch_hint(original, &input.old_str)
        );
    }

    if !input.allow_multiple && occurrences > 1 {
        bail!(
            "str_replace: old_str found {} times in {}. Set allow_multiple=true to replace all occurrences, or make old_str more specific to match exactly one occurrence.",
            occurrences,
            input.path
        );
    }

    let replacement_count = if input.allow_multiple { occurrences } else { 1 };
    let removed = input
        .old_str
        .len()
        .checked_mul(replacement_count)
        .context("str_replace removal byte count overflowed")?;
    let added = input
        .new_str
        .len()
        .checked_mul(replacement_count)
        .context("str_replace replacement byte count overflowed")?;
    let resulting_len = original
        .len()
        .checked_sub(removed)
        .and_then(|len| len.checked_add(added))
        .context("str_replace result byte count overflowed")?;
    if resulting_len > workspace_io::MAX_WORKSPACE_TEXT_BYTES {
        bail!(
            "str_replace result would be too large ({} bytes; max {})",
            resulting_len,
            workspace_io::MAX_WORKSPACE_TEXT_BYTES
        );
    }

    let replaced = if input.allow_multiple {
        original.replace(&input.old_str, &input.new_str)
    } else {
        original.replacen(&input.old_str, &input.new_str, 1)
    };

    if replaced.as_str() == original.as_str() {
        bail!(
            "str_replace: replacement produced no change in {}",
            input.path
        );
    }

    target.publish_checked(
        replaced.as_bytes(),
        true,
        Some((&snapshot.sha256, snapshot.bytes)),
    )?;
    record_edit_receipt(session_id, confined, &target, replaced.as_bytes());

    let count = if input.allow_multiple { occurrences } else { 1 };

    let change_desc = if input.new_str.is_empty() {
        format!("Removed {} occurrence(s)", count)
    } else {
        format!(
            "Replaced {} occurrence(s){}",
            count,
            if input.allow_multiple && count > 1 {
                " (all)"
            } else {
                ""
            }
        )
    };

    Ok(ToolOutput {
        summary: format!(
            "{} in {} ({}).",
            change_desc,
            input.path,
            format_bytes(replaced.len() as u64)
        ),
        content: String::new(),
    })
}

/// Show the first disagreement after a unique, exact opening line. This is
/// diagnostic only: never normalize whitespace or apply a guessed edit.
fn mismatch_hint(original: &str, requested: &str) -> String {
    let anchor = requested.lines().next().unwrap_or_default();
    if anchor.trim().len() < 8 || !requested.contains('\n') {
        return String::new();
    }
    let mut offset = 0;
    let mut start = None;
    for line in original.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == anchor {
            if start.is_some() {
                return String::new();
            }
            start = Some(offset);
        }
        offset += line.len();
    }
    let Some(start) = start else {
        return String::new();
    };
    let actual = &original[start..];
    let mut matched = actual
        .bytes()
        .zip(requested.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !actual.is_char_boundary(matched) || !requested.is_char_boundary(matched) {
        matched -= 1;
    }
    let line = original[..start + matched]
        .bytes()
        .filter(|b| *b == b'\n')
        .count()
        + 1;
    let before = requested[..matched]
        .chars()
        .rev()
        .take(40)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    let supplied = format!(
        "{before}{}",
        requested[matched..].chars().take(100).collect::<String>()
    );
    let current = format!(
        "{before}{}",
        actual[matched..].chars().take(100).collect::<String>()
    );
    format!("\nNo edit was applied. A unique opening line matches, but the text first differs near line {line}. Escaped excerpts (\\n means a newline; excerpts may be partial):\nRequested: {supplied:?}\nCurrent:   {current:?}\nUse the current text for an exact retry, or read that region for more context.")
}

fn record_edit_receipt(
    session_id: Option<&str>,
    confined: bool,
    target: &workspace_io::LockedWorkspaceTarget<'_>,
    contents: &[u8],
) {
    if !confined {
        return;
    }
    if let Some(session_id) = session_id {
        let sha256 = format!("sha256:{:x}", Sha256::digest(contents));
        crate::runtime::read_files::record(
            session_id,
            target.canonical_path(),
            &sha256,
            contents.len() as u64,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn replaces_single_occurrence() {
        let root_dir = tempdir().unwrap();
        let root = root_dir.path().to_path_buf();
        std::fs::write(
            root.join("test.rs"),
            "fn hello() {\n    println!(\"hi\");\n}\n",
        )
        .unwrap();

        let result = execute(
            &root,
            true,
            StrReplaceInput {
                path: "test.rs".to_string(),
                old_str: "println!(\"hi\")".to_string(),
                new_str: "println!(\"hello\")".to_string(),
                allow_multiple: false,
            },
        )
        .unwrap();

        let content = std::fs::read_to_string(root.join("test.rs")).unwrap();
        assert!(content.contains("println!(\"hello\")"));
        assert!(!content.contains("println!(\"hi\")"));
        assert!(result.summary.contains("Replaced 1 occurrence"));
    }

    #[test]
    fn replaces_multiple_occurrences() {
        let root_dir = tempdir().unwrap();
        let root = root_dir.path().to_path_buf();
        std::fs::write(
            root.join("test.rs"),
            "fn a() { foo(); }\nfn b() { foo(); }\nfn c() { foo(); }\n",
        )
        .unwrap();

        let result = execute(
            &root,
            true,
            StrReplaceInput {
                path: "test.rs".to_string(),
                old_str: "foo()".to_string(),
                new_str: "bar()".to_string(),
                allow_multiple: true,
            },
        )
        .unwrap();

        let content = std::fs::read_to_string(root.join("test.rs")).unwrap();
        assert!(!content.contains("foo()"));
        assert_eq!(content.matches("bar()").count(), 3);
        assert!(result.summary.contains("Replaced 3 occurrence(s) (all)"));
    }

    #[test]
    fn rejects_multiple_without_flag() {
        let root_dir = tempdir().unwrap();
        let root = root_dir.path().to_path_buf();
        std::fs::write(root.join("test.rs"), "x\ny\nx\n").unwrap();

        let err = execute(
            &root,
            true,
            StrReplaceInput {
                path: "test.rs".to_string(),
                old_str: "x".to_string(),
                new_str: "z".to_string(),
                allow_multiple: false,
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("found 2 times"));
        assert!(err.to_string().contains("allow_multiple=true"));
    }

    #[test]
    fn rejects_missing_old_str() {
        let root_dir = tempdir().unwrap();
        let root = root_dir.path().to_path_buf();
        std::fs::write(root.join("test.rs"), "hello world\n").unwrap();

        let err = execute(
            &root,
            true,
            StrReplaceInput {
                path: "test.rs".to_string(),
                old_str: "goodbye".to_string(),
                new_str: "farewell".to_string(),
                allow_multiple: false,
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("old_str not found"));
    }

    #[test]
    fn missing_blank_line_reports_current_text_without_editing() {
        let dir = tempdir().unwrap();
        let original = "# header\ndef fruit_radius(t):\n    return t\n\n\n# Next section\n";
        let requested = "def fruit_radius(t):\n    return t\n\n# Next section\n";
        std::fs::write(dir.path().join("scene.py"), original).unwrap();
        let error = execute(
            dir.path(),
            true,
            StrReplaceInput {
                path: "scene.py".into(),
                old_str: requested.into(),
                new_str: "replacement".into(),
                allow_multiple: false,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("near line 5"), "{error}");
        assert!(error.contains("Current:"));
        assert!(error.contains("\\n\\n\\n# Next section"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("scene.py")).unwrap(),
            original
        );
        // A corrected exact retry succeeds through the ordinary editor path.
        execute(
            dir.path(),
            true,
            StrReplaceInput {
                path: "scene.py".into(),
                old_str: original.trim_start_matches("# header\n").into(),
                new_str: "replacement\n".into(),
                allow_multiple: false,
            },
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("scene.py")).unwrap(),
            "# header\nreplacement\n"
        );
    }

    #[test]
    fn mismatch_hints_are_bounded_unicode_safe_and_require_unique_anchors() {
        let requested = "def example():\n    énd\n";
        let current = format!("def example():\n    ê{}", "🙂".repeat(100_000));
        let hint = mismatch_hint(&current, requested);
        assert!(hint.contains("Current:"));
        assert!(hint.contains('ê'));
        assert!(hint.len() < 1500);
        assert!(mismatch_hint("def example():\nfoo\ndef example():\nbar\n", requested).is_empty());
        assert!(mismatch_hint("unrelated\n", requested).is_empty());
        assert!(mismatch_hint("x\nwrong", "x\nvalue").is_empty());
        assert!(mismatch_hint("def example():\n", "def example():\nmore").contains("Current:"));
    }

    #[test]
    fn rejects_empty_old_str() {
        let root_dir = tempdir().unwrap();
        let root = root_dir.path().to_path_buf();
        std::fs::write(root.join("test.rs"), "content\n").unwrap();

        let err = execute(
            &root,
            true,
            StrReplaceInput {
                path: "test.rs".to_string(),
                old_str: "".to_string(),
                new_str: "x".to_string(),
                allow_multiple: false,
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("cannot be empty"));
    }

    #[test]
    fn can_delete_text_with_empty_new_str() {
        let root_dir = tempdir().unwrap();
        let root = root_dir.path().to_path_buf();
        std::fs::write(
            root.join("test.rs"),
            "keep this // remove this\nkeep that\n",
        )
        .unwrap();

        execute(
            &root,
            true,
            StrReplaceInput {
                path: "test.rs".to_string(),
                old_str: " // remove this".to_string(),
                new_str: "".to_string(),
                allow_multiple: false,
            },
        )
        .unwrap();

        let content = std::fs::read_to_string(root.join("test.rs")).unwrap();
        assert_eq!(content, "keep this\nkeep that\n");
    }

    #[test]
    fn requires_exact_whitespace_match() {
        let root_dir = tempdir().unwrap();
        let root = root_dir.path().to_path_buf();
        std::fs::write(root.join("test.rs"), "fn  foo() {}\n").unwrap();

        let err = execute(
            &root,
            true,
            StrReplaceInput {
                path: "test.rs".to_string(),
                old_str: "fn foo()".to_string(),
                new_str: "fn bar()".to_string(),
                allow_multiple: false,
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("old_str not found"));
    }

    #[test]
    fn concurrent_read_modify_write_edits_do_not_lose_updates() {
        let root_dir = tempdir().unwrap();
        let root = std::sync::Arc::new(root_dir.path().to_path_buf());
        std::fs::write(root.join("shared.txt"), "x").unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(12));
        let mut threads = Vec::new();
        for index in 0..12 {
            let root = std::sync::Arc::clone(&root);
            let barrier = std::sync::Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                execute(
                    &root,
                    true,
                    StrReplaceInput {
                        path: "shared.txt".to_string(),
                        old_str: "x".to_string(),
                        new_str: format!("x{index}"),
                        allow_multiple: false,
                    },
                )
                .unwrap();
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }
        let content = std::fs::read_to_string(root.join("shared.txt")).unwrap();
        let expected_len = 1 + (0..12).map(|index| index.to_string().len()).sum::<usize>();
        assert_eq!(
            content.len(),
            expected_len,
            "lost a concurrent edit: {content}"
        );
        for index in 0..12 {
            assert!(
                content.contains(&index.to_string()),
                "missing edit {index}: {content}"
            );
        }
    }

    #[test]
    fn caps_pathological_occurrence_counts() {
        let root_dir = tempdir().unwrap();
        std::fs::write(
            root_dir.path().join("many.txt"),
            "x".repeat(MAX_REPLACE_OCCURRENCES + 1),
        )
        .unwrap();
        let error = execute(
            root_dir.path(),
            true,
            StrReplaceInput {
                path: "many.txt".to_string(),
                old_str: "x".to_string(),
                new_str: "y".to_string(),
                allow_multiple: true,
            },
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("unbounded replacement"),
            "{error:#}"
        );
    }
}
