//! Headroom-derived tool-output compressor — Phoenix's main compressor.
//!
//! Donor: `chopratejas/headroom` (Apache-2.0), `crates/headroom-core`. Ported
//! here as deterministic Rust: the log compressor's level-scored line
//! selection (errors + stack traces + summaries survive, noise collapses),
//! the smart-crusher's array sampling (first/last fractions, error-pinning,
//! schema-preserving — only original items, no invented text), the search
//! compressor's per-file grouping, and CCR reversibility (the original is
//! written to disk and the marker carries the path, so nothing is ever
//! unrecoverable). NOT ported: the donor's ML tiers (fastembed embeddings,
//! magika detection) — they drag ONNX Runtime into a lean CLI; cheap
//! heuristics stand in for content detection.
//!
//! Sits at tool-result ingestion in the mesh: every compressible result is
//! compressed BEFORE its first trip to the model, replacing the old blind
//! 4000-char head-cut that destroyed the tail of build logs (where cargo
//! and pytest put their summaries).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Tools whose output is fair game: mechanical, high-volume, low-value-per-line
/// surfaces (build/test logs, search hit lists, directory dumps, structured app
/// JSON). The line/log compressor is built for THESE.
///
/// Deliberately EXEMPT — these carry vital content the agent needs verbatim, and
/// the log compressor (which keeps only error/summary lines) shreds prose:
/// `read`/`str_replace` (verbatim file text), browser state (element indexes must
/// not shift), memory/skill loads (the agent asked for that text), and the WEB
/// research tools — `web_search`, `web_fetch`, `web_scrape`, `web_crawl`. Web
/// output is the researcher's actual findings; it is already bounded by the
/// tool's own `max_chars`, so re-compressing it only drops the article body and
/// forces the agent to re-read the CCR cache (observed live, session 9395fcad).
const COMPRESSIBLE_TOOLS: &[&str] = &[
    "bash",
    "grep",
    "glob",
    "codebase_search",
    "list_directory",
    "composio_search",
    "composio_schemas",
    "composio_run",
];

/// Below this, compression buys nothing worth a receipt line.
const MIN_COMPRESS_CHARS: usize = 4_000;

/// Output budget — under the wire cap (4000) so the legacy blind head-cut
/// in `truncate_tool_output` never fires on compressed output.
const BUDGET_CHARS: usize = 3_600;
const MAX_CCR_ORIGINAL_BYTES: usize = 64 * 1024 * 1024;

/// Donor `SmartCrusherConfig` defaults: analyze arrays of >= 5 items, keep
/// at most 15, 30% of the keep budget at the head, 15% at the tail.
const MIN_ITEMS_TO_ANALYZE: usize = 5;
const MAX_ITEMS_AFTER_CRUSH: usize = 15;
const ARRAY_FIRST_KEEP: usize = 5; // round(15 * 0.3)
const ARRAY_LAST_KEEP: usize = 2; // round(15 * 0.15)

/// Error keywords (donor `error_keywords.rs`, trimmed to the high-signal
/// set). A line containing one is pinned through every compression path.
const ERROR_KEYWORDS: &[&str] = &[
    "error",
    "fail",
    "panic",
    "fatal",
    "exception",
    "traceback",
    "denied",
    "refused",
    "unauthorized",
    "not found",
    "timed out",
    "timeout",
    "cannot",
    "unable to",
];

/// Summary-line markers: the lines a human reads FIRST in build/test output.
const SUMMARY_MARKERS: &[&str] = &[
    "test result",
    "passing",
    "passed",
    "failed",
    "warning",
    "exit code",
    "exit status",
    "finished",
    "compiling",
    "± ",
    "files changed",
];

pub struct CompressionOutcome {
    pub text: String,
    pub original_chars: usize,
    pub stored_path: Option<PathBuf>,
}

