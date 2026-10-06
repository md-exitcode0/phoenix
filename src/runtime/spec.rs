//! Shared Phoenix agent framework types.
//!
//! These types define the small common framework every specialist shares,
//! while still allowing each agent to have a very different workflow,
//! tool surface, and output contract.

use serde::{Deserialize, Serialize};

use crate::session::SubAgentType;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSpec {
    pub name: String,
    pub target: AgentTargetSpec,
    pub system_prompt: String,
    pub default_model: String,
    pub tool_allowlist: Vec<String>,
    pub permissions: PermissionProfile,
    pub workflow: WorkflowContract,
    pub output: OutputContract,
}

impl AgentSpec {
    pub fn summary(&self) -> String {
        format!(
            "{} [{}] tools={} output={}",
            self.name,
            self.target.label(),
            self.tool_allowlist.join(", "),
            self.output.label
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentTargetSpec {
    Orchestrator,
    Specialist(SubAgentType),
}

impl AgentTargetSpec {
    pub fn label(&self) -> String {
        match self {
            Self::Orchestrator => "orchestrator".to_string(),
            Self::Specialist(agent) => agent.to_string().to_lowercase(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionProfile {
    pub can_delegate: bool,
    pub can_use_shell: bool,
    pub can_write_files: bool,
    pub can_access_network: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowContract {
    pub execution_style: ExecutionStyle,
    pub must_report_to_orchestrator: bool,
    pub review_required_before_done: bool,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExecutionStyle {
    RouteOnly,
    CodeExecution,
    StagedResearch,
    BrowserAutomation,
    DesignIteration,
    DataAnalysis,
    SecurityAssessment,
    DesktopAutomation,
    AdversarialReview,
    TestEngineering,
    TaskPlanning,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputContract {
    pub label: String,
    pub required_artifacts: Vec<String>,
    pub final_answer_style: String,
}
