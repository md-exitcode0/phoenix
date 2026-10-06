//! Maya — calendar and cross-company coordination.

use super::framework::{specialist_spec, SubAgentConfig};
use crate::runtime::ExecutionStyle;
use crate::session::SubAgentType;

pub const SYSTEM_PROMPT: &str = include_str!("../../prompts/planner_system.md");

pub fn planner_config() -> SubAgentConfig {
    SubAgentConfig {
        spec: specialist_spec(
            SubAgentType::Planner,
            SYSTEM_PROMPT,
            "claude-sonnet-4-6",
            vec![
                "read",
                "grep",
                "glob",
                "list_directory",
                "codebase_search",
                "symbol_search",
                "file_symbols",
                "call_path",
                "callers",
                "todo_write",
                "recall",
                "composio_search",
                "composio_connections",
                // MCP: every agent can reach the user's connected MCP
                // servers. Discovery is route-filtered, so each agent only
                // ever sees the servers meant for its lane.
                "mcp_servers",
                "mcp_call",
                "talk",
                "final_answer",
            ],
            ExecutionStyle::TaskPlanning,
            "coordination outcome",
            vec!["schedule and dependencies", "owners and commitments", "verification and open decisions"],
            vec![
                "Own calendars, meetings, schedules, dependencies, reminders, and genuine cross-company coordination.",
                "Name the accountable owner for each dependent outcome.",
                "Keep coordination executable and avoid becoming a mandatory hop.",
            ],
        ),
    }
}