/// The main entry: compress one tool result at ingestion. `None` means
/// "send verbatim" (small output, exempt tool, or compression saved
/// nothing). The original is stored under `<state_root>/ccr/` and the
/// returned text ends with a retrieval marker — reversibility is the
/// contract that makes aggressive compression safe.
pub fn compress_tool_result(
    tool_name: &str,
    output: &str,
    state_root: &Path,
) -> Option<CompressionOutcome> {
    if !COMPRESSIBLE_TOOLS.contains(&tool_name) {
        return None;
    }
    let original_chars = output.chars().count();
    if original_chars < MIN_COMPRESS_CHARS {
        return None;
    }

    let compressed = if let Ok(value) = serde_json::from_str::<serde_json::Value>(output.trim()) {
        crush_json(&value)
    } else if looks_like_search_output(output) {
        compress_search(output)
    } else {
        compress_lines(output)
    };

    let compressed_chars = compressed.chars().count();
    // Donor lossless threshold: a path must save >= 30% to be worth taking.
    if (compressed_chars as f64) > (original_chars as f64) * 0.7 {
        return None;
    }

    // Lossy compression is enabled only when the original is durably and
    // exactly retrievable. If storage is unsafe/full/corrupt, return None so
    // the caller sends the verbatim tool result instead.
    let stored = store_original(output, state_root)?;
    let marker = format!(
        "\n<<headroom: {original_chars} chars compressed to {compressed_chars}; errors/summaries kept verbatim. FULL original: {} — read it ONLY if the kept content is insufficient>>",
        stored.display()
    );
    let text = format!("{compressed}{marker}");
    // Same tally the lossless pass feeds — the turn receipt shows ONE honest
    // "compressed" figure across both tool-output lanes.
    crate::tools::compress::record_saved_bytes(output.len().saturating_sub(text.len()) as u64);
    Some(CompressionOutcome {
        text,
        original_chars,
        stored_path: Some(stored),
    })
}

// ─── CCR store (reversibility) ──────────────────────────────────────────

fn store_original(output: &str, state_root: &Path) -> Option<PathBuf> {
    if output.len() > MAX_CCR_ORIGINAL_BYTES {
        return None;
    }
    let mut hasher = Sha256::new();
    hasher.update(output.as_bytes());
    let digest = hasher.finalize();
    let key: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    let dir = state_root.join("ccr");
    let path = dir.join(format!("{key}.txt"));
    match crate::config::private_io::atomic_write_private_if_missing(&path, output.as_bytes()) {
        Ok(true) => Some(path),
        Ok(false) => {
            // A digest key is a retrieval promise. Never return it unless the
            // pre-existing inode is the exact original; a stale/corrupt file
            // must not make the compression marker falsely claim reversibility.
            let stored = match crate::config::private_io::read_private_file(&path) {
                Ok(Some(stored)) => stored,
                Ok(None) => {
                    tracing::warn!(
                        "headroom CCR entry disappeared before verification: {}",
                        path.display()
                    );
                    return None;
                }
                Err(error) => {
                    tracing::warn!(
                        "headroom CCR entry could not be verified at {}: {error:#}",
                        path.display()
                    );
                    return None;
                }
            };
            if stored == output.as_bytes() {
                Some(path)
            } else {
                tracing::warn!(
                    "headroom CCR digest path contains different bytes; compression disabled: {}",
                    path.display()
                );
                None
            }
        }
        Err(error) => {
            tracing::warn!("headroom CCR original was not stored: {error:#}");
            None
        }
    }
}

// ─── Log / generic text (donor log_compressor) ──────────────────────────

fn line_is_error(line: &str) -> bool {
    let lower = line.to_lowercase();
    ERROR_KEYWORDS.iter().any(|kw| lower.contains(kw))
}

fn line_is_summary(line: &str) -> bool {
    let lower = line.to_lowercase();
    SUMMARY_MARKERS.iter().any(|m| lower.contains(m))
}

