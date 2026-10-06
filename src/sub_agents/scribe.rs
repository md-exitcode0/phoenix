//! Nico — communications, inbox continuity, and human-quality writing.

use super::framework::{specialist_spec, SubAgentConfig};
use crate::runtime::ExecutionStyle;
use crate::session::SubAgentType;

pub const SYSTEM_PROMPT: &str = include_str!("../../prompts/scribe_system.md");

pub fn scribe_config() -> SubAgentConfig {
    SubAgentConfig {
        spec: specialist_spec(
            SubAgentType::Scribe,
            SYSTEM_PROMPT,
            "claude-sonnet-4-6",
            vec![
                "read",
                "grep",
                "glob",
                "write",
                "str_replace",
                "web_search",
                "web_fetch",
                "composio_search",
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
            "communications outcome",
            vec!["thread state", "draft or verified action", "approval and provider receipt"],
            vec![
                "Own inbox continuity, replies, follow-ups, verification handoffs, and the user's communication voice.",
                "Agent-creation runs: read the house prompts corpus BEFORE drafting a new agent's system prompt.",
                "A verification code is ephemeral and never grants another coworker inbox access.",
            ],
        ),
    }
}
