//! Finance Operations coworker.

use super::framework::{specialist_spec, SubAgentConfig};
use crate::runtime::ExecutionStyle;
use crate::session::SubAgentType;

pub const SYSTEM_PROMPT: &str = include_str!("../../prompts/finance_system.md");

pub fn finance_config() -> SubAgentConfig {
    SubAgentConfig {
        spec: specialist_spec(
            SubAgentType::Finance,
            SYSTEM_PROMPT,
            "claude-sonnet-4-6",
            vec![
                "recall",
                "memory_recall",
                "composio_search",
                "composio_schemas",
                "composio_run",
                "composio_connections",
                "browser_state",
                "browser_navigate",
                "browser_click",
                "browser_input",
                "browser_select_dropdown",
                "read",
                "write",
                "bash",
                "todo_write",
                "skill",
                "skill_search",
                "talk",
                "ask_user",
                "final_answer",
            ],
            ExecutionStyle::DataAnalysis,
            "finance operations result",
            vec!["source and account scope", "verified outcome", "receipts and approvals"],
            vec![
                "Own the finance outcome across tools instead of stopping at one app boundary.",
                "Keep monetary, subscription, account, and external-send actions behind the exact approval policy.",
                "Reconcile totals and preserve source receipts before reporting completion.",
            ],
        ),
    }
}