/// Anchor hard-keep (LLMLingua-2's keep/drop classification, made non-neural):
/// lines carrying an ADDRESS the model navigates by — `path:line` references,
/// code signatures, backticked identifiers — survive compression even when they
/// are not errors or summaries. After aggressive middle-trimming the model then
/// still knows WHERE it saw a fact, so it re-fetches by address instead of
/// re-grepping blind. Dedupe (digit-normalized) collapses repeated refs to the
/// same file, so a verbose build log can't explode the keep set.
fn line_is_anchor(line: &str) -> bool {
    if line.len() > 400 {
        return false;
    }
    if has_path_line_ref(line) {
        return true;
    }
    let trimmed = line.trim_start();
    const SIGNATURE_STARTS: &[&str] = &[
        "pub fn ",
        "pub async fn ",
        "async fn ",
        "fn ",
        "pub struct ",
        "struct ",
        "pub trait ",
        "trait ",
        "pub enum ",
        "enum ",
        "impl ",
        "class ",
        "def ",
        "function ",
        "interface ",
    ];
    if SIGNATURE_STARTS
        .iter()
        .any(|start| trimmed.starts_with(start))
    {
        return true;
    }
    // An identifier the output called out in backticks.
    line.bytes().filter(|&b| b == b'`').count() >= 2
}

/// True when the line contains a `path:line` reference — a token holding a
/// `/` or `.` immediately followed by `:<digits>`.
fn has_path_line_ref(line: &str) -> bool {
    let bytes = line.as_bytes();
    for (index, &byte) in bytes.iter().enumerate() {
        if byte == b':' && bytes.get(index + 1).is_some_and(u8::is_ascii_digit) {
            let start = line[..index]
                .rfind(|c: char| c.is_whitespace() || c == '(' || c == '"' || c == '\'')
                .map(|p| p + 1)
                .unwrap_or(0);
            let token = &line[start..index];
            if token.len() >= 3 && (token.contains('/') || token.contains('.')) {
                return true;
            }
        }
    }
    false
}

/// Donor conservative-dedupe: normalize the variable tail of a line so two
/// occurrences of the same warning collapse, but distinct messages (same
/// shape, different prefix) stay distinct.
fn dedupe_key(line: &str) -> String {
    let prefix_end = line.find([':', '=']).unwrap_or(line.len());
    let (prefix, tail) = line.split_at(prefix_end);
    let normalized_tail: String = tail
        .chars()
        .map(|c| if c.is_ascii_digit() { '#' } else { c })
        .collect();
    format!("{}{}", prefix.trim(), normalized_tail.trim())
}

/// Score-and-select line compression: head and tail always survive, error
/// lines survive with one context line each side, summary lines survive,
/// repeated lines collapse. Omitted runs become one receipt line. A final
/// budget pass trims from the middle of the kept set, never the ends.
fn compress_lines(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let head = 12.min(total);
    let tail_start = total.saturating_sub(20);

    let mut keep = vec![false; total];
    let mut seen: HashSet<String> = HashSet::new();
    for (index, line) in lines.iter().enumerate() {
        let mut want = index < head || index >= tail_start;
        if !want && line_is_error(line) && seen.insert(dedupe_key(line)) {
            want = true;
            if index > 0 {
                keep[index - 1] = true;
            }
            if index + 1 < total {
                keep[index + 1] = true;
            }
        }
        if !want && line_is_summary(line) && seen.insert(dedupe_key(line)) {
            want = true;
        }
        if !want && line_is_anchor(line) && seen.insert(dedupe_key(line)) {
            want = true;
        }
        if want {
            keep[index] = true;
        }
    }

    let kept_indices: Vec<usize> = (0..total).filter(|&i| keep[i]).collect();
    let kept_chars: usize = kept_indices
        .iter()
        .map(|&i| lines[i].chars().count() + 1)
        .sum();

    // Budget enforcement: drop from the middle of the kept set (the donor's
    // first/last allocation), never the head or the tail summaries.
    let mut allowed: HashSet<usize> = kept_indices.iter().copied().collect();
    if kept_chars > BUDGET_CHARS && kept_indices.len() > 30 {
        let mut budget = BUDGET_CHARS;
        let mut chosen: HashSet<usize> = HashSet::new();
        let mut front = kept_indices.iter();
        let mut back = kept_indices.iter().rev();
        let (mut from_front, mut from_back) = (true, 0usize);
        // Alternate 2-from-front / 1-from-back (donor 0.3 / 0.15 ratio).
        loop {
            let next = if from_front {
                front.next()
            } else {
                back.next()
            };
            let Some(&index) = next else { break };
            if chosen.contains(&index) {
                break; // pointers met
            }
            let cost = lines[index].chars().count() + 1;
            if cost > budget {
                break;
            }
            budget -= cost;
            chosen.insert(index);
            if from_front {
                from_back += 1;
                if from_back % 3 == 0 {
                    from_front = false;
                }
            } else {
                from_front = true;
            }
        }
        allowed = chosen;
    }

    let mut out = String::new();
    let mut omitted = 0usize;
    for (index, line) in lines.iter().enumerate() {
        if allowed.contains(&index) {
            if omitted > 0 {
                out.push_str(&format!("··· {omitted} lines omitted ···\n"));
                omitted = 0;
            }
            out.push_str(line);
            out.push('\n');
        } else {
            omitted += 1;
        }
    }
    if omitted > 0 {
        out.push_str(&format!("··· {omitted} lines omitted ···\n"));
    }
    out
}

