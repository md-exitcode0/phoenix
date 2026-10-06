//! Remy — systems, reliability, security, and independent review.

use super::framework::{specialist_spec, SubAgentConfig};
use crate::runtime::ExecutionStyle;
use crate::session::SubAgentType;

pub const SYSTEM_PROMPT: &str = include_str!("../../prompts/critic_system.md");

pub fn critic_config() -> SubAgentConfig {
    SubAgentConfig {
        spec: specialist_spec(
            SubAgentType::Critic,
            SYSTEM_PROMPT,
            "claude-sonnet-4-6",
            vec![
                "read",
                "grep",
                "glob",
                "list_directory",
                "symbol_search",
                "file_symbols",
                "callers",
                // Composio: read-only discovery only. Critic verifies claims
                // against connected services; it does not execute side effects.
                "composio_search",
                "composio_connections",
                // MCP: every agent can reach the user's connected MCP
                // servers. Discovery is route-filtered, so each agent only
                // ever sees the servers meant for its lane.
                "mcp_servers",
                "mcp_call",
                "todo_write",
                "recall",
                "talk",
                "final_answer",
            ],
            ExecutionStyle::AdversarialReview,
            "systems reliability result",
            vec!["verdict or incident state", "ranked findings with evidence", "recovery verification and residual risk"],
            vec![
                "Own reliability and security outcomes; find what is wrong and prove it.",
                "Trace repeated failures into durable incident learning and severity-rank findings.",
                "Verify recovery; if no issues are found, say so with residual risk.",
            ],
        ),
    }
}
