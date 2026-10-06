//! read tool - read files for context

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{workspace_io, ToolOutput};

/// Lines returned when the caller gives no `limit` — big enough to take in a
/// whole module at once, bounded so a giant generated file can't flood context.
const DEFAULT_LINE_LIMIT: usize = 2000;
/// Hard ceiling on lines per call even when `limit` asks for more.
const MAX_LINE_LIMIT: usize = 4000;
/// Byte cap on the returned page: a file with very long lines can blow context
/// even within the line budget, so the page is byte-truncated past this.
const MAX_RETURN_BYTES: usize = 256 * 1024;
const MAX_QUESTION_BYTES: usize = 16 * 1024;
const MAX_PDF_BYTES: usize = 64 * 1024 * 1024;
const MAX_PDF_TEXT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadInput {
    pub path: String,
    /// The question this read is expected to answer. Stored with the content
    /// hash so colleagues can reuse evidence without pretending they read it.
    #[serde(default)]
    pub question: Option<String>,
    /// 1-based line to start from. Omitted = start of file.
    #[serde(default)]
    pub offset: Option<usize>,
    /// Maximum lines to return from `offset`. Omitted = a full page (2000).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Add exact source line labels for reviews; default preserves edit anchors.
    #[serde(default)]
    pub line_numbers: bool,
}

pub fn execute(workspace_root: &Path, confined: bool, input: ReadInput) -> Result<ToolOutput> {
    Ok(execute_with_receipt(workspace_root, confined, input)?.output)
}

pub(crate) struct ReadExecution {
    pub(crate) output: ToolOutput,
    pub(crate) sha256: String,
    pub(crate) bytes: u64,
}

pub(crate) fn execute_with_receipt(
    workspace_root: &Path,
    confined: bool,
    input: ReadInput,
) -> Result<ReadExecution> {
    workspace_io::validate_path_input(&input.path)?;
    if input
        .question
        .as_ref()
        .is_some_and(|question| question.len() > MAX_QUESTION_BYTES)
    {
        bail!("read question is too long (max {MAX_QUESTION_BYTES} bytes)");
    }
    if Path::new(&input.path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
    {
        return execute_pdf_with_receipt(workspace_root, confined, &input);
    }
    let snapshot = workspace_io::read_regular_utf8_snapshot(
        workspace_root,
        &input.path,
        confined,
        workspace_io::MAX_WORKSPACE_TEXT_BYTES,
    )?;
    page_text(input, snapshot.text, snapshot.sha256, snapshot.bytes)
}

fn execute_pdf_with_receipt(
    workspace_root: &Path,
    confined: bool,
    input: &ReadInput,
) -> Result<ReadExecution> {
    let bytes =
        workspace_io::read_regular_bytes(workspace_root, &input.path, confined, MAX_PDF_BYTES)?;
    if !bytes.starts_with(b"%PDF-") {
        bail!("file has a .pdf name but no PDF signature: {}", input.path);
    }
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("source.pdf");
    let extracted = temp.path().join("extracted.txt");
    std::fs::write(&source, &bytes)?;
    let timeout = "/usr/bin/timeout";
    let pdftotext = if Path::new("/usr/bin/pdftotext").is_file() {
        "/usr/bin/pdftotext"
    } else {
        "pdftotext"
    };
    if !Path::new(timeout).is_file() {
        bail!("PDF extraction needs /usr/bin/timeout");
    }
    let output = Command::new(timeout)
        .arg("20s")
        .arg(pdftotext)
        .arg("-layout")
        .arg("-nopgbrk")
        .arg(&source)
        .arg(&extracted)
        .output()?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail.trim().chars().take(500).collect::<String>();
        if output.status.code() == Some(124) {
            bail!("PDF text extraction timed out after 20s: {}", input.path);
        }
        bail!(
            "PDF text extraction failed for {}{}",
            input.path,
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        );
    }
    let extracted_bytes =
        workspace_io::read_regular_bytes_at(temp.path(), &extracted, false, MAX_PDF_TEXT_BYTES)?;
    let text = String::from_utf8(extracted_bytes)
        .map_err(|error| anyhow::anyhow!("extracted PDF text was not UTF-8: {error}"))?;
    page_text(input.clone(), text, sha256, bytes.len() as u64)
}

