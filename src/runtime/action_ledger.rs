//! Private structured audit trail for runtime actions.
//!
//! The ledger deliberately stores summaries and hashes, never raw tool input
//! or output. Start and finish are separate records so a crash leaves an
//! honest unclosed action rather than silently erasing the attempt.

use serde::{Deserialize, Serialize};

use super::extensions::{ActionContext, ExtensionEvaluation};

const SCHEMA_VERSION: u32 = 1;
const LEDGER_MAX_BYTES: u64 = 50 * 1024 * 1024;
const SUMMARY_MAX_CHARS: usize = 512;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionPhase {
    Started,
    Finished,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionLedgerRecord {
    pub schema_version: u32,
    pub event_id: String,
    pub phase: ActionPhase,
    pub timestamp: String,
    pub session_id: Option<String>,
    pub agent_id: String,
    pub tool_name: String,
    pub category: String,
    pub permission_mode: String,
    pub interaction_mode: String,
    pub review_mode: String,
    pub workspace_sha256: String,
    pub input_sha256: String,
    pub input_summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extension_evaluations: Vec<ExtensionEvaluation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_confirmed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_summary: Option<String>,
}

pub fn record_started(
    context: &ActionContext,
    evaluations: &[ExtensionEvaluation],
) -> anyhow::Result<()> {
    append(&ActionLedgerRecord {
        schema_version: SCHEMA_VERSION,
        event_id: context.event_id.clone(),
        phase: ActionPhase::Started,
        timestamp: context.started_at.clone(),
        session_id: context.session_id.clone(),
        agent_id: context.agent_id.clone(),
        tool_name: context.tool_name.clone(),
        category: context.category.clone(),
        permission_mode: context.permission_mode.clone(),
        interaction_mode: context.interaction_mode.clone(),
        review_mode: context.review_mode.clone(),
        workspace_sha256: context.workspace_sha256.clone(),
        input_sha256: context.input_sha256.clone(),
        input_summary: redact_summary(&context.input_summary),
        extension_evaluations: evaluations.to_vec(),
        success: None,
        execution_confirmed: None,
        elapsed_ms: None,
        output_sha256: None,
        output_summary: None,
    })
}

pub fn record_finished(
    context: &ActionContext,
    evaluations: &[ExtensionEvaluation],
    success: bool,
    execution_confirmed: bool,
    elapsed_ms: u64,
    output_sha256: String,
    output_summary: &str,
) -> anyhow::Result<()> {
    append(&ActionLedgerRecord {
        schema_version: SCHEMA_VERSION,
        event_id: context.event_id.clone(),
        phase: ActionPhase::Finished,
        timestamp: chrono::Utc::now().to_rfc3339(),
        session_id: context.session_id.clone(),
        agent_id: context.agent_id.clone(),
        tool_name: context.tool_name.clone(),
        category: context.category.clone(),
        permission_mode: context.permission_mode.clone(),
        interaction_mode: context.interaction_mode.clone(),
        review_mode: context.review_mode.clone(),
        workspace_sha256: context.workspace_sha256.clone(),
        input_sha256: context.input_sha256.clone(),
        input_summary: redact_summary(&context.input_summary),
        extension_evaluations: evaluations.to_vec(),
        success: Some(success),
        execution_confirmed: Some(execution_confirmed),
        elapsed_ms: Some(elapsed_ms),
        output_sha256: Some(output_sha256),
        output_summary: Some(redact_summary(output_summary)),
    })
}

fn append(record: &ActionLedgerRecord) -> anyhow::Result<()> {
    if crate::config::test_isolated_from_live_home() {
        return Ok(());
    }
    let path = crate::config::phoenix_home().join("audit/actions.jsonl");
    let mut rendered = serde_json::to_vec(record)?;
    rendered.push(b'\n');
    super::append_private_rotating_log_at(&path, &rendered, LEDGER_MAX_BYTES)?;
    Ok(())
}

pub fn ledger_path() -> std::path::PathBuf {
    crate::config::phoenix_home().join("audit/actions.jsonl")
}

pub fn recent(limit: usize) -> anyhow::Result<Vec<ActionLedgerRecord>> {
    if crate::config::test_isolated_from_live_home() {
        return Ok(Vec::new());
    }
    let limit = limit.clamp(1, 500);
    let Some(bytes) = crate::config::private_io::read_private_file_limited(
        &ledger_path(),
        LEDGER_MAX_BYTES as usize,
    )?
    else {
        return Ok(Vec::new());
    };
    let text = std::str::from_utf8(&bytes)?;
    let mut rows = text
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<ActionLedgerRecord>(line).ok())
        .take(limit)
        .collect::<Vec<_>>();
    rows.reverse();
    Ok(rows)
}

fn redact_summary(value: &str) -> String {
    let mut out = Vec::new();
    let mut redact_next = false;
    for token in value.split_whitespace().take(160) {
        let lower = token
            .trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_' && ch != '-')
            .to_ascii_lowercase();
        let looks_secret = ["ck_", "sk-", "sk_", "ghp_", "github_pat_", "xoxb-", "xoxp-"]
            .iter()
            .any(|prefix| lower.starts_with(prefix))
            || (lower.len() >= 36
                && lower
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
                && lower.chars().any(|ch| ch.is_ascii_alphabetic())
                && lower.chars().any(|ch| ch.is_ascii_digit()));
        if redact_next || looks_secret {
            out.push("[redacted]");
            redact_next = false;
            continue;
        }
        out.push(token);
        redact_next = matches!(
            lower.as_str(),
            "authorization" | "bearer" | "token" | "password" | "secret" | "api_key" | "apikey"
        );
    }
    let rendered = out.join(" ");
    if rendered.chars().count() <= SUMMARY_MAX_CHARS {
        rendered
    } else {
        format!(
            "{}…",
            rendered.chars().take(SUMMARY_MAX_CHARS).collect::<String>()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_redact_known_and_opaque_secrets() {
        let rendered = redact_summary("token ck_live_abcdefgh password hunter2 github_pat_abcdef123456789012345678901234567890");
        assert!(!rendered.contains("ck_live"));
        assert!(!rendered.contains("hunter2"));
        assert!(!rendered.contains("github_pat"));
        assert!(rendered.contains("[redacted]"));
    }

    #[test]
    fn ledger_persists_start_and_finish_without_raw_secrets() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let context = crate::runtime::extensions::ActionContext::new(
            Some("session-a".into()),
            "iris".into(),
            "composio_run".into(),
            "token ck_live_never-store-this send figma".into(),
            serde_json::json!({"token":"ck_live_never-store-this","action":"send"}),
            root.path(),
            "workspace".into(),
            "execute".into(),
            "shadow".into(),
        );
        let evaluations = vec![crate::runtime::extensions::ExtensionEvaluation {
            extension: "test".into(),
            verdict: crate::runtime::extensions::ExtensionVerdict::Observe,
            code: "test_observation".into(),
            reason: "test only".into(),
        }];
        record_started(&context, &evaluations).unwrap();
        record_finished(
            &context,
            &evaluations,
            true,
            true,
            12,
            "abc".into(),
            "Bearer another-secret completed",
        )
        .unwrap();
        let rows = recent(10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].phase, ActionPhase::Started);
        assert_eq!(rows[1].phase, ActionPhase::Finished);
        assert_eq!(rows[1].success, Some(true));
        let raw = std::fs::read_to_string(ledger_path()).unwrap();
        assert!(!raw.contains("never-store-this"));
        assert!(!raw.contains("another-secret"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(ledger_path())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}
