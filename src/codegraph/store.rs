//! The CodeGraph store: index build/refresh + query surface.
//!
//! `CodeGraph` owns the SQLite connection and exposes the operations agents and
//! the librarian use:
//! - [`CodeGraph::refresh`] — incremental whole-repo index (hash-gated).
//! - [`CodeGraph::search`] — FTS symbol search ("where is auth handled?").
//! - [`CodeGraph::callers`] / [`CodeGraph::callees`] — direct call edges.
//! - [`CodeGraph::impact`] — transitive caller closure (ranked by distance).
//! - [`CodeGraph::file_symbols`] — outline of one file.
//! - [`CodeGraph::stats`] / [`CodeGraph::overview`] — project map for preload.
//!
//! Relationships are stored by callee *name* and resolved to definitions lazily
//! at query time (a name may resolve to several defs — overloads, trait impls —
//! and we surface all of them rather than guess).

use std::collections::{HashSet, VecDeque};
use std::path::Path;

use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OptionalExtension};

use super::{extract, schema};
use crate::tools::is_ignored_dir;

/// Maximum files indexed in one refresh — guards against a giant tree.
const MAX_INDEXED_FILES: usize = 20_000;
/// Transitive-closure ceiling for `impact`, so a hot symbol can't explode.
const MAX_IMPACT_NODES: usize = 200;
/// Visited-node budget for the call-path BFS — bounds pathological graphs
/// without cutting real paths short in normal repos.
const MAX_PATH_NODES: usize = 4000;
const MAX_QUERY_RESULTS: usize = 1_000;
const MAX_QUERY_BYTES: usize = 4 * 1024;
const MAX_QUERY_TERMS: usize = 64;
const MAX_SOURCE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SCANNED_ENTRIES: usize = 100_000;
const MAX_SCANNED_DIRECTORIES: usize = 20_000;
const MAX_SCAN_DEPTH: usize = 128;
const MAX_INDEX_DB_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_INDEX_PATH_BYTES: usize = 4 * 1024;
const MAX_SYMBOL_NAME_BYTES: usize = 4 * 1024;
const MAX_SYMBOL_TEXT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Struct,
    Enum,
    Trait,
    Impl,
    Module,
    TypeAlias,
    Const,
    Static,
    Macro,
}

impl SymbolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Impl => "impl",
            Self::Module => "module",
            Self::TypeAlias => "type",
            Self::Const => "const",
            Self::Static => "static",
            Self::Macro => "macro",
        }
    }
}

/// A relationship rooted at a symbol, stored by target name (see module docs).
#[derive(Debug, Clone)]
pub struct SymbolRelation {
    pub target_name: String,
    pub kind: String,
    pub line: usize,
}

/// A symbol row as returned by queries.
#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub kind: String,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub signature: Option<String>,
    pub doc: Option<String>,
}

/// Snapshot of the index for preload/overview.
#[derive(Debug, Clone, Default)]
pub struct GraphStats {
    pub file_count: usize,
    pub symbol_count: usize,
    pub relationship_count: usize,
    pub by_kind: Vec<(String, usize)>,
}

pub struct CodeGraph {
    conn: Connection,
}

