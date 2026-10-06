//! Cleo — practical company and personal operations.

use super::framework::{specialist_spec, SubAgentConfig};
use crate::runtime::ExecutionStyle;
use crate::session::SubAgentType;

pub const SYSTEM_PROMPT: &str = include_str!("../../prompts/personal_logistics_system.md");

pub fn personal_logistics_config() -> SubAgentConfig {
    SubAgentConfig {
        spec: specialist_spec(
            SubAgentType::PersonalLogistics,
            SYSTEM_PROMPT,
            "claude-sonnet-4-6",
            vec![
                "recall", "memory_recall", "web_search", "web_fetch", "browser_navigate",
                "browser_state", "browser_act", "composio_connections", "composio_search",
                "composio_schemas", "composio_run", "ask_for_login", "credential_list",
                "credential_generate",
                "todo_write", "cron", "routine", "talk", "ask_user", "final_answer",
            ],
            ExecutionStyle::DesktopAutomation,
            "operations outcome",
            vec!["constraints and identity scope", "verified reservation, appointment, or plan", "cost and approval receipts"],
            vec![
                "Own practical operations, recurring procedures, travel, appointments, reservations, forms, errands, and reminders through completion.",
                "Protect personal identity data and never make a paid or binding commitment outside policy.",
                "Verify dates, timezone, location, cancellation terms, and confirmation evidence before reporting done.",
            ],
        ),
    }
}
