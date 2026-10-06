//! vital_memory_write + VITALS.md — Phoenix's always-on "vital memory".
//!
//! `VITALS.md` lives at the Phoenix home ROOT (NOT under the librarian's
//! `~/.phoenix/memory/` tree — deliberately outside librarian control). It holds
//! the FEW durable facts that must ride in EVERY turn's context: the user's
//! standing preferences, goals, hard "don't"s, and standing instructions.
//! Its active facts are injected into the runtime context block on every turn,
//! so the agent honors them without being reminded. Correction history remains
//! in the file for inspection and is excluded from that context.
//!
//! The orchestrator curates it with `vital_memory_write` — SPARINGLY. This is not
//! a journal of the conversation; it is the handful of things that change how
//! Phoenix should behave for this user across all future sessions.

use std::path::PathBuf;

use anyhow::{bail, Result};
use serde::Deserialize;

use crate::vital_memory_document::{append_history, split_history, VitalRevision};

use super::ToolOutput;

/// The fixed buckets, in display order. A closed set keeps VITALS.md organized
/// and bounded instead of an ever-growing flat dump.
const SECTIONS: [(&str, &str); 4] = [
    ("preference", "## Preferences"),
    ("goal", "## Goals"),
    ("avoid", "## Do NOT"),
    ("instruction", "## Always do"),
];

/// Keep it vital, not verbose: hard caps so the always-injected block stays small.
const MAX_PER_SECTION: usize = 12;
const MAX_TOTAL: usize = 36;
const MAX_NOTE_CHARS: usize = 240;
const MAX_VITALS_FILE_BYTES: usize = crate::vital_memory_document::MAX_FILE_BYTES;

#[derive(Debug, Deserialize)]
pub struct VitalMemoryWriteInput {
    /// The durable fact to remember, as ONE short line (no trailing period needed).
    pub note: String,
    /// Which bucket: "preference" | "goal" | "avoid" | "instruction".
    pub category: String,
    /// Optional: one existing entry to supersede (case-insensitive).
    /// Exact matches take priority; otherwise the substring must match just
    /// one entry. Missing or ambiguous matches fail without changing the file.
    #[serde(default)]
    pub replaces: Option<String>,
}

pub fn vitals_path() -> PathBuf {
    crate::config::phoenix_vitals_path()
}

fn canonical_category(raw: &str) -> Option<&'static str> {
    let c = raw.trim().to_lowercase();
    match c.as_str() {
        "preference" | "preferences" | "pref" | "like" | "likes" => Some("preference"),
        "goal" | "goals" | "objective" => Some("goal"),
        "avoid" | "do not" | "dont" | "don't" | "never" | "dislike" => Some("avoid"),
        "instruction" | "instructions" | "always" | "do" | "always do" | "rule" => {
            Some("instruction")
        }
        _ => None,
    }
}

/// Parse the current VITALS.md into (canonical_key -> bullet lines).
fn parse_sections(raw: &str) -> Vec<(&'static str, Vec<String>)> {
    let mut buckets: Vec<(&'static str, Vec<String>)> =
        SECTIONS.iter().map(|(key, _)| (*key, Vec::new())).collect();
    let mut current: Option<usize> = None;
    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(stripped) = trimmed.strip_prefix("## ") {
            current = SECTIONS
                .iter()
                .position(|(_, header)| header[3..].eq_ignore_ascii_case(stripped));
            continue;
        }
        if let Some(idx) = current {
            if let Some(bullet) = trimmed.strip_prefix("- ") {
                let bullet = bullet.trim();
                if !bullet.is_empty() {
                    buckets[idx].1.push(bullet.to_string());
                }
            }
        }
    }
    buckets
}

