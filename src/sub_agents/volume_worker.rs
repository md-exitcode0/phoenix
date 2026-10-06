//! Ephemeral generic worker used only by the `volume_work` batch tool.

use super::framework::{specialist_spec, SubAgentConfig};
use crate::runtime::ExecutionStyle;
use crate::session::SubAgentType;

pub const ROLE: &str = "volume_worker";
pub const SYSTEM_PROMPT: &str = include_str!("../../prompts/volume_worker_system.md");

pub fn agent_type() -> SubAgentType {
    SubAgentType::custom(ROLE)
}

pub fn is_agent(agent: SubAgentType) -> bool {
    matches!(agent, SubAgentType::Custom(id) if crate::session::custom_agent_label(id) == ROLE)
}

pub fn config() -> SubAgentConfig {
    SubAgentConfig {
        spec: specialist_spec(
            agent_type(),
            SYSTEM_PROMPT,
            "claude-sonnet-4-6",
            vec![
                "read",
                "write",
                "str_replace",
                "bash",
                "grep",
                "glob",
                "list_directory",
                "codebase_search",
                "browser_state",
                "browser_navigate",
                "computer_status",
                "web_search",
                "web_fetch",
                "todo_write",
                "final_answer",
            ],
            ExecutionStyle::CodeExecution,
            "volume worker result",
            vec!["result", "acceptance evidence"],
            vec![
                "Handle exactly one self-contained batch item.",
                "Use direct tools; never create another volume batch.",
                "Use only the item-private terminal, desktop, and browser context.",
                "Return compact evidence to the calling coworker.",
            ],
        ),
    }
}
