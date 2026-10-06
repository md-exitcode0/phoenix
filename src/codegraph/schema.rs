//! SQLite schema for the symbol graph.
//!
//! Adapted from the CodeGraph donor's data model (symbols / files /
//! relationships / snapshots), trimmed to what Phoenix queries today and shaped
//! for `rusqlite`. FTS5 backs `symbol_search`; the `relationships` table backs
//! `callers`/`callees`/`impact`. The `files.hash` column drives incremental
//! refresh — a file is re-extracted only when its content hash changes.

use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension};

/// Bump when the DDL changes in a backwards-incompatible way; [`open`] drops and
/// rebuilds the index when the stored version differs, so a stale schema never
/// silently returns wrong results.
pub const SCHEMA_VERSION: i64 = 1;

const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS files (
    id           INTEGER PRIMARY KEY,
    path         TEXT NOT NULL UNIQUE,
    language     TEXT NOT NULL,
    hash         TEXT NOT NULL,
    symbol_count INTEGER NOT NULL DEFAULT 0,
    indexed_at   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS symbols (
    id        INTEGER PRIMARY KEY,
    file_id   INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    name      TEXT NOT NULL,
    kind      TEXT NOT NULL,
    start_line INTEGER NOT NULL,
    end_line   INTEGER NOT NULL,
    signature TEXT,
    doc       TEXT,
    language  TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
CREATE INDEX IF NOT EXISTS idx_symbols_file ON symbols(file_id);

-- Relationships are stored by symbol NAME (source) → callee NAME (target_name)
-- rather than resolved id, because a call site references a name that may be
-- defined in another file not yet indexed. Resolution to a target symbol id is
-- done lazily at query time. `kind` ∈ {calls, imports, implements, uses_type}.
CREATE TABLE IF NOT EXISTS relationships (
    id          INTEGER PRIMARY KEY,
    source_id   INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
    target_name TEXT NOT NULL,
    kind        TEXT NOT NULL,
    line        INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_rel_source ON relationships(source_id);
CREATE INDEX IF NOT EXISTS idx_rel_target ON relationships(target_name);

-- FTS5 over symbol name + signature + doc for meaning-style search.
CREATE VIRTUAL TABLE IF NOT EXISTS symbols_fts USING fts5(
    name, signature, doc,
    content='symbols',
    content_rowid='id'
);

-- Keep the FTS index in sync with the symbols table via triggers.
CREATE TRIGGER IF NOT EXISTS symbols_ai AFTER INSERT ON symbols BEGIN
    INSERT INTO symbols_fts(rowid, name, signature, doc)
    VALUES (new.id, new.name, new.signature, new.doc);
END;
CREATE TRIGGER IF NOT EXISTS symbols_ad AFTER DELETE ON symbols BEGIN
    INSERT INTO symbols_fts(symbols_fts, rowid, name, signature, doc)
    VALUES('delete', old.id, old.name, old.signature, old.doc);
END;
"#;

/// Open the index at `path`, creating/migrating schema as needed. If the stored
/// `schema_version` differs from [`SCHEMA_VERSION`], the file is rebuilt from
/// scratch so we never query a stale shape.
pub fn open(path: &std::path::Path) -> Result<Connection> {
    crate::config::private_io::prepare_private_parent(path)
        .context("preparing codegraph index parent")?;
    crate::config::private_io::reject_symlink_components(path)?;
    let new_or_empty = match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            bail!("refusing unsafe codegraph index {}", path.display());
        }
        Ok(metadata) => metadata.len() == 0,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => return Err(error.into()),
    };

    let mut conn =
        Connection::open_with_flags(path, OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW)
            .context("failed to open codegraph index")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .context("enabling codegraph foreign keys")?;

    let meta_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='meta')",
            [],
            |row| row.get(0),
        )
        .context("inspecting codegraph schema marker")?;
    let existing = if meta_exists {
        let raw: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .optional()
            .context("reading codegraph schema version")?;
        let raw = raw.context("codegraph index is missing its schema_version marker")?;
        Some(
            raw.parse::<i64>()
                .with_context(|| format!("invalid codegraph schema version {raw:?}"))?,
        )
    } else if new_or_empty {
        None
    } else {
        bail!(
            "existing codegraph index {} has no schema marker; refusing a destructive rebuild",
            path.display()
        );
    };

    if existing != Some(SCHEMA_VERSION) {
        rebuild(&mut conn)?;
    }

    conn.pragma_update(None, "journal_mode", "WAL")
        .context("enabling codegraph WAL mode")?;

    Ok(conn)
}