/// Render buckets back to a stable VITALS.md (fixed section order → byte-stable
/// for the always-injected context cache, changing only when content changes).
fn render(buckets: &[(&'static str, Vec<String>)]) -> String {
    let mut out = String::from("# VITALS — Phoenix's vital memory\n\n");
    out.push_str(
        "> Always-on. The user's durable preferences, goals, boundaries, and standing\n\
         > instructions. Curated by the orchestrator via `vital_memory_write`.\n\n",
    );
    for (key, header) in SECTIONS.iter() {
        out.push_str(header);
        out.push('\n');
        if let Some((_, lines)) = buckets.iter().find(|(k, _)| k == key) {
            for line in lines {
                out.push_str("- ");
                out.push_str(line);
                out.push('\n');
            }
        }
        out.push('\n');
    }
    out
}

pub fn execute(input: VitalMemoryWriteInput) -> Result<ToolOutput> {
    execute_at(&vitals_path(), input)
}

fn execute_at(path: &std::path::Path, input: VitalMemoryWriteInput) -> Result<ToolOutput> {
    let note = input.note.trim().to_string();
    if note.is_empty() {
        bail!("vital_memory_write needs a non-empty `note`.");
    }
    if note.contains(['\n', '\r']) {
        bail!("vital memory entries must be one line");
    }
    if note.chars().count() > MAX_NOTE_CHARS {
        bail!(
            "vital memory entries must be short ({MAX_NOTE_CHARS} chars max) — this is for durable facts, not transcripts. Got {} chars.",
            note.chars().count()
        );
    }
    let Some(category) = canonical_category(&input.category) else {
        bail!(
            "unknown category `{}`. Use one of: preference, goal, avoid, instruction.",
            input.category
        );
    };

    let replacement_needle = input.replaces.as_ref().map(|s| s.trim().to_lowercase());
    let (action, rendered) = crate::config::private_io::read_modify_write_private(
        path,
        |current| {
            let raw = match current {
                None => String::new(),
                Some(bytes) => std::str::from_utf8(bytes)
                    .map_err(|error| anyhow::anyhow!("VITALS.md is not valid UTF-8: {error}"))?
                    .to_string(),
            };
            if raw.len() > MAX_VITALS_FILE_BYTES {
                bail!("VITALS.md exceeds {MAX_VITALS_FILE_BYTES} bytes; inspect and consolidate it before updating");
            }
            let (active, mut revisions) = split_history(&raw).map_err(anyhow::Error::msg)?;
            let mut buckets = parse_sections(active);

            // Resolve one fact before changing anything. The read and replacement share the same
            // cross-process `.VITALS.md.lock`, so Canvas and agents cannot
            // erase one another's concurrent edits.
            let mut removed = 0usize;
            if let Some(needle) = replacement_needle.as_deref() {
                if needle.is_empty() {
                    bail!("`replaces` must identify one existing vital entry; omit it when adding a new fact");
                }
                let find = |exact: bool| -> Vec<(usize, usize)> {
                    buckets.iter().enumerate().flat_map(|(bucket, (_, lines))| {
                        lines.iter().enumerate().filter_map(move |(line, text)| {
                            let text = text.to_lowercase();
                            (if exact { text == needle } else { text.contains(needle) })
                                .then_some((bucket, line))
                        })
                    }).collect()
                };
                let exact = find(true);
                let matches = if exact.is_empty() { find(false) } else { exact };
                if matches.len() != 1 {
                    bail!("`replaces` matched {} entries; provide the full text of one existing vital entry. No facts changed", matches.len());
                }
                let (bucket, line) = matches[0];
                let previous_note = buckets[bucket].1.remove(line);
                revisions.push(VitalRevision {
                    corrected_at: chrono::Utc::now().to_rfc3339(),
                    source: "vital_memory_write".into(),
                    previous_category: buckets[bucket].0.into(),
                    previous_note,
                    category: category.into(),
                    note: note.clone(),
                    extra: Default::default(),
                });
                removed = 1;
            }

            let bucket = buckets
                .iter_mut()
                .find(|(key, _)| *key == category)
                .expect("category bucket exists");
            let duplicate = bucket
                .1
                .iter()
                .any(|line| line.eq_ignore_ascii_case(&note));
            if !duplicate {
                bucket.1.push(note.clone());
            }

            if bucket.1.len() > MAX_PER_SECTION {
                bail!(
                    "the `{category}` section is full ({MAX_PER_SECTION} max). Consolidate or supersede an existing entry (use `replaces`) instead of adding more.",
                );
            }
            let total: usize = buckets.iter().map(|(_, lines)| lines.len()).sum();
            if total > MAX_TOTAL {
                bail!(
                    "VITALS.md is at capacity ({MAX_TOTAL} entries). Supersede or remove stale entries (use `replaces`) before adding new ones.",
                );
            }

            let active = render(&buckets);
            let rendered = append_history(&active, &revisions).map_err(anyhow::Error::msg)?;
            let action = if duplicate && removed == 0 {
                format!("already remembered (no change): \"{note}\"")
            } else if removed > 0 {
                format!(
                    "remembered \"{note}\" under {category}; superseded {removed} prior entr{}",
                    if removed == 1 { "y" } else { "ies" }
                )
            } else {
                format!("remembered \"{note}\" under {category}")
            };
            Ok(((action, active), rendered.into_bytes()))
        },
    )
    .map_err(|error| anyhow::anyhow!("failed to update VITALS.md: {error:#}"))?;

    Ok(ToolOutput {
        summary: format!("Vital memory: {action}."),
        content: format!(
            "Vital memory updated ({action}). Active facts are injected into future turns. Correction history is retained in VITALS.md's phoenix-vital-revisions-v1 comment for inspection, outside prompt context.\n\nCurrent active facts:\n{}",
            rendered.trim()
        ),
    })
}

/// The always-injected context block, or None when nothing has been pinned yet
/// (headers-only / missing file → inject nothing).
pub fn context_block() -> Option<String> {
    context_block_at(&vitals_path())
}

fn unavailable_context() -> String {
    "=== VITAL MEMORY ===\nVITAL MEMORY INTEGRITY FAILURE: VITALS.md could not be safely read. Standing instructions may be unavailable and memory updates are blocked. Tell the user the file needs inspection; do not assume it contains no preferences or invent recovered facts.\n=== END VITAL MEMORY ===".into()
}

fn context_block_at(path: &std::path::Path) -> Option<String> {
    let bytes = match crate::config::private_io::read_private_file_limited(
        path,
        MAX_VITALS_FILE_BYTES,
    ) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return None,
        Err(error) => {
            tracing::warn!(path = %path.display(), error = %error, "vital memory is unavailable");
            return Some(unavailable_context());
        }
    };
    let raw = match String::from_utf8(bytes) {
        Ok(raw) => raw,
        Err(error) => {
            tracing::warn!(path = %path.display(), error = %error, "vital memory is not valid UTF-8");
            return Some(unavailable_context());
        }
    };
    let (active, integrity_error) = match split_history(&raw) {
        Ok((active, _)) => (active, None),
        Err(error) => {
            tracing::warn!(path = %path.display(), error = %error, "vital correction history is invalid");
            (
                crate::vital_memory_document::active_prefix(&raw),
                Some(error),
            )
        }
    };
    let buckets = parse_sections(active);
    if integrity_error.is_none() && buckets.iter().all(|(_, lines)| lines.is_empty()) {
        return None;
    }
    let mut body = String::new();
    if integrity_error.is_some() {
        body.push_str("VITAL MEMORY INTEGRITY FAILURE: correction history is damaged or unsupported. The readable active facts below are preserved exactly; history is excluded and memory updates are blocked. Tell the user VITALS.md needs inspection before updating it. Do not invent repairs or infer superseded facts from the damaged history.\n\n");
    }
    for (key, header) in SECTIONS.iter() {
        if let Some((_, lines)) = buckets.iter().find(|(k, _)| k == key) {
            if lines.is_empty() {
                continue;
            }
            body.push_str(header);
            body.push('\n');
            for line in lines {
                body.push_str("- ");
                body.push_str(line);
                body.push('\n');
            }
        }
    }
    Some(format!(
        "=== VITAL MEMORY (always on — the user's durable preferences, goals, and boundaries; honor these without being reminded) ===\n{}=== END VITAL MEMORY ===",
        body
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vital_memory_document::HISTORY_START;

    #[test]
    fn writes_dedups_and_supersedes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("VITALS.md");

        // empty store → nothing injected
        assert!(context_block_at(&path).is_none());

        execute_at(
            &path,
            VitalMemoryWriteInput {
                note: "Prefers concise answers".into(),
                category: "preference".into(),
                replaces: None,
            },
        )
        .unwrap();
        // dup is a no-op
        let r = execute_at(
            &path,
            VitalMemoryWriteInput {
                note: "prefers concise answers".into(),
                category: "preference".into(),
                replaces: None,
            },
        )
        .unwrap();
        assert!(r.summary.to_lowercase().contains("already remembered"));

        execute_at(
            &path,
            VitalMemoryWriteInput {
                note: "Fable refers to video games".into(),
                category: "instruction".into(),
                replaces: None,
            },
        )
        .unwrap();

        execute_at(
            &path,
            VitalMemoryWriteInput {
                note: "Means the AI model Fable 5, NOT video games".into(),
                category: "instruction".into(),
                replaces: Some("video games".into()),
            },
        )
        .unwrap();

        let block = context_block_at(&path).expect("vitals present");
        assert!(block.contains("Prefers concise answers"));
        assert!(block.contains("Fable 5"));
        // exactly one 'concise' bullet (dedup worked)
        assert_eq!(block.matches("Prefers concise answers").count(), 1);
        assert!(block.contains("VITAL MEMORY"));
    }

    fn save(path: &std::path::Path, note: &str, replaces: Option<&str>) -> Result<ToolOutput> {
        execute_at(
            path,
            VitalMemoryWriteInput {
                note: note.into(),
                category: "preference".into(),
                replaces: replaces.map(str::to_string),
            },
        )
    }

    #[test]
    fn corrections_require_one_match_and_fail_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("VITALS.md");
        save(&path, "Prefers tea in the morning", None).unwrap();
        save(&path, "Prefers tea after lunch", None).unwrap();
        let before = std::fs::read(&path).unwrap();
        for needle in ["", "  ", "coffee", "prefers tea"] {
            assert!(
                save(&path, "Prefers coffee", Some(needle)).is_err(),
                "{needle:?}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
        assert!(save(&path, "New fact\n- injected fact", None).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn exact_match_wins_over_overlapping_substrings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("VITALS.md");
        save(&path, "Prefers tea", None).unwrap();
        save(&path, "Prefers tea after lunch", None).unwrap();
        save(&path, "Prefers coffee", Some("PREFERS TEA")).unwrap();
        let context = context_block_at(&path).unwrap();
        assert!(!context.contains("- Prefers tea\n"));
        assert!(context.contains("Prefers tea after lunch"));
        assert!(context.contains("Prefers coffee"));
    }

    #[test]
    fn correction_history_survives_writes_and_stays_out_of_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("VITALS.md");
        save(&path, "OLD_FACT prefers tea -->", None).unwrap();
        save(&path, "Prefers coffee", Some("old_fact")).unwrap();
        save(&path, "Prefers keyboard navigation", None).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let (_, history) = split_history(&raw).unwrap();
        assert_eq!(history.len(), 1);
        let revision = &history[0];
        assert_eq!(revision.previous_note, "OLD_FACT prefers tea -->");
        assert_eq!(revision.previous_category, "preference");
        assert_eq!(revision.note, "Prefers coffee");
        assert_eq!(revision.source, "vital_memory_write");
        chrono::DateTime::parse_from_rfc3339(&revision.corrected_at).unwrap();
        let context = context_block_at(&path).unwrap();
        assert!(!context.contains("OLD_FACT"));
        assert!(!context.contains("phoenix-vital-revisions"));
        assert!(context.contains("Prefers coffee"));
        assert!(context.contains("Prefers keyboard navigation"));
    }

    #[test]
    fn malformed_or_oversized_history_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("VITALS.md");
        for original in [
            format!("# VITALS\n{HISTORY_START}not-json\n-->"),
            "x".repeat(MAX_VITALS_FILE_BYTES + 1),
        ] {
            std::fs::write(&path, &original).unwrap();
            assert!(context_block_at(&path)
                .unwrap()
                .contains("VITAL MEMORY INTEGRITY FAILURE"));
            assert!(save(&path, "Prefers coffee", None).is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        }
    }

    #[test]
    fn literal_history_marker_notes_and_legacy_facts_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("VITALS.md");
        let legacy = "## Preferences\n- Old note mentions <!-- phoenix-vital-revisions-v1 literally\n- Later fact must survive\n\n## Always do\n- Ask before sending messages\n";
        std::fs::write(&path, legacy).unwrap();
        let (active, history) = split_history(legacy).unwrap();
        assert_eq!(active, legacy);
        assert!(history.is_empty());
        let note = "<!-- phoenix-vital-revisions-v1";
        save(&path, note, None).unwrap();
        let context = context_block_at(&path).unwrap();
        assert!(context.contains(&format!("- {note}\n")));
        assert!(context.contains("Later fact must survive"));
        assert!(context.contains("Ask before sending messages"));
        assert!(!context.contains("INTEGRITY FAILURE"));
        save(&path, "Updated literal-marker fact", Some(note)).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let (active, history) = split_history(&raw).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].previous_note, note);
        assert!(active.contains("Old note mentions <!-- phoenix-vital-revisions-v1 literally"));
        assert!(active.contains("Later fact must survive"));
        assert!(!active.lines().any(|line| line == format!("- {note}")));
        assert!(!context_block_at(&path)
            .unwrap()
            .contains("INTEGRITY FAILURE"));
    }

    #[test]
    fn damaged_history_preserves_active_instructions_and_reports_integrity_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("VITALS.md");
        for history in [
            format!("{HISTORY_START}not-json\n-->"),
            format!("{HISTORY_START}{{\n## Preferences\n- HISTORY_IS_NOT_ACTIVE"),
            "<!-- phoenix-vital-revisions-v9\n[]\n-->".into(),
            format!("{HISTORY_START}[]\n-->\n## Preferences\n- UNTRUSTED_SUFFIX"),
        ] {
            let original = format!("## Always do\n- Ask before sending messages\n\n{history}");
            std::fs::write(&path, &original).unwrap();
            let context = context_block_at(&path).unwrap();
            assert!(context.contains("Ask before sending messages"));
            assert!(context.contains("VITAL MEMORY INTEGRITY FAILURE"));
            assert!(context.contains("Tell the user"));
            assert!(!context.contains("HISTORY_IS_NOT_ACTIVE"));
            assert!(!context.contains("UNTRUSTED_SUFFIX"));
            assert!(save(&path, "New fact", None).is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        }
    }

    #[test]
    fn competing_corrections_cannot_leave_both_facts_active() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("VITALS.md");
        save(&path, "Old preference", None).unwrap();
        let results = std::thread::scope(|scope| {
            let a = scope.spawn(|| save(&path, "New preference A", Some("Old preference")));
            let b = scope.spawn(|| save(&path, "New preference B", Some("Old preference")));
            [a.join().unwrap().is_ok(), b.join().unwrap().is_ok()]
        });
        assert_eq!(results.iter().filter(|ok| **ok).count(), 1);
        let context = context_block_at(&path).unwrap();
        assert!(!context.contains("Old preference"));
        assert_ne!(
            context.contains("New preference A"),
            context.contains("New preference B")
        );
        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(split_history(&raw).unwrap().1.len(), 1);
    }

    #[test]
    fn rejects_unknown_category_and_long_notes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("VITALS.md");
        assert!(execute_at(
            &path,
            VitalMemoryWriteInput {
                note: "x".into(),
                category: "banana".into(),
                replaces: None,
            }
        )
        .is_err());
        assert!(execute_at(
            &path,
            VitalMemoryWriteInput {
                note: "a".repeat(MAX_NOTE_CHARS + 1),
                category: "goal".into(),
                replaces: None,
            }
        )
        .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_writers_preserve_both_updates_and_private_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("VITALS.md");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        std::thread::scope(|scope| {
            for note in ["Prefers dark mode", "Prefers keyboard navigation"] {
                let path = path.clone();
                let barrier = barrier.clone();
                scope.spawn(move || {
                    barrier.wait();
                    execute_at(
                        &path,
                        VitalMemoryWriteInput {
                            note: note.to_string(),
                            category: "preference".to_string(),
                            replaces: None,
                        },
                    )
                    .unwrap();
                });
            }
            barrier.wait();
        });

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("Prefers dark mode"));
        assert!(raw.contains("Prefers keyboard navigation"));
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn unreadable_existing_payload_fails_closed_without_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("VITALS.md");
        let original = b"\xff\xfeexisting private state";
        std::fs::write(&path, original).unwrap();
        assert!(context_block_at(&path)
            .unwrap()
            .contains("Standing instructions may be unavailable"));
        let result = execute_at(
            &path,
            VitalMemoryWriteInput {
                note: "must not replace the file".to_string(),
                category: "instruction".to_string(),
                replaces: None,
            },
        );
        assert!(result.is_err());
        assert_eq!(std::fs::read(path).unwrap(), original);
    }

    /// Regression guard for the warmer-style preference: when VITALS.md carries
    /// the "warmer, more human, conversational style" bullet, context_block_at
    /// must surface it verbatim in the always-injected block so every turn's
    /// context honors it.
    #[test]
    fn warmer_style_preference_is_injected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("VITALS.md");

        // empty store → nothing injected yet
        assert!(context_block_at(&path).is_none());

        let warmer_note =
            "Phoenix should communicate in a warmer, more human, conversational style with \
             natural paragraphs; avoid robotic bullet-point-only technical replies unless \
             bullets genuinely help.";
        execute_at(
            &path,
            VitalMemoryWriteInput {
                note: warmer_note.into(),
                category: "preference".into(),
                replaces: None,
            },
        )
        .unwrap();

        let block = context_block_at(&path).expect("vitals present");
        // The full preference text rides in the injected block verbatim.
        assert!(block.contains(warmer_note));
        // It lands under the Preferences header, not Goals/Do NOT/Always do.
        assert!(block.contains("## Preferences"));
        // The always-on wrapper is present so the runtime treats it as vital.
        assert!(block.contains("VITAL MEMORY"));
        assert!(block.contains("END VITAL MEMORY"));
        // Exactly one occurrence — no accidental duplication.
        assert_eq!(block.matches("warmer, more human").count(), 1);
    }
}
