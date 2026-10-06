//! CodeGraph agent tools — the query surface over Phoenix's symbol index.
//!
//! These let any agent reason from a project map instead of re-reading files:
//! - `symbol_search` — find where something lives by meaning-style query.
//! - `callers` — who calls a symbol (before you change it).
//! - `callees` — what a symbol calls.
//! - `impact` — transitive callers affected if you change a symbol.
//! - `file_symbols` — compact outline of one file (cheaper than `read`).
//!
//! Query tools open the existing index at `<state_root>/codegraph.sqlite`.
//! The runtime refreshes once at turn start and once after a completed coding
//! task; queries never crawl the workspace between edits. `index_codebase` is
//! the explicit recovery/manual refresh surface. Output is the compact,
//! token-thrifty format from `codegraph::format`.

use std::path::Path;

use anyhow::{bail, Result};
use serde::Deserialize;

use crate::codegraph::{self, CodeGraph};

use super::ToolOutput;

#[derive(Debug, Deserialize)]
pub struct SymbolSearchInput {
    pub query: String,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct SymbolNameInput {
    /// Symbol name (function/type/etc). Bare name, not a path. `symbol` is
    /// accepted as an alias because models frequently reach for it (and often
    /// pass a qualified `file.rs::symbol` form, which `bare_symbol` normalizes).
    #[serde(alias = "symbol")]
    pub name: String,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Reduce a possibly-qualified symbol reference to the bare symbol the graph
/// indexes: `src/runtime/runner.rs::execute_first_slice` -> `execute_first_slice`,
/// `Type::method` -> `method`. A plain bare name passes through unchanged.
fn bare_symbol(raw: &str) -> &str {
    raw.trim()
        .rsplit("::")
        .next()
        .unwrap_or(raw)
        .rsplit('/')
        .next()
        .unwrap_or(raw)
        .trim()
}

#[derive(Debug, Deserialize)]
pub struct FileSymbolsInput {
    /// Workspace-relative file path.
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct CallPathInput {
    /// Start symbol (bare name or qualified — normalized like the other tools).
    pub from: String,
    /// Target symbol.
    pub to: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct IndexCodebaseInput {}

/// Open the index without refreshing between queries. A completely new index
/// receives one initial build so direct tool use outside the mesh is useful.
fn open_indexed(state_root: &Path, workspace_root: &Path) -> Result<CodeGraph> {
    let graph = CodeGraph::open(&codegraph::index_path(state_root))?;
    // The graph database is Phoenix-global, while callers can switch between
    // workspaces. A non-empty index is not proof that it belongs to THIS
    // workspace. The tiny project-map projection carries the canonical root;
    // if it is absent or stamped for another project, rebuild once before the
    // first query instead of confidently answering from a different repo.
    let needs_refresh = graph.stats()?.file_count == 0
        || codegraph::project_map(state_root, workspace_root, 1).is_none();
    if needs_refresh && codegraph::looks_like_project_root(workspace_root) {
        drop(graph);
        codegraph::refresh_index(state_root, workspace_root)?;
        return CodeGraph::open(&codegraph::index_path(state_root));
    }
    Ok(graph)
}

pub fn index_codebase(
    state_root: &Path,
    workspace_root: &Path,
    _input: IndexCodebaseInput,
) -> Result<ToolOutput> {
    if !codegraph::looks_like_project_root(workspace_root) {
        bail!(
            "index_codebase requires a project root with a VCS or build-system marker: {}",
            workspace_root.display()
        );
    }
    let receipt = codegraph::refresh_index(state_root, workspace_root)?;
    Ok(ToolOutput {
        summary: format!(
            "codebase index refreshed: {reindexed} changed file(s), {} total file(s), {} symbol(s).",
            receipt.indexed_files, receipt.symbols,
            reindexed = receipt.changed_files,
        ),
        content: format!(
            "Index ready for exploration.\nChanged files indexed: {reindexed}\nIndexed files: {}\nSymbols: {}\nRelationships: {}",
            receipt.indexed_files,
            receipt.symbols,
            receipt.relationships,
            reindexed = receipt.changed_files,
        ),
    })
}

pub fn symbol_search(
    state_root: &Path,
    workspace_root: &Path,
    input: SymbolSearchInput,
) -> Result<ToolOutput> {
    let query = input.query.trim();
    if query.is_empty() {
        bail!("symbol_search query cannot be empty");
    }
    let limit = input.limit.unwrap_or(12).clamp(1, 40);
    let graph = open_indexed(state_root, workspace_root)?;
    let results = graph.search(query, limit)?;
    Ok(ToolOutput {
        summary: format!("symbol_search: {} match(es) for {query:?}.", results.len()),
        content: codegraph::format::symbol_list("Symbols", &results, limit),
    })
}

pub fn callers(
    state_root: &Path,
    workspace_root: &Path,
    input: SymbolNameInput,
) -> Result<ToolOutput> {
    let name = bare_symbol(&input.name);
    if name.is_empty() {
        bail!("callers requires a symbol name");
    }
    let limit = input.limit.unwrap_or(25).clamp(1, 100);
    let graph = open_indexed(state_root, workspace_root)?;
    let results = graph.callers(name, limit)?;
    Ok(ToolOutput {
        summary: format!("callers of `{name}`: {}.", results.len()),
        content: codegraph::format::symbol_list(&format!("Callers of `{name}`"), &results, limit),
    })
}

pub fn callees(
    state_root: &Path,
    workspace_root: &Path,
    input: SymbolNameInput,
) -> Result<ToolOutput> {
    let name = bare_symbol(&input.name);
    if name.is_empty() {
        bail!("callees requires a symbol name");
    }
    let limit = input.limit.unwrap_or(25).clamp(1, 100);
    let graph = open_indexed(state_root, workspace_root)?;
    let results = graph.callees(name, limit)?;
    Ok(ToolOutput {
        summary: format!("callees of `{name}`: {}.", results.len()),
        content: codegraph::format::callee_list(&format!("`{name}` calls"), &results, limit),
    })
}

pub fn impact(
    state_root: &Path,
    workspace_root: &Path,
    input: SymbolNameInput,
) -> Result<ToolOutput> {
    let name = bare_symbol(&input.name);
    if name.is_empty() {
        bail!("impact requires a symbol name");
    }
    let graph = open_indexed(state_root, workspace_root)?;
    let results = graph.impact(name)?;
    Ok(ToolOutput {
        summary: format!("impact of `{name}`: {} affected symbol(s).", results.len()),
        content: codegraph::format::impact_list(name, &results, 60),
    })
}

pub fn call_path(
    state_root: &Path,
    workspace_root: &Path,
    input: CallPathInput,
) -> Result<ToolOutput> {
    let from = bare_symbol(&input.from);
    let to = bare_symbol(&input.to);
    if from.is_empty() || to.is_empty() {
        bail!("call_path requires both `from` and `to` symbol names");
    }
    let graph = open_indexed(state_root, workspace_root)?;
    let chain = graph.call_path(from, to)?;
    let summary = if chain.is_empty() {
        format!("call_path `{from}` → `{to}`: no static path found.")
    } else {
        format!("call_path `{from}` → `{to}`: {} hop(s).", chain.len() - 1)
    };
    Ok(ToolOutput {
        summary,
        content: codegraph::format::path_list(from, to, &chain),
    })
}

pub fn file_symbols(
    state_root: &Path,
    workspace_root: &Path,
    input: FileSymbolsInput,
) -> Result<ToolOutput> {
    let path = input.path.trim();
    if path.is_empty() {
        bail!("file_symbols requires a path");
    }
    let graph = open_indexed(state_root, workspace_root)?;
    let results = graph.file_symbols(path)?;
    Ok(ToolOutput {
        summary: format!("{path}: {} symbol(s).", results.len()),
        content: codegraph::format::symbol_list(&format!("Symbols in {path}"), &results, 200),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn bare_symbol_normalizes_qualified_references() {
        assert_eq!(bare_symbol("execute_first_slice"), "execute_first_slice");
        assert_eq!(
            bare_symbol("src/runtime/runner.rs::execute_first_slice"),
            "execute_first_slice"
        );
        assert_eq!(bare_symbol("ToolExecutor::execute"), "execute");
        assert_eq!(bare_symbol("  spaced  "), "spaced");
    }

    #[test]
    fn symbol_name_input_accepts_symbol_alias() {
        // Models often pass `symbol` (and a qualified path); both must resolve.
        let parsed: SymbolNameInput =
            serde_json::from_value(serde_json::json!({"symbol": "mod.rs::execute"})).unwrap();
        assert_eq!(bare_symbol(&parsed.name), "execute");
    }

    #[test]
    fn symbol_search_tool_finds_symbol() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("auth.rs"),
            "pub fn authenticate_user(t: &str) -> bool { true }\n",
        )
        .unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        let state = dir.path().join(".phoenix");

        let out = symbol_search(
            &state,
            dir.path(),
            SymbolSearchInput {
                query: "authenticate".to_string(),
                limit: None,
            },
        )
        .unwrap();
        assert!(out.content.contains("authenticate_user"));
    }

    #[test]
    fn queries_hold_a_stable_snapshot_until_one_explicit_refresh() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        let source = src.join("lib.rs");
        fs::write(&source, "pub fn first_symbol() {}\n").unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        let state = dir.path().join(".phoenix");

        let first = symbol_search(
            &state,
            dir.path(),
            SymbolSearchInput {
                query: "first_symbol".to_string(),
                limit: None,
            },
        )
        .unwrap();
        assert!(first.content.contains("first_symbol"));

        fs::write(
            &source,
            "pub fn first_symbol() {}\npub fn added_after_edit() {}\n",
        )
        .unwrap();
        let stale = symbol_search(
            &state,
            dir.path(),
            SymbolSearchInput {
                query: "added_after_edit".to_string(),
                limit: None,
            },
        )
        .unwrap();
        assert!(!stale.content.contains("added_after_edit"));

        let receipt = index_codebase(&state, dir.path(), IndexCodebaseInput::default()).unwrap();
        assert!(receipt.summary.contains("1 changed file(s)"));
        let fresh = symbol_search(
            &state,
            dir.path(),
            SymbolSearchInput {
                query: "added_after_edit".to_string(),
                limit: None,
            },
        )
        .unwrap();
        assert!(fresh.content.contains("added_after_edit"));
    }

    #[test]
    fn first_query_rebuilds_a_nonempty_index_owned_by_another_workspace() {
        let dir = tempdir().unwrap();
        let state = dir.path().join(".phoenix");
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        for workspace in [&first, &second] {
            fs::create_dir_all(workspace.join("src")).unwrap();
            fs::write(workspace.join("Cargo.toml"), "[package]\n").unwrap();
        }
        fs::write(first.join("src/lib.rs"), "fn only_in_first() {}\n").unwrap();
        fs::write(
            second.join("src/lib.rs"),
            "fn browser_surface_for_second_workspace() {}\n",
        )
        .unwrap();

        index_codebase(&state, &first, IndexCodebaseInput::default()).unwrap();
        let result = symbol_search(
            &state,
            &second,
            SymbolSearchInput {
                query: "browser_surface_for_second_workspace".to_string(),
                limit: None,
            },
        )
        .unwrap();
        assert!(
            result
                .content
                .contains("browser_surface_for_second_workspace"),
            "{}",
            result.content
        );
        assert!(!result.content.contains("only_in_first"));
    }

    #[test]
    fn call_path_tool_renders_the_hop_chain() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("lib.rs"),
            "fn handler() { service(); }\nfn service() { store(); }\nfn store() {}\n",
        )
        .unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        let state = dir.path().join(".phoenix");

        let out = call_path(
            &state,
            dir.path(),
            CallPathInput {
                from: "handler".to_string(),
                to: "store".to_string(),
            },
        )
        .unwrap();
        assert!(out.summary.contains("2 hop(s)"), "{}", out.summary);
        assert!(out.content.contains("service"), "{}", out.content);

        // A miss stays honest about static-only edges.
        let miss = call_path(
            &state,
            dir.path(),
            CallPathInput {
                from: "store".to_string(),
                to: "handler".to_string(),
            },
        )
        .unwrap();
        assert!(
            miss.content.contains("static call edges only"),
            "{}",
            miss.content
        );
    }

    #[test]
    fn callers_tool_reports_callers() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("lib.rs"),
            "fn login() { authenticate_user(\"x\"); }\nfn authenticate_user(t: &str) {}\n",
        )
        .unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        let state = dir.path().join(".phoenix");

        let out = callers(
            &state,
            dir.path(),
            SymbolNameInput {
                name: "authenticate_user".to_string(),
                limit: None,
            },
        )
        .unwrap();
        assert!(out.content.contains("login"));
    }
}