impl CodeGraph {
    /// Open (creating/migrating) the index at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        crate::config::private_io::prepare_private_parent(path)
            .context("preparing codegraph index directory")?;
        crate::config::private_io::reject_symlink_components(path)?;
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                bail!("refusing unsafe codegraph index {}", path.display());
            }
            Ok(metadata) if metadata.len() > MAX_INDEX_DB_BYTES => {
                bail!(
                    "codegraph index {} is too large ({} bytes; max {MAX_INDEX_DB_BYTES})",
                    path.display(),
                    metadata.len()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        validate_sqlite_sidecars(path)?;
        let conn = schema::open(path)?;
        crate::config::private_io::reject_symlink_components(path)?;
        validate_sqlite_sidecars(path)?;
        validate_stored_text_bounds(&conn)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(Self { conn })
    }

    /// Incrementally index every supported file under `workspace_root`. Files
    /// whose content hash is unchanged are skipped; deleted files are dropped.
    /// Returns the number of files (re)indexed this pass.
    pub fn refresh(&mut self, workspace_root: &Path) -> Result<usize> {
        let workspace_root = std::fs::canonicalize(workspace_root)
            .with_context(|| format!("resolving workspace {}", workspace_root.display()))?;
        if !std::fs::metadata(&workspace_root)?.is_dir() {
            bail!("codegraph workspace is not a directory");
        }
        let files = discover_files(&workspace_root)?;
        let mut seen_paths = HashSet::new();
        let mut reindexed = 0usize;

        let tx = self.conn.transaction()?;
        for abs in &files {
            let rel_path = abs
                .strip_prefix(&workspace_root)
                .context("discovered source escaped the workspace")?;
            let rel = rel_path
                .to_str()
                .context("codegraph source path is not UTF-8")?
                .to_string();
            if rel.len() > MAX_INDEX_PATH_BYTES {
                bail!("codegraph source path exceeds {MAX_INDEX_PATH_BYTES} bytes");
            }
            seen_paths.insert(rel.clone());

            let Some(language) = extract::language_for_path(abs) else {
                continue;
            };
            let source = read_source_file(abs)?;
            let hash = content_hash(&source);

            let existing_hash: Option<String> = tx
                .query_row("SELECT hash FROM files WHERE path = ?1", [&rel], |r| {
                    r.get(0)
                })
                .optional()?;
            if existing_hash.as_deref() == Some(hash.as_str()) {
                continue; // unchanged → skip
            }

            // Changed (or new): drop old rows for this file, then re-extract.
            tx.execute("DELETE FROM files WHERE path = ?1", [&rel])?;
            let symbols = extract::extract_file(language, &source);
            validate_extracted_symbols(&symbols)?;
            tx.execute(
                "INSERT INTO files(path, language, hash, symbol_count, indexed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    rel,
                    language,
                    hash,
                    symbols.len() as i64,
                    chrono::Utc::now().to_rfc3339()
                ],
            )?;
            let file_id = tx.last_insert_rowid();

            for sym in symbols {
                tx.execute(
                    "INSERT INTO symbols(file_id, name, kind, start_line, end_line, signature, doc, language)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    rusqlite::params![
                        file_id,
                        sym.name,
                        sym.kind.as_str(),
                        sym.start_line as i64,
                        sym.end_line as i64,
                        sym.signature,
                        sym.doc,
                        language,
                    ],
                )?;
                let symbol_id = tx.last_insert_rowid();
                for rel_edge in sym.relations {
                    tx.execute(
                        "INSERT INTO relationships(source_id, target_name, kind, line)
                         VALUES (?1, ?2, ?3, ?4)",
                        rusqlite::params![
                            symbol_id,
                            rel_edge.target_name,
                            rel_edge.kind,
                            rel_edge.line as i64
                        ],
                    )?;
                }
            }
            reindexed += 1;
        }

        // Drop files that vanished from disk.
        {
            let mut stmt = tx.prepare("SELECT path FROM files LIMIT ?1")?;
            let stored: Vec<String> = stmt
                .query_map([MAX_INDEXED_FILES as i64 + 1], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<_>>()?;
            if stored.len() > MAX_INDEXED_FILES {
                bail!("codegraph index contains more than {MAX_INDEXED_FILES} files");
            }
            drop(stmt);
            for path in stored {
                if !seen_paths.contains(&path) {
                    tx.execute("DELETE FROM files WHERE path = ?1", [&path])?;
                }
            }
        }

        tx.commit()?;
        Ok(reindexed)
    }

    /// FTS symbol search. `query` is matched against name/signature/doc.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Symbol>> {
        if query.len() > MAX_QUERY_BYTES {
            bail!(
                "codegraph query is too large ({} bytes; max {MAX_QUERY_BYTES})",
                query.len()
            );
        }
        let fts_query = sanitize_fts_query(query);
        if fts_query.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT s.name, s.kind, f.path, s.start_line, s.end_line, s.signature, s.doc
             FROM symbols_fts
             JOIN symbols s ON s.id = symbols_fts.rowid
             JOIN files f ON f.id = s.file_id
             WHERE symbols_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![fts_query, bounded_limit(limit)],
            row_to_symbol,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Symbols that call `name` (direct callers).
    pub fn callers(&self, name: &str, limit: usize) -> Result<Vec<Symbol>> {
        validate_lookup(name, "symbol name")?;
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT s.name, s.kind, f.path, s.start_line, s.end_line, s.signature, s.doc
             FROM relationships r
             JOIN symbols s ON s.id = r.source_id
             JOIN files f ON f.id = s.file_id
             WHERE r.target_name = ?1 AND r.kind = 'calls'
             ORDER BY f.path, s.start_line
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![name, bounded_limit(limit)], row_to_symbol)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Names that `name`'s body calls (direct callees), resolved to defs when
    /// known. Returns (callee_name, resolved_symbol?) pairs.
    pub fn callees(&self, name: &str, limit: usize) -> Result<Vec<(String, Option<Symbol>)>> {
        validate_lookup(name, "symbol name")?;
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT r.target_name
             FROM relationships r
             JOIN symbols s ON s.id = r.source_id
             WHERE s.name = ?1 AND r.kind = 'calls'
             ORDER BY r.target_name
             LIMIT ?2",
        )?;
        let names: Vec<String> = stmt
            .query_map(rusqlite::params![name, bounded_limit(limit)], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;

        let mut out = Vec::new();
        for callee in names {
            let resolved = self.find_definition(&callee)?;
            out.push((callee, resolved));
        }
        Ok(out)
    }

    /// Transitive closure of callers (who is affected if `name` changes),
    /// breadth-first, ranked by distance. Capped at [`MAX_IMPACT_NODES`].
    pub fn impact(&self, name: &str) -> Result<Vec<(usize, Symbol)>> {
        validate_lookup(name, "symbol name")?;
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(name.to_string());
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        queue.push_back((name.to_string(), 0));
        let mut result = Vec::new();

        while let Some((current, depth)) = queue.pop_front() {
            if result.len() >= MAX_IMPACT_NODES {
                break;
            }
            let callers = self.callers(&current, 100)?;
            for caller in callers {
                if result.len() >= MAX_IMPACT_NODES {
                    break;
                }
                if visited.insert(caller.name.clone()) {
                    result.push((depth + 1, caller.clone()));
                    queue.push_back((caller.name, depth + 1));
                }
            }
        }
        result.sort_by_key(|(distance, _)| *distance);
        Ok(result)
    }

    /// Shortest call path from `from` to `to`, breadth-first over call edges
    /// (the graphify "path/DFS: how X reaches Y" surface). Returns the chain
    /// including both endpoints, each resolved to its definition when known;
    /// empty when no path exists within [`MAX_PATH_NODES`] visited symbols.
    pub fn call_path(&self, from: &str, to: &str) -> Result<Vec<(String, Option<Symbol>)>> {
        validate_lookup(from, "source symbol")?;
        validate_lookup(to, "target symbol")?;
        if from == to {
            let def = self.find_definition(from)?;
            return Ok(vec![(from.to_string(), def)]);
        }
        let mut parent: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(from.to_string());
        let mut queue: VecDeque<String> = VecDeque::new();
        queue.push_back(from.to_string());
        let mut found = false;

        'search: while let Some(current) = queue.pop_front() {
            let remaining = MAX_PATH_NODES.saturating_sub(visited.len());
            if remaining == 0 {
                break;
            }
            for callee in self.callee_names(&current, remaining)? {
                if visited.insert(callee.clone()) {
                    parent.insert(callee.clone(), current.clone());
                    if callee == to {
                        found = true;
                        break 'search;
                    }
                    queue.push_back(callee);
                }
            }
            if visited.len() >= MAX_PATH_NODES {
                break;
            }
        }
        if !found {
            return Ok(Vec::new());
        }

        let mut chain = vec![to.to_string()];
        while let Some(prev) = parent.get(chain.last().expect("chain is non-empty")) {
            chain.push(prev.clone());
        }
        chain.reverse();
        let mut resolved = Vec::with_capacity(chain.len());
        for name in chain {
            let definition = self.find_definition(&name)?;
            resolved.push((name, definition));
        }
        Ok(resolved)
    }

    /// Direct callee names of a symbol, unresolved — the cheap edge lookup the
    /// path BFS runs per node (resolution happens once, on the final chain).
    fn callee_names(&self, name: &str, limit: usize) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT DISTINCT r.target_name
             FROM relationships r
             JOIN symbols s ON s.id = r.source_id
             WHERE s.name = ?1 AND r.kind = 'calls'
             ORDER BY r.target_name LIMIT ?2",
        )?;
        let names = stmt
            .query_map(rusqlite::params![name, bounded_limit(limit)], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(names)
    }

    /// All symbols defined in a file, in source order (a compact outline).
    pub fn file_symbols(&self, path: &str) -> Result<Vec<Symbol>> {
        validate_lookup(path, "file path")?;
        let mut stmt = self.conn.prepare(
            "SELECT s.name, s.kind, f.path, s.start_line, s.end_line, s.signature, s.doc
             FROM symbols s
             JOIN files f ON f.id = s.file_id
             WHERE f.path = ?1
             ORDER BY s.start_line LIMIT ?2",
        )?;
        let symbols: Vec<Symbol> = stmt
            .query_map(
                rusqlite::params![path, MAX_QUERY_RESULTS as i64 + 1],
                row_to_symbol,
            )?
            .collect::<rusqlite::Result<_>>()?;
        if symbols.len() > MAX_QUERY_RESULTS {
            bail!("file has more than {MAX_QUERY_RESULTS} indexed symbols");
        }
        Ok(symbols)
    }

    /// First definition matching `name` (used by callee resolution).
    pub fn find_definition(&self, name: &str) -> Result<Option<Symbol>> {
        validate_lookup(name, "symbol name")?;
        let mut stmt = self.conn.prepare(
            "SELECT s.name, s.kind, f.path, s.start_line, s.end_line, s.signature, s.doc
             FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE s.name = ?1
             ORDER BY CASE s.kind WHEN 'function' THEN 0 ELSE 1 END, f.path
             LIMIT 1",
        )?;
        let mut rows = stmt.query_map([name], row_to_symbol)?;
        rows.next().transpose().map_err(Into::into)
    }

    /// Index statistics for the project map / preload overview.
    pub fn stats(&self) -> Result<GraphStats> {
        let file_count = self
            .conn
            .query_row("SELECT COUNT(*) FROM files", [], |row| row_usize(row, 0))?;
        let symbol_count = self
            .conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| row_usize(row, 0))?;
        let relationship_count =
            self.conn
                .query_row("SELECT COUNT(*) FROM relationships", [], |row| {
                    row_usize(row, 0)
                })?;

        let mut stmt = self.conn.prepare(
            "SELECT kind, COUNT(*) FROM symbols GROUP BY kind ORDER BY COUNT(*) DESC LIMIT 64",
        )?;
        let by_kind = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row_usize(row, 1)?)))?
            .collect::<rusqlite::Result<_>>()?;

        Ok(GraphStats {
            file_count,
            symbol_count,
            relationship_count,
            by_kind,
        })
    }

    /// Load-bearing symbols (donor: graphify's god-node analysis): the symbols
    /// with the highest fan-in across the call graph. These are the project's
    /// hot core — an experienced dev knows them before touching anything; an
    /// agent reading the map should too.
    pub fn hotspots(&self, limit: usize) -> Result<Vec<(String, String, usize)>> {
        let mut stmt = self.conn.prepare(
            "SELECT r.target_name, MIN(f.path), COUNT(*) AS fan_in
             FROM relationships r
             JOIN symbols s ON s.name = r.target_name
             JOIN files f ON f.id = s.file_id
             WHERE r.kind = 'calls'
             GROUP BY r.target_name
             HAVING fan_in >= 3
             ORDER BY fan_in DESC, r.target_name
             LIMIT ?1",
        )?;
        let rows = stmt
            .query_map([bounded_limit(limit)], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    row_usize(r, 2)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Top-level module/file map for preload: the files with the most symbols,
    /// so an agent can see project shape at a glance without reading anything.
    pub fn overview(&self, limit: usize) -> Result<Vec<(String, usize)>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, symbol_count FROM files
             WHERE symbol_count > 0
             ORDER BY symbol_count DESC, path
             LIMIT ?1",
        )?;
        let rows = stmt
            .query_map([bounded_limit(limit)], |r| {
                Ok((r.get::<_, String>(0)?, row_usize(r, 1)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }
}

fn row_to_symbol(row: &rusqlite::Row) -> rusqlite::Result<Symbol> {
    Ok(Symbol {
        name: row.get(0)?,
        kind: row.get(1)?,
        path: row.get(2)?,
        start_line: row_usize(row, 3)?,
        end_line: row_usize(row, 4)?,
        signature: row.get(5)?,
        doc: row.get(6)?,
    })
}

fn row_usize(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<usize> {
    let value = row.get::<_, i64>(index)?;
    usize::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}

fn bounded_limit(limit: usize) -> i64 {
    limit.min(MAX_QUERY_RESULTS) as i64
}

fn validate_lookup(value: &str, label: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > MAX_QUERY_BYTES {
        bail!("{label} must be 1..={MAX_QUERY_BYTES} bytes");
    }
    Ok(())
}

fn validate_stored_text_bounds(connection: &Connection) -> Result<()> {
    let oversized: i64 = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM files WHERE length(CAST(path AS BLOB))>?1
            UNION ALL
            SELECT 1 FROM symbols
             WHERE length(CAST(name AS BLOB))>?2
                OR length(CAST(signature AS BLOB))>?3
                OR length(CAST(doc AS BLOB))>?3
            UNION ALL
            SELECT 1 FROM relationships
             WHERE length(CAST(target_name AS BLOB))>?2
                OR length(CAST(kind AS BLOB))>?2
            LIMIT 1
        )",
        rusqlite::params![
            MAX_INDEX_PATH_BYTES as i64,
            MAX_SYMBOL_NAME_BYTES as i64,
            MAX_SYMBOL_TEXT_BYTES as i64
        ],
        |row| row.get(0),
    )?;
    if oversized != 0 {
        bail!("codegraph index contains an oversized text field");
    }
    Ok(())
}

fn validate_sqlite_sidecars(path: &Path) -> Result<()> {
    for (suffix, max_bytes) in [
        ("-wal", 1024 * 1024 * 1024u64),
        ("-shm", 64 * 1024 * 1024u64),
    ] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let sidecar = std::path::PathBuf::from(name);
        match std::fs::symlink_metadata(&sidecar) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                bail!(
                    "refusing unsafe codegraph SQLite sidecar {}",
                    sidecar.display()
                );
            }
            Ok(metadata) if metadata.len() > max_bytes => {
                bail!(
                    "codegraph SQLite sidecar {} is too large ({} bytes; max {max_bytes})",
                    sidecar.display(),
                    metadata.len()
                );
            }
            Ok(_) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o600))?;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn validate_extracted_symbols(symbols: &[extract::ExtractedSymbol]) -> Result<()> {
    for symbol in symbols {
        if symbol.name.len() > MAX_SYMBOL_NAME_BYTES
            || symbol
                .signature
                .as_ref()
                .is_some_and(|value| value.len() > MAX_SYMBOL_TEXT_BYTES)
            || symbol
                .doc
                .as_ref()
                .is_some_and(|value| value.len() > MAX_SYMBOL_TEXT_BYTES)
            || symbol.relations.iter().any(|relation| {
                relation.target_name.len() > MAX_SYMBOL_NAME_BYTES
                    || relation.kind.len() > MAX_SYMBOL_NAME_BYTES
            })
        {
            bail!("source extraction produced an oversized symbol field");
        }
    }
    Ok(())
}

