//! Orchestrator agent implementation

// Suppress dead-code warnings for struct fields and helpers that are wired up
// but not yet exercised in the main execution path. Without this attribute,
// rustc 1.95.0 triggers an ICE in annotate_snippets when rendering the warning
// spans for items in this module.
#![allow(dead_code)]

use anyhow::{bail, Context};

use crate::runtime::{
    AgentArtifact, AgentOutcome, AgentSpec, AgentTarget, AgentTargetSpec, ArtifactKind,
    DelegationMode, ExecutionStyle, OrchestratorDecision, OutputContract, PermissionProfile,
    TaskEnvelope, ToolCallResult, WorkflowContract,
};
use crate::session::{Session, SessionStore};

pub const ORCHESTRATOR_SYSTEM_PROMPT: &str = include_str!("../../prompts/orchestrator_system.md");

pub struct Orchestrator {
    session: Session,
    session_manager: SessionStore,
    spec: AgentSpec,
}

impl Orchestrator {
    pub fn new(model: &str, system_prompt: &str) -> Self {
        // Overlay file (if present) replaces the embedded prompt: RSI tier 1.
        let system_prompt = crate::config::prompt_overlay("orchestrator_system", system_prompt);
        let system_prompt = system_prompt.as_str();
        let mut session_manager = SessionStore::default();
        let session = session_manager.create_main(model, system_prompt);
        let spec = AgentSpec {
            name: "Orchestrator".to_string(),
            target: AgentTargetSpec::Orchestrator,
            system_prompt: system_prompt.to_string(),
            default_model: model.to_string(),
            tool_allowlist: crate::tools::merge_with_universal_tools(vec![
                "talk".to_string(),
                "read".to_string(),
                "write".to_string(),
                "glob".to_string(),
                "grep".to_string(),
                "codebase_search".to_string(),
                "symbol_search".to_string(),
                "file_symbols".to_string(),
                "bash".to_string(),
                "ask_user".to_string(),
                "todo_write".to_string(),
                "recall".to_string(),
                "memory_recall".to_string(),
                "memory_save".to_string(),
                "vital_memory_write".to_string(),
                "cron".to_string(),
                "skill".to_string(),
                "skill_install".to_string(),
                "create_agent".to_string(),
                "agent_provision".to_string(),
                "tools_create".to_string(),
                "reverse_skill".to_string(),
                "routine".to_string(),
                "work".to_string(),
                "skill_search".to_string(),
                "composio_search".to_string(),
                "composio_schemas".to_string(),
                "composio_run".to_string(),
                "composio_connections".to_string(),
                "mcp_servers".to_string(),
                "mcp_call".to_string(),
                "list_directory".to_string(),
                "image_analyze".to_string(),
                "final_answer".to_string(),
            ]),
            permissions: PermissionProfile {
                can_delegate: true,
                can_use_shell: true,
                can_write_files: true,
                can_access_network: true,
            },
            workflow: WorkflowContract {
                execution_style: ExecutionStyle::RouteOnly,
                must_report_to_orchestrator: false,
                review_required_before_done: false,
                notes: vec![
                    "Route work instead of doing specialist work directly.".to_string(),
                    "Choose between local handling, handoff, and parallel delegation.".to_string(),
                ],
            },
            output: OutputContract {
                label: "user-facing answer".to_string(),
                // These become model instructions, not internal routing fields.
                // Requiring decision/reasoning artifacts made chat replies read
                // like forms despite the shared conversational voice contract.
                required_artifacts: vec![],
                final_answer_style: "Answer the user's request in natural language. Lead with the answer, result or bad news; take a side when one option is better; keep it warm, plain and as short as the ask allows. Credit teammates briefly by name. Leave paths, commands, counts and receipts out unless the user asked or needs one to act. End with the one natural next step when there is one. Use labeled sections only when the user requests them or the material needs them.".to_string(),
            },
        };

        Self {
            session,
            session_manager,
            spec,
        }
    }

    pub fn spec(&self) -> &AgentSpec {
        &self.spec
    }

    pub fn route_task(&self, task: &TaskEnvelope) -> anyhow::Result<OrchestratorDecision> {
        match task.target_agent {
            AgentTarget::Orchestrator => Ok(self.route_to_coder(
                "First runtime slice defaults orchestrator work to coder execution.",
            )),
            AgentTarget::Specialist(agent_type) => Ok(OrchestratorDecision {
                mode: DelegationMode::Handoff,
                target: AgentTarget::Specialist(agent_type),
                rationale: format!("Task explicitly targets specialist {}", agent_type),
            }),
        }
    }

