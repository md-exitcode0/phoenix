//! First executable Phoenix runtime slice.
//!
//! This runner is intentionally narrow: it proves the shared framework with
//! the first vertical slice of `orchestrator -> coder`.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::librarian::SessionCache;
use crate::orchestrator::Orchestrator;
use crate::providers::{ChatMessage, LLMProvider};
use crate::runtime::delegation::{
    agent_display_name, specialist_from_talk_name, specialist_is_executable,
};
use crate::runtime::limits::AGENT_TEMPERATURE;
use crate::runtime::memory_hooks;
use crate::runtime::prompt::{assemble_prompt_with_context, PromptAssembly};
use crate::runtime::r#loop::{
    native_tool_error_content, native_tool_result_content, push_native_errors_for_calls,
};
use crate::runtime::{
    parse_agent_turn_response, summarize_tool_input, AgentArtifact, AgentOutcome, AgentTarget,
    AgentTurnResponse, ArtifactKind, DelegationMode, FinalResponse, LibrarianPassRecord,
    MemoryBundle, OrchestratorDecision, PendingSpecialists, ProviderTurn, RequestedToolCall,
    SessionScope, TaskEnvelope, ToolCallResult,
};
use crate::runtime::{AskUserHandler, CliEvent, LibrarianPhase, PermissionCheck};
use crate::session::{Message, Session, SessionStore, SubAgentType};
use crate::sub_agents::{
    coder_config, parse_coder_turn_result, specialist_config, CoderToolCall, CoderTurnResult,
};
use crate::tools::{
    tool_definitions_for_agent, BoundedToolOutcome, TalkInput, TalkReplyStatus, TalkResult,
    ToolExecutor,
};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeExecution {
    pub main_session_id: String,
    pub specialist_session_id: String,
    pub main_session_status: PersistenceStatus,
    pub specialist_session_status: PersistenceStatus,
    pub main_session_path: PathBuf,
    pub specialist_session_path: PathBuf,
    pub main_cache_status: PersistenceStatus,
    pub specialist_cache_status: PersistenceStatus,
    pub main_cache_path: PathBuf,
    pub specialist_cache_path: PathBuf,
    pub decision: OrchestratorDecision,
    pub main_bundle: MemoryBundle,
    pub specialist_bundle: MemoryBundle,
    pub librarian_passes: Vec<LibrarianPassRecord>,
    pub specialist_prompt: PromptAssembly,
    pub specialist_outcome: AgentOutcome,
    pub outcome: AgentOutcome,
    pub orchestrator_parse: ParseRecord,
    pub coder_parse: ParseRecord,
}