// ─── Search output (donor search_compressor) ────────────────────────────

fn looks_like_search_output(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < 20 {
        return false;
    }
    let match_shaped = lines
        .iter()
        .filter(|line| {
            let mut parts = line.splitn(3, ':');
            matches!(
                (parts.next(), parts.next()),
                (Some(path), Some(num))
                    if !path.is_empty() && num.chars().all(|c| c.is_ascii_digit()) && !num.is_empty()
            )
        })
        .count();
    match_shaped * 10 >= lines.len() * 6 // >= 60%
}

/// Group matches per file: 3 matches each, capped file list, exact counts
/// for everything dropped. The agent learns WHERE the hits are without
/// paying for every hit.
fn compress_search(text: &str) -> String {
    let mut files: Vec<(String, Vec<&str>)> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let file = line.splitn(2, ':').next().unwrap_or("").to_string();
        match files.last_mut() {
            Some((current, matches)) if *current == file => matches.push(line),
            _ => files.push((file, vec![line])),
        }
    }
    let total_files = files.len();
    let total_matches: usize = files.iter().map(|(_, m)| m.len()).sum();
    let mut out = format!("{total_matches} matches across {total_files} files:\n");
    for (file, matches) in files.iter().take(40) {
        for line in matches.iter().take(3) {
            out.push_str(line);
            out.push('\n');
        }
        if matches.len() > 3 {
            out.push_str(&format!("  (+{} more in {file})\n", matches.len() - 3));
        }
    }
    if total_files > 40 {
        let rest: Vec<&str> = files[40..]
            .iter()
            .take(20)
            .map(|(f, _)| f.as_str())
            .collect();
        out.push_str(&format!(
            "+{} more files: {}{}\n",
            total_files - 40,
            rest.join(", "),
            if total_files > 60 { ", …" } else { "" }
        ));
    }
    out
}

// ─── JSON (donor smart_crusher) ─────────────────────────────────────────

/// Crush every large array of objects in the document (first/last keeps +
/// error-pinned items, donor fractions), then minify. Schema-preserving:
/// the output contains only items from the original; drop counts ride in
/// trailing notes OUTSIDE the JSON, never as invented keys inside it.
fn crush_json(value: &serde_json::Value) -> String {
    let mut crushed = value.clone();
    let mut notes: Vec<String> = Vec::new();
    crush_arrays(&mut crushed, "$", &mut notes);
    let body = serde_json::to_string(&crushed).unwrap_or_else(|_| value.to_string());
    if notes.is_empty() {
        body
    } else {
        format!("{body}\n[{}]", notes.join("; "))
    }
}

