//! Database agent.

use super::framework::{specialist_spec, SubAgentConfig};
use crate::runtime::ExecutionStyle;
use crate::session::SubAgentType;

pub const SYSTEM_PROMPT: &str = include_str!("../../prompts/database_system.md");

pub fn database_config() -> SubAgentConfig {
    SubAgentConfig {
        spec: specialist_spec(
            SubAgentType::Database,
            SYSTEM_PROMPT,
            "claude-sonnet-4-6",
            vec![
                "read",
                "write",
                "str_replace",
                "grep",
                "glob",
                "list_directory",
                "bash",
                // Composio: connected DB/data toolkits (Postgres, Supabase,
                // Airtable, …). One composio_run beats hand-writing a driver.
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
                "symbol_search",
                "file_symbols",
                "callers",
            ],
            ExecutionStyle::DataAnalysis,
            "database result",
            vec!["schema notes", "query or migration output"],
            vec![
                "Inspect real schema before proposing SQL.",
                "Flag destructive operations and rollback paths.",
                "Verify queries or migrations when a safe check exists.",
            ],
        ),
    }
}
