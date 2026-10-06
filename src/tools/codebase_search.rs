//! codebase_search — semantic-style code discovery (BM25 over file chunks).
//!
//! Cursor's tool is embedding-backed; Phoenix uses fast lexical scoring over
//! line-bounded chunks so agents can find relevant code by meaning-like queries
//! without a vector DB. Prefer `grep` when you know the exact symbol/string.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use super::{is_ignored_dir, relative_display, resolve_workspace_path, workspace_io, ToolOutput};

const MAX_FILES_SCANNED: usize = 2_500;
const MAX_WALK_ENTRIES: usize = 50_000;
const MAX_QUERY_BYTES: usize = 4 * 1024;
const MAX_TARGET_DIRECTORIES: usize = 64;
const MAX_SEARCH_FILE_BYTES: usize = 1024 * 1024;
const MAX_AGGREGATE_SCAN_BYTES: usize = 16 * 1024 * 1024;
const MAX_CHUNK_RECORDS: usize = 20_000;
const MAX_RESULTS_DEFAULT: usize = 12;
const MAX_RESULTS_CAP: usize = 25;
const CHUNK_LINES: usize = 40;
const CHUNK_OVERLAP: usize = 8;
const MAX_CHUNK_CHARS: usize = 2_400;
const MAX_SCORING_CHUNK_BYTES: usize = 16 * 1024;
const MAX_OUTPUT_BYTES: usize = 48 * 1024;
const QUALITY_WARN_FILE_COUNT: usize = 500;
const MAX_SCAN_WARNINGS: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodebaseSearchInput {
    pub query: String,
    #[serde(default)]
    pub target_directories: Vec<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Clone)]
struct Chunk {
    path: String,
    start_line: usize,
    end_line: usize,
    text: String,
    score: f64,
}

#[derive(Default)]
struct ScanStatus {
    warnings: Vec<String>,
    suppressed_warnings: usize,
    walk_entries: usize,
    walk_capped: bool,
    file_capped: bool,
}

impl ScanStatus {
    fn warn(&mut self, warning: impl Into<String>) {
        if self.warnings.len() < MAX_SCAN_WARNINGS {
            self.warnings.push(warning.into());
        } else {
            self.suppressed_warnings += 1;
        }
    }

    fn is_partial(&self) -> bool {
        self.walk_capped
            || self.file_capped
            || !self.warnings.is_empty()
            || self.suppressed_warnings > 0
    }

    fn disclosure(&self) -> String {
        if !self.is_partial() {
            return String::new();
        }
        let mut details = self.warnings.clone();
        if self.file_capped {
            details.push(format!("file cap {MAX_FILES_SCANNED} reached"));
        }
        if self.walk_capped {
            details.push(format!("walk-entry cap {MAX_WALK_ENTRIES} reached"));
        }
        if self.suppressed_warnings > 0 {
            details.push(format!(
                "{} additional warning(s) suppressed",
                self.suppressed_warnings
            ));
        }
        format!("\nPartial scan: {}\n", details.join("; "))
    }
}

