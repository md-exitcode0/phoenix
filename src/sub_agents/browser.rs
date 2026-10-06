//! Historical browser session identity plus the universal browser craft prompt.
//!
//! New work runs on the addressed visible coworker's identity and private
//! Chromium profile. `SubAgentType::Browser` remains deserializable only so
//! existing histories survive the redesign; it is not executable or visible.
//! The craft prompt remains available for prompt migration and browser policy.

use super::framework::{specialist_spec, SubAgentConfig};
use crate::runtime::ExecutionStyle;
use crate::session::SubAgentType;

pub const SYSTEM_PROMPT: &str = include_str!("../../prompts/browser_system.md");

pub fn browser_config() -> SubAgentConfig {
    SubAgentConfig {
        spec: specialist_spec(
            SubAgentType::Browser,
            SYSTEM_PROMPT,
            "claude-sonnet-4-6",
            vec![
                // Native browser-use action surface (Chromium over CDP).
                "browser_act",
                "browser_navigate",
                "browser_search",
                "browser_go_back",
                "browser_wait",
                "browser_click",
                "browser_input",
                "browser_send_keys",
                "browser_scroll",
                "browser_find_text",
                "browser_search_page",
                "browser_find_elements",
                "browser_upload_file",
                "browser_extract",
                "browser_screenshot",
                "browser_save_as_pdf",
                "browser_download",
                "browser_dropdown_options",
                "browser_select_dropdown",
                "browser_switch",
                "browser_close",
                "browser_evaluate",
                "browser_console",
                "browser_status",
                "browser_state",
                "ask_for_login",
                "teach_workflow",
                // Connected-app actions (Composio For-You MCP): when the task's
                // service is connected, one composio_run beats a 12-step UI drive.
                "composio_search",
                "composio_schemas",
                "composio_run",
                "composio_connections",
                // MCP: every agent can reach the user's connected MCP
                // servers. Discovery is route-filtered, so each agent only
                // ever sees the servers meant for its lane.
                "mcp_servers",
                "mcp_call",
                // Workspace artifacts for long collection tasks.
                "read",
                "image_analyze",
                "write",
                "str_replace",
                // Progress tracking, chaining, structured finish.
                "todo_write",
                "recall",
                "skill",
                "talk",
                "final_answer",
            ],
            ExecutionStyle::BrowserAutomation,
            "browser task result",
            vec!["action log", "extracted findings"],
            vec![
                "Ground every decision in the returned browser state; verify before claiming success.",
                "One clear step goal at a time; page-changing actions last in a batch.",
                "Handle popups/cookie banners first; apply user-specified filters before browsing.",
                "Never invent element indexes or fill data gaps from memory.",
            ],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_prompt_carries_the_runtime_role() {
        assert!(SYSTEM_PROMPT.starts_with("You are a Phoenix coworker"));
        assert!(SYSTEM_PROMPT.contains("Browser is a universal tool"));
        assert!(!SYSTEM_PROMPT.contains("Surf"));
        assert!(!SYSTEM_PROMPT.contains("browser_swarm"));
        assert!(SYSTEM_PROMPT.contains("clicking, forms, logins"));
        assert!(SYSTEM_PROMPT.contains("verify from page state or screenshot"));
        assert!(SYSTEM_PROMPT.contains("Track tabs you open"));
        assert!(SYSTEM_PROMPT.contains("final_answer"));
        assert!(SYSTEM_PROMPT.contains("ask_for_login"));
        assert!(!SYSTEM_PROMPT.contains("browser_visibility"));
        assert!(!SYSTEM_PROMPT.contains("browser_login_handoff"));
        assert!(!SYSTEM_PROMPT.contains("EVALUATE"));
        assert!(!SYSTEM_PROMPT.contains("BROWSER RULES"));
    }

    #[test]
    fn browser_prompt_carries_service_detection_and_deep_link_routing() {
        // Strawberry parity (2026-07-02): outcome → service → surface mapping
        // before the first navigate, connector rung first, deep action links
        // over front-door clicking. Pin so prompt edits can't drop it.
        assert!(SYSTEM_PROMPT.contains("# Service Detection And Routing"));
        assert!(SYSTEM_PROMPT.contains("cal.new"));
        assert!(SYSTEM_PROMPT.contains("connected connector beats driving the UI"));
    }

    #[test]
    fn browser_prompt_carries_call_economy_and_deliverable_rules() {
        // Live Reddit check burned 9 of 19 calls on ceremony (self-invented
        // artifact files, todo ticking, re-observation). Pin the economy rules.
        assert!(SYSTEM_PROMPT.contains("Call economy"));
        assert!(SYSTEM_PROMPT.contains("final answer text unless the brief or user names a file"));
        assert!(SYSTEM_PROMPT.contains("Do not invent action logs"));
    }

    #[test]
    fn browser_prompt_carries_strawberry_research_and_pacing_doctrine() {
        // Reverse-engineered from Strawberry (2026-07-01): gap-closing passes +
        // flag-missing over guessing, and calm humanlike pacing for outreach on
        // the user's real logged-in session. Pin so a prompt edit can't drop them.
        assert!(SYSTEM_PROMPT.contains("gap-closing passes"));
        assert!(SYSTEM_PROMPT.contains("missing rather than guessing"));
        assert!(SYSTEM_PROMPT.contains("calm human pace"));
        assert!(SYSTEM_PROMPT.contains("looks automated"));
    }

    #[test]
    fn browser_allowlist_covers_the_full_donor_surface() {
        let spec = browser_config().spec;
        for action in crate::tools::browser_native::BROWSER_ACTIONS {
            let tool = format!("browser_{action}");
            assert!(
                spec.tool_allowlist.contains(&tool),
                "missing browser tool in allowlist: {tool}"
            );
        }
        assert!(spec.tool_allowlist.contains(&"final_answer".to_string()));
        assert!(spec.tool_allowlist.contains(&"talk".to_string()));
    }
}
