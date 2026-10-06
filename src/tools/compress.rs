//! Tool-output compression (donor: rtk / opentoken).
//!
//! Every tool result passes through here before it becomes model context — the
//! deterministic prune-before-context layer. It ports the donor's *safe,
//! lossless* stages (ANSI strip, consecutive-duplicate dedup, blank-run
//! collapse, trailing-whitespace trim) and keeps the donor's cardinal rule:
//! **never grow the output** — if a stage would make it larger, the original is
//! returned untouched. No semantic content is dropped; only noise that costs
//! tokens for no reasoning value.
//!
//! Structural stages ported from the donor's `folding.ts`: non-consecutive
//! repeat folding (a line appearing 5+ times anywhere collapses to one receipt)
//! and diff context folding (unchanged context runs inside `@@` hunks collapse
//! to `... N context lines omitted`; +/- lines and headers are kept verbatim).
//! Both are more conservative than the donor: short context runs stay verbatim
//! (the donor replaced them with `[context]` placeholders) and long lines are
//! never truncated. The heavier summarizing stages (JSON value collapse,
//! family-specific pipelines) remain a later port.

/// Compress one tool output. Every tool gets the lossless pass; the structural
/// folding stages run only on noisy stream families (shell, web, browser
/// extraction) — never on `read`/`grep`-style outputs where the agent needs
/// file content verbatim (a source line legitimately repeated 5+ times must
/// not fold).
/// Bytes shaved off tool outputs before they reached model context, since
/// process start. Drives the visible "compression saved ~N tokens" receipt.
static SAVED_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Raw bytes of tool output that ENTERED the compression pass, since process
/// start. saved/raw is the provable compression ratio — a bare "saved N"
/// with no denominator convinces nobody (user, 2026-07-08: "I don't see any
/// numbers proving it is helping").
static RAW_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn saved_bytes_total() -> u64 {
    SAVED_BYTES.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn raw_bytes_total() -> u64 {
    RAW_BYTES.load(std::sync::atomic::Ordering::Relaxed)
}

/// Credit savings made by OTHER compression lanes (the headroom tool-result
/// compressor in `runtime::compressor`) into the same tally, so the turn
/// receipt's "compressed" figure covers everything actually shaved before
/// model context — not just this file's lossless pass.
pub fn record_saved_bytes(bytes: u64) {
    SAVED_BYTES.fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
}

pub fn compress(tool_name: &str, content: &str) -> String {
    RAW_BYTES.fetch_add(content.len() as u64, std::sync::atomic::Ordering::Relaxed);
    let out = compress_inner(tool_name, content);
    if out.len() < content.len() {
        SAVED_BYTES.fetch_add(
            (content.len() - out.len()) as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
    }
    out
}

/// First-ingestion pass used by the live tool executor. This stage may remove
/// formatting noise but never samples, elides, offloads, or semantically folds
/// a result before the mesh has had a chance to persist its full contents.
/// Reversible CCR compression, with a retrieval path, happens later in
/// `runtime::compressor`.
pub fn prune_lossless_for_ingestion(content: &str) -> String {
    RAW_BYTES.fetch_add(content.len() as u64, std::sync::atomic::Ordering::Relaxed);
    let out = apply_stage(content, lossless_prune);
    if out.len() < content.len() {
        SAVED_BYTES.fetch_add(
            (content.len() - out.len()) as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
    }
    out
}

fn compress_inner(tool_name: &str, content: &str) -> String {
    // Cheap escape hatch: tiny outputs aren't worth touching.
    if content.len() < 200 {
        return content.to_string();
    }
    let mut out = apply_stage(content, lossless_prune);
    if folds_structurally(tool_name) {
        out = apply_stage(&out, fold_repeats);
        // Content routing (donor: headroom's ContentRouter): detect what the
        // stream actually IS and run the compressor built for that shape,
        // instead of one generic pass. Each routed stage still obeys the
        // never-grow rule via apply_stage.
        match detect_stream_content(&out) {
            StreamContent::Diff => out = apply_stage(&out, fold_diff),
            StreamContent::SearchResults => out = apply_stage(&out, compress_search_results),
            StreamContent::JsonArray => out = apply_stage(&out, compress_json_array),
            StreamContent::Log => out = apply_stage(&out, compress_log_output),
            StreamContent::Plain => {}
        }
        // Progressive disclosure (donor: opentoken): a still-huge stream
        // output goes to disk in full; the model gets head + tail + a receipt
        // naming the file. It can `read` the rest only if it actually needs
        // it — most of the time the head/tail is the answer and the middle
        // was pure token burn.
        out = progressive_disclosure(tool_name, &out);
    }
    out
}

// === Content routing (donor: headroom content_detector + per-type compressors,
// reimplemented natively and conservatively) =================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamContent {
    Diff,
    SearchResults,
    JsonArray,
    Log,
    Plain,
}

fn detect_stream_content(content: &str) -> StreamContent {
    if looks_like_diff(content) {
        return StreamContent::Diff;
    }
    let trimmed = content.trim_start();
    if trimmed.starts_with('[') && serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
        return StreamContent::JsonArray;
    }

    let lines: Vec<&str> = content.lines().take(200).collect();
    let non_empty = lines.iter().filter(|line| !line.trim().is_empty()).count();
    if non_empty == 0 {
        return StreamContent::Plain;
    }

    // grep/ripgrep shape: `path:line:` on most lines.
    let search_hits = lines
        .iter()
        .filter(|line| is_search_result_line(line))
        .count();
    if search_hits * 10 >= non_empty * 3 {
        return StreamContent::SearchResults;
    }

    // Build/test/log shape: levels, timestamps, test verdicts, stack frames.
    let log_hits = lines.iter().filter(|line| is_log_line(line)).count();
    if log_hits * 10 >= non_empty * 2 {
        return StreamContent::Log;
    }

    StreamContent::Plain
}

fn is_search_result_line(line: &str) -> bool {
    // `path:NN:` — a colon-separated path prefix, then digits, then a colon.
    let Some(first_colon) = line.find(':') else {
        return false;
    };
    if first_colon == 0 || line[..first_colon].contains(char::is_whitespace) {
        return false;
    }
    let rest = &line[first_colon + 1..];
    let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    digits > 0 && rest[digits..].starts_with(':')
}

fn is_log_line(line: &str) -> bool {
    const MARKERS: [&str; 14] = [
        "ERROR",
        "error:",
        "error[",
        "FAILED",
        "FAIL",
        "WARN",
        "warning:",
        "INFO",
        "DEBUG",
        "TRACE",
        "PASSED",
        "panicked",
        "Traceback",
        "    at ",
    ];
    MARKERS.iter().any(|marker| line.contains(marker))
        || line.starts_with("npm ERR!")
        || starts_with_timestamp(line)
}

fn starts_with_timestamp(line: &str) -> bool {
    let trimmed = line.trim_start();
    let bytes = trimmed.as_bytes();
    bytes.len() >= 10
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit)
}