/// FTS5 is picky about syntax; reduce a free-text query to safe OR-ed prefix
/// terms so natural-language queries don't error.
fn sanitize_fts_query(query: &str) -> String {
    let terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| t.len() >= 2)
        .take(MAX_QUERY_TERMS)
        .map(|t| format!("{t}*"))
        .collect();
    terms.join(" OR ")
}

fn content_hash(source: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(source.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Discover supported source files under the workspace, pruning the same heavy
/// directories the other tools skip (target/, donor clones, .git, etc.).
fn discover_files(root: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut scanned_entries = 0usize;
    let mut scanned_directories = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        scanned_directories += 1;
        if scanned_directories > MAX_SCANNED_DIRECTORIES {
            bail!("codegraph scan exceeded {MAX_SCANNED_DIRECTORIES} directories");
        }
        let entries = std::fs::read_dir(&dir)
            .with_context(|| format!("reading codegraph directory {}", dir.display()))?;
        for entry in entries {
            scanned_entries += 1;
            if scanned_entries > MAX_SCANNED_ENTRIES {
                bail!("codegraph scan exceeded {MAX_SCANNED_ENTRIES} entries");
            }
            let entry =
                entry.with_context(|| format!("reading an entry below {}", dir.display()))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .with_context(|| format!("inspecting {}", path.display()))?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .context("codegraph directory name is not UTF-8")?;
                if is_ignored_dir(name) {
                    continue;
                }
                if depth >= MAX_SCAN_DEPTH {
                    bail!("codegraph scan exceeded depth {MAX_SCAN_DEPTH}");
                }
                stack.push((path, depth + 1));
            } else if file_type.is_file() && extract::language_for_path(&path).is_some() {
                out.push(path);
                if out.len() > MAX_INDEXED_FILES {
                    bail!("workspace has more than {MAX_INDEXED_FILES} supported source files");
                }
            }
        }
    }
    Ok(out)
}

