//! Sub-agent framework
//!
//! Shared framework for highly specialized Phoenix sub-agents.

use crate::runtime::{
    AgentSpec, AgentTargetSpec, ExecutionStyle, OutputContract, PermissionProfile, WorkflowContract,
};
use crate::session::{Session, SubAgentType};

/// Base sub-agent definition.
pub struct SubAgentConfig {
    pub spec: AgentSpec,
}

/// Create a new sub-agent session
pub fn create_sub_agent(config: &SubAgentConfig) -> Session {
    let agent_type = match config.spec.target {
        AgentTargetSpec::Specialist(agent_type) => agent_type,
        AgentTargetSpec::Orchestrator => {
            panic!("sub-agent config cannot target orchestrator")
        }
    };

    Session::new_sub_agent(
        agent_type,
        &config.spec.default_model,
        &config.spec.system_prompt,
    )
}

pub fn specialist_spec(
    agent_type: SubAgentType,
    system_prompt: &str,
    model: &str,
    tool_allowlist: Vec<&str>,
    execution_style: ExecutionStyle,
    output_label: &str,
    required_artifacts: Vec<&str>,
    workflow_notes: Vec<&str>,
) -> AgentSpec {
    let mut tool_allowlist = tool_allowlist
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    // Every specialist gets knowledge-graph recall: it is read-only, local,
    // and token-free, and mid-task is exactly where a half-remembered prior
    // fix, decision, or preference is needed. The shared contract sets the
    // "recall before guessing" habit that goes with it.
    if !tool_allowlist.iter().any(|tool| tool == "memory_recall") {
        tool_allowlist.push("memory_recall".to_string());
    }
    // …and the write half of the same habit: a one-line durable note into the
    // knowledge graph the moment something worth keeping is learned (the
    // memory beat in the shared contract tells them when).
    if !tool_allowlist.iter().any(|tool| tool == "memory_save") {
        tool_allowlist.push("memory_save".to_string());
    }
    // …and the unblock hatch: a blocked specialist asking the user one popup
    // question ("ruff isn't installed — install it, or skip linting?") beats
    // both silently skipping the step and burning rounds on workarounds. The
    // shared contract sets the when/when-not.
    if !tool_allowlist.iter().any(|tool| tool == "ask_user") {
        tool_allowlist.push("ask_user".to_string());
    }
    // A coworker's role changes what it knows, remembers, owns, and tries
    // first. It must not create a tool silo. Append the live catalog after the
    // role-optimized order so every agent can research, browse, code, operate
    // apps, use connected services, teach workflows, and ask teammates.
    let tool_allowlist = crate::tools::merge_with_universal_tools(tool_allowlist);
    crate::tools::validate_tool_allowlist(&tool_allowlist)
        .unwrap_or_else(|error| panic!("invalid tool allowlist for {}: {}", agent_type, error));
    let can_use_shell = tool_allowlist.iter().any(|tool| tool == "bash");
    let can_write_files = tool_allowlist
        .iter()
        .any(|tool| matches!(tool.as_str(), "write" | "str_replace"));
    let can_access_network = tool_allowlist
        .iter()
        .any(|tool| matches!(tool.as_str(), "web_search" | "web_fetch"));

    // Overlay file (if present) replaces the embedded prompt: RSI tier 1.
    let overlay_name = format!(
        "{}_system",
        crate::runtime::delegation::specialist_label(agent_type)
    );
    let system_prompt = crate::config::prompt_overlay(&overlay_name, system_prompt);

    AgentSpec {
        name: agent_type.to_string(),
        target: AgentTargetSpec::Specialist(agent_type),
        system_prompt,
        default_model: model.to_string(),
        tool_allowlist,
        permissions: PermissionProfile {
            // Specialists CAN pass work directly to a teammate — a chain baton
            // via `talk` (coder→tester→coder) is first-class, not an
            // orchestrator-only power. This flag is ADVISORY (nothing gates on
            // it); it read `false` only as leftover scaffolding, which
            // misdescribed the mesh and invited a future "let's enforce it" that
            // would re-force Phoenix into the middleman seat. Kept honest.
            can_delegate: true,
            can_use_shell,
            can_write_files,
            can_access_network,
        },
        workflow: WorkflowContract {
            execution_style,
            // Advisory only, deliberately un-wired (no gate reads these): a
            // specialist reports UP when it finishes, but in a team chain "up"
            // is the peer that handed off, not always the orchestrator, and
            // review is a judgement call — never a forced step. Team routing
            // stays the team's choice, not a mandate.
            must_report_to_orchestrator: true,
            review_required_before_done: false,
            notes: workflow_notes.into_iter().map(str::to_string).collect(),
        },
        output: OutputContract {
            label: output_label.to_string(),
            required_artifacts: required_artifacts.into_iter().map(str::to_string).collect(),
            final_answer_style: "In direct conversation, answer the user naturally and match their requested depth. \
                For delegated work, return the result to the assigning coworker through the designated return path, \
                with the evidence, artifacts, verification and unresolved limits they need. \
                A specialist role does not require a report format; use structure when the request or material calls for it.".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_specialist_spec_with_expected_shape() {
        let spec = specialist_spec(
            SubAgentType::Coder,
            "system",
            "model-x",
            vec!["read", "write"],
            ExecutionStyle::CodeExecution,
            "code report",
            vec!["plan", "tests"],
            vec!["note a", "note b"],
        );

        assert_eq!(spec.name, "Coder");
        assert_eq!(spec.default_model, "model-x");
        // Role-preferred tools remain first, but the complete live catalog is
        // available to every coworker.
        assert_eq!(&spec.tool_allowlist[..2], &["read", "write"]);
        for tool in crate::tools::universal_agent_tool_names() {
            assert!(
                spec.tool_allowlist.contains(&tool),
                "missing universal tool {tool}"
            );
        }
        assert!(!spec.tool_allowlist.contains(&"browser_swarm".to_string()));
        assert!(!spec.tool_allowlist.contains(&"agent_pipeline".to_string()));
        assert!(!spec.tool_allowlist.contains(&"tools_assign".to_string()));
        // Specialists can pass work directly to teammates (advisory flag).
        assert!(spec.permissions.can_delegate);
        assert!(spec.permissions.can_use_shell);
        assert!(spec.permissions.can_write_files);
        assert!(spec.permissions.can_access_network);
        assert_eq!(spec.output.label, "code report");
        assert_eq!(spec.workflow.notes.len(), 2);
    }

    #[test]
    fn creates_sub_agent_session_from_specialist_spec() {
        // prompt_overlay is intentionally rooted in PHOENIX_HOME. Isolate the
        // test so parallel setup/overlay tests cannot swap the expected
        // specialist identity between these two reads.
        let home = tempfile::tempdir().expect("isolated Phoenix home");
        let _home_guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let config = SubAgentConfig {
            spec: specialist_spec(
                SubAgentType::Researcher,
                "research-system",
                "model-y",
                vec!["read"],
                ExecutionStyle::StagedResearch,
                "research report",
                vec!["report"],
                vec!["plan first"],
            ),
        };

        let session = create_sub_agent(&config);
        assert_eq!(session.model, "model-y");
        assert_eq!(
            session.system_prompt,
            crate::config::prompt_overlay("researcher_system", "research-system")
        );
        match session.kind {
            crate::session::SessionKind::SubAgent(SubAgentType::Researcher) => {}
            other => panic!("unexpected session kind: {:?}", other),
        }
    }

    #[test]
    fn rejects_unknown_tool_names_in_specialist_spec() {
        let result = std::panic::catch_unwind(|| {
            specialist_spec(
                SubAgentType::Coder,
                "system",
                "model-x",
                vec!["read", "not-real"],
                ExecutionStyle::CodeExecution,
                "code report",
                vec!["plan"],
                vec!["note a"],
            )
        });

        assert!(result.is_err());
    }
}