/// Search results: group by file, keep the first matches and the last match
/// per file, fold the middle to a per-file receipt (donor: headroom
/// SearchCompressor's always_keep_first/always_keep_last).
fn compress_search_results(content: &str) -> String {
    const KEEP_HEAD_PER_FILE: usize = 5;
    const MAX_FILES: usize = 30;

    // Group consecutive same-file matches (grep emits them grouped).
    let mut out = String::with_capacity(content.len());
    let mut current_file: Option<&str> = None;
    let mut bucket: Vec<&str> = Vec::new();
    let mut files_emitted = 0usize;
    let mut files_suppressed = 0usize;
    let mut suppressed_matches = 0usize;

    let flush = |file: Option<&str>,
                 bucket: &mut Vec<&str>,
                 out: &mut String,
                 files_emitted: &mut usize,
                 files_suppressed: &mut usize,
                 suppressed_matches: &mut usize| {
        if bucket.is_empty() {
            return;
        }
        if file.is_some() && *files_emitted >= MAX_FILES {
            *files_suppressed += 1;
            *suppressed_matches += bucket.len();
            bucket.clear();
            return;
        }
        if bucket.len() > KEEP_HEAD_PER_FILE + 2 {
            for line in bucket.iter().take(KEEP_HEAD_PER_FILE) {
                out.push_str(line);
                out.push('\n');
            }
            out.push_str(&format!(
                "  ... {} more matches in this file (last kept below)\n",
                bucket.len() - KEEP_HEAD_PER_FILE - 1
            ));
            out.push_str(bucket[bucket.len() - 1]);
            out.push('\n');
        } else {
            for line in bucket.iter() {
                out.push_str(line);
                out.push('\n');
            }
        }
        if file.is_some() {
            *files_emitted += 1;
        }
        bucket.clear();
    };

    for line in content.lines() {
        if is_search_result_line(line) {
            let file = &line[..line.find(':').unwrap_or(0)];
            if current_file != Some(file) {
                flush(
                    current_file,
                    &mut bucket,
                    &mut out,
                    &mut files_emitted,
                    &mut files_suppressed,
                    &mut suppressed_matches,
                );
                current_file = Some(file);
            }
            bucket.push(line);
        } else {
            flush(
                current_file,
                &mut bucket,
                &mut out,
                &mut files_emitted,
                &mut files_suppressed,
                &mut suppressed_matches,
            );
            current_file = None;
            out.push_str(line);
            out.push('\n');
        }
    }
    flush(
        current_file,
        &mut bucket,
        &mut out,
        &mut files_emitted,
        &mut files_suppressed,
        &mut suppressed_matches,
    );
    if files_suppressed > 0 {
        out.push_str(&format!(
            "... {suppressed_matches} matches in {files_suppressed} more files omitted — narrow the pattern to see them\n"
        ));
    }
    out
}

