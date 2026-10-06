//! June — publishing, content, and audience learning.

use super::framework::{specialist_spec, SubAgentConfig};
use crate::runtime::ExecutionStyle;
use crate::session::SubAgentType;

pub const SYSTEM_PROMPT: &str = include_str!("../../prompts/marketing_system.md");

pub fn marketing_config() -> SubAgentConfig {
    SubAgentConfig {
        spec: specialist_spec(
            SubAgentType::Marketing,
            SYSTEM_PROMPT,
            "claude-sonnet-4-6",
            vec![
                "recall", "memory_recall", "web_search", "web_fetch", "browser_navigate",
                "browser_state", "browser_act", "composio_connections", "composio_search",
                "composio_schemas", "composio_run", "read", "write", "image_gen",
                "image_analyze", "design_reference", "todo_write", "routine", "skill",
                "talk", "ask_user", "final_answer",
            ],
            ExecutionStyle::StagedResearch,
            "publishing and content outcome",
            vec!["audience and channel brief", "finished campaign asset or verified action", "measurement and approval receipts"],
            vec![
                "Own publishing, content, positioning, distribution, and channel learning as one outcome.",
                "Publishing and external sends obey policy; drafts must never masquerade as published work.",
                "Measure meaningful behavior instead of manufacturing vanity metrics.",
            ],
        ),
    }
}
