//! Shared VITALS correction-history format for the runtime and Canvas writers.
//! History is inspected and validated separately from the active Markdown facts.

use serde::{Deserialize, Serialize};

pub const MAX_FILE_BYTES: usize = 64 * 1024;
pub const HISTORY_START: &str = "<!-- phoenix-vital-revisions-v1\n";
const HISTORY_PREFIX: &str = "<!-- phoenix-vital-revisions";
const HISTORY_END: &str = "\n-->";

/// Tool provenance, not a claim that the fact was independently verified.
/// Unknown fields survive a read/write round trip rather than being discarded.
#[derive(Debug, Deserialize, Serialize)]
pub struct VitalRevision {
    pub corrected_at: String,
    pub source: String,
    pub previous_category: String,
    pub previous_note: String,
    pub category: String,
    pub note: String,
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
}

/// Only a marker at the start of a line begins history. Active one-line notes
/// are bullets and may mention the marker literally without becoming history.
/// Even when history is damaged, never parse its tail as standing instructions.
pub fn active_prefix(raw: &str) -> &str {
    raw.match_indices(HISTORY_PREFIX)
        .find(|(offset, _)| *offset == 0 || raw.as_bytes()[offset - 1] == b'\n')
        .map_or(raw, |(offset, _)| &raw[..offset])
}

pub fn split_history(raw: &str) -> Result<(&str, Vec<VitalRevision>), String> {
    let active = active_prefix(raw);
    if active.len() == raw.len() {
        return Ok((raw, Vec::new()));
    }
    let tail = raw[active.len()..]
        .strip_prefix(HISTORY_START)
        .ok_or_else(|| {
            "unsupported or damaged vital correction history marker; inspect VITALS.md".to_string()
        })?;
    let (json, suffix) = tail.split_once(HISTORY_END).ok_or_else(|| {
        "vital correction history is incomplete; inspect VITALS.md before updating it".to_string()
    })?;
    if !suffix.trim().is_empty() {
        return Err("unexpected content after vital correction history; inspect VITALS.md before updating it".into());
    }
    let revisions: Vec<VitalRevision> = serde_json::from_str(json)
        .map_err(|error| format!("invalid vital correction history: {error}; inspect VITALS.md"))?;
    validate_revisions(&revisions)?;
    Ok((active, revisions))
}

fn validate_revisions(revisions: &[VitalRevision]) -> Result<(), String> {
    for revision in revisions {
        let category_valid =
            |category: &str| matches!(category, "preference" | "goal" | "avoid" | "instruction");
        let note_valid = |note: &str| {
            !note.trim().is_empty() && !note.contains(['\n', '\r']) && note.chars().count() <= 240
        };
        if chrono::DateTime::parse_from_rfc3339(&revision.corrected_at).is_err()
            || revision.source.trim().is_empty()
            || !category_valid(&revision.previous_category)
            || !category_valid(&revision.category)
            || !note_valid(&revision.previous_note)
            || !note_valid(&revision.note)
        {
            return Err(
                "invalid vital correction revision fields; inspect VITALS.md before updating it"
                    .into(),
            );
        }
    }
    Ok(())
}

pub fn append_history(active: &str, revisions: &[VitalRevision]) -> Result<String, String> {
    // A new revision can contain a legacy fact that predates today's limits.
    // Reject it before publication, rather than creating unreadable history.
    validate_revisions(revisions)?;
    let mut rendered = active.to_string();
    if !revisions.is_empty() {
        let json = serde_json::to_string(revisions)
            .map_err(|error| error.to_string())?
            .replace('<', "\\u003c")
            .replace('>', "\\u003e");
        rendered.push_str(HISTORY_START);
        rendered.push_str(&json);
        rendered.push_str(HISTORY_END);
        rendered.push('\n');
    }
    if rendered.len() > MAX_FILE_BYTES {
        return Err(format!("vital memory and correction history exceed {MAX_FILE_BYTES} bytes; inspect and consolidate VITALS.md before updating. No facts changed"));
    }
    Ok(rendered)
}