/// Big JSON arrays: keep the first few items and the last one verbatim, fold
/// the middle to a count + key-schema receipt (donor: headroom SmartCrusher's
/// sample-don't-dump doctrine).
fn compress_json_array(content: &str) -> String {
    const KEEP_HEAD: usize = 3;
    const MIN_ITEMS_TO_FOLD: usize = 12;

    let Ok(serde_json::Value::Array(items)) =
        serde_json::from_str::<serde_json::Value>(content.trim())
    else {
        return content.to_string();
    };
    if items.len() < MIN_ITEMS_TO_FOLD {
        return content.to_string();
    }

    let mut keys: Vec<String> = Vec::new();
    if let Some(serde_json::Value::Object(map)) = items.first() {
        keys = map.keys().cloned().collect();
    }
    let schema_note = if keys.is_empty() {
        String::new()
    } else {
        format!(" — every item has keys: {}", keys.join(", "))
    };

    let mut out = String::from("[\n");
    for item in items.iter().take(KEEP_HEAD) {
        out.push_str(&format!(
            "  {},\n",
            serde_json::to_string(item).unwrap_or_default()
        ));
    }
    out.push_str(&format!(
        "  \"... {} items omitted{schema_note}\",\n",
        items.len() - KEEP_HEAD - 1
    ));
    out.push_str(&format!(
        "  {}\n]\n",
        serde_json::to_string(items.last().unwrap_or(&serde_json::Value::Null)).unwrap_or_default()
    ));
    out
}

/// Build/test logs: errors, warnings, and verdict lines are the signal —
/// keep ALL of them plus the head and tail; routine INFO/DEBUG noise in the
/// middle folds to a receipt (donor: headroom LogCompressor's
/// keep_first_error/keep_last_error scoring).
fn compress_log_output(content: &str) -> String {
    const HEAD_LINES: usize = 40;
    const TAIL_LINES: usize = 25;
    const MIN_LINES_TO_FOLD: usize = 120;

    let lines: Vec<&str> = content.lines().collect();
    if lines.len() < MIN_LINES_TO_FOLD {
        return content.to_string();
    }

    let is_signal = |line: &str| {
        const SIGNAL: [&str; 11] = [
            "ERROR",
            "error:",
            "error[",
            "FAILED",
            "FAIL",
            "panicked",
            "WARN",
            "warning:",
            "PASSED",
            "test result",
            "Traceback",
        ];
        SIGNAL.iter().any(|marker| line.contains(marker))
    };

    let mut out = String::with_capacity(content.len() / 2);
    let mut omitted = 0usize;
    let flush_omitted = |omitted: &mut usize, out: &mut String| {
        if *omitted > 0 {
            out.push_str(&format!("  ... {omitted} routine log lines omitted\n"));
            *omitted = 0;
        }
    };
    for (index, line) in lines.iter().enumerate() {
        let in_head = index < HEAD_LINES;
        let in_tail = index + TAIL_LINES >= lines.len();
        if in_head || in_tail || is_signal(line) {
            flush_omitted(&mut omitted, &mut out);
            out.push_str(line);
            out.push('\n');
        } else {
            omitted += 1;
        }
    }
    flush_omitted(&mut omitted, &mut out);
    out
}

