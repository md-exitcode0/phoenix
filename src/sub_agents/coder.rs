//! Coder agent - steal from swe-agent + kilocode

use anyhow::Context;
use serde::{Deserialize, Serialize};

use super::framework::{specialist_spec, SubAgentConfig};
use crate::runtime::{AgentArtifact, ArtifactKind, ExecutionStyle, ToolCallResult};
use crate::session::SubAgentType;

pub const SYSTEM_PROMPT: &str = include_str!("../../prompts/coder_system.md");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoderToolCall {
    pub name: String,
    pub input_summary: String,
    pub outcome: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoderTurnResult {
    pub summary: String,
    pub final_markdown: String,
    pub changes_made: Vec<String>,
    pub verification: Vec<String>,
    pub execution_mode: String,
    pub tool_transcript: Vec<CoderToolCall>,
}

impl CoderTurnResult {
    pub fn to_artifacts(&self) -> Vec<AgentArtifact> {
        let changes = if self.changes_made.is_empty() {
            "No changes recorded.".to_string()
        } else {
            self.changes_made
                .iter()
                .map(|item| format!("- {item}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let verification = if self.verification.is_empty() {
            "No verification recorded.".to_string()
        } else {
            self.verification
                .iter()
                .map(|item| format!("- {item}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let transcript = if self.tool_transcript.is_empty() {
            format!(
                "Execution mode: {}\n- No tool calls were executed in this turn.",
                self.execution_mode
            )
        } else {
            let entries = self
                .tool_transcript
                .iter()
                .map(|call| {
                    format!(
                        "- {} | input: {} | outcome: {}",
                        call.name, call.input_summary, call.outcome
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("Execution mode: {}\n{}", self.execution_mode, entries)
        };

        vec![
            AgentArtifact {
                kind: ArtifactKind::Reply,
                title: "Coder reply".to_string(),
                body: self.final_markdown.clone(),
            },
            AgentArtifact {
                kind: ArtifactKind::Plan,
                title: "Coder summary".to_string(),
                body: self.summary.clone(),
            },
            AgentArtifact {
                kind: ArtifactKind::FilePatch,
                title: "Changes made".to_string(),
                body: changes,
            },
            AgentArtifact {
                kind: ArtifactKind::Finding,
                title: "Verification".to_string(),
                body: verification,
            },
            AgentArtifact {
                kind: ArtifactKind::ToolTranscript,
                title: "Execution transcript".to_string(),
                body: transcript,
            },
        ]
    }

    pub fn to_tool_results(&self) -> Vec<ToolCallResult> {
        if self.tool_transcript.is_empty() {
            return vec![ToolCallResult {
                tool_name: "scaffold_execution".to_string(),
                input_summary: self.execution_mode.clone(),
                success: true,
                output: format!(
                    "Coder completed a {} turn without real tool calls.",
                    self.execution_mode
                ),
            }];
        }

        self.tool_transcript
            .iter()
            .map(|call| ToolCallResult {
                tool_name: call.name.clone(),
                input_summary: call.input_summary.clone(),
                success: true,
                output: call.outcome.clone(),
            })
            .collect()
    }
}

pub fn parse_coder_turn_result(content: &str) -> anyhow::Result<CoderTurnResult> {
    serde_json::from_str(content).context("failed to parse coder turn result JSON")
}

pub fn coder_config() -> SubAgentConfig {
    SubAgentConfig {
        spec: specialist_spec(
            SubAgentType::Coder,
            SYSTEM_PROMPT,
            "claude-sonnet-4-6",
            vec![
                "read",
                "write",
                "str_replace",
                "grep",
                "codebase_search",
                "symbol_search",
                "callers",
                "callees",
                "impact",
                "file_symbols",
                "call_path",
                "glob",
                "list_directory",
                "bash",
                "ui_snap",
                "todo_write",
                "recall",
                "skill",
                "skill_install",
                "skill_search",
                "composio_search",
                "composio_schemas",
                "composio_run",
                "composio_connections",
                // MCP: every agent can reach the user's connected MCP
                // servers. Discovery is route-filtered, so each agent only
                // ever sees the servers meant for its lane.
                "mcp_servers",
                "mcp_call",
                "talk",
                "final_answer",
            ],
            ExecutionStyle::CodeExecution,
            "repository outcome",
            vec!["Answer the assigned question or report the completed change, with supporting evidence and verification limits. Omit inapplicable plan, diff, and test sections."],
            vec![
                "Inspect code before changing it.",
                "Use specialized code-navigation tools before raw shell output floods context.",
                "Return the result to the actual requester; group contributions belong in the group, not an obligatory Orchestrator relay.",
            ],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn investigation_contract_does_not_require_a_diff_or_orchestrator_relay() {
        let spec = coder_config().spec;
        let contract = format!("{}\n{}", spec.workflow.notes.join("\n"), spec.output.required_artifacts.join("\n"));
        assert!(!contract.contains("Talk back to Orchestrator"));
        assert!(contract.contains("actual requester"));
        assert!(contract.contains("Omit inapplicable plan, diff, and test sections"));
    }

    #[test]
    fn permanent_prompt_injects_phoenix_lean_build_and_index_lifecycle() {
        assert!(SYSTEM_PROMPT.contains("# Phoenix Lean Build Ladder"));
        assert!(SYSTEM_PROMPT.contains("Does this codebase already have"));
        assert!(SYSTEM_PROMPT.contains("Indexing codebase"));
        assert!(SYSTEM_PROMPT.contains("Exploring codebase"));
        assert!(SYSTEM_PROMPT.contains("once when the coding task finishes"));
        assert!(SYSTEM_PROMPT.contains("Finish where the work belongs"));
        assert!(SYSTEM_PROMPT.contains("use `final_answer` and report directly to the user"));
        assert!(!SYSTEM_PROMPT.contains("Run the Ponytail"));
        assert!(!SYSTEM_PROMPT.contains("`ponytail:`"));
    }

    #[test]
    fn parses_coder_turn_result_json() {
        let json = serde_json::json!({
            "summary": "Implemented the runtime contract.",
            "final_markdown": "## Result\nImplemented the runtime contract.",
            "changes_made": ["Added parser", "Updated runner"],
            "verification": ["cargo test"],
            "execution_mode": "scaffold_no_tools",
            "tool_transcript": []
        })
        .to_string();

        let parsed = parse_coder_turn_result(&json).unwrap();
        assert_eq!(parsed.execution_mode, "scaffold_no_tools");
        assert_eq!(parsed.changes_made.len(), 2);
        assert!(parsed.final_markdown.contains("## Result"));
    }

    #[test]
    fn turns_coder_result_into_runtime_artifacts() {
        let parsed = CoderTurnResult {
            summary: "Implemented parser".to_string(),
            final_markdown: "## Done\nImplemented parser".to_string(),
            changes_made: vec!["Added parser".to_string()],
            verification: vec!["cargo test".to_string()],
            execution_mode: "scaffold_no_tools".to_string(),
            tool_transcript: vec![],
        };

        let artifacts = parsed.to_artifacts();
        assert_eq!(artifacts.len(), 5);
        assert!(artifacts[0].body.contains("## Done"));
        assert!(artifacts[4].body.contains("No tool calls were executed"));
    }

    #[test]
    fn preserves_real_tool_transcript_entries() {
        let parsed = CoderTurnResult {
            summary: "Ran a real tool-backed turn".to_string(),
            final_markdown: "## Done\nTool-backed turn".to_string(),
            changes_made: vec!["Updated runtime".to_string()],
            verification: vec!["cargo test".to_string()],
            execution_mode: "tool_backed".to_string(),
            tool_transcript: vec![CoderToolCall {
                name: "read".to_string(),
                input_summary: "src/main.rs".to_string(),
                outcome: "Read file successfully.".to_string(),
            }],
        };

        let artifacts = parsed.to_artifacts();
        let tool_results = parsed.to_tool_results();

        assert!(artifacts[4].body.contains("Execution mode: tool_backed"));
        assert!(artifacts[4].body.contains("read | input: src/main.rs"));
        assert_eq!(tool_results.len(), 1);
        assert_eq!(tool_results[0].tool_name, "read");
        assert_eq!(tool_results[0].output, "Read file successfully.");
    }

    #[test]
    fn rejects_invalid_coder_turn_result_json() {
        let error = parse_coder_turn_result("not json").unwrap_err();
        assert!(error
            .to_string()
            .contains("failed to parse coder turn result JSON"));
    }
}