    pub fn route_to_coder(&self, rationale: impl Into<String>) -> OrchestratorDecision {
        OrchestratorDecision {
            mode: DelegationMode::Handoff,
            target: AgentTarget::Specialist(crate::session::SubAgentType::Coder),
            rationale: rationale.into(),
        }
    }

    pub fn finalize_after_specialist(
        &self,
        task: &TaskEnvelope,
        decision: &OrchestratorDecision,
        specialist_outcome: &AgentOutcome,
    ) -> AgentOutcome {
        AgentOutcome {
            completion: specialist_outcome.completion,
            agent: AgentTarget::Orchestrator,
            summary: specialist_outcome.summary.clone(),
            artifacts: vec![
                AgentArtifact {
                    kind: ArtifactKind::Plan,
                    title: "Routing decision".to_string(),
                    body: format!(
                        "Task: {}\nMode: {:?}\nTarget: {:?}\nRationale: {}",
                        task.title, decision.mode, decision.target, decision.rationale
                    ),
                },
                AgentArtifact {
                    kind: ArtifactKind::Reply,
                    title: "Final reply".to_string(),
                    body: specialist_outcome.summary.clone(),
                },
            ],
            tool_results: vec![
                ToolCallResult {
                    tool_name: "talk".to_string(),
                    input_summary: format!("coder: {}", task.title),
                    success: true,
                    output: "Orchestrator delivered talk to coder and received a specialist reply."
                        .to_string(),
                },
                ToolCallResult {
                    tool_name: "receive_specialist_result".to_string(),
                    input_summary: "coder".to_string(),
                    success: true,
                    output: format!(
                        "Orchestrator received result from {:?}.",
                        specialist_outcome.agent
                    ),
                },
            ],
            provider_response: None,
        }
    }

    // NOTE: the orchestrator's live turn loop is NOT here. This module holds the
    // orchestrator's spec + the routing-decision helpers (`route_task`,
    // `finalize_after_specialist`); the actual per-turn loop — receive message,
    // librarian preload, prompt build, provider call, librarian save, respond —
    // runs in `runtime::mesh::turn_loop`. (Was a stale "TODO: implement main
    // loop" that falsely implied the orchestrator had no loop.)
}

pub fn parse_orchestrator_decision(content: &str) -> anyhow::Result<OrchestratorDecision> {
    let value: serde_json::Value =
        serde_json::from_str(content).context("failed to parse orchestrator decision JSON")?;
    let object = value
        .as_object()
        .context("orchestrator decision JSON must be an object")?;

    let mode_value = object
        .get("mode")
        .and_then(|value| value.as_str())
        .context("orchestrator decision missing string field `mode`")?;
    let mode = parse_delegation_mode(mode_value)?;

    let target_value = object
        .get("target")
        .context("orchestrator decision missing field `target`")?;
    let target = parse_agent_target(target_value)?;

    let rationale = object
        .get("rationale")
        .and_then(|value| value.as_str())
        .unwrap_or("No rationale provided.")
        .trim()
        .to_string();

    Ok(OrchestratorDecision {
        mode,
        target,
        rationale,
    })
}

fn parse_delegation_mode(value: &str) -> anyhow::Result<DelegationMode> {
    match normalize_token(value).as_str() {
        "staylocal" | "stay_local" | "local" => Ok(DelegationMode::StayLocal),
        "handoff" | "hand_off" => Ok(DelegationMode::Handoff),
        "parallel" => Ok(DelegationMode::Parallel),
        other => bail!("unsupported orchestrator delegation mode `{other}`"),
    }
}

fn parse_agent_target(value: &serde_json::Value) -> anyhow::Result<AgentTarget> {
    if let Some(target) = value.as_str() {
        return parse_agent_target_token(target);
    }

    let object = value
        .as_object()
        .context("orchestrator target must be a string or object")?;
    if object.contains_key("orchestrator") || object.contains_key("Orchestrator") {
        return Ok(AgentTarget::Orchestrator);
    }

    let specialist = object
        .get("specialist")
        .or_else(|| object.get("Specialist"))
        .and_then(|value| value.as_str())
        .context("orchestrator target object must include string `specialist`")?;
    parse_specialist_target(specialist)
}

fn parse_agent_target_token(value: &str) -> anyhow::Result<AgentTarget> {
    match normalize_token(value).as_str() {
        "orchestrator" => Ok(AgentTarget::Orchestrator),
        "coder" => parse_specialist_target(value),
        other => bail!("unsupported orchestrator target `{other}`"),
    }
}

