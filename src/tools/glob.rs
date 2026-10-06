//! glob tool - find files by pattern

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::{is_ignored_dir, relative_display, resolve_workspace_path, ToolOutput};

const MAX_GLOB_RESULTS: usize = 100;
const MAX_GLOB_PATTERN_BYTES: usize = 4 * 1024;
/// Hard ceiling on candidate paths the matcher will inspect, so a `**/*` pattern
/// over a tree full of build artifacts / donor clones can't spin the CPU.
const MAX_GLOB_CANDIDATES: usize = 50_000;

#[derive(Debug)]
struct GlobPlan {
    root: PathBuf,
    pattern: String,
    display_prefix: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobInput {
    pub pattern: String,
}

pub fn execute(workspace_root: &Path, confined: bool, input: GlobInput) -> Result<ToolOutput> {
    execute_with_caps(
        workspace_root,
        confined,
        input,
        MAX_GLOB_RESULTS,
        MAX_GLOB_CANDIDATES,
    )
}

fn execute_with_caps(
    workspace_root: &Path,
    confined: bool,
    input: GlobInput,
    result_cap: usize,
    candidate_cap: usize,
) -> Result<ToolOutput> {
    if input.pattern.trim().is_empty()
        || input.pattern.len() > MAX_GLOB_PATTERN_BYTES
        || Path::new(&input.pattern).is_absolute()
    {
        bail!("unsafe glob pattern: {}", input.pattern);
    }

    let plan = glob_plan(workspace_root, confined, &input.pattern)?;
    let root = plan.root;
    // The `glob` crate has no `{a,b}` alternation, but agents raised on
    // ripgrep/Claude Code emit brace patterns constantly — expand them
    // ourselves instead of failing the call.
    let (expanded, brace_capped) = expand_braces_report(&plan.pattern);
    let patterns = expanded
        .into_iter()
        .map(|pattern| glob::Pattern::new(&pattern).context("invalid glob pattern"))
        .collect::<Result<Vec<_>>>()?;
    let mut paths = Vec::new();
    let mut candidates = 0usize;
    let mut candidate_capped = false;
    let mut result_capped = false;
    let mut scan_error_count = 0usize;
    let mut scan_errors = Vec::new();
    let mut pending = VecDeque::from([root.clone()]);
    'walk: while let Some(directory) = pending.pop_front() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                scan_error_count += 1;
                if scan_errors.len() < 3 {
                    scan_errors.push(format!("{}: {error}", directory.display()));
                }
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    scan_error_count += 1;
                    if scan_errors.len() < 3 {
                        scan_errors.push(format!("{}: {error}", directory.display()));
                    }
                    continue;
                }
            };
            candidates += 1;
            if candidates > candidate_cap {
                candidate_capped = true;
                break 'walk;
            }
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    scan_error_count += 1;
                    if scan_errors.len() < 3 {
                        scan_errors.push(format!("{}: {error}", path.display()));
                    }
                    continue;
                }
            };
            // Never traverse or return a symlink. This also keeps a dangling
            // link from being mistaken for a partial path match.
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                // This must happen before enqueueing. Filtering matches after
                // `glob::glob("**/name")` still makes that crate descend into
                // every target/, .git/, donor clone, and node_modules tree.
                if !entry.file_name().to_str().is_some_and(is_ignored_dir) {
                    pending.push_back(path.clone());
                }
            }
            let relative: PathBuf = match path.strip_prefix(&root) {
                Ok(relative) => relative.to_path_buf(),
                Err(_) => continue,
            };
            if patterns.iter().any(|pattern| {
                pattern.matches_path_with(
                    &relative,
                    glob::MatchOptions {
                        case_sensitive: true,
                        require_literal_separator: true,
                        require_literal_leading_dot: false,
                    },
                )
            }) {
                let matched = relative_display(&root, &path);
                let display = if plan.display_prefix.as_os_str().is_empty()
                    || plan.display_prefix == Path::new(".")
                {
                    matched
                } else {
                    plan.display_prefix
                        .join(matched)
                        .to_string_lossy()
                        .to_string()
                };
                if !paths.contains(&display) {
                    paths.push(display);
                }
                if paths.len() >= result_cap {
                    result_capped = true;
                    break 'walk;
                }
            }
        }
    }
    paths.sort();

    let mut reasons = Vec::new();
    if brace_capped {
        reasons.push("brace expansion cap reached".to_string());
    }
    if candidate_capped {
        reasons.push(format!("candidate cap {candidate_cap} reached"));
    }
    if result_capped {
        reasons.push(format!("result cap {result_cap} reached"));
    }
    if scan_error_count > 0 {
        reasons.push(format!(
            "{scan_error_count} filesystem error(s): {}",
            scan_errors.join("; ")
        ));
    }
    let disclosure = if reasons.is_empty() {
        String::new()
    } else {
        format!(" Partial results: {}.", reasons.join("; "))
    };

    Ok(ToolOutput {
        summary: format!(
            "Found {} path(s) for pattern {}.{disclosure}",
            paths.len(),
            input.pattern
        ),
        content: paths.join("\n"),
    })
}