fn crush_arrays(value: &mut serde_json::Value, path: &str, notes: &mut Vec<String>) {
    match value {
        serde_json::Value::Array(items) => {
            let object_count = items.iter().filter(|i| i.is_object()).count();
            if items.len() >= MIN_ITEMS_TO_ANALYZE
                && object_count * 2 >= items.len()
                && items.len() > MAX_ITEMS_AFTER_CRUSH
            {
                let total = items.len();
                // Pin items carrying error signal (donor rare-status rule).
                let pinned: Vec<usize> = items
                    .iter()
                    .enumerate()
                    .skip(ARRAY_FIRST_KEEP)
                    .take(total - ARRAY_FIRST_KEEP - ARRAY_LAST_KEEP)
                    .filter(|(_, item)| line_is_error(&item.to_string()))
                    .map(|(index, _)| index)
                    .take(MAX_ITEMS_AFTER_CRUSH - ARRAY_FIRST_KEEP - ARRAY_LAST_KEEP)
                    .collect();
                let keep: HashSet<usize> = (0..ARRAY_FIRST_KEEP)
                    .chain(total - ARRAY_LAST_KEEP..total)
                    .chain(pinned.iter().copied())
                    .collect();
                let mut index = 0usize;
                items.retain(|_| {
                    let kept = keep.contains(&index);
                    index += 1;
                    kept
                });
                notes.push(format!(
                    "{path}: kept {} of {total} items ({} error-pinned)",
                    items.len(),
                    pinned.len()
                ));
            }
            for (index, item) in items.iter_mut().enumerate() {
                crush_arrays(item, &format!("{path}[{index}]"), notes);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                crush_arrays(child, &format!("{path}.{key}"), notes);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big_log() -> String {
        let mut lines: Vec<String> = (0..800)
            .map(|i| format!("INFO build step {i} ok"))
            .collect();
        lines[400] = "error[E0308]: mismatched types in src/runtime/mesh.rs:42".to_string();
        lines[401] = "  expected `u64`, found `usize`".to_string();
        lines[798] = "test result: FAILED. 317 passed; 1 failed".to_string();
        lines.join("\n")
    }

    #[test]
    fn exempt_tools_and_small_outputs_pass_through() {
        let dir = tempfile::tempdir().unwrap();
        assert!(compress_tool_result("read", &big_log(), dir.path()).is_none());
        assert!(compress_tool_result("bash", "short output", dir.path()).is_none());
        // Web research tools are exempt — their findings must reach the agent
        // whole (session 9395fcad: the log compressor was shredding article bodies).
        assert!(compress_tool_result("web_fetch", &big_log(), dir.path()).is_none());
        assert!(compress_tool_result("web_search", &big_log(), dir.path()).is_none());
    }

    #[test]
    fn log_compression_keeps_errors_and_summary_and_stores_original() {
        let dir = tempfile::tempdir().unwrap();
        let log = big_log();
        let outcome = compress_tool_result("bash", &log, dir.path()).expect("compresses");
        assert!(outcome.text.contains("error[E0308]"));
        assert!(outcome.text.contains("test result: FAILED"));
        assert!(outcome.text.contains("lines omitted"));
        assert!(outcome.text.chars().count() < log.chars().count() / 2);
        let stored = outcome.stored_path.expect("ccr stored");
        assert_eq!(std::fs::read_to_string(stored).unwrap(), log);
    }

    #[test]
    fn json_arrays_crush_to_first_last_and_error_pins() {
        let dir = tempfile::tempdir().unwrap();
        let items: Vec<serde_json::Value> = (0..200)
            .map(|i| {
                serde_json::json!({
                    "id": i,
                    "status": if i == 100 { "error: quota exceeded" } else { "ok" },
                    "payload": "x".repeat(40),
                })
            })
            .collect();
        let raw = serde_json::to_string_pretty(&serde_json::json!({ "items": items })).unwrap();
        let outcome = compress_tool_result("composio_run", &raw, dir.path()).expect("crushes");
        assert!(outcome.text.contains("quota exceeded"), "error item pinned");
        assert!(outcome.text.contains("kept"));
        assert!(outcome.text.contains("\"id\":0"));
        assert!(outcome.text.contains("\"id\":199"));
        assert!(!outcome.text.contains("\"id\":50"));
    }

    #[test]
    fn search_output_groups_per_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut text = String::new();
        for file in 0..60 {
            for line in 0..8 {
                text.push_str(&format!(
                    "src/module_{file}.rs:{line}: let value = compute_{file}();\n"
                ));
            }
        }
        let outcome = compress_tool_result("grep", &text, dir.path()).expect("compresses");
        assert!(outcome.text.starts_with("480 matches across 60 files"));
        assert!(outcome.text.contains("(+5 more in src/module_0.rs)"));
        assert!(outcome.text.contains("+20 more files"));
    }

    #[test]
    fn anchor_lines_survive_log_compression() {
        // Middle-of-log lines that are neither errors nor summaries but carry
        // an ADDRESS (path:line, signature, backticked identifier) must survive
        // — they are what lets the model re-fetch by address instead of
        // re-grepping blind after compression.
        let dir = tempfile::tempdir().unwrap();
        let mut lines: Vec<String> = (0..900)
            .map(|i| format!("INFO routine build step {i} completed without incident"))
            .collect();
        lines[300] = "note: candidate defined in src/config/loader.rs:88".to_string();
        lines[420] = "pub fn resolve_provider(config: &Config) -> Provider".to_string();
        lines[500] = "see `session_cache_read` and `vital_memory_write` for details".to_string();
        let log = lines.join("\n");
        let outcome = compress_tool_result("bash", &log, dir.path()).expect("compresses");
        assert!(
            outcome.text.contains("src/config/loader.rs:88"),
            "path:line anchor kept"
        );
        assert!(
            outcome.text.contains("pub fn resolve_provider"),
            "signature anchor kept"
        );
        assert!(
            outcome.text.contains("`session_cache_read`"),
            "backtick anchor kept"
        );
    }

    #[test]
    fn path_line_refs_are_detected_and_timestamps_are_not() {
        assert!(has_path_line_ref("  --> src/runtime/mesh.rs:42:7"));
        assert!(has_path_line_ref("at loader.py:15"));
        assert!(
            !has_path_line_ref("finished at 12:30:00"),
            "clock times are not paths"
        );
        assert!(!has_path_line_ref("ratio 3:1 observed"));
    }

    #[test]
    fn incompressible_content_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        // Unique high-entropy lines: nothing dedupes, head+tail+errors keep
        // everything… but budget still forces a saving, so use few lines of
        // huge unique content instead — under 30 kept lines, no trim.
        let text: String = (0..25)
            .map(|i| format!("{i} error unique {}", "y".repeat(200)))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(compress_tool_result("bash", &text, dir.path()).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn ccr_original_is_private_and_unsafe_existing_inode_is_not_promised() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let dir = tempfile::tempdir().unwrap();
        let output = big_log();
        let path = store_original(&output, dir.path()).expect("stored");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        std::fs::remove_file(&path).unwrap();
        let outside = dir.path().join("outside");
        std::fs::write(&outside, "untouched").unwrap();
        symlink(&outside, &path).unwrap();
        assert!(store_original(&output, dir.path()).is_none());
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "untouched");
    }

    #[test]
    fn ccr_does_not_reuse_corrupt_digest_entry() {
        let dir = tempfile::tempdir().unwrap();
        let output = big_log();
        let path = store_original(&output, dir.path()).unwrap();
        crate::config::private_io::atomic_write_private(&path, b"wrong").unwrap();
        assert!(store_original(&output, dir.path()).is_none());
    }
}