pub struct AgentRunner {
    memory_root: PathBuf,
    state_root: PathBuf,
    workspace_root: PathBuf,
    /// Controls whether this runner may touch Phoenix's durable Cognee,
    /// session, or session-cache stores. Scaffold diagnostics disable it so a
    /// mock turn cannot contend with the gateway, pollute a real transcript,
    /// or be picked up later by the session-digest worker.
    memory_policy: RunnerMemoryPolicy,
    provider: Arc<dyn LLMProvider>,
    event_tx: Option<mpsc::Sender<CliEvent>>,
    permission_check: Option<PermissionCheck>,
    ask_user_handler: Option<AskUserHandler>,
    /// When set, overrides orchestrator model for specialist (coder) turns.
    specialist_model: Option<String>,
    /// When set, overrides the model used for hidden librarian preload/save/prune passes.
    librarian_model: Option<String>,
    /// Provider for librarian passes + compaction (None = main provider).
    librarian_provider: Option<Arc<dyn LLMProvider>>,
    /// Provider for specialist turns (None = main provider) — brain/executor split.
    specialist_provider: Option<Arc<dyn LLMProvider>>,
    /// Per-agent provider overrides (account-fallback chains) keyed by role.
    agent_providers: std::collections::HashMap<String, Arc<dyn LLMProvider>>,
    /// Reasoning effort (minimal/low/medium/high) for providers that support it.
    reasoning_effort: Option<String>,
    /// Primary model per individual specialist (`[profile.llm.agent_models]`).
    agent_models: std::collections::HashMap<String, String>,
    /// Reasoning effort per lane (`[profile.llm.efforts]`).
    role_efforts: std::collections::HashMap<String, String>,
    role_service_tiers: std::collections::HashMap<String, String>,
    /// User-selected usable context ceiling per lane
    /// (`[profile.llm.context_windows]`).
    role_context_windows: std::collections::HashMap<String, u64>,
    /// Cheap image model for screenshot grounding (None = vision disabled).
    vision: Option<crate::runtime::vision::VisionConfig>,
    /// Acting model is multimodal: attach screenshots natively, skip the
    /// caption sidecar (config `native_vision`).
    native_vision: bool,
    /// Filesystem confinement policy applied to the tool executor.
    permission_mode: crate::tools::PermissionMode,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RunnerMemoryPolicy {
    #[default]
    Enabled,
    DisabledDiagnostic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PersistenceStatus {
    Created,
    Resumed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParseRecord {
    pub label: String,
    pub status: ParseStatus,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParseStatus {
    NotApplicable,
    Parsed,
    FallbackUsed,
    Failed,
}

impl ParseStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::NotApplicable => "not-applicable",
            Self::Parsed => "parsed",
            Self::FallbackUsed => "fallback-used",
            Self::Failed => "failed",
        }
    }
}

impl ParseRecord {
    fn not_applicable(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            status: ParseStatus::NotApplicable,
            detail: detail.into(),
        }
    }

    fn parsed(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            status: ParseStatus::Parsed,
            detail: detail.into(),
        }
    }

    fn fallback(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            status: ParseStatus::FallbackUsed,
            detail: detail.into(),
        }
    }
}

impl PersistenceStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Created => "newly created",
            Self::Resumed => "resumed",
        }
    }
}

impl AgentRunner {
    fn check_tool_permission(&self, _tool_name: &str, _input_summary: &str) -> bool {
        if let Some(ref check) = self.permission_check {
            let allowed = check(_tool_name, _input_summary);
            self.emit(CliEvent::PermissionCheck {
                tool_name: _tool_name.to_string(),
                input_summary: _input_summary.to_string(),
                status: if allowed { "allowed" } else { "denied" }.to_string(),
                detail: if allowed {
                    "Permission callback allowed execution.".to_string()
                } else {
                    "Permission callback denied execution; current runtime still logs instead of prompting."
                        .to_string()
                },
            });
            if !allowed {
                tracing::warn!(
                    "Permission denied for tool `{}`: {}",
                    _tool_name,
                    _input_summary
                );
            }
            allowed
        } else {
            self.emit(CliEvent::PermissionCheck {
                tool_name: _tool_name.to_string(),
                input_summary: _input_summary.to_string(),
                status: "runtime-gated".to_string(),
                detail: "Phoenix recorded a permission receipt for this command; no interactive approval board is wired yet."
                    .to_string(),
            });
            // The compatibility runner has no separate callback by default;
            // ToolExecutor still applies the selected workspace/full-access
            // policy. The production mesh uses its async approval board.
            true
        }
    }