/// Split a glob into a concrete scan root and a matcher relative to that root.
/// Parent traversal is allowed only in the concrete prefix, where the shared
/// path resolver can enforce Workspace versus Full Access. A `..` after the
/// first wildcard would make matching semantics ambiguous and remains denied.
fn glob_plan(workspace_root: &Path, confined: bool, raw_pattern: &str) -> Result<GlobPlan> {
    let parts = raw_pattern.split('/').collect::<Vec<_>>();
    let first_meta = parts
        .iter()
        .position(|part| part.chars().any(|ch| matches!(ch, '*' | '?' | '[' | '{')));
    let split_at = first_meta.unwrap_or_else(|| parts.len().saturating_sub(1));
    let (prefix_parts, pattern_parts) = parts.split_at(split_at);

    if pattern_parts.is_empty()
        || pattern_parts
            .iter()
            .any(|part| matches!(*part, "." | ".." | ""))
    {
        bail!("unsafe glob pattern: {raw_pattern}");
    }

    let prefix = if prefix_parts.is_empty() {
        ".".to_string()
    } else {
        prefix_parts.join("/")
    };
    let root = resolve_workspace_path(workspace_root, &prefix, true, confined)?;
    if !root.is_dir() {
        bail!("glob prefix is not a directory: {prefix}");
    }

    Ok(GlobPlan {
        root,
        pattern: pattern_parts.join("/"),
        display_prefix: PathBuf::from(prefix),
    })
}

/// Expand `{a,b,c}` alternation groups into plain glob patterns, recursively
/// (left-most group first). A pattern with no braces returns as-is. Capped so
/// a pathological pattern can't explode.
#[cfg(test)]
fn expand_braces(pattern: &str) -> Vec<String> {
    expand_braces_report(pattern).0
}

fn expand_braces_report(pattern: &str) -> (Vec<String>, bool) {
    const MAX_EXPANSIONS: usize = 64;
    let mut capped = false;
    let expanded = expand_braces_inner(pattern, MAX_EXPANSIONS, &mut capped);
    (expanded, capped)
}

