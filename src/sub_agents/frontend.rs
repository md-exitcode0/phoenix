//! Frontend agent.

use super::framework::{specialist_spec, SubAgentConfig};
use crate::runtime::ExecutionStyle;
use crate::session::SubAgentType;

pub const SYSTEM_PROMPT: &str = include_str!("../../prompts/frontend_system.md");

pub fn frontend_config() -> SubAgentConfig {
    SubAgentConfig {
        spec: specialist_spec(
            SubAgentType::Frontend,
            SYSTEM_PROMPT,
            "claude-sonnet-4-6",
            vec![
                "design_website",
                "image_gen",
                "image_analyze",
                "ui_snap",
                "read",
                "write",
                "str_replace",
                "grep",
                "glob",
                "list_directory",
                "bash",
                // Composio: connected sources for design content/data (Sheets,
                // Notion, Figma exports) — fetch via composio_run over scraping.
                "composio_search",
                "composio_schemas",
                "composio_run",
                "composio_connections",
                // MCP: design-reference servers (LandingFolio and friends) hand
                // back real screenshots of shipped pages. Discovery is filtered
                // by each server's `route`, so this only ever surfaces servers
                // meant for the design lane.
                "mcp_servers",
                "mcp_call",
                "todo_write",
                "recall",
                "skill",
                "skill_search",
                "skill_install",
                "talk",
                "final_answer",
                "symbol_search",
                "file_symbols",
                "callers",
            ],
            ExecutionStyle::DesignIteration,
            "frontend result",
            vec!["changed files", "verification"],
            vec![
                "Design and build UI/UX work.",
                "Use the existing design system when present.",
                "Verify responsive and state behavior when practical.",
            ],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontend_host_role_defers_design_to_the_managed_phase_contract() {
        assert!(SYSTEM_PROMPT.starts_with("You are the Product Design and Frontend coworker"));
        assert!(SYSTEM_PROMPT.contains("current phase prompt and selected reference images"));
        assert!(SYSTEM_PROMPT.contains("validating each result"));
        assert!(SYSTEM_PROMPT.contains("no user-facing design-mode toggle"));
        assert!(SYSTEM_PROMPT.contains("read-only review"));
        assert!(SYSTEM_PROMPT.contains("Permission checks, cancellation, workspace boundaries"));
        assert!(SYSTEM_PROMPT.len() < 4_000, "Phase instructions must not compete with another design handbook");
    }

    #[test]
    fn iris_does_not_stack_the_withdrawn_design_doctrine() {
        for term in ["Plain meaning and usefulness", "fresh-visitor perspective", "Do not write riddles", "Accent should be scarce", "display/body pairing by default", "load `taste/SKILL.md` first"] {
            assert!(!SYSTEM_PROMPT.contains(term), "Retired Iris doctrine remains active: {term}");
        }
        let tools = frontend_config().spec.tool_allowlist;
        for name in ["image_gen", "image_analyze", "ui_snap", "read", "write", "bash"] {
            assert!(tools.iter().any(|tool| tool == name), "Missing working capability: {name}");
        }
    }
}
