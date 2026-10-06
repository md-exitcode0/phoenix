//! Hacker agent.

use super::framework::{specialist_spec, SubAgentConfig};
use crate::runtime::ExecutionStyle;
use crate::session::SubAgentType;

pub const SYSTEM_PROMPT: &str = include_str!("../../prompts/hacker_system.md");

pub fn hacker_config() -> SubAgentConfig {
    SubAgentConfig {
        spec: specialist_spec(
            SubAgentType::Hacker,
            SYSTEM_PROMPT,
            "claude-sonnet-4-6",
            vec![
                "read",
                "grep",
                "glob",
                "bash",
                "web_search",
                "web_fetch",
                // Composio: connected security/tooling toolkits — discover with
                // search, then run a connected action instead of bolting one on.
                "composio_search",
                "composio_schemas",
                "composio_run",
                "composio_connections",
                // Local MCP servers (e.g. T3MP3ST's security_recon) — discover
                // with mcp_servers, then invoke with mcp_call.
                "mcp_servers",
                "mcp_call",
                "todo_write",
                "recall",
                "skill",
                "talk",
                "final_answer",
                "symbol_search",
                "file_symbols",
                "callers",
            ],
            ExecutionStyle::SecurityAssessment,
            "security result",
            vec!["findings", "evidence", "recommended actions"],
            vec![
                "Authorized testing only: run active tools (scans, recon, external offensive MCP servers) ONLY against targets the user owns or has explicit written permission to test. State the authorization/scope before acting; if it is not established, ask — never touch a target on assumption.",
                "Keep findings evidence-backed.",
                "Rank by real impact and exploitability.",
            ],
        ),
    }
}