pub fn execute(
    workspace_root: &Path,
    confined: bool,
    input: CodebaseSearchInput,
) -> Result<ToolOutput> {
    let query = input.query.trim();
    if query.is_empty() {
        bail!("codebase_search query cannot be empty");
    }
    if query.len() > MAX_QUERY_BYTES {
        bail!(
            "codebase_search query is too long ({} bytes; max {})",
            query.len(),
            MAX_QUERY_BYTES
        );
    }
    if input.target_directories.len() > MAX_TARGET_DIRECTORIES {
        bail!(
            "codebase_search has too many target_directories ({}; max {})",
            input.target_directories.len(),
            MAX_TARGET_DIRECTORIES
        );
    }

    let limit = input
        .limit
        .unwrap_or(MAX_RESULTS_DEFAULT)
        .clamp(1, MAX_RESULTS_CAP);

    let root = resolve_workspace_path(workspace_root, ".", true, confined)?;
    let search_roots =
        resolve_search_roots(workspace_root, &root, &input.target_directories, confined)?;

    let query_tokens = tokenize(query);
    if query_tokens.is_empty() {
        bail!("codebase_search query has no searchable tokens");
    }

    let mut files = Vec::new();
    let mut scan_status = ScanStatus::default();
    for search_root in &search_roots {
        collect_files(search_root, &mut files, &mut scan_status)?;
        if scan_status.file_capped || scan_status.walk_capped {
            break;
        }
    }
    files.sort();
    files.dedup();

    let discovered_file_count = files.len();
    let quality_note = if discovered_file_count > QUALITY_WARN_FILE_COUNT {
        format!(
            "\nNote: searched {} files; result quality may degrade above ~{} files (Cursor guidance).\n",
            discovered_file_count, QUALITY_WARN_FILE_COUNT
        )
    } else {
        String::new()
    };

    let mut chunks = Vec::new();
    let mut document_frequency: HashMap<String, usize> = HashMap::new();
    let mut chunk_records: Vec<(String, usize, usize, String, HashMap<String, usize>)> = Vec::new();
    let mut scanned_file_count = 0usize;
    let mut scanned_bytes = 0usize;
    let mut chunk_limit_reached = false;

    for path in &files {
        if scanned_bytes >= MAX_AGGREGATE_SCAN_BYTES {
            scan_status.warn(format!(
                "aggregate scan-byte cap {MAX_AGGREGATE_SCAN_BYTES} reached"
            ));
            break;
        }
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) => {
                scan_status.warn(format!("could not inspect {}: {error}", path.display()));
                continue;
            }
        };
        if metadata.file_type().is_symlink() {
            // DirectoryEntry::file_type is already no-follow, but re-check at
            // read time in case the entry was replaced after collection.
            scan_status.warn(format!(
                "skipped symlink that appeared at {}",
                path.display()
            ));
            continue;
        }
        if metadata.len() > MAX_SEARCH_FILE_BYTES as u64 {
            scan_status.warn(format!(
                "skipped oversized {} ({} bytes; per-file max {})",
                path.display(),
                metadata.len(),
                MAX_SEARCH_FILE_BYTES
            ));
            continue;
        }
        if scanned_bytes.saturating_add(metadata.len() as usize) > MAX_AGGREGATE_SCAN_BYTES {
            scan_status.warn(format!(
                "aggregate scan-byte cap {MAX_AGGREGATE_SCAN_BYTES} reached before {}",
                path.display()
            ));
            break;
        }
        let bytes = match workspace_io::read_regular_bytes_at(
            &root,
            path,
            confined,
            MAX_SEARCH_FILE_BYTES.min(MAX_AGGREGATE_SCAN_BYTES - scanned_bytes),
        ) {
            Ok(bytes) => bytes,
            Err(error) => {
                scan_status.warn(format!("could not read {}: {error:#}", path.display()));
                continue;
            }
        };
        scanned_bytes = scanned_bytes.saturating_add(bytes.len());
        if bytes.contains(&0) {
            scan_status.warn(format!("skipped binary-looking file {}", path.display()));
            continue;
        }
        let content = match String::from_utf8(bytes) {
            Ok(content) => content,
            Err(_) => {
                scan_status.warn(format!("skipped non-UTF-8 file {}", path.display()));
                continue;
            }
        };
        scanned_file_count += 1;
        let rel = relative_display(&root, path);
        let (file_chunks, truncated_chunks) = chunk_file(&content);
        if truncated_chunks > 0 {
            scan_status.warn(format!(
                "{} long chunk(s) truncated while scanning {}",
                truncated_chunks,
                path.display()
            ));
        }
        for (start, end, text) in file_chunks {
            if chunk_records.len() >= MAX_CHUNK_RECORDS {
                scan_status.warn(format!("chunk cap {MAX_CHUNK_RECORDS} reached"));
                chunk_limit_reached = true;
                break;
            }
            let tf = term_frequency(&tokenize(&text));
            for token in tf.keys() {
                *document_frequency.entry(token.clone()).or_insert(0) += 1;
            }
            chunk_records.push((rel.clone(), start, end, text, tf));
        }
        if chunk_limit_reached {
            break;
        }
    }

    let partial_note = scan_status.disclosure();

    let doc_count = chunk_records.len().max(1);
    for (rel, start, end, text, tf) in chunk_records {
        let score = bm25_score(&query_tokens, &tf, doc_count, &document_frequency);
        if score > 0.0 {
            chunks.push(Chunk {
                path: rel,
                start_line: start,
                end_line: end,
                text,
                score,
            });
        }
    }

    chunks.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    chunks.truncate(limit);

    if chunks.is_empty() {
        return Ok(ToolOutput {
            summary: format!(
                "codebase_search found no matches for {query:?}{}.",
                if scan_status.is_partial() {
                    " in a partial scan"
                } else {
                    ""
                }
            ),
            content: format!(
                "Successfully scanned {scanned_file_count} of {discovered_file_count} discovered file(s) ({scanned_bytes} bytes) under {}.\nTry different wording, broader target_directories, or use grep for exact symbols.\n{quality_note}{partial_note}",
                search_roots_display(&search_roots, &root)
            ),
        });
    }

    let mut content = format!(
        "codebase_search: top {} chunk(s) for query {:?}\nSuccessfully scanned {} of {} discovered file(s) ({} bytes) under {}.{quality_note}{partial_note}\n",
        chunks.len(),
        query,
        scanned_file_count,
        discovered_file_count,
        scanned_bytes,
        search_roots_display(&search_roots, &root),
    );

    for (idx, chunk) in chunks.iter().enumerate() {
        let snippet = truncate_chunk(&chunk.text);
        content.push_str(&format!(
            "\n--- result {} (score {:.3}) ---\n{}:{}-{}\n{snippet}\n",
            idx + 1,
            chunk.score,
            chunk.path,
            chunk.start_line,
            chunk.end_line,
        ));
        if content.len() > MAX_OUTPUT_BYTES {
            super::truncate_bytes(&mut content, MAX_OUTPUT_BYTES);
            content.push_str("\n[codebase_search output truncated]\n");
            break;
        }
    }

    Ok(ToolOutput {
        summary: format!(
            "codebase_search returned {} chunk(s) for {:?}{}.",
            chunks.len().min(limit),
            query,
            if scan_status.is_partial() {
                " from a partial scan"
            } else {
                ""
            }
        ),
        content,
    })
}

