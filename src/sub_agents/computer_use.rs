//! computer_use agent.

use super::framework::{specialist_spec, SubAgentConfig};
use crate::runtime::ExecutionStyle;
use crate::session::SubAgentType;

pub const SYSTEM_PROMPT: &str = include_str!("../../prompts/computer_use_system.md");

pub fn computer_use_config() -> SubAgentConfig {
    SubAgentConfig {
        spec: specialist_spec(
            SubAgentType::ComputerUse,
            SYSTEM_PROMPT,
            "claude-sonnet-4-6",
            vec![
                "computer_status",
                "computer_screenshot",
                "computer_read_text",
                "computer_move",
                "computer_locate",
                "computer_click",
                "computer_drag",
                "computer_scroll",
                "computer_type",
                "computer_key",
                "computer_act",
                "computer_list_windows",
                "computer_focus_window",
                "computer_capture_window",
                "computer_window_act",
                "computer_lower_window",
                "computer_app_targets",
                "computer_app_inspect",
                "computer_app_locate",
                "computer_app_read",
                "computer_open",
                "computer_wait",
                // Composio: a connected app's API beats driving its UI. When the
                // target service is connected, one composio_run replaces a long
                // window-act sequence (and works regardless of focus/stacking).
                "composio_search",
                "composio_schemas",
                "composio_run",
                "composio_connections",
                // MCP: every agent can reach the user's connected MCP
                // servers. Discovery is route-filtered, so each agent only
                // ever sees the servers meant for its lane.
                "mcp_servers",
                "mcp_call",
                "read",
                "list_directory",
                // Look at an image FILE directly (vision model on the path) —
                // never open a viewer and screenshot the screen to "see" it.
                "image_analyze",
                "todo_write",
                "recall",
                "skill",
                "talk",
                "final_answer",
            ],
            ExecutionStyle::DesktopAutomation,
            "desktop action report",
            vec!["screen result", "blockers"],
            vec![
                "Use desktop and app tools for on-screen work.",
                "Verify visible changes before claiming success.",
                "Approval is required for irreversible actions.",
            ],
        ),
    }
}
