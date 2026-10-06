//! Researcher agent.

use super::framework::{specialist_spec, SubAgentConfig};
use crate::runtime::ExecutionStyle;
use crate::session::SubAgentType;

pub const SYSTEM_PROMPT: &str = include_str!("../../prompts/researcher_system.md");

pub fn researcher_config() -> SubAgentConfig {
    SubAgentConfig {
        spec: specialist_spec(
            SubAgentType::Researcher,
            SYSTEM_PROMPT,
            "claude-sonnet-4-6",
            vec![
                "read",
                // Prefer local evidence inspection without delegating merely
                // because the evidence happens to require a command.
                "bash",
                "grep",
                "glob",
                "web_search",
                "web_fetch",
                "web_scrape",
                "web_crawl",
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
            ExecutionStyle::StagedResearch,
            "research report",
            vec!["source bundle", "report"],
            vec![
                "Own web-related research and current facts.",
                "Use browser for interactive/social/logged-in web work.",
                "Use coder for bulk extraction or file generation.",
            ],
        ),
    }
}
