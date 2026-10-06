//! Tier 3 of the compression cascade — retrieval over the compaction archive.
//!
//! Tiers 1 & 2 (importance-trim + anchored summary in `runtime::compaction`)
//! keep the WORKING window small by folding old turns out of it. That fold is
//! only safe if the folded facts can be pulled BACK on demand instead of
//! re-derived — otherwise the agent re-reads the same file or re-runs the same
//! search it already did a thousand tokens ago (the "99% reduction costs more"
//! failure the governing rule warns about). Compaction already archives every
//! folded message verbatim to `<session>.archive.jsonl`; this tool makes that
//! archive SEARCHABLE so the long-term store is a real store, not a write-only
//! log. It is also the substrate the Oracle will read to judge a run against
//! what already happened.
//!
//! Scoring is lexical (term overlap + importance/recency weighting), which
//! answers the common "what did we find about X" recall well and needs no index
//! build. Embedding-backed semantic recall is the natural upgrade — the provider
//! already exposes `embeddings()` — and would slot in behind this same tool.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::session::Message;
use crate::tools::ToolOutput;

#[derive(Debug, Default, Deserialize)]
pub struct RecallInput {
    #[serde(default)]
    pub tool_prefix: Option<String>,
    #[serde(default)]
    pub failed_only: Option<bool>,
    /// What fact, finding, decision, or value you are trying to pull back from
    /// earlier in this session — e.g. "the postgres DSN", "what the researcher
    /// found about the rate limit", "why we chose zen as the browser source".
    #[serde(default)]
    pub query: String,
    /// How many matches to return (default 5, max 15).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Exact one-based archive line returned by search; bypasses ranking.
    #[serde(default)]
    pub line: Option<usize>,
    /// Unicode character offset within that verbatim JSON record.
    #[serde(default)]
    pub offset: Option<usize>,
}

const DEFAULT_LIMIT: usize = 5;
const MAX_LIMIT: usize = 15;
const SNIPPET_CHARS: usize = 500;
const MAX_QUERY_BYTES: usize = 4 * 1024;
const MAX_QUERY_TERMS: usize = 64;
const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;
const MAX_ARCHIVE_LINE_BYTES: usize = 1024 * 1024;
const MAX_ARCHIVE_MESSAGES: usize = 100_000;

/// Words too common to discriminate — dropped from the query so scoring keys on
/// the content terms, not the connective tissue.
const STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "but", "for", "of", "to", "in", "on", "at", "by", "is", "are",
    "was", "were", "be", "been", "what", "which", "that", "this", "with", "about", "did", "do",
    "does", "we", "i", "it", "from", "how", "why", "when", "where",
];