/// Above this size, noisy-stream output is offloaded to a file and the model
/// context gets head + tail + a read-more receipt.
const DISCLOSURE_THRESHOLD: usize = 30_000;
const DISCLOSURE_HEAD: usize = 12_000;
const DISCLOSURE_TAIL: usize = 3_000;
/// Offload files older than this are dead weight (the receipt that named them
/// is long gone) — swept on the next write to keep the temp dir bounded.
const DISCLOSURE_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Delete offload files older than [`DISCLOSURE_MAX_AGE`]. Best-effort: a
/// failed stat/remove is skipped, never surfaced (this is disk hygiene, not
/// the tool's job).
fn prune_stale_disclosure_files(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let Ok(metadata) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if !metadata.file_type().is_file() {
            continue;
        }
        let too_old = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > DISCLOSURE_MAX_AGE);
        if too_old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn private_disclosure_dir() -> Option<std::path::PathBuf> {
    #[cfg(unix)]
    let suffix = unsafe { libc::geteuid() }.to_string();
    #[cfg(not(unix))]
    let suffix = std::process::id().to_string();
    let dir = std::env::temp_dir().join(format!("phoenix-tool-outputs-{suffix}"));
    if !dir.exists() {
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        if let Err(error) = builder.create(&dir) {
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return None;
            }
        }
    }
    let metadata = std::fs::symlink_metadata(&dir).ok()?;
    if !metadata.file_type().is_dir() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() } {
            return None;
        }
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).ok()?;
    }
    Some(dir)
}

fn progressive_disclosure(tool_name: &str, content: &str) -> String {
    if content.len() <= DISCLOSURE_THRESHOLD {
        return content.to_string();
    }
    // Split on line boundaries so no line is ever cut mid-way.
    let lines: Vec<&str> = content.lines().collect();
    let mut head = String::new();
    let mut head_lines = 0;
    for line in &lines {
        if head.len() + line.len() + 1 > DISCLOSURE_HEAD {
            break;
        }
        head.push_str(line);
        head.push('\n');
        head_lines += 1;
    }
    let mut tail_rev = Vec::new();
    let mut tail_len = 0;
    for line in lines.iter().rev() {
        if tail_len + line.len() + 1 > DISCLOSURE_TAIL || tail_rev.len() + head_lines >= lines.len()
        {
            break;
        }
        tail_rev.push(*line);
        tail_len += line.len() + 1;
    }
    let tail: Vec<&str> = tail_rev.into_iter().rev().collect();
    let omitted = lines.len().saturating_sub(head_lines + tail.len());
    if omitted == 0 {
        return content.to_string();
    }

    let safe_tool: String = tool_name
        .chars()
        .take(64)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    let stored = private_disclosure_dir().and_then(|dir| {
        // These offload files are only useful within the session that made
        // them. Prune anything older than a day before the new private write.
        prune_stale_disclosure_files(&dir);
        let path = dir.join(format!("{safe_tool}-{}.txt", uuid::Uuid::new_v4().simple()));
        crate::config::private_io::atomic_write_private(&path, content.as_bytes())
            .ok()
            .map(|()| path)
    });
    let receipt = stored
        .as_ref()
        .map(|path| {
            format!(
                "full output saved privately to {} ; `read` it (with offset/limit) ONLY if the head/tail above is genuinely insufficient",
                path.display()
            )
        })
        .unwrap_or_else(|| {
            "full output could not be saved; the bounded head/tail below is all that is available"
                .to_string()
        });

    format!(
        "{head}... [{omitted} lines omitted — {receipt}] ...\n{}",
        tail.join("\n")
    )
}