fn parse_specialist_target(value: &str) -> anyhow::Result<AgentTarget> {
    match normalize_token(value).as_str() {
        "coder" => Ok(AgentTarget::Specialist(crate::session::SubAgentType::Coder)),
        other => bail!("unsupported specialist target `{other}`"),
    }
}

fn normalize_token(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '-')
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orchestrator_chat_prompt_does_not_require_routing_sections() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let orchestrator = Orchestrator::new("model-z", ORCHESTRATOR_SYSTEM_PROMPT);
        let task = TaskEnvelope::new(
            "natural-reply", AgentTarget::Orchestrator, "Answer", "What changed?",
        );
        let prompt = crate::runtime::prompt::assemble_prompt(
            orchestrator.spec(), &task, &crate::librarian::LoadedMemories::default(),
            &Session::new_main("model-z", ORCHESTRATOR_SYSTEM_PROMPT),
        );
        assert!(prompt.user_prompt.contains("Required artifacts: No required artifacts."));
        assert!(!prompt.user_prompt.contains("Required artifacts: decision, reasoning"));
        assert!(!prompt.user_prompt.contains("short routing summary"));
        assert!(prompt.user_prompt.contains("Answer the user's request in natural language."));
    }

    #[test]
    fn orchestrator_spec_has_coordinator_tools_and_permissions() {
        let orchestrator = Orchestrator::new("model-z", ORCHESTRATOR_SYSTEM_PROMPT);
        assert_eq!(orchestrator.spec().name, "Orchestrator");
        assert!(matches!(
            orchestrator.spec().workflow.execution_style,
            ExecutionStyle::RouteOnly
        ));
        assert!(orchestrator.spec().permissions.can_delegate);
        assert!(orchestrator.spec().permissions.can_use_shell);
        assert!(orchestrator.spec().permissions.can_write_files);
        assert!(orchestrator.spec().permissions.can_access_network);
        // Phoenix and every coworker receive the complete capability catalog.
        // Responsibility and judgment govern who owns work; runtime permission
        // and approval gates govern whether a concrete call may execute.
        assert_eq!(
            orchestrator.spec().tool_allowlist.len(),
            crate::tools::universal_agent_tool_names().len()
        );
        // Local MCP meta-tools: the orchestrator can discover/route external
        // MCP providers (T3MP3ST etc.).
        assert!(orchestrator
            .spec()
            .tool_allowlist
            .contains(&"mcp_servers".to_string()));
        assert!(orchestrator
            .spec()
            .tool_allowlist
            .contains(&"mcp_call".to_string()));
        assert!(orchestrator
            .spec()
            .tool_allowlist
            .contains(&"cron".to_string()));
        assert!(orchestrator
            .spec()
            .tool_allowlist
            .contains(&"recall".to_string()));
        assert!(orchestrator
            .spec()
            .tool_allowlist
            .contains(&"agent_provision".to_string()));
        assert!(crate::tools::tool_spec("agent_pipeline").is_none());
        assert!(crate::tools::tool_spec("tools_assign").is_none());
        assert!(orchestrator
            .spec()
            .tool_allowlist
            .contains(&"reverse_skill".to_string()));
        assert!(orchestrator
            .spec()
            .tool_allowlist
            .contains(&"work".to_string()));
        assert!(orchestrator
            .spec()
            .tool_allowlist
            .contains(&"routine".to_string()));
        assert!(orchestrator
            .spec()
            .tool_allowlist
            .contains(&"browser_navigate".to_string()));
        assert!(orchestrator
            .spec()
            .tool_allowlist
            .contains(&"web_search".to_string()));
        assert!(orchestrator
            .spec()
            .tool_allowlist
            .contains(&"vital_memory_write".to_string()));
        assert!(orchestrator
            .spec()
            .tool_allowlist
            .contains(&"final_answer".to_string()));
        assert!(orchestrator
            .spec()
            .tool_allowlist
            .contains(&"talk".to_string()));
        assert!(orchestrator
            .spec()
            .tool_allowlist
            .contains(&"write".to_string()));
        assert!(orchestrator
            .spec()
            .tool_allowlist
            .contains(&"bash".to_string()));
        assert!(orchestrator
            .spec()
            .tool_allowlist
            .contains(&"ask_user".to_string()));
        for tool in ["design_reference", "image_gen", "image_analyze", "ui_snap"] {
            assert!(
                orchestrator.spec().tool_allowlist.iter().any(|t| t == tool),
                "Phoenix is missing universal visual capability `{tool}`"
            );
        }
    }

    #[test]
    fn orchestrator_can_produce_coder_handoff_decision() {
        let orchestrator = Orchestrator::new("model-z", ORCHESTRATOR_SYSTEM_PROMPT);
        let decision = orchestrator.route_to_coder("coding task");

        assert!(matches!(decision.mode, DelegationMode::Handoff));
        assert!(matches!(
            decision.target,
            AgentTarget::Specialist(crate::session::SubAgentType::Coder)
        ));
        assert_eq!(decision.rationale, "coding task");
    }

    #[test]
    fn orchestrator_routes_orchestrator_task_to_coder_in_first_slice() {
        let orchestrator = Orchestrator::new("model-z", ORCHESTRATOR_SYSTEM_PROMPT);
        let task = TaskEnvelope::new(
            "session-z",
            AgentTarget::Orchestrator,
            "Implement runtime",
            "Build the first coding slice.",
        );

        let decision = orchestrator.route_task(&task).unwrap();
        assert!(matches!(decision.mode, DelegationMode::Handoff));
        assert!(matches!(
            decision.target,
            AgentTarget::Specialist(crate::session::SubAgentType::Coder)
        ));
    }

    #[test]
    fn parses_model_friendly_orchestrator_json() {
        let parsed = parse_orchestrator_decision(
            r#"{
              "mode": "handoff",
              "target": { "specialist": "coder" },
              "rationale": "Coding work."
            }"#,
        )
        .unwrap();

        assert!(matches!(parsed.mode, DelegationMode::Handoff));
        assert!(matches!(
            parsed.target,
            AgentTarget::Specialist(crate::session::SubAgentType::Coder)
        ));
        assert_eq!(parsed.rationale, "Coding work.");
    }

    #[test]
    fn rejects_invalid_orchestrator_json() {
        let error = parse_orchestrator_decision("not json").unwrap_err();
        assert!(error
            .to_string()
            .contains("failed to parse orchestrator decision JSON"));
    }
}

