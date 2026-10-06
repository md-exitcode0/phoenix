//! Elena — durable knowledge, documents, reports, and presentations.

use super::framework::{specialist_spec, SubAgentConfig};
use crate::runtime::ExecutionStyle;
use crate::session::SubAgentType;

pub const SYSTEM_PROMPT: &str = include_str!("../../prompts/presentation_system.md");

pub fn presentation_config() -> SubAgentConfig {
    SubAgentConfig {
        spec: specialist_spec(
            SubAgentType::Presentation,
            SYSTEM_PROMPT,
            "claude-sonnet-4-6",
            vec![
                "design_reference",
                "read",
                "write",
                "str_replace",
                "grep",
                "glob",
                "list_directory",
                "bash",
                "ui_snap",
                "image_analyze",
                // Composio: pull deck substance from connected sources (Sheets,
                // Notion, Drive) — composio_run over copy-paste scraping.
                "composio_search",
                "composio_schemas",
                "composio_run",
                "composio_connections",
                // MCP: every agent can reach the user's connected MCP
                // servers. Discovery is route-filtered, so each agent only
                // ever sees the servers meant for its lane.
                "mcp_servers",
                "mcp_call",
                "todo_write",
                "recall",
                "skill",
                "skill_search",
                "skill_install",
                "talk",
                "final_answer",
            ],
            ExecutionStyle::DesignIteration,
            "knowledge and documents result",
            vec!["deliverable path", "source map", "render or export verification"],
            vec![
                "Keep durable company knowledge findable and create polished deliverables from provided substance.",
                "Do not invent facts or assets.",
                "Name artifact paths clearly and store incidental Markdown under Phoenix artifacts, not across the workspace.",
            ],
        ),
    }
}
