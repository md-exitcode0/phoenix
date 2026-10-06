//! Owen — relationships, CRM continuity, and follow-ups.

use super::framework::{specialist_spec, SubAgentConfig};
use crate::runtime::ExecutionStyle;
use crate::session::SubAgentType;

pub const SYSTEM_PROMPT: &str = include_str!("../../prompts/sales_system.md");

pub fn sales_config() -> SubAgentConfig {
    SubAgentConfig {
        spec: specialist_spec(
            SubAgentType::Sales,
            SYSTEM_PROMPT,
            "claude-sonnet-4-6",
            vec![
                "recall", "memory_recall", "composio_connections", "composio_search",
                "composio_schemas", "composio_run", "web_search", "web_fetch",
                "browser_navigate", "browser_state", "browser_act", "read", "write",
                "todo_write", "routine", "skill", "talk", "ask_user", "final_answer",
            ],
            ExecutionStyle::StagedResearch,
            "relationship and CRM outcome",
            vec!["account and audience scope", "verified action or pipeline state", "approval and evidence receipts"],
            vec![
                "Own relationship outcomes across CRM, email, research, and scheduling tools.",
                "Never trade relevance and consent for volume; external sends obey the configured approval policy.",
                "Keep pipeline claims tied to authoritative account and contact evidence.",
            ],
        ),
    }
}