pub fn execute(input: RecallInput, archive_dir: &Path, session_id: &str) -> Result<ToolOutput> {
    let query = input.query.trim();
    let tool_prefix=input.tool_prefix.as_deref().map(str::trim).filter(|prefix|!prefix.is_empty());
    let filtered=tool_prefix.is_some()||input.failed_only==Some(true);
    if tool_prefix.is_some_and(|prefix|prefix.len()>128||prefix.chars().any(char::is_control)) {
        bail!("tool_prefix must be at most 128 bytes without control characters");
    }
    if input.line.is_some()&&filtered {bail!("tool filters select search results; clear them for an exact-line read");}
    if query.is_empty() && input.line.is_none() && !filtered {
        bail!("recall needs a `query` — name the fact or finding you're trying to pull back.");
    }
    if query.len() > MAX_QUERY_BYTES {
        bail!(
            "recall query is too large ({} bytes; max {MAX_QUERY_BYTES})",
            query.len()
        );
    }
    crate::session::SessionStore::validate_session_id(session_id)
        .context("recall received an invalid session id")?;
    let limit = input.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

    let path = archive_dir.join(format!("{session_id}.archive.jsonl"));
    let raw = match crate::config::private_io::read_private_file_limited(&path, MAX_ARCHIVE_BYTES)?
    {
        Some(bytes) => String::from_utf8(bytes)
            .with_context(|| format!("compaction archive {} is not UTF-8", path.display()))?,
        None => {
            return Ok(ToolOutput {
                summary: "no folded history yet".to_string(),
                content: "Nothing has been compacted out of this session yet — the whole conversation is still in your context. Scroll up instead of recalling.".to_string(),
            });
        }
    };

    if let Some(line_number) = input.line {
        if line_number == 0 { bail!("archive line numbers start at 1"); }
        let line = raw.lines().nth(line_number - 1).context("archive line does not exist")?;
        if line.len() > MAX_ARCHIVE_LINE_BYTES { bail!("archive record exceeds safe read size"); }
        serde_json::from_str::<Message>(line).context("requested archive record is corrupt")?;
        let offset = input.offset.unwrap_or(0);
        let total = line.chars().count();
        if offset > total { bail!("offset exceeds the archive record's {total} characters"); }
        let text: String = line.chars().skip(offset).take(6_000).collect();
        let end = offset + text.chars().count();
        let next = (end < total).then_some(end);
        return Ok(ToolOutput {
            summary: format!("archive line {line_number}, characters {offset}..{end} of {total}"),
            content: serde_json::json!({
                "archive_line": line_number, "offset": offset, "next_offset": next,
                "total_characters": total, "verbatim_json_fragment": text,
                "provenance": "Exact archived record, not independent verification of its claims. Read subsequent corrections and verify mutable state.",
            }).to_string(),
        });
    }
    if input.offset.is_some() { bail!("offset requires an archive line"); }

    let terms = query_terms(query);
    if terms.is_empty() && !filtered {
        bail!("recall query has no distinctive terms — use the specific words you're looking for.");
    }
    if terms.len() > MAX_QUERY_TERMS {
        bail!("recall query has too many distinct terms (max {MAX_QUERY_TERMS})");
    }

    // Later lines were folded more recently; carry the line number for the
    // recency tiebreak so equally-relevant hits prefer the newer one.
    // Keep only the requested best hits. A 64 MiB archive can contain many
    // matching messages; retaining every rendered duplicate would multiply
    // memory use even though at most 15 are returned.
    let mut scored: Vec<(f64, usize, String)> = Vec::with_capacity(limit);
    let mut total_matches = 0usize;
    let mut message_count = 0usize;
    for (line_no, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        message_count += 1;
        if message_count > MAX_ARCHIVE_MESSAGES {
            bail!(
                "compaction archive {} has more than {MAX_ARCHIVE_MESSAGES} messages",
                path.display()
            );
        }
        if line.len() > MAX_ARCHIVE_LINE_BYTES {
            bail!(
                "compaction archive {} has an oversized message at line {} ({} bytes; max {MAX_ARCHIVE_LINE_BYTES})",
                path.display(),
                line_no + 1,
                line.len()
            );
        }
        let message = serde_json::from_str::<Message>(line).with_context(|| {
            format!(
                "compaction archive {} is corrupt at line {}",
                path.display(),
                line_no + 1
            )
        })?;
        if filtered {
            let Message::ToolResult{tool_name,success,..}=&message else {continue;};
            if tool_prefix.is_some_and(|prefix|!tool_name.starts_with(prefix))
                || (input.failed_only==Some(true)&&*success) {continue;}
        }
        let rendered = render(&message);
        let score = if terms.is_empty(){1.0}else{score_message(&terms, &rendered, &message)};
        if score > 0.0 {
            total_matches += 1;
            scored.push((score, line_no, rendered));
            scored.sort_by(|a, b| {
                b.0.partial_cmp(&a.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(b.1.cmp(&a.1))
            });
            scored.truncate(limit);
        }
    }

    if scored.is_empty() {
        return Ok(ToolOutput {
            summary: format!("no archive match for: {query}"),
            content: format!(
                "No folded message matches \u{201c}{query}\u{201d}. It may never have been in this session, or the wording differs — try the specific terms, or re-run the tool that would produce it."
            ),
        });
    }

    let total = total_matches;
    let shown = total.min(limit);
    let mut out = format!(
        "{shown} of {total} folded message(s) matched \u{201c}{query}\u{201d} (ranked excerpts, not independently verified facts). Use recall with line to retrieve the exact record; follow next_offset for later fragments. Distinguish user reports, model claims, and tool observations; later corrections and fresh evidence may supersede them:\n"
    );
    for (rank, (_score, line, rendered)) in scored.into_iter().take(limit).enumerate() {
        out.push_str(&format!(
            "\n#{} [archive line {}] {}\n",
            rank + 1,
            line + 1,
            snippet(&rendered, SNIPPET_CHARS)
        ));
    }
    Ok(ToolOutput {
        summary: format!("recalled {shown} of {total} matches"),
        content: out,
    })
}

/// Lowercased, de-stopworded, deduped query terms (length >= 2).
fn query_terms(query: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut terms = Vec::new();
    for raw in query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
    {
        let term = raw.to_lowercase();
        if term.len() < 2 || STOPWORDS.contains(&term.as_str()) {
            continue;
        }
        if seen.insert(term.clone()) {
            terms.push(term);
        }
    }
    terms
}

/// Lexical relevance with diminishing returns on repeats, then an importance
/// multiplier: user/talk messages (decisions, findings, the user's words) and
/// failed tool results (errors) are exactly what recall exists to recover.
fn score_message(terms: &[String], rendered: &str, message: &Message) -> f64 {
    let lower = rendered.to_lowercase();
    let mut score = 0.0;
    for term in terms {
        let count = lower.matches(term.as_str()).count();
        if count > 0 {
            score += 1.0 + (count as f64 - 1.0) * 0.2;
        }
    }
    if score == 0.0 {
        return 0.0;
    }
    match message {
        Message::User { .. } | Message::Talk { .. } | Message::GroupContribution { .. } => {
            score * 1.3
        }
        Message::ToolResult { success: false, .. } => score * 1.2,
        _ => score,
    }
}

fn render(message: &Message) -> String {
    match message {
        Message::User { content } => format!("[user] {content}"),
        Message::Assistant { content } => format!("[assistant] {content}"),
        Message::Talk {
            from,
            to,
            subject,
            body,
            ..
        } => format!("[talk {from} -> {to}: {subject}] {body}"),
        Message::GroupContribution {
            agent_id,
            display_name,
            subject,
            body,
            ..
        } => format!("[group {display_name} ({agent_id}): {subject}] {body}"),
        Message::ToolResult {
            tool_name,
            input,
            success,
            output,
        } => format!(
            "[tool {tool_name}({input}) {}] {output}",
            if *success { "ok" } else { "FAILED" }
        ),
    }
}

fn snippet(text: &str, cap: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= cap {
        collapsed
    } else {
        format!(
            "{}\u{2026}",
            collapsed.chars().take(cap).collect::<String>()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_failure_filter_ignores_success_text_and_other_tools() {
        let dir=tempfile::tempdir().unwrap();
        write_archive(dir.path(),"filtered",&[
            user("computer_act failed according to an unverified claim"),
            tool("computer_act","{}",true,"No failed steps"),
            tool("bash","test",false,"failure in another tool family"),
            tool("computer_key","num1",false,"unknown key num1"),
        ]);
        let out=execute(RecallInput{tool_prefix:Some("computer_".into()),failed_only:Some(true),..Default::default()},dir.path(),"filtered").unwrap();
        assert!(out.content.contains("archive line 4"));
        assert!(out.content.contains("unknown key num1"));
        assert!(!out.content.contains("No failed steps"));
        assert!(!out.content.contains("another tool family"));
        assert!(!out.content.contains("unverified claim"));
        assert!(execute(RecallInput{line:Some(4),failed_only:Some(true),..Default::default()},dir.path(),"filtered").is_err());
    }

    fn write_archive(dir: &Path, session: &str, messages: &[Message]) {
        use std::io::Write;
        let path = dir.join(format!("{session}.archive.jsonl"));
        let mut file = std::fs::File::create(path).unwrap();
        for m in messages {
            writeln!(file, "{}", serde_json::to_string(m).unwrap()).unwrap();
        }
    }

    fn user(text: &str) -> Message {
        Message::User {
            content: text.to_string(),
        }
    }
    fn tool(name: &str, input: &str, success: bool, output: &str) -> Message {
        Message::ToolResult {
            tool_name: name.to_string(),
            input: input.to_string(),
            success,
            output: output.to_string(),
        }
    }

    #[test]
    fn query_terms_drops_stopwords_and_dedupes() {
        let terms = query_terms("what was the POSTGRES dsn for the postgres db");
        assert!(terms.contains(&"postgres".to_string()));
        assert!(terms.contains(&"dsn".to_string()));
        assert!(!terms.contains(&"the".to_string()));
        // "postgres" appears twice but is deduped.
        assert_eq!(terms.iter().filter(|t| *t == "postgres").count(), 1);
    }

    #[test]
    fn recall_finds_the_relevant_folded_fact() {
        let dir = tempfile::tempdir().unwrap();
        write_archive(
            dir.path(),
            "s1",
            &[
                user("let's refactor the auth module"),
                tool(
                    "read",
                    "db.rs",
                    true,
                    "DB_DSN = postgres://ro@host:5433/exp",
                ),
                tool("bash", "ls", true, "a.txt b.txt c.txt"),
            ],
        );
        let out = execute(
            RecallInput {
                query: "postgres dsn".to_string(),
                limit: None,
                ..Default::default()
            },
            dir.path(),
            "s1",
        )
        .unwrap();
        assert!(
            out.content.contains("postgres://ro@host:5433/exp"),
            "got: {}",
            out.content
        );
        // The irrelevant listing must not outrank it / appear first.
        assert!(
            out.content.find("postgres").unwrap() < out.content.find("a.txt").unwrap_or(usize::MAX)
        );
    }

    #[test]
    fn exact_archive_pages_reconstruct_long_unicode_record_without_loss() {
        let dir = tempfile::tempdir().unwrap();
        let body = format!("{}\nExact final measurement: 240.000 mm; assessment completed.\n", "café 🦊 漢字 ".repeat(2_000));
        let message = user(&body);
        write_archive(dir.path(), "pages", &[message]);
        let original = std::fs::read_to_string(dir.path().join("pages.archive.jsonl")).unwrap();
        let search = execute(RecallInput { query: "measurement assessment".into(), ..Default::default() }, dir.path(), "pages").unwrap();
        assert!(search.content.contains("archive line 1"));
        assert!(!search.content.contains("ground truth"));
        let mut reconstructed = String::new();
        let mut offset = 0;
        let mut pages = 0;
        loop {
            let input: RecallInput = serde_json::from_value(serde_json::json!({"line":1,"offset":offset})).unwrap();
            let output = execute(input, dir.path(), "pages").unwrap();
            let value: serde_json::Value = serde_json::from_str(&output.content).unwrap();
            reconstructed.push_str(value["verbatim_json_fragment"].as_str().unwrap());
            pages += 1;
            match value["next_offset"].as_u64() {
                Some(next) => { assert!(next > offset); offset = next; }
                None => break,
            }
        }
        assert!(pages > 1);
        assert_eq!(reconstructed, original.trim_end_matches('\n'));
        assert!(matches!(serde_json::from_str::<Message>(&reconstructed).unwrap(), Message::User { content } if content == body));
    }

    #[test]
    fn exact_archive_rejects_invalid_coordinates_and_keeps_provenance() {
        let dir = tempfile::tempdir().unwrap();
        write_archive(dir.path(), "coords", &[user("Lesson 3 and 4 are complete.")]);
        for input in [
            RecallInput { line: Some(0), ..Default::default() },
            RecallInput { line: Some(2), ..Default::default() },
            RecallInput { line: Some(1), offset: Some(999_999), ..Default::default() },
            RecallInput { query: "lesson".into(), offset: Some(1), ..Default::default() },
        ] { assert!(execute(input, dir.path(), "coords").is_err()); }
        let result = execute(RecallInput { line: Some(1), ..Default::default() }, dir.path(), "coords").unwrap();
        assert!(result.content.contains("not independent verification"));
    }

    #[test]
    fn recall_reports_no_match_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        write_archive(dir.path(), "s1", &[user("hello world")]);
        let out = execute(
            RecallInput {
                query: "kubernetes ingress".to_string(),
                limit: None,
                ..Default::default()
            },
            dir.path(),
            "s1",
        )
        .unwrap();
        assert!(out.content.contains("No folded message matches"));
    }

    #[test]
    fn recall_with_no_archive_says_scroll_up() {
        let dir = tempfile::tempdir().unwrap();
        let out = execute(
            RecallInput {
                query: "anything".to_string(),
                limit: None,
                ..Default::default()
            },
            dir.path(),
            "missing",
        )
        .unwrap();
        assert!(out.content.contains("Scroll up"));
    }

    #[test]
    fn recall_rejects_invalid_session_ids_and_corrupt_archives() {
        let dir = tempfile::tempdir().unwrap();
        assert!(execute(
            RecallInput {
                query: "fact".to_string(),
                limit: None,
                ..Default::default()
            },
            dir.path(),
            "../escape",
        )
        .is_err());

        std::fs::write(dir.path().join("safe.archive.jsonl"), b"{not-json}\n").unwrap();
        let error = execute(
            RecallInput {
                query: "fact".to_string(),
                limit: None,
                ..Default::default()
            },
            dir.path(),
            "safe",
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("corrupt at line 1"));
    }

    #[cfg(unix)]
    #[test]
    fn recall_rejects_symlinked_and_oversized_archives_without_following_them() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::write(&outside, serde_json::to_vec(&user("secret fact")).unwrap()).unwrap();
        symlink(&outside, dir.path().join("linked.archive.jsonl")).unwrap();
        assert!(execute(
            RecallInput {
                query: "secret".to_string(),
                limit: None,
                ..Default::default()
            },
            dir.path(),
            "linked",
        )
        .is_err());

        let oversized = std::fs::File::create(dir.path().join("huge.archive.jsonl")).unwrap();
        oversized.set_len(MAX_ARCHIVE_BYTES as u64 + 1).unwrap();
        assert!(execute(
            RecallInput {
                query: "fact".to_string(),
                limit: None,
                ..Default::default()
            },
            dir.path(),
            "huge",
        )
        .is_err());
    }
}