fn expand_braces_inner(pattern: &str, max: usize, capped: &mut bool) -> Vec<String> {
    let Some(open) = pattern.find('{') else {
        return vec![pattern.to_string()];
    };
    // Find the matching close brace for this group (no nested-group support in
    // the donor agents' usage; treat the first unmatched `}` as the close).
    let Some(close_rel) = pattern[open..].find('}') else {
        return vec![pattern.to_string()];
    };
    let close = open + close_rel;
    let (head, rest) = (&pattern[..open], &pattern[close + 1..]);
    let mut out = Vec::new();
    for option in pattern[open + 1..close].split(',') {
        for tail in expand_braces_inner(rest, max, capped) {
            out.push(format!("{head}{option}{tail}"));
            if out.len() >= max {
                *capped = true;
                return out;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{execute, execute_with_caps, expand_braces, GlobInput};

    #[test]
    fn no_braces_passes_through() {
        assert_eq!(expand_braces("src/**/*.rs"), vec!["src/**/*.rs"]);
    }

    #[test]
    fn single_group_expands() {
        assert_eq!(
            expand_braces("*.{ts,tsx}"),
            vec!["*.ts".to_string(), "*.tsx".to_string()]
        );
    }

    #[test]
    fn multiple_groups_expand_cartesian() {
        assert_eq!(
            expand_braces("{a,b}/{c,d}.md").len(),
            4,
            "two 2-way groups -> 4 patterns"
        );
    }

    #[test]
    fn agent_style_file_list_expands() {
        let pats = expand_braces("{design.md,DESIGN.md,.hallmark/log.json,**/package.json}");
        assert_eq!(pats.len(), 4);
        assert!(pats.contains(&"design.md".to_string()));
        assert!(pats.contains(&"**/package.json".to_string()));
    }

    #[test]
    fn recursive_match_prunes_ignored_trees() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("docs/nested")).unwrap();
        std::fs::create_dir_all(root.path().join("target/deep")).unwrap();
        std::fs::write(root.path().join("wanted.md"), "yes").unwrap();
        std::fs::write(root.path().join("docs/nested/wanted.md"), "yes").unwrap();
        std::fs::write(root.path().join("target/deep/wanted.md"), "no").unwrap();

        let out = execute(
            root.path(),
            true,
            GlobInput {
                pattern: "**/wanted.md".into(),
            },
        )
        .unwrap();

        assert_eq!(out.content, "docs/nested/wanted.md\nwanted.md");
    }

    #[test]
    fn full_access_parent_glob_uses_safe_concrete_prefix() {
        let parent = tempfile::tempdir().unwrap();
        let workspace = parent.path().join("current");
        let sibling = parent.path().join("Doorquoter");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(sibling.join("docs")).unwrap();
        std::fs::write(sibling.join("README.md"), "root").unwrap();
        std::fs::write(sibling.join("docs/quote.md"), "nested").unwrap();

        let out = execute(
            &workspace,
            false,
            GlobInput {
                pattern: "../Doorquoter/**/*.md".into(),
            },
        )
        .unwrap();

        assert_eq!(
            out.content,
            "../Doorquoter/README.md\n../Doorquoter/docs/quote.md"
        );
    }

    #[test]
    fn confined_parent_glob_is_rejected_by_shared_path_policy() {
        let parent = tempfile::tempdir().unwrap();
        let workspace = parent.path().join("current");
        let sibling = parent.path().join("Doorquoter");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();

        let error = execute(
            &workspace,
            true,
            GlobInput {
                pattern: "../Doorquoter/**/*.md".into(),
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("path escapes workspace"));
    }

    #[test]
    fn parent_segment_after_wildcard_remains_unsafe() {
        let root = tempfile::tempdir().unwrap();
        let error = execute(
            root.path(),
            false,
            GlobInput {
                pattern: "src/**/../secret.md".into(),
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("unsafe glob pattern"));
    }

    #[cfg(unix)]
    #[test]
    fn recursive_match_never_follows_or_returns_symlinks() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.md"), "no").unwrap();
        symlink(outside.path(), root.path().join("linked-dir")).unwrap();
        symlink(
            outside.path().join("secret.md"),
            root.path().join("linked-file.md"),
        )
        .unwrap();
        std::fs::write(root.path().join("visible.md"), "yes").unwrap();

        let out = execute(
            root.path(),
            true,
            GlobInput {
                pattern: "**/*.md".into(),
            },
        )
        .unwrap();
        assert_eq!(out.content, "visible.md");
    }

    #[test]
    fn candidate_cap_is_reported_as_partial_not_complete() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..5 {
            std::fs::write(root.path().join(format!("{index}.txt")), "x").unwrap();
        }
        let out = execute_with_caps(
            root.path(),
            true,
            GlobInput {
                pattern: "*.txt".into(),
            },
            100,
            2,
        )
        .unwrap();
        assert!(out.summary.contains("Partial results"), "{}", out.summary);
        assert!(out.summary.contains("candidate cap 2"), "{}", out.summary);
    }
}