fn resolve_search_roots(
    workspace_root: &Path,
    root: &Path,
    target_directories: &[String],
    confined: bool,
) -> Result<Vec<PathBuf>> {
    if target_directories.is_empty() {
        return Ok(vec![root.to_path_buf()]);
    }

    let mut roots = Vec::new();
    for dir in target_directories {
        let trimmed = dir.trim();
        if trimmed.is_empty() {
            continue;
        }
        workspace_io::validate_path_input(trimmed)?;
        workspace_io::reject_final_symlink_input(
            workspace_root,
            trimmed,
            "codebase_search target",
        )?;
        let resolved = resolve_workspace_path(workspace_root, trimmed, true, confined)?;
        if !resolved.is_dir() {
            bail!("target_directories entry is not a directory: {trimmed}");
        }
        roots.push(resolved);
    }

    roots.sort();
    roots.dedup();
    if roots.is_empty() {
        Ok(vec![root.to_path_buf()])
    } else {
        Ok(roots)
    }
}

fn search_roots_display(roots: &[PathBuf], workspace_root: &Path) -> String {
    roots
        .iter()
        .map(|p| relative_display(workspace_root, p))
        .collect::<Vec<_>>()
        .join(", ")
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>, status: &mut ScanStatus) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        if status.walk_entries >= MAX_WALK_ENTRIES {
            status.walk_capped = true;
            break;
        }
        if out.len() >= MAX_FILES_SCANNED {
            status.file_capped = true;
            break;
        }
        let entries = match fs::read_dir(&path) {
            Ok(e) => e,
            Err(error) => {
                status.warn(format!("could not list {}: {error}", path.display()));
                continue;
            }
        };
        for entry in entries {
            status.walk_entries += 1;
            if status.walk_entries > MAX_WALK_ENTRIES {
                status.walk_capped = true;
                break;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    status.warn(format!(
                        "could not read entry in {}: {error}",
                        path.display()
                    ));
                    continue;
                }
            };
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    status.warn(format!("could not inspect {}: {error}", path.display()));
                    continue;
                }
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if is_ignored_dir(name) {
                    continue;
                }
                stack.push(path);
            } else if file_type.is_file() && is_searchable_file(&path) {
                if out.len() >= MAX_FILES_SCANNED {
                    status.file_capped = true;
                    break;
                }
                out.push(path);
            }
        }
        if status.file_capped || status.walk_capped {
            break;
        }
    }
    Ok(())
}

fn is_searchable_file(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return matches!(
            path.file_name().and_then(|n| n.to_str()),
            Some("Dockerfile" | "Makefile" | "LICENSE" | "AGENTS.md" | "CURSOR.md")
        );
    };
    matches!(
        ext,
        "rs" | "md"
            | "toml"
            | "json"
            | "yaml"
            | "yml"
            | "js"
            | "ts"
            | "tsx"
            | "jsx"
            | "py"
            | "sh"
            | "sql"
            | "html"
            | "css"
            | "txt"
            | "go"
            | "java"
            | "kt"
            | "c"
            | "cpp"
            | "h"
            | "hpp"
            | "zig"
            | "lua"
            | "rb"
            | "swift"
            | "scala"
            | "vue"
            | "svelte"
    )
}