    pub fn new(memory_root: impl Into<PathBuf>, provider: Arc<dyn LLMProvider>) -> Self {
        let memory_root = memory_root.into();
        let state_root = default_state_root(&memory_root);
        let workspace_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            memory_root,
            state_root,
            workspace_root,
            memory_policy: RunnerMemoryPolicy::Enabled,
            provider,
            event_tx: None,
            permission_check: None,
            ask_user_handler: None,
            specialist_model: None,
            librarian_model: None,
            librarian_provider: None,
            specialist_provider: None,
            agent_providers: std::collections::HashMap::new(),
            reasoning_effort: None,
            agent_models: std::collections::HashMap::new(),
            role_efforts: std::collections::HashMap::new(),
            role_service_tiers: std::collections::HashMap::new(),
            role_context_windows: std::collections::HashMap::new(),
            vision: None,
            native_vision: false,
            permission_mode: crate::tools::PermissionMode::Workspace,
        }
    }

    pub fn with_specialist_model(mut self, model: impl Into<String>) -> Self {
        self.specialist_model = Some(model.into());
        self
    }

    /// Cheap image model for screenshot grounding (None = vision disabled).
    pub fn with_vision(mut self, vision: Option<crate::runtime::vision::VisionConfig>) -> Self {
        self.vision = vision;
        self
    }

    /// Acting model is multimodal: attach screenshots natively, skip the
    /// caption sidecar (config `native_vision`).
    pub fn with_native_vision(mut self, enabled: bool) -> Self {
        self.native_vision = enabled;
        self
    }

    pub fn with_librarian_model(mut self, model: impl Into<String>) -> Self {
        self.librarian_model = Some(model.into());
        self
    }

    pub fn with_librarian_provider(mut self, provider: Option<Arc<dyn LLMProvider>>) -> Self {
        self.librarian_provider = provider;
        self
    }

    /// Set the runner's durable-memory policy. Normal and `--real` turns keep
    /// the default; deterministic scaffold diagnostics keep both memory and
    /// their synthetic session state in-process only.
    pub fn with_memory_policy(mut self, policy: RunnerMemoryPolicy) -> Self {
        self.memory_policy = policy;
        self
    }

    pub fn with_specialist_provider(mut self, provider: Option<Arc<dyn LLMProvider>>) -> Self {
        self.specialist_provider = provider;
        self
    }

    pub fn with_agent_providers(
        mut self,
        providers: std::collections::HashMap<String, Arc<dyn LLMProvider>>,
    ) -> Self {
        self.agent_providers = providers;
        self
    }

    /// Provider used for librarian passes (falls back to the main provider).
    fn librarian_provider(&self) -> Arc<dyn LLMProvider> {
        self.librarian_provider
            .clone()
            .unwrap_or_else(|| Arc::clone(&self.provider))
    }

    /// Reasoning effort to request from providers that support it (e.g. Codex).
    /// `None` leaves the provider default.
    pub fn with_reasoning_effort(mut self, effort: Option<String>) -> Self {
        self.reasoning_effort = effort.filter(|e| !e.trim().is_empty());
        self
    }

    /// Primary model per individual specialist (`[profile.llm.agent_models]`).
    pub fn with_agent_models(mut self, models: std::collections::HashMap<String, String>) -> Self {
        self.agent_models = models;
        self
    }

    /// Per-lane reasoning efforts (`[profile.llm.efforts]`).
    pub fn with_role_efforts(mut self, efforts: std::collections::HashMap<String, String>) -> Self {
        self.role_efforts = efforts;
        self
    }

    pub fn with_role_service_tiers(mut self, tiers: std::collections::HashMap<String, String>) -> Self {
        self.role_service_tiers = tiers;
        self
    }

    pub fn with_role_context_windows(
        mut self,
        windows: std::collections::HashMap<String, u64>,
    ) -> Self {
        self.role_context_windows = windows;
        self
    }

    fn librarian_model<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.librarian_model.as_deref().unwrap_or(fallback)
    }

    /// Set an event channel for real-time CLI rendering.
    pub fn with_event_channel(mut self, tx: mpsc::Sender<CliEvent>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    /// Set a permission check callback for risky commands.
    pub fn with_permission_check(mut self, check: PermissionCheck) -> Self {
        self.permission_check = Some(check);
        self
    }

    pub fn with_ask_user_handler(mut self, handler: AskUserHandler) -> Self {
        self.ask_user_handler = Some(handler);
        self
    }

    fn emit(&self, event: CliEvent) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.try_send(event);
        }
    }

    fn build_tool_executor(&self) -> Result<ToolExecutor> {
        let mut executor = ToolExecutor::new(&self.workspace_root)?
            .with_permission_mode(self.permission_mode)
            .with_state_root(&self.state_root)
            .with_agent("phoenix")
            .with_browser_instance(Some("agent-phoenix"));
        if let Some(handler) = &self.ask_user_handler {
            executor = executor.with_ask_user_handler(handler.clone());
        }
        Ok(executor)
    }

    /// Emit LibrarianPass events for all recorded passes.
    fn emit_librarian_pass(&self, pass: &LibrarianPassRecord) {
        // Prune is archived (disabled) — it does no work, so don't surface it.
        if matches!(pass.phase, LibrarianPhase::Prune) {
            return;
        }
        self.emit(CliEvent::LibrarianPass {
            phase: match pass.phase {
                LibrarianPhase::Preload => "preload",
                LibrarianPhase::Prune => "prune",
                LibrarianPhase::Save => "save",
            }
            .to_string(),
            scope: match &pass.session_scope {
                SessionScope::Main => "main".to_string(),
                SessionScope::Specialist(st) => st.to_string().to_lowercase(),
            },
            loaded_count: pass.memory_paths.len() + pass.knowledge_paths.len(),
            saved_count: pass.saved_memory_paths.len(),
            pruned_count: pass.pruned_message_count,
            receipts: pass.receipts.clone(),
            summary: if pass.summary.is_empty() {
                format!(
                    "{}: {} memory(s), {} save(s)",
                    match pass.phase {
                        LibrarianPhase::Preload => "Preload".to_string(),
                        LibrarianPhase::Prune =>
                            format!("Pruned {} message(s)", pass.pruned_message_count),
                        LibrarianPhase::Save => {
                            if pass.saved_memory_paths.is_empty() {
                                "Skipped save".to_string()
                            } else {
                                format!("Saved {} file(s)", pass.saved_memory_paths.len())
                            }
                        }
                    },
                    pass.memory_paths.len(),
                    pass.saved_memory_paths.len()
                )
            } else {
                pass.summary.clone()
            },
        });
    }

    fn emit_librarian_passes(&self, passes: &[LibrarianPassRecord]) {
        for pass in passes {
            self.emit_librarian_pass(pass);
        }
    }

    pub fn with_workspace(
        memory_root: impl Into<PathBuf>,
        workspace_root: impl Into<PathBuf>,
        provider: Arc<dyn LLMProvider>,
    ) -> Self {
        let memory_root = memory_root.into();
        let state_root = default_state_root(&memory_root);
        Self {
            memory_root,
            state_root,
            workspace_root: workspace_root.into(),
            memory_policy: RunnerMemoryPolicy::Enabled,
            provider,
            event_tx: None,
            permission_check: None,
            ask_user_handler: None,
            specialist_model: None,
            librarian_model: None,
            librarian_provider: None,
            specialist_provider: None,
            agent_providers: std::collections::HashMap::new(),
            reasoning_effort: None,
            agent_models: std::collections::HashMap::new(),
            role_efforts: std::collections::HashMap::new(),
            role_service_tiers: std::collections::HashMap::new(),
            role_context_windows: std::collections::HashMap::new(),
            vision: None,
            native_vision: false,
            permission_mode: crate::tools::PermissionMode::Workspace,
        }
    }

    /// Set the filesystem confinement policy (Workspace default, or Yolo).
    pub fn with_permission_mode(mut self, mode: crate::tools::PermissionMode) -> Self {
        self.permission_mode = mode;
        self
    }
}

mod legacy_slices;
mod provider_loop;
mod slices;
pub(crate) use slices::persist_group_user_boundary;
mod talk;

mod support;
pub(crate) use support::*;

#[cfg(test)]
#[path = "../runner_tests.rs"]
mod runner_tests;
