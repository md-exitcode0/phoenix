//! Sub-agents framework

mod browser;
mod coder;
mod computer_use;
mod critic;
mod database;
mod finance;
mod framework;
mod frontend;
mod hacker;
mod marketing;
mod personal_logistics;
mod planner;
mod presentation;
pub mod registry;
mod researcher;
mod sales;
mod scribe;
mod tester;
pub mod volume_worker;

pub use coder::{coder_config, parse_coder_turn_result, CoderToolCall, CoderTurnResult};
pub use framework::SubAgentConfig;
pub use researcher::researcher_config;

use crate::session::SubAgentType;

/// Resolve the runtime config for a roster specialist. Execution is gated
/// separately by `specialist_is_executable`; this just maps the type to its
/// config so `deliver_talk` can run any executable specialist generically.
/// Built-ins get their compiled config plus any `~/.phoenix/agents/<role>/
/// agent.toml` override; custom agents load entirely from the registry.
pub fn specialist_config(agent: SubAgentType) -> SubAgentConfig {
    let config = match agent {
        SubAgentType::Coder => coder_config(),
        SubAgentType::Researcher => researcher_config(),
        SubAgentType::Browser => browser::browser_config(),
        SubAgentType::Frontend => frontend::frontend_config(),
        SubAgentType::Database => database::database_config(),
        SubAgentType::Hacker => hacker::hacker_config(),
        SubAgentType::Presentation => presentation::presentation_config(),
        SubAgentType::Finance => finance::finance_config(),
        SubAgentType::ComputerUse => computer_use::computer_use_config(),
        SubAgentType::Critic => critic::critic_config(),
        SubAgentType::Tester => tester::tester_config(),
        SubAgentType::Planner => planner::planner_config(),
        SubAgentType::Scribe => scribe::scribe_config(),
        SubAgentType::Sales => sales::sales_config(),
        SubAgentType::Marketing => marketing::marketing_config(),
        SubAgentType::PersonalLogistics => personal_logistics::personal_logistics_config(),
        SubAgentType::Custom(_) if volume_worker::is_agent(agent) => {
            return volume_worker::config()
        }
        SubAgentType::Custom(id) => return registry::custom_config(id),
    };
    let role = crate::runtime::delegation::specialist_label(agent);
    registry::apply_builtin_override(role, config)
}

#[cfg(test)]
mod skill_roster_tests {
    use super::*;

    #[test]
    fn every_coworker_receives_the_complete_live_tool_catalog() {
        let universal = crate::tools::universal_agent_tool_names();
        for agent in [
            SubAgentType::Coder,
            SubAgentType::Researcher,
            SubAgentType::Frontend,
            SubAgentType::Database,
            SubAgentType::Hacker,
            SubAgentType::Presentation,
            SubAgentType::Finance,
            SubAgentType::Critic,
            SubAgentType::Tester,
            SubAgentType::Planner,
            SubAgentType::Scribe,
            SubAgentType::Sales,
            SubAgentType::Marketing,
            SubAgentType::PersonalLogistics,
        ] {
            let tools = specialist_config(agent).spec.tool_allowlist;
            for tool in &universal {
                assert!(
                    tools.iter().any(|t| t == tool),
                    "{agent:?} is missing universal capability `{tool}`"
                );
            }
            assert!(
                !tools.iter().any(|tool| tool == "browser_swarm"),
                "the retired hidden browser swarm must not leak into {agent:?}"
            );
            assert!(!tools.iter().any(|tool| tool == "agent_pipeline"));
            assert!(!tools.iter().any(|tool| tool == "tools_assign"));
        }
    }

    #[test]
    fn every_named_coworker_receives_the_visual_studio() {
        const DESIGN_TOOLS: [&str; 4] =
            ["design_reference", "image_gen", "image_analyze", "ui_snap"];
        for role in registry::BUILTIN_ROLES {
            let agent = registry::builtin_by_label(role).expect("named coworker resolves");
            let tools = specialist_config(agent).spec.tool_allowlist;
            for tool in DESIGN_TOOLS {
                assert!(
                    tools.iter().any(|candidate| candidate == tool),
                    "named coworker `{role}` is missing visual capability `{tool}`"
                );
            }
        }

        let SubAgentType::Custom(future_id) =
            SubAgentType::custom("future_visual_coworker_capability_test")
        else {
            unreachable!("custom coworker must use the custom variant")
        };
        let future = registry::custom_config(future_id);
        for tool in DESIGN_TOOLS {
            assert!(
                future
                    .spec
                    .tool_allowlist
                    .iter()
                    .any(|candidate| candidate == tool),
                "a newly created coworker is missing visual capability `{tool}`"
            );
        }
    }
}