#[cfg(test)]
mod async_default_tests {
    use super::*;

    #[test]
    fn orchestrator_prompt_distinguishes_required_and_background_work() {
        assert!(ORCHESTRATOR_SYSTEM_PROMPT.contains("mode is a real execution choice"));
        assert!(ORCHESTRATOR_SYSTEM_PROMPT.contains("mode 1"));
        assert!(ORCHESTRATOR_SYSTEM_PROMPT.contains("integrate the return before you finish"));
        assert!(ORCHESTRATOR_SYSTEM_PROMPT.contains("never stop, wait, poll"));
        assert!(ORCHESTRATOR_SYSTEM_PROMPT.contains("does not justify a second unsolicited final"));
    }

    #[test]
    fn orchestrator_prompt_teaches_accountable_owner_routing() {
        // One durable owner runs the outcome. Maya coordinates only when the
        // work is genuinely cross-functional; tool choice never changes owner.
        assert!(ORCHESTRATOR_SYSTEM_PROMPT.contains("You are the root operator"));
        assert!(ORCHESTRATOR_SYSTEM_PROMPT.contains("one accountable owner"));
        assert!(ORCHESTRATOR_SYSTEM_PROMPT.contains("cross-company coordination"));
        assert!(ORCHESTRATOR_SYSTEM_PROMPT.contains("title is not a capability silo"));
    }

    #[test]
    fn orchestrator_prompt_speaks_like_a_chief_of_staff_not_a_receipt() {
        let prompt = ORCHESTRATOR_SYSTEM_PROMPT;
        assert!(prompt.contains("# How You Sound"));
        assert!(prompt.contains("**You speak for the team in one voice.**"));
        assert!(prompt.contains("**Status first while work runs.**"));
        assert!(prompt.contains("**Stay a step ahead.**"));
        assert!(prompt.contains("**Never make the user your project manager.**"));
        assert!(prompt.contains("## Doesn't sound like"));
        // The prompt models the writing it asks for: no em dashes, and no
        // compiled teammate names (YOUR TEAM is the only roster).
        assert!(!prompt.contains('\u{2014}'), "em dash in the chief of staff prompt");
        for stale in ["Theo", "Robin", "Leon", "Remy", "Rory", "Avery", "Iris", "Leo ", "Maya", "Planner"] {
            assert!(!prompt.contains(stale), "compiled teammate name: {stale}");
        }
    }
}
