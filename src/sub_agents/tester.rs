//! Tester agent.

use super::framework::{specialist_spec, SubAgentConfig};
use crate::runtime::ExecutionStyle;
use crate::session::SubAgentType;

pub const SYSTEM_PROMPT: &str = include_str!("../../prompts/tester_system.md");

pub fn tester_config() -> SubAgentConfig {
    SubAgentConfig {
        spec: specialist_spec(
            SubAgentType::Tester,
            SYSTEM_PROMPT,
            "claude-sonnet-4-6",
            vec![
                "read",
                "image_analyze",
                "write",
                "str_replace",
                "grep",
                "glob",
                "list_directory",
                "bash",
                // Composio: connected test/issue toolkits (Linear, Jira, CI) —
                // file or query via composio_run instead of the UI.
                "composio_search",
                "composio_schemas",
                "composio_run",
                "composio_connections",
                // MCP: every agent can reach the user's connected MCP
                // servers. Discovery is route-filtered, so each agent only
                // ever sees the servers meant for its lane.
                "mcp_servers",
                "mcp_call",
                "symbol_search",
                "file_symbols",
                "callers",
                "todo_write",
                "recall",
                "skill",
                "skill_search",
                "skill_install",
                "talk",
                "final_answer",
            ],
            ExecutionStyle::TestEngineering,
            "test result",
            vec!["test files", "run output with counts", "coverage gaps"],
            vec![
                "Write and run tests.",
                "Report exact commands and counts.",
                "Do not weaken tests to get green.",
            ],
        ),
    }
}
