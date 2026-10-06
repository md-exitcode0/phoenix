//! PhoenixAgent Library
//!
//! PhoenixAgent shared library.

pub mod auth;
pub mod channels;
pub mod codegraph;
pub mod config;
pub mod cron;
pub mod debug_session;
pub mod librarian;
pub mod notifications;
pub mod onboarding;
pub mod orchestrator;
pub mod providers;
pub mod runtime;
pub mod security;
pub mod session;
pub mod settings;
pub mod sub_agents;
pub mod tools;
pub mod voice;
pub(crate) mod vital_memory_document;

// Re-export commonly used types
pub use config::{LLMProfile, PhoenixConfig, Profile};
pub use librarian::{GroundingLabel, KnowledgeDoc, LoadedMemories, MemoryEntry, SessionCache};
pub use orchestrator::Orchestrator;
pub use providers::{
    ChatMessage, CompletionRequest, CompletionResponse, ContractAuthType, LLMProvider, MessageRole,
    ProviderFactory,
};
pub use runtime::{
    AgentArtifact, AgentOutcome, AgentRunner, AgentRuntime, AgentSpec, AgentTarget,
    AgentTargetSpec, AgentTurnResponse, ArtifactKind, ContextItem, DelegationMode, ExecutionStyle,
    FinalResponse, LibrarianPassRecord, LibrarianPhase, MemoryBundle, OrchestratorDecision,
    OutputContract, PermissionProfile, ProviderTurn, RequestedToolCall, RuntimeExecution,
    SessionScope, TaskEnvelope, ToolCall, ToolCallResult, ToolSpec, TraceContext,
    TurnToolTranscriptEntry, WorkflowContract,
};
pub use session::{Message, Session, SessionStore, SubAgentType};
pub use sub_agents::{parse_coder_turn_result, CoderToolCall, CoderTurnResult};