fn rebuild(conn: &mut Connection) -> Result<()> {
    let tx = conn
        .transaction()
        .context("starting codegraph schema rebuild")?;
    tx.execute_batch(
        r#"
        DROP TRIGGER IF EXISTS symbols_ai;
        DROP TRIGGER IF EXISTS symbols_ad;
        DROP TABLE IF EXISTS symbols_fts;
        DROP TABLE IF EXISTS relationships;
        DROP TABLE IF EXISTS symbols;
        DROP TABLE IF EXISTS files;
        DROP TABLE IF EXISTS meta;
        "#,
    )
    .context("failed to drop stale codegraph schema")?;
    tx.execute_batch(DDL)
        .context("failed to create codegraph schema")?;
    tx.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', ?1)",
        [SCHEMA_VERSION.to_string()],
    )?;
    tx.commit().context("committing codegraph schema rebuild")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn open_creates_schema_and_is_idempotent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("codegraph.sqlite");

        let conn = open(&path).unwrap();
        let version: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION.to_string());
        drop(conn);

        // Re-open: must not error and must preserve the schema row.
        let conn2 = open(&path).unwrap();
        let count: i64 = conn2
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn missing_or_invalid_schema_markers_never_destroy_existing_tables() {
        let dir = tempdir().unwrap();
        let missing_path = dir.path().join("missing-marker.sqlite");
        let conn = Connection::open(&missing_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sentinel(value TEXT); INSERT INTO sentinel VALUES('keep');",
        )
        .unwrap();
        drop(conn);
        assert!(open(&missing_path).is_err());
        let conn = Connection::open(&missing_path).unwrap();
        let value: String = conn
            .query_row("SELECT value FROM sentinel", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "keep");
        drop(conn);

        let invalid_path = dir.path().join("invalid-marker.sqlite");
        let conn = Connection::open(&invalid_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);\
             INSERT INTO meta VALUES('schema_version','not-a-number');\
             CREATE TABLE sentinel(value TEXT); INSERT INTO sentinel VALUES('keep');",
        )
        .unwrap();
        drop(conn);
        assert!(open(&invalid_path).is_err());
        let conn = Connection::open(&invalid_path).unwrap();
        let value: String = conn
            .query_row("SELECT value FROM sentinel", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "keep");
    }

    #[test]
    fn explicit_old_schema_version_rebuilds_atomically() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("old.sqlite");
        let conn = open(&path).unwrap();
        conn.execute("UPDATE meta SET value='0' WHERE key='schema_version'", [])
            .unwrap();
        conn.execute(
            "INSERT INTO files(path,language,hash,symbol_count,indexed_at) VALUES('old.rs','rust','x',0,'now')",
            [],
        )
        .unwrap();
        drop(conn);

        let conn = open(&path).unwrap();
        let files: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
            .unwrap();
        assert_eq!(files, 0);
    }

    #[cfg(unix)]
    #[test]
    fn schema_open_does_not_follow_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let target = dir.path().join("target.sqlite");
        std::fs::write(&target, b"not a database").unwrap();
        let linked = dir.path().join("linked.sqlite");
        symlink(&target, &linked).unwrap();
        assert!(open(&linked).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"not a database");
    }
}
