//! Runtime extension hooks and the adaptive action firewall.
//!
//! Hooks run at the single shared tool boundary, so mesh, legacy provider, CLI,
//! and future remote runners cannot silently drift into different policy and
//! audit behavior. Extensions receive raw input only in memory; the durable
//! ledger stores redacted summaries and content hashes.

use std::sync::{Arc, OnceLock, RwLock};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub struct ActionContext {
    pub event_id: String,
    pub started_at: String,
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
    pub input: serde_json::Value,
}

impl ActionContext {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: Option<String>,
        agent_id: String,
        tool_name: String,
        input_summary: String,
        input: serde_json::Value,
        workspace: &std::path::Path,
        permission_mode: String,
        interaction_mode: String,
        review_mode: String,
    ) -> Self {
        let input_bytes = serde_json::to_vec(&input).unwrap_or_default();
        Self {
            event_id: uuid::Uuid::new_v4().to_string(),
            started_at: chrono::Utc::now().to_rfc3339(),
            session_id,
            agent_id,
            category: action_category(&tool_name).to_string(),
            tool_name,
            permission_mode,
            interaction_mode,
            review_mode,
            workspace_sha256: sha256(workspace.to_string_lossy().as_bytes()),
            input_sha256: sha256(&input_bytes),
            input_summary,
            input,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionVerdict {
    Allow,
    Observe,
    Block,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionEvaluation {
    pub extension: String,
    pub verdict: ExtensionVerdict,
    pub code: String,
    pub reason: String,
}

pub trait RuntimeExtension: Send + Sync {
    fn id(&self) -> &'static str;
    fn before_action(&self, context: &ActionContext) -> ExtensionEvaluation;
    fn after_action(&self, _context: &ActionContext, _outcome: &ActionOutcome) {}
}

#[derive(Debug, Clone)]
pub struct ActionOutcome {
    pub success: bool,
    pub execution_confirmed: bool,
    pub elapsed_ms: u64,
    pub output_sha256: String,
    pub output_summary: String,
}

pub struct ActionReceipt {
    pub context: ActionContext,
    pub evaluations: Vec<ExtensionEvaluation>,
    started: std::time::Instant,
}

impl ActionReceipt {
    pub fn block_reason(&self) -> Option<String> {
        self.evaluations
            .iter()
            .find(|evaluation| evaluation.verdict == ExtensionVerdict::Block)
            .map(|evaluation| {
                format!(
                    "Action firewall blocked `{}` before execution ({}): {}",
                    self.context.tool_name, evaluation.code, evaluation.reason
                )
            })
    }
}

static EXTENSIONS: OnceLock<RwLock<Vec<Arc<dyn RuntimeExtension>>>> = OnceLock::new();

fn extensions() -> &'static RwLock<Vec<Arc<dyn RuntimeExtension>>> {
    EXTENSIONS.get_or_init(|| RwLock::new(vec![Arc::new(ActionFirewall)]))
}

/// Register a process-local extension. Duplicate ids are replaced atomically.
/// This is the stable hook used by future sandboxes, remote runners, and local
/// product extensions; built-ins go through the same interface.
pub fn register(extension: Arc<dyn RuntimeExtension>) {
    let mut rows = extensions()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    rows.retain(|row| row.id() != extension.id());
    rows.push(extension);
}

pub fn begin_action(context: ActionContext) -> ActionReceipt {
    let evaluations = extensions()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .map(|extension| extension.before_action(&context))
        .collect::<Vec<_>>();
    if let Err(error) = super::action_ledger::record_started(&context, &evaluations) {
        tracing::warn!("action ledger start append failed: {error:#}");
    }
    ActionReceipt {
        context,
        evaluations,
        started: std::time::Instant::now(),
    }
}

pub fn finish_action(receipt: ActionReceipt, success: bool, confirmed: bool, output: &str) {
    let outcome = ActionOutcome {
        success,
        execution_confirmed: confirmed,
        elapsed_ms: receipt.started.elapsed().as_millis() as u64,
        output_sha256: sha256(output.as_bytes()),
        output_summary: output.lines().next().unwrap_or_default().to_string(),
    };
    for extension in extensions()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
    {
        extension.after_action(&receipt.context, &outcome);
    }
    if let Err(error) = super::action_ledger::record_finished(
        &receipt.context,
        &receipt.evaluations,
        outcome.success,
        outcome.execution_confirmed,
        outcome.elapsed_ms,
        outcome.output_sha256,
        &outcome.output_summary,
    ) {
        tracing::warn!("action ledger finish append failed: {error:#}");
    }
}

struct ActionFirewall;

impl RuntimeExtension for ActionFirewall {
    fn id(&self) -> &'static str {
        "action_firewall"
    }

    fn before_action(&self, context: &ActionContext) -> ExtensionEvaluation {
        let mode = context.review_mode.trim().to_ascii_lowercase();
        if mode == "off" {
            return allow(
                self.id(),
                "review_disabled",
                "Adaptive action review is disabled.",
            );
        }
        let Some((severity, code, reason)) = classify_risk(context) else {
            return allow(
                self.id(),
                "no_anomaly",
                "No additional action anomaly was detected.",
            );
        };
        let enforce = mode == "enforce" && severity == "high";
        ExtensionEvaluation {
            extension: self.id().to_string(),
            verdict: if enforce {
                ExtensionVerdict::Block
            } else {
                ExtensionVerdict::Observe
            },
            code: code.to_string(),
            reason: if enforce {
                reason.to_string()
            } else {
                format!("shadow {severity}-risk observation: {reason}")
            },
        }
    }
}

fn classify_risk(context: &ActionContext) -> Option<(&'static str, &'static str, &'static str)> {
    let summary = context.input_summary.to_ascii_lowercase();
    if context.tool_name == "bash"
        && [
            "rm -rf /",
            "mkfs.",
            "shutdown -h",
            "shutdown now",
            "reboot",
            ":(){:|:&};:",
            "dd if=/dev/zero of=/dev/",
        ]
        .iter()
        .any(|needle| summary.contains(needle))
    {
        return Some((
            "high",
            "broad_destructive_shell",
            "the command resembles a machine-wide destructive operation",
        ));
    }
    if matches!(
        context.tool_name.as_str(),
        "bash" | "computer_click" | "computer_type"
    ) && context.permission_mode == "full_access"
    {
        return Some((
            "medium",
            "unstructured_full_access",
            "an unstructured computer action is running with Full Access",
        ));
    }
    if context.tool_name.starts_with("mcp_") || context.tool_name.starts_with("composio_") {
        let serialized = context.input.to_string().to_ascii_lowercase();
        if ["delete", "remove", "purchase", "publish", "send", "revoke"]
            .iter()
            .any(|term| serialized.contains(term))
        {
            return Some((
                "medium",
                "external_effect_via_extension",
                "an extension call appears capable of an outward or destructive effect",
            ));
        }
    }
    None
}

fn allow(extension: &str, code: &str, reason: &str) -> ExtensionEvaluation {
    ExtensionEvaluation {
        extension: extension.to_string(),
        verdict: ExtensionVerdict::Allow,
        code: code.to_string(),
        reason: reason.to_string(),
    }
}

fn action_category(tool_name: &str) -> &'static str {
    match tool_name {
        "read" | "glob" | "grep" | "list_directory" | "codebase_search" | "symbol_search"
        | "callers" | "callees" | "impact" | "file_symbols" | "call_path" => "workspace_read",
        "write" | "str_replace" | "fast_apply" => "workspace_write",
        "bash" => "shell",
        name if name.starts_with("browser_") => "browser",
        name if name.starts_with("computer_") => "computer",
        name if name.starts_with("mcp_") || name.starts_with("composio_") => "extension",
        "talk" | "work" | "volume_work" | "create_agent" | "agent_provision" => "collaboration",
        "memory_recall" | "memory_save" | "recall" | "vital_memory_write" => "memory",
        "credential_generate" | "credential_list" | "browser_input_credential" | "ask_for_pass" | "pass_use" => "credential",
        _ => "tool",
    }
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(tool: &str, summary: &str, mode: &str) -> ActionContext {
        ActionContext::new(
            Some("session".into()),
            "phoenix".into(),
            tool.into(),
            summary.into(),
            serde_json::json!({"command": summary}),
            std::path::Path::new("/tmp/workspace"),
            "full_access".into(),
            "execute".into(),
            mode.into(),
        )
    }

    #[test]
    fn firewall_observes_in_shadow_and_blocks_only_high_risk_in_enforce() {
        let firewall = ActionFirewall;
        let shadow = firewall.before_action(&context("bash", "rm -rf /", "shadow"));
        assert_eq!(shadow.verdict, ExtensionVerdict::Observe);
        let enforce = firewall.before_action(&context("bash", "rm -rf /", "enforce"));
        assert_eq!(enforce.verdict, ExtensionVerdict::Block);
        let ordinary = firewall.before_action(&context("read", "README.md", "enforce"));
        assert_eq!(ordinary.verdict, ExtensionVerdict::Allow);
    }
}