fn chunk_file(content: &str) -> (Vec<(usize, usize, String)>, usize) {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return (Vec::new(), 0);
    }

    let mut chunks = Vec::new();
    let mut truncated_chunks = 0usize;
    let mut start = 0usize;
    while start < lines.len() {
        let end = (start + CHUNK_LINES).min(lines.len());
        let mut text = lines[start..end].join("\n");
        if text.len() > MAX_SCORING_CHUNK_BYTES {
            super::truncate_bytes(&mut text, MAX_SCORING_CHUNK_BYTES);
            truncated_chunks += 1;
        }
        if !text.trim().is_empty() {
            chunks.push((start + 1, end, text));
        }
        if end >= lines.len() {
            break;
        }
        start = end.saturating_sub(CHUNK_OVERLAP);
        if start >= end {
            start = end;
        }
    }
    (chunks, truncated_chunks)
}

fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            current.push(ch.to_ascii_lowercase());
        } else if !current.is_empty() {
            if current.len() >= 2 {
                tokens.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
    }
    if current.len() >= 2 {
        tokens.push(current);
    }
    tokens
}

fn term_frequency(tokens: &[String]) -> HashMap<String, usize> {
    let mut tf = HashMap::new();
    for token in tokens {
        *tf.entry(token.clone()).or_insert(0) += 1;
    }
    tf
}

fn bm25_score(
    query_tokens: &[String],
    tf: &HashMap<String, usize>,
    doc_count: usize,
    df: &HashMap<String, usize>,
) -> f64 {
    let k1 = 1.2;
    let b = 0.75;
    let avg_dl = 80.0_f64;
    let dl = tf.values().sum::<usize>() as f64;

    let mut unique_query: Vec<&String> = query_tokens.iter().collect();
    unique_query.sort();
    unique_query.dedup();

    let mut score = 0.0;
    for token in unique_query {
        let term_freq = *tf.get(token).unwrap_or(&0) as f64;
        if term_freq == 0.0 {
            continue;
        }
        let df_count = *df.get(token).unwrap_or(&0) as f64;
        let idf = ((doc_count as f64 - df_count + 0.5) / (df_count + 0.5) + 1.0).ln();
        let numerator = term_freq * (k1 + 1.0);
        let denominator = term_freq + k1 * (1.0 - b + b * (dl / avg_dl));
        score += idf * (numerator / denominator);
    }
    score
}

fn truncate_chunk(text: &str) -> String {
    if text.len() <= MAX_CHUNK_CHARS {
        return text.to_string();
    }
    let mut truncated = text.to_string();
    super::truncate_bytes(&mut truncated, MAX_CHUNK_CHARS);
    truncated.push_str("\n[chunk truncated]");
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn finds_relevant_chunk_by_natural_language_query() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("runner.rs"),
            "pub fn execute_orchestrator_loop() {\n    // routes to coder\n}\n",
        )
        .unwrap();
        fs::write(
            src.join("noise.txt"),
            "unrelated shopping list and lorem ipsum",
        )
        .unwrap();

        let out = execute(
            dir.path(),
            true,
            CodebaseSearchInput {
                query: "orchestrator loop routes to coder".to_string(),
                target_directories: vec!["src".to_string()],
                limit: Some(3),
            },
        )
        .unwrap();

        assert!(out.content.contains("runner.rs"));
        assert!(out.content.contains("execute_orchestrator_loop"));
    }

    #[test]
    fn rejects_empty_query() {
        let dir = tempdir().unwrap();
        let err = execute(
            dir.path(),
            true,
            CodebaseSearchInput {
                query: "  ".to_string(),
                target_directories: vec![],
                limit: None,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn oversized_searchable_file_is_disclosed_as_partial() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("huge.rs");
        fs::File::create(&path)
            .unwrap()
            .set_len(MAX_SEARCH_FILE_BYTES as u64 + 1)
            .unwrap();
        let out = execute(
            dir.path(),
            true,
            CodebaseSearchInput {
                query: "needle".to_string(),
                target_directories: vec![],
                limit: None,
            },
        )
        .unwrap();
        assert!(out.content.contains("Partial scan"), "{}", out.content);
        assert!(out.content.contains("oversized"), "{}", out.content);
    }

    #[cfg(unix)]
    #[test]
    fn search_never_follows_symlinked_files_or_directories() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("secret.rs"), "unique_hidden_needle").unwrap();
        symlink(outside.path(), dir.path().join("linked-src")).unwrap();
        symlink(
            outside.path().join("secret.rs"),
            dir.path().join("alias.rs"),
        )
        .unwrap();
        fs::write(dir.path().join("local.rs"), "ordinary local code").unwrap();

        let out = execute(
            dir.path(),
            true,
            CodebaseSearchInput {
                query: "unique_hidden_needle".to_string(),
                target_directories: vec![],
                limit: None,
            },
        )
        .unwrap();
        assert!(!out.content.contains("secret.rs"), "{}", out.content);
        assert!(!out.content.contains("alias.rs"), "{}", out.content);
    }
}
