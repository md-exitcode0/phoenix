//! CodeGraph — Phoenix's native symbol index.
//!
//! Phoenix-engineered adaptation of the CodeGraph donor pattern (which is
//! TypeScript + web-tree-sitter + better-sqlite3 over MCP). We keep only the
//! ideas, not the architecture: native `tree-sitter` extraction into a local
//! `rusqlite` graph stored at `.phoenix/codegraph.sqlite`, queried by agents so
//! they reason from a project map instead of re-reading files blindly.
//!
//! Strategy shift this enables (taught in agent prompts): *query the graph for
//! structure, read files for implementation detail.* That is where the ~70%
//! context-token saving comes from — a `callers`/`impact` query returns a few
//! compact lines instead of multiple full-file reads.
//!
//! ## Layers
//! - [`schema`] — SQLite DDL + connection bootstrap.
//! - [`extract`] — tree-sitter AST → symbols + relationships (Rust first).
//! - [`store`] — index build/refresh + the query surface (search/callers/...).
//! - [`format`] — compact, agent-optimized rendering of query results.
//!
//! Languages: Rust today; the schema and extractor dispatch are language-keyed
//! so Python/TS/Go slot in later without a rewrite.

mod extract;
pub mod format;
mod schema;
mod store;

pub use store::CodeGraph;

use std::path::{Path, PathBuf};

const PROJECT_MAP_SNAPSHOT_MAX_BYTES: usize = 512 * 1024;

#[derive(serde::Serialize, serde::Deserialize)]
struct ProjectMapSnapshot {
    workspace_root: String,
    rendered: String,
}

/// Canonical on-disk location of the symbol index, under the Phoenix state root
/// (`.phoenix/`). Gitignored runtime state, same tier as sessions/cache.
pub fn index_path(state_root: &Path) -> PathBuf {
    state_root.join("codegraph.sqlite")
}

fn project_map_snapshot_path(state_root: &Path) -> PathBuf {
    state_root.join("codegraph-project-map.json")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexRefreshReceipt {
    pub changed_files: usize,
    pub indexed_files: usize,
    pub symbols: usize,
    pub relationships: usize,
}

/// Incrementally refresh one project index. This is the single refresh path
/// used at turn boundaries and by the explicit recovery tool; structural query
/// tools only read the stable snapshot.
pub fn refresh_index(
    state_root: &Path,
    workspace_root: &Path,
) -> anyhow::Result<IndexRefreshReceipt> {
    if !looks_like_project_root(workspace_root) {
        anyhow::bail!(
            "codebase indexing requires a project root with a VCS or build-system marker: {}",
            workspace_root.display()
        );
    }
    let mut graph = CodeGraph::open(&index_path(state_root))?;
    let changed_files = graph.refresh(workspace_root)?;
    let stats = graph.stats()?;
    let overview = graph.overview(12).unwrap_or_default();
    let hotspots = graph.hotspots(8).unwrap_or_default();
    let snapshot = ProjectMapSnapshot {
        workspace_root: std::fs::canonicalize(workspace_root)?
            .to_string_lossy()
            .into_owned(),
        rendered: format::overview_with_hotspots(&stats, &overview, &hotspots),
    };
    crate::config::private_io::atomic_write_private(
        &project_map_snapshot_path(state_root),
        &serde_json::to_vec(&snapshot)?,
    )?;
    Ok(IndexRefreshReceipt {
        changed_files,
        indexed_files: stats.file_count,
        symbols: stats.symbol_count,
        relationships: stats.relationship_count,
    })
}

/// Read the stable index snapshot and return a compact project-map string for
/// preload injection, or `None` when the workspace has no indexed symbols.
///
/// This function is deliberately read-only. A recursive refresh in the
/// first-token path made cold turns wait on a full workspace crawl before the
/// model could do anything. Turn finalization already refreshes changed files
/// once, which keeps the snapshot current without taxing every prompt. Errors
/// are swallowed into `None` — a missing map must never break a turn.
pub fn project_map(state_root: &Path, workspace_root: &Path, top_files: usize) -> Option<String> {
    // Prompt start must never open the potentially-hundreds-of-megabytes
    // SQLite graph. `CodeGraph::open` performs deliberate integrity bounds
    // checks, and those checks are appropriate for indexing/query tools but
    // too expensive for the first-token path. Index refresh atomically writes
    // this tiny, workspace-stamped projection once.
    let bytes = crate::config::private_io::read_private_file_limited(
        &project_map_snapshot_path(state_root),
        PROJECT_MAP_SNAPSHOT_MAX_BYTES,
    )
    .ok()??;
    let snapshot: ProjectMapSnapshot = serde_json::from_slice(&bytes).ok()?;
    let active_workspace = std::fs::canonicalize(workspace_root).ok()?;
    if snapshot.workspace_root != active_workspace.to_string_lossy() {
        return None;
    }
    // The snapshot is already bounded to twelve files at write time. Preserve
    // the parameter for API compatibility; callers currently request twelve.
    let _ = top_files;
    (!snapshot.rendered.trim().is_empty()).then_some(snapshot.rendered)
}

/// A directory is a project root when it carries a VCS or build-system marker.
pub fn looks_like_project_root(dir: &Path) -> bool {
    const MARKERS: [&str; 8] = [
        ".git",
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "go.mod",
        "pom.xml",
        "build.gradle",
        "Makefile",
    ];
    MARKERS.iter().any(|m| dir.join(m).exists())
}