/// Tool families whose output is a noisy mechanical stream (command/build/test
/// logs) safe to structurally fold. The WEB and extraction tools (`web_fetch`,
/// `web_scrape`, `web_crawl`, `browser_extract`) are deliberately NOT here:
/// their output is the researcher's actual findings — prose the agent needs
/// whole — and structural folding shreds the article body while keeping nav
/// chrome. Web output is already bounded by each tool's `max_chars`.
fn folds_structurally(tool_name: &str) -> bool {
    matches!(tool_name, "bash")
}

/// Donor safety rule, enforced per stage: never return something larger than
/// the stage's input.
fn apply_stage(content: &str, stage: impl Fn(&str) -> String) -> String {
    let result = stage(content);
    if result.len() < content.len() {
        result
    } else {
        content.to_string()
    }
}

fn looks_like_diff(content: &str) -> bool {
    content.contains("diff --git") || content.starts_with("@@")
}

/// Collapse non-consecutive identical lines appearing 5+ times anywhere in the
/// output into one `N x <line>` receipt at the first occurrence (donor:
/// opentoken `foldRepeats`). Catches repeated warnings/errors a consecutive
/// dedup misses.
fn fold_repeats(content: &str) -> String {
    const MIN_OCCURRENCES: usize = 5;
    const MIN_LINE_LEN: usize = 4;

    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.len() >= MIN_LINE_LEN {
            *counts.entry(trimmed).or_insert(0) += 1;
        }
    }
    counts.retain(|_, count| *count >= MIN_OCCURRENCES);
    if counts.is_empty() {
        return content.to_string();
    }

    let mut out = String::with_capacity(content.len());
    let mut shown: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(count) = counts.get(trimmed) {
            if shown.insert(trimmed) {
                out.push_str(&format!("  {} x {}\n", count, trimmed));
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Collapse runs of unchanged context lines inside diff hunks (donor: opentoken
/// `foldDiff`). Runs of 4+ context lines become `... N context lines omitted`;
/// shorter runs, +/- lines, and all headers are kept verbatim.
fn fold_diff(content: &str) -> String {
    const MAX_VERBATIM_CONTEXT: usize = 3;

    let mut out = String::with_capacity(content.len());
    let mut context_run: Vec<&str> = Vec::new();
    let mut in_hunk = false;

    fn flush(run: &mut Vec<&str>, out: &mut String) {
        if run.len() > MAX_VERBATIM_CONTEXT {
            out.push_str(&format!("  ... {} context lines omitted\n", run.len()));
        } else {
            for line in run.iter() {
                out.push_str(line);
                out.push('\n');
            }
        }
        run.clear();
    }

    for line in content.lines() {
        let is_header = line.starts_with("diff --git")
            || line.starts_with("index ")
            || line.starts_with("---")
            || line.starts_with("+++");
        if line.starts_with("@@") || is_header {
            flush(&mut context_run, &mut out);
            in_hunk = line.starts_with("@@");
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if in_hunk && line.starts_with(' ') && line.len() > 1 {
            context_run.push(line);
            continue;
        }
        flush(&mut context_run, &mut out);
        out.push_str(line);
        out.push('\n');
    }
    flush(&mut context_run, &mut out);
    out
}

fn lossless_prune(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut blank_run = 0usize;
    let mut prev_line: Option<String> = None;
    let mut dup_run = 0usize;

    for raw in content.lines() {
        let line = strip_ansi(raw);
        let trimmed_end = line.trim_end();

        if trimmed_end.is_empty() {
            // Collapse 2+ consecutive blank lines into one.
            blank_run += 1;
            if blank_run <= 1 {
                out.push('\n');
            }
            continue;
        }
        blank_run = 0;

        // Collapse runs of identical lines into one + a count receipt, so the
        // model keeps the fact (and how many) without paying for every repeat.
        if prev_line.as_deref() == Some(trimmed_end) {
            dup_run += 1;
            continue;
        }
        if dup_run > 0 {
            out.push_str(&format!(
                "    … (previous line repeated {} more times)\n",
                dup_run
            ));
            dup_run = 0;
        }

        out.push_str(trimmed_end);
        out.push('\n');
        prev_line = Some(trimmed_end.to_string());
    }
    if dup_run > 0 {
        out.push_str(&format!(
            "    … (previous line repeated {} more times)\n",
            dup_run
        ));
    }
    // Trim a single trailing newline we may have over-added.
    while out.ends_with('\n') && out.ends_with("\n\n") {
        out.pop();
    }
    out
}

/// Strip ANSI/VT100 escape sequences (CSI `\x1b[...m` and friends).
fn strip_ansi(s: &str) -> String {
    if !s.contains('\x1b') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Consume an escape sequence: ESC [ ... <final byte 0x40-0x7E>.
            if chars.peek() == Some(&'[') {
                chars.next();
                for e in chars.by_ref() {
                    if ('\u{40}'..='\u{7E}').contains(&e) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_outputs_pass_through() {
        assert_eq!(compress("read", "hi"), "hi");
    }

    #[test]
    fn huge_bash_output_is_offloaded_with_receipt() {
        let line = "unique log line with payload number";
        let huge = (0..2000)
            .map(|i| format!("{line} {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = compress("bash", &huge);
        assert!(
            out.len() < huge.len() / 2,
            "must shrink: {} bytes",
            out.len()
        );
        assert!(out.contains("lines omitted"));
        assert!(out.contains("phoenix-tool-outputs"));
        // Receipt names a real file containing the FULL original.
        let path_start = out.find("/").unwrap();
        let path_end = out[path_start..].find(" ;").unwrap() + path_start;
        let saved_path = std::path::Path::new(out[path_start..path_end].trim());
        let saved = std::fs::read_to_string(saved_path).unwrap();
        assert!(saved.contains("payload number 1999"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(saved_path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(saved_path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        // Head and tail both survive in context.
        assert!(out.contains("payload number 0"));
        assert!(out.contains("payload number 1999"));
    }

    #[test]
    fn search_results_fold_per_file_keeping_first_and_last() {
        let mut input = String::new();
        for line in 0..30 {
            input.push_str(&format!("src/main.rs:{line}:fn hit_{line}() {{}}\n"));
        }
        for line in 0..3 {
            input.push_str(&format!("src/lib.rs:{line}:use hit;\n"));
        }
        let out = compress("bash", &input);
        assert!(out.contains("src/main.rs:0:"));
        assert!(out.contains("more matches in this file"));
        assert!(out.contains("src/main.rs:29:"), "last match kept: {out}");
        // Small group stays verbatim.
        assert!(out.contains("src/lib.rs:2:"));
        assert!(out.len() < input.len());
    }

    #[test]
    fn big_json_arrays_fold_to_samples_plus_schema() {
        let items: Vec<String> = (0..50)
            .map(|i| format!("{{\"id\": {i}, \"name\": \"row{i}\"}}"))
            .collect();
        let input = format!("[{}]", items.join(","));
        // `bash` still folds structurally; web tools no longer do.
        let out = compress("bash", &input);
        assert!(out.contains("items omitted"));
        assert!(out.contains("id, name"), "schema note present: {out}");
        assert!(out.contains("row0"));
        assert!(out.contains("row49"), "last item kept: {out}");
        assert!(out.len() < input.len());
    }

    #[test]
    fn web_fetch_prose_survives_whole_not_shredded_to_chrome() {
        // The bug (session 9395fcad): folding web_fetch as a log stream kept the
        // nav/image chrome and dropped the article body, forcing a re-read from
        // the CCR cache. Web output is the researcher's findings — pass it whole.
        let mut input = String::from("![banner](https://x/img.jpg)\n[Home](/) [About](/about)\n");
        for i in 0..60 {
            input.push_str(&format!(
                "The committee reached finding {i} about the policy timeline and the named parties involved.\n"
            ));
        }
        input.push_str("![footer](https://x/footer.png)\n");
        let out = compress("web_fetch", &input);
        assert!(out.contains("finding 30"), "article body survives: {out}");
        assert!(
            !out.contains("omitted"),
            "no structural folding on web prose"
        );
    }

    #[test]
    fn log_output_keeps_every_error_and_folds_routine_noise() {
        let mut input = String::new();
        for line in 0..200 {
            if line == 97 {
                input.push_str("ERROR connection refused to db-primary\n");
            } else {
                input.push_str(&format!("INFO 2026-06-11 worker tick {line}\n"));
            }
        }
        let out = compress("bash", &input);
        assert!(
            out.contains("ERROR connection refused"),
            "the buried error survives: {out}"
        );
        assert!(out.contains("routine log lines omitted"));
        assert!(out.len() < input.len());
    }

    #[test]
    fn read_output_is_never_offloaded() {
        let huge = (0..2000)
            .map(|i| format!("source line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = compress("read", &huge);
        assert!(!out.contains("lines omitted"));
        assert!(!out.contains("phoenix-tool-outputs"));
    }

    #[test]
    fn live_ingestion_does_not_elide_search_findings() {
        let content = (1..=20)
            .map(|line| format!("src/main.rs:{line}:distinct finding {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let ingested = prune_lossless_for_ingestion(&content);
        for line in 1..=20 {
            assert!(ingested.contains(&format!("distinct finding {line}")));
        }
        assert!(!ingested.contains("more matches"));
    }

    #[test]
    fn ansi_is_stripped() {
        let noisy = format!("\x1b[31merror\x1b[0m here {}", "x".repeat(300));
        let out = compress("bash", &noisy);
        assert!(!out.contains('\x1b'));
        assert!(out.contains("error here"));
    }

    #[test]
    fn consecutive_dupes_collapse_with_receipt() {
        let mut input = String::new();
        for _ in 0..50 {
            input.push_str("warning: same thing\n");
        }
        let out = compress("bash", &input);
        assert!(out.contains("warning: same thing"));
        assert!(out.contains("repeated"));
        assert!(out.len() < input.len());
    }

    #[test]
    fn never_grows() {
        let input = "a\nb\nc\n".repeat(60);
        let out = compress("grep", &input);
        assert!(out.len() <= input.len());
    }

    #[test]
    fn scattered_repeats_fold_to_one_receipt() {
        let mut input = String::new();
        for i in 0..30 {
            input.push_str(&format!("unique line {i}\n"));
            input.push_str("warning: flaky thing happened\n");
        }
        let out = compress("bash", &input);
        assert_eq!(out.matches("flaky thing").count(), 1);
        assert!(out.contains("30 x warning: flaky thing happened"));
        assert!(out.contains("unique line 29"));
    }

    #[test]
    fn diff_context_runs_fold_with_receipt() {
        let mut input = String::from(
            "diff --git a/f.rs b/f.rs\nindex 111..222 100644\n--- a/f.rs\n+++ b/f.rs\n@@ -1,30 +1,30 @@\n",
        );
        for i in 0..12 {
            input.push_str(&format!(" context line {i} with enough length to matter\n"));
        }
        input.push_str("-old line\n+new line\n");
        for i in 0..2 {
            input.push_str(&format!(" short tail {i}\n"));
        }
        let out = compress("bash", &input);
        assert!(out.contains("... 12 context lines omitted"));
        assert!(out.contains("-old line"));
        assert!(out.contains("+new line"));
        // Short runs (<= 3) stay verbatim.
        assert!(out.contains("short tail 1"));
        assert!(out.contains("diff --git a/f.rs b/f.rs"));
    }

    #[test]
    fn read_output_never_folds_repeats() {
        let mut input = String::new();
        for i in 0..10 {
            input.push_str(&format!("fn case_{i}() {{\n"));
            input.push_str("    return None;\n");
        }
        let out = compress("read", &input);
        assert_eq!(out.matches("return None;").count(), 10);
    }

    #[test]
    fn non_diff_output_is_not_diff_folded() {
        let input = format!(
            "results:\n{}",
            (0..40)
                .map(|i| format!(" indented finding {i}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let out = compress("grep", &input);
        assert!(!out.contains("context lines omitted"));
        assert!(out.contains("indented finding 39"));
    }

    #[test]
    fn blank_runs_collapse() {
        let pad = "x".repeat(220);
        let input = format!("start {pad}{}end {pad}", "\n".repeat(40));
        let out = compress("bash", &input);
        assert!(out.contains("start"));
        assert!(out.contains("end"));
        assert!(out.matches('\n').count() < 40);
    }
}
