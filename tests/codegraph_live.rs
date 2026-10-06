//! Live smoke test: build the CodeGraph index against Phoenix's own source and
//! run real queries. Confirms tree-sitter extraction + SQLite queries work on a
//! real multi-thousand-symbol codebase, not just unit fixtures.
//!
//! Run explicitly: `cargo test --test codegraph_live -- --ignored --nocapture`.

use phoenix_agent::codegraph::CodeGraph;
use tempfile::tempdir;

#[test]
#[ignore = "indexes the whole workspace; run on demand"]
fn indexes_phoenix_source_and_queries_real_symbols() {
    // Workspace root = the phoenix_agent crate dir (CARGO_MANIFEST_DIR).
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let index_dir = tempdir().unwrap();
    let index_path = index_dir.path().join("codegraph.sqlite");

    let mut graph = CodeGraph::open(&index_path).unwrap();
    let reindexed = graph.refresh(workspace).unwrap();
    assert!(
        reindexed > 50,
        "expected to index many files, got {reindexed}"
    );

    let stats = graph.stats().unwrap();
    println!(
        "Indexed {} files, {} symbols, {} relationships",
        stats.file_count, stats.symbol_count, stats.relationship_count
    );
    assert!(stats.symbol_count > 500, "expected >500 symbols");

    // A symbol we know exists: the executor.
    let hits = graph.search("ToolExecutor", 5).unwrap();
    assert!(
        hits.iter().any(|s| s.name == "ToolExecutor"),
        "should find ToolExecutor in the index"
    );

    // callers/impact on a known function should resolve without error.
    let callers = graph.callers("refresh", 20).unwrap();
    println!("callers of `refresh`: {}", callers.len());

    // Incremental: second refresh with no edits reindexes nothing.
    let again = graph.refresh(workspace).unwrap();
    assert_eq!(again, 0, "second refresh should be a no-op");
}