fn read_source_file(path: &Path) -> Result<String> {
    use std::io::Read;

    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspecting source file {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("refusing non-regular source file {}", path.display());
    }
    if metadata.len() > MAX_SOURCE_BYTES {
        bail!(
            "source file {} is too large ({} bytes; max {MAX_SOURCE_BYTES})",
            path.display(),
            metadata.len()
        );
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(path)?;
    if !file.metadata()?.is_file() {
        bail!("source file changed to a non-regular file while opening");
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        bail!("source file grew beyond {MAX_SOURCE_BYTES} bytes while reading");
    }
    String::from_utf8(bytes).with_context(|| format!("source file {} is not UTF-8", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn build_index(files: &[(&str, &str)]) -> (tempfile::TempDir, CodeGraph) {
        let dir = tempdir().unwrap();
        for (rel, content) in files {
            let path = dir.path().join(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, content).unwrap();
        }
        let index = dir.path().join(".phoenix").join("codegraph.sqlite");
        let mut graph = CodeGraph::open(&index).unwrap();
        graph.refresh(dir.path()).unwrap();
        (dir, graph)
    }

    #[test]
    fn call_path_finds_multi_hop_chain_and_reports_misses() {
        let (_dir, graph) = build_index(&[(
            "src/flow.rs",
            "fn handler() { service(); }\nfn service() { store(); }\nfn store() {}\nfn lonely() {}\n",
        )]);
        let chain = graph.call_path("handler", "store").unwrap();
        let names: Vec<&str> = chain.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["handler", "service", "store"]);
        // Each hop resolves to a definition with a real location.
        assert!(chain.iter().all(|(_, def)| def.is_some()));

        // No static path → empty, not an error.
        assert!(graph.call_path("handler", "lonely").unwrap().is_empty());
        // Degenerate same-symbol path is the single node.
        assert_eq!(graph.call_path("store", "store").unwrap().len(), 1);
    }

    #[test]
    fn indexes_and_searches_symbols() {
        let (_dir, graph) = build_index(&[(
            "src/auth.rs",
            "/// Authenticates a user token.\npub fn authenticate_user(token: &str) -> bool { verify_token(token) }\nfn verify_token(t: &str) -> bool { true }\n",
        )]);

        let results = graph.search("authenticate", 5).unwrap();
        assert!(results.iter().any(|s| s.name == "authenticate_user"));
        let stats = graph.stats().unwrap();
        assert_eq!(stats.file_count, 1);
        assert!(stats.symbol_count >= 2);
    }

    #[test]
    fn callers_and_callees_resolve() {
        let (_dir, graph) = build_index(&[(
            "src/lib.rs",
            "fn login() { authenticate_user(\"x\"); }\nfn api_guard() { authenticate_user(\"y\"); }\nfn authenticate_user(t: &str) { verify(t); }\nfn verify(t: &str) {}\n",
        )]);

        let callers = graph.callers("authenticate_user", 10).unwrap();
        let caller_names: Vec<&str> = callers.iter().map(|s| s.name.as_str()).collect();
        assert!(caller_names.contains(&"login"));
        assert!(caller_names.contains(&"api_guard"));

        let callees = graph.callees("authenticate_user", 10).unwrap();
        assert!(callees.iter().any(|(n, _)| n == "verify"));
    }

    #[test]
    fn impact_is_transitive() {
        let (_dir, graph) = build_index(&[(
            "src/lib.rs",
            "fn a() { b(); }\nfn b() { c(); }\nfn c() {}\n",
        )]);
        // Changing c affects b (distance 1) and a (distance 2).
        let impact = graph.impact("c").unwrap();
        let names: Vec<&str> = impact.iter().map(|(_, s)| s.name.as_str()).collect();
        assert!(names.contains(&"b"));
        assert!(names.contains(&"a"));
    }

    #[test]
    fn hotspots_resolve_paths_from_the_files_table() {
        let (_dir, graph) = build_index(&[(
            "src/lib.rs",
            "fn a() { core(); }\nfn b() { core(); }\nfn c() { core(); }\nfn core() {}\n",
        )]);
        let hotspots = graph.hotspots(8).unwrap();
        assert!(
            hotspots
                .iter()
                .any(|(name, path, fan_in)| name == "core" && path == "src/lib.rs" && *fan_in == 3),
            "{hotspots:?}"
        );
    }

    #[test]
    fn refresh_is_incremental_on_unchanged_files() {
        let (dir, mut graph) = build_index(&[("src/lib.rs", "fn a() {}\n")]);
        // Second refresh with no changes should reindex 0 files.
        let reindexed = graph.refresh(dir.path()).unwrap();
        assert_eq!(reindexed, 0);
    }

    #[test]
    fn file_symbols_lists_outline() {
        let (_dir, graph) = build_index(&[("src/m.rs", "struct A;\nfn f() {}\nenum E { X }\n")]);
        let outline = graph.file_symbols("src/m.rs").unwrap();
        let names: Vec<&str> = outline.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"A"));
        assert!(names.contains(&"f"));
        assert!(names.contains(&"E"));
    }

    #[test]
    fn refresh_rejects_oversized_sources_and_queries_are_bounded() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        let source = fs::File::create(dir.path().join("src/huge.rs")).unwrap();
        source.set_len(MAX_SOURCE_BYTES + 1).unwrap();
        let index = dir.path().join(".phoenix/codegraph.sqlite");
        let mut graph = CodeGraph::open(&index).unwrap();
        assert!(graph.refresh(dir.path()).is_err());
        assert!(graph.search(&"x".repeat(MAX_QUERY_BYTES + 1), 10).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn scan_does_not_follow_source_or_index_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("secret.rs"), "fn secret() {}\n").unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/real.rs"), "fn real() {}\n").unwrap();
        symlink(
            outside.path().join("secret.rs"),
            dir.path().join("src/linked.rs"),
        )
        .unwrap();
        let index = dir.path().join(".phoenix/codegraph.sqlite");
        let mut graph = CodeGraph::open(&index).unwrap();
        graph.refresh(dir.path()).unwrap();
        assert!(graph
            .search("real", 10)
            .unwrap()
            .iter()
            .any(|s| s.name == "real"));
        assert!(graph.search("secret", 10).unwrap().is_empty());
        drop(graph);

        let target = dir.path().join("target.sqlite");
        fs::write(&target, b"not sqlite").unwrap();
        let linked_index = dir.path().join("linked.sqlite");
        symlink(&target, &linked_index).unwrap();
        assert!(CodeGraph::open(&linked_index).is_err());
        std::fs::remove_file(&linked_index).unwrap();
        symlink(&target, dir.path().join("linked.sqlite-wal")).unwrap();
        assert!(CodeGraph::open(&linked_index).is_err());
    }
}