fn page_text(
    input: ReadInput,
    text: String,
    sha256: String,
    source_bytes: u64,
) -> Result<ReadExecution> {
    if text.as_bytes().contains(&0) {
        bail!("refusing to read binary file: {}", input.path);
    }

    // `split_inclusive` keeps each line's terminator, so concatenating a slice
    // reproduces the file's exact bytes (CRLF and trailing-newline included).
    // Default output remains byte-verbatim for str_replace. Numbered review
    // mode is explicit and labels actual source offsets, not display rows.
    let total = text.split_inclusive('\n').count();
    if total == 0 {
        return Ok(ReadExecution {
            output: ToolOutput {
                summary: format!("Read {} — empty file (0 lines).", input.path),
                content: String::new(),
            },
            sha256,
            bytes: source_bytes,
        });
    }

    // 1-based offset → 0-based start; offset 0 and 1 both mean "from the top".
    let start = input.offset.unwrap_or(1).max(1) - 1;
    if start >= total {
        bail!(
            "read offset {} is past the end of {} ({} lines).",
            start + 1,
            input.path,
            total
        );
    }
    let limit = input
        .limit
        .unwrap_or(DEFAULT_LINE_LIMIT)
        .clamp(1, MAX_LINE_LIMIT);
    let requested_end = (start + limit).min(total);

    let mut content = String::with_capacity(MAX_RETURN_BYTES.min(text.len()));
    let mut byte_truncated = false;
    let mut complete_lines = 0usize;
    for (index, line) in text
        .split_inclusive('\n')
        .skip(start)
        .take(requested_end - start)
        .enumerate()
    {
        let prefix = if input.line_numbers { format!("{} | ", start + index + 1) } else { String::new() };
        let rendered_bytes = prefix.len() + line.len();
        if rendered_bytes > MAX_RETURN_BYTES {
            bail!(
                "read line {} in {} is {} bytes, exceeding the {}-byte per-line return limit; use grep or a bounded shell byte-range command for this generated/minified line",
                start + index + 1,
                input.path,
                rendered_bytes,
                MAX_RETURN_BYTES
            );
        }
        let remaining = MAX_RETURN_BYTES.saturating_sub(content.len());
        if rendered_bytes <= remaining {
            content.push_str(&prefix);
            content.push_str(line);
            complete_lines += 1;
            continue;
        }
        // Never return a partial source line while claiming the line was read:
        // `str_replace` depends on byte-verbatim anchors. Stop before this line
        // so `offset=<line>` can resume without a gap or duplicate prefix.
        byte_truncated = true;
        break;
    }

    let end = start + complete_lines;
    let more_after = total - end;
    let mut summary = format!(
        "Read lines {}-{} of {} from {}.",
        start + 1,
        end,
        total,
        input.path
    );
    if more_after > 0 {
        summary.push_str(&format!(
            " {more_after} more line(s) below — call read again with offset={} for the next page.",
            end + 1
        ));
    }
    if input.line_numbers {
        summary.push_str(" Numbered source for citation; `N | ` prefixes are not file content. For exact edit anchors use line_numbers=false.");
    }
    if byte_truncated {
        summary.push_str(
            " (page stopped at the byte cap before the next line; no partial line was returned)",
        );
    }

    Ok(ReadExecution {
        output: ToolOutput { summary, content },
        sha256,
        bytes: source_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    fn write_file(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    fn tiny_pdf(text: &str) -> Vec<u8> {
        let escaped = text
            .replace('\\', "\\\\")
            .replace('(', "\\(")
            .replace(')', "\\)");
        let stream = format!("BT /F1 12 Tf 72 720 Td ({escaped}) Tj ET\n");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_string(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
            format!("<< /Length {} >>\nstream\n{stream}endstream", stream.len()),
        ];
        let mut pdf = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            let _ = write!(pdf, "{} 0 obj\n{}\nendobj\n", index + 1, object);
        }
        let xref = pdf.len();
        let _ = write!(pdf, "xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
        for offset in offsets {
            let _ = writeln!(pdf, "{offset:010} 00000 n ");
        }
        let _ = write!(
            pdf,
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        );
        pdf.into_bytes()
    }

    #[test]
    fn reads_pdf_text_without_opening_a_shared_desktop_window() {
        if !Path::new("/usr/bin/pdftotext").is_file() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lesson.pdf"), tiny_pdf("Phoenix PDF text")).unwrap();
        let out = execute(
            dir.path(),
            true,
            ReadInput {
                path: "lesson.pdf".into(),
                line_numbers: false,
                question: Some("What text is in this lesson?".into()),
                offset: None,
                limit: None,
            },
        )
        .unwrap();
        assert!(out.content.contains("Phoenix PDF text"), "{}", out.content);
    }

    #[test]
    fn reads_a_specific_line_range_in_one_call() {
        let dir = tempfile::tempdir().unwrap();
        let body: String = (1..=100).map(|i| format!("line {i}\n")).collect();
        write_file(dir.path(), "f.txt", &body);

        let out = execute(
            dir.path(),
            true,
            ReadInput {
                path: "f.txt".into(),
                line_numbers: false,
                question: None,
                offset: Some(40),
                limit: Some(3),
            },
        )
        .unwrap();
        assert_eq!(out.content, "line 40\nline 41\nline 42\n");
        assert!(
            out.summary.contains("lines 40-42 of 100"),
            "{}",
            out.summary
        );
    }

    #[test]
    fn full_read_pages_and_points_at_the_next_offset() {
        let dir = tempfile::tempdir().unwrap();
        let body: String = (1..=DEFAULT_LINE_LIMIT + 50)
            .map(|i| format!("l{i}\n"))
            .collect();
        write_file(dir.path(), "big.txt", &body);

        let out = execute(
            dir.path(),
            true,
            ReadInput {
                path: "big.txt".into(),
                line_numbers: false,
                question: None,
                offset: None,
                limit: None,
            },
        )
        .unwrap();
        assert!(out.content.starts_with("l1\n"));
        assert!(out.content.ends_with(&format!("l{DEFAULT_LINE_LIMIT}\n")));
        assert!(
            out.summary
                .contains(&format!("offset={}", DEFAULT_LINE_LIMIT + 1)),
            "{}",
            out.summary
        );
    }

    #[test]
    fn slice_is_byte_verbatim_so_str_replace_still_matches() {
        // CRLF endings must survive the round-trip (no \n normalization).
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "crlf.txt", "a\r\nb\r\nc\r\n");
        let out = execute(
            dir.path(),
            true,
            ReadInput {
                path: "crlf.txt".into(),
                line_numbers: false,
                question: None,
                offset: Some(2),
                limit: Some(1),
            },
        )
        .unwrap();
        assert_eq!(out.content, "b\r\n");
    }

    #[test]
    fn offset_past_end_errors_clearly() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "s.txt", "one\ntwo\n");
        let err = execute(
            dir.path(),
            true,
            ReadInput {
                path: "s.txt".into(),
                line_numbers: false,
                question: None,
                offset: Some(99),
                limit: None,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("past the end"), "{err}");
    }

    #[test]
    fn rejects_non_utf8_instead_of_returning_unmatchable_lossy_text() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bad.txt"), [b'a', 0xff, b'b']).unwrap();
        let error = execute(
            dir.path(),
            true,
            ReadInput {
                path: "bad.txt".into(),
                line_numbers: false,
                question: None,
                offset: None,
                limit: None,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("not UTF-8"), "{error:#}");
    }

    #[test]
    fn byte_cap_stops_before_a_line_and_reports_the_true_resume_offset() {
        let dir = tempfile::tempdir().unwrap();
        let first = format!("{}\n", "a".repeat(MAX_RETURN_BYTES - 2));
        write_file(dir.path(), "wide.txt", &format!("{first}second\nthird\n"));
        let out = execute(
            dir.path(),
            true,
            ReadInput {
                path: "wide.txt".into(),
                line_numbers: false,
                question: None,
                offset: None,
                limit: None,
            },
        )
        .unwrap();
        assert_eq!(out.content, first);
        assert!(out.summary.contains("lines 1-1 of 3"), "{}", out.summary);
        assert!(out.summary.contains("offset=2"), "{}", out.summary);
        assert!(!out.content.contains("second"));
    }

    #[test]
    fn an_individually_unpageable_line_fails_instead_of_claiming_it_was_read() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "minified.txt",
            &"x".repeat(MAX_RETURN_BYTES + 1),
        );
        let error = execute(
            dir.path(),
            true,
            ReadInput {
                path: "minified.txt".into(),
                line_numbers: false,
                question: None,
                offset: None,
                limit: None,
            },
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("per-line return limit"),
            "{error:#}"
        );
    }

    #[test]
    fn numbered_review_uses_source_offsets_and_preserves_raw_mode() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "review.txt", "first\r\nβeta\r\n\r\nlast");
        let mut input: ReadInput = serde_json::from_value(serde_json::json!({"path":"review.txt","offset":2,"limit":3})).unwrap();
        assert!(!input.line_numbers);
        assert_eq!(execute(dir.path(), true, input.clone()).unwrap().content, "βeta\r\n\r\nlast");
        input.line_numbers = true;
        let output = execute(dir.path(), true, input).unwrap();
        assert_eq!(output.content, "2 | βeta\r\n3 | \r\n4 | last");
        assert!(output.summary.contains("prefixes are not file content"));
    }

    #[test]
    fn numbered_page_budget_includes_prefix_and_resumes_without_a_gap() {
        let dir = tempfile::tempdir().unwrap();
        let first = format!("{}\n", "a".repeat(MAX_RETURN_BYTES - 6));
        write_file(dir.path(), "numbered.txt", &format!("{first}second\n"));
        let input: ReadInput = serde_json::from_value(serde_json::json!({"path":"numbered.txt","line_numbers":true})).unwrap();
        let output = execute(dir.path(), true, input.clone()).unwrap();
        assert_eq!(output.content, format!("1 | {first}"));
        assert!(output.content.len() <= MAX_RETURN_BYTES);
        assert!(output.summary.contains("offset=2"));
        let output = execute(dir.path(), true, ReadInput {offset:Some(2), ..input}).unwrap();
        assert_eq!(output.content, "2 | second\n");
    }
}
