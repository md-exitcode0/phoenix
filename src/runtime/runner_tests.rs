use super::*;
use async_trait::async_trait;

use crate::providers::contracts::NativeToolCall;

#[test]
fn outcome_preserves_runtime_interruption_and_legacy_unknown_status() {
    for mode in ["provider_failure_with_preserved_evidence", "provider_error_fallback",
        "runtime_boundary_with_preserved_evidence", "no_progress_repeat_guard", "incomplete_work", "provider_validation_fallback"] {
        let response = FinalResponse { summary:"Saved work".into(), final_markdown:"The saved file is available.".into(),
            changes_made:vec![], verification:vec![], execution_mode:mode.into(), tool_transcript:vec![] };
        let outcome=agent_outcome_from_final(AgentTarget::Orchestrator,"Reply",response,None);
        assert_eq!(outcome.completion,crate::runtime::OutcomeCompletion::Incomplete,"{mode}");
        assert_eq!(outcome.summary,"The saved file is available.");
        let mut serialized=serde_json::to_value(&outcome).unwrap();
        assert_eq!(serialized["completion"],"incomplete");
        serialized.as_object_mut().unwrap().remove("completion");
        assert_eq!(serde_json::from_value::<AgentOutcome>(serialized).unwrap().completion,crate::runtime::OutcomeCompletion::Unknown);
    }
    let response=FinalResponse { summary:"Done".into(), final_markdown:"Verified result.".into(), changes_made:vec![],
        verification:vec!["Checked saved artifact".into()],execution_mode:"provider_tools_final_answer".into(),tool_transcript:vec![] };
    assert_eq!(agent_outcome_from_final(AgentTarget::Orchestrator,"Reply",response,None).completion,crate::runtime::OutcomeCompletion::Completed);
}

#[test]
fn assistant_session_content_strips_visible_reasoning_blocks() {
    let response = crate::providers::CompletionResponse {
        content:
            "[Reasoning]\nprivate scratchpad\n[/Reasoning]\nNative tool request: final_answer({})"
                .to_string(),
        model: "mock".to_string(),
        usage: crate::providers::TokenUsage::new(1, 1),
        reasoning: Some("hidden provider reasoning".to_string()),
        stop_reason: Some("stop".to_string()),
        tool_calls: vec![],
        provider_replay: None,
    };

    let content = assistant_session_content(&response);

    assert!(!content.contains("[Reasoning]"));
    assert!(!content.contains("private scratchpad"));
    assert!(!content.contains("hidden provider reasoning"));
    // This fixture has NO tool_calls — the string is text the model itself
    // typed, and it passes through untouched. Phoenix must not rewrite what a
    // model actually said; the bug was Phoenix ADDING that line, covered below.
    assert!(content.contains("Native tool request"));
}

#[test]
fn assistant_session_content_strips_inline_reasoning_tags() {
    let response = crate::providers::CompletionResponse {
        content: "<think>private chain of thought</think>Visible answer".to_string(),
        model: "mock".to_string(),
        usage: crate::providers::TokenUsage::new(1, 1),
        reasoning: None,
        stop_reason: Some("stop".to_string()),
        tool_calls: vec![],
        provider_replay: None,
    };

    let content = assistant_session_content(&response);
    assert_eq!(content, "Visible answer");
    assert!(!content.contains("private chain of thought"));
}

#[test]
fn plain_text_final_never_wraps_inline_reasoning() {
    let response = coerce_plain_text_final_response(
        &AgentTarget::Orchestrator,
        "<think>private planning scratchpad</think>The verified result.",
        &[],
    )
    .expect("visible answer remains a valid provider-native final");

    assert_eq!(response.final_markdown, "The verified result.");
    assert!(!response
        .final_markdown
        .contains("private planning scratchpad"));
}

#[test]
fn plain_text_promise_is_not_mistaken_for_completed_work() {
    assert!(coerce_plain_text_final_response(
        &AgentTarget::Specialist(crate::session::SubAgentType::Coder),
        "Let me download it now and complete the task.",
        &[],
    )
    .is_none());
}

#[test]
fn a_tool_call_is_never_narrated_into_the_stored_content() {
    // The root cause of the transcript-reply bug. Stored assistant content is
    // replayed into the next turn as plain prose (prompt.rs: "Assistant\n{}"),
    // so synthesizing "Native tool request: read({...})" into it taught the
    // model that assistant turns look like transcripts. After a compaction fold
    // one session held 68 such messages and replies degenerated into
    // transcripts. The call travels structurally instead.
    let response = crate::providers::CompletionResponse {
        content: "Reading the file now.".to_string(),
        model: "mock".to_string(),
        usage: crate::providers::TokenUsage::new(1, 1),
        reasoning: None,
        stop_reason: Some("tool_calls".to_string()),
        tool_calls: vec![NativeToolCall {
            id: "call-1".to_string(),
            tool_name: "read".to_string(),
            arguments: serde_json::json!({"path": "/a"}),
        }],
        provider_replay: None,
    };

    let content = assistant_session_content(&response);
    assert_eq!(content, "Reading the file now.");
    assert!(!content.contains("Native tool request"));
    assert!(!content.contains("read("));
}

#[test]
fn pinning_persists_memory_dedups_and_refreshes_on_edit() {
    use crate::librarian::{GroundingLabel, LoadedMemories, MemoryEntry};
    let dir = tempfile::tempdir().unwrap();
    let mem_root = dir.path().join("memory");
    std::fs::create_dir_all(mem_root.join("HOT/projects")).unwrap();
    let file = mem_root.join("HOT/projects/X.md");
    std::fs::write(&file, "original content").unwrap();
    let path = "memory/HOT/projects/X.md";

    let mut store = SessionStore::new(dir.path().join("sessions"));
    let _ = store.load_or_create_main("sid", "m", "sys").unwrap();
    let mut cache = SessionCache::new("sid");

    let one = |content: &str| LoadedMemories {
        memories: vec![MemoryEntry {
            path: path.to_string(),
            content: content.to_string(),
            excerpt: String::new(),
            grounding: GroundingLabel::Retrieved,
            score: 0.9,
            reasons: vec![],
            tier: "HOT".to_string(),
        }],
        knowledge_docs: vec![],
        ranked_context_items: vec![],
        omitted_items: vec![],
        grounding_receipts: vec![],
        trust_receipts: vec![],
        completion_state: String::new(),
        open_questions: vec![],
        recommended_next_agent_or_tool: None,
        context_budget_used: 0,
    };

    // First turn: pin the loaded memory; ephemeral copy is cleared.
    let mut loaded = one("original content");
    pin_loaded_memories_into_session(&mut store, "sid", &mem_root, &mut loaded, &mut cache);
    assert!(loaded.memories.is_empty(), "ephemeral memories cleared");
    let session = store.get("sid").unwrap();
    assert_eq!(session.pinned_memory.len(), 1);
    assert_eq!(session.pinned_memory[0].content, "original content");
    assert_eq!(cache.persisted_memory, vec![path.to_string()]);

    // Next turn: librarian loads NOTHING (it's pinned), but the file was
    // edited on disk → the pinned copy refreshes, and it is not duplicated.
    std::fs::write(&file, "EDITED content").unwrap();
    let mut empty = one("ignored");
    empty.memories.clear();
    pin_loaded_memories_into_session(&mut store, "sid", &mem_root, &mut empty, &mut cache);
    let session = store.get("sid").unwrap();
    assert_eq!(session.pinned_memory.len(), 1, "deduped, not re-added");
    assert_eq!(
        session.pinned_memory[0].content, "EDITED content",
        "edit on disk refreshed the pinned copy"
    );
}

#[test]
fn phatic_messages_skip_librarian() {
    for msg in [
        "thanks!",
        "ok cool",
        "Thank you so much!!",
        "great work bro",
        "hi!",
        "wow that was amazing",
    ] {
        assert!(is_phatic_message(msg), "{msg} should be phatic");
    }
}

#[test]
fn actionable_messages_keep_librarian() {
    for msg in [
        "thanks, now fix the failing test",
        "ok run the build",
        "design me a website",
        "great — what about the gateway logs?",
        "remember that I prefer dark themes",
        "",
    ] {
        assert!(!is_phatic_message(msg), "{msg:?} should NOT be phatic");
    }
}

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::tempdir;

use crate::providers::{
    CompletionRequest, CompletionResponse, ContractAuthType, MessageRole, ModelInfo,
    StreamingResponse, TokenUsage,
};
use crate::runtime::{ContextItem, LibrarianPhase};

struct MockProvider;
struct SequencedProvider {
    calls: AtomicUsize,
    orchestrator_content: String,
    coder_content: String,
    native_images: bool,
    native_script: std::sync::Mutex<std::collections::VecDeque<String>>,
    native_requests: std::sync::Mutex<Vec<CompletionRequest>>,
}

struct ThreeStepProvider {
    calls: AtomicUsize,
    first: String,
    second: String,
    third: String,
}

impl SequencedProvider {
    fn new(orchestrator_content: impl Into<String>, coder_content: impl Into<String>) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            orchestrator_content: orchestrator_content.into(),
            coder_content: coder_content.into(),
            native_images: false,
            native_script: std::sync::Mutex::new(std::collections::VecDeque::new()),
            native_requests: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl ThreeStepProvider {
    fn new(first: impl Into<String>, second: impl Into<String>, third: impl Into<String>) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            first: first.into(),
            second: second.into(),
            third: third.into(),
        }
    }
}

#[async_trait]
impl LLMProvider for MockProvider {
    fn name(&self) -> &str {
        "mock"
    }

    fn display_name(&self) -> &str {
        "Mock"
    }

    fn base_url(&self) -> &str {
        "mock://provider"
    }

    fn auth_type(&self) -> ContractAuthType {
        ContractAuthType::None
    }

    fn env_vars(&self) -> Vec<&str> {
        vec![]
    }

    fn default_headers(&self) -> HashMap<String, String> {
        HashMap::new()
    }

    fn has_model(&self, _model: &str) -> bool {
        true
    }

    fn default_model(&self) -> &str {
        "mock-model"
    }

    fn fallback_models(&self) -> Vec<&str> {
        vec![]
    }

    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        if request.messages.iter().any(|message| {
            message.role == MessageRole::System && message.content.contains("You are the Librarian")
        }) {
            return Ok(CompletionResponse {
                content: librarian_test_response(&request),
                model: request.model,
                usage: TokenUsage::new(5, 8),
                reasoning: None,
                stop_reason: Some("stop".to_string()),
                tool_calls: vec![],
                provider_replay: None,
            });
        }

        let user_message = request
            .messages
            .iter()
            .find(|message| message.role == MessageRole::User)
            .map(|message| message.content.clone())
            .unwrap_or_default();

        Ok(CompletionResponse {
            content: serde_json::json!({
                "summary": "Coder handled the scaffold task.",
                "final_markdown": format!(
                    "## Result\nScaffold coder handled the task.\n\n### Prompt Snapshot\n{}",
                    user_message.lines().take(6).collect::<Vec<_>>().join("\n")
                ),
                "changes_made": [
                    "Parsed the scaffold prompt into a structured coder result.",
                    "Returned polished Markdown separately from internal result fields."
                ],
                "verification": [
                    "Scaffold execution only; no real tools were run."
                ],
                "execution_mode": "scaffold_no_tools",
                "tool_transcript": []
            })
            .to_string(),
            model: request.model,
            usage: TokenUsage::new(10, 20),
            reasoning: None,
            stop_reason: Some("stop".to_string()),
            tool_calls: vec![],
            provider_replay: None,
        })
    }

    async fn stream(&self, _request: CompletionRequest) -> anyhow::Result<StreamingResponse> {
        Ok(StreamingResponse {
            content: String::new(),
            reasoning: None,
            done: true,
        })
    }

    async fn embeddings(&self, _texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(vec![])
    }

    async fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        Ok(vec![])
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

#[async_trait]
impl LLMProvider for SequencedProvider {
    fn supports_native_images(&self) -> bool { self.native_images }
    fn name(&self) -> &str {
        "sequenced"
    }

    fn display_name(&self) -> &str {
        "Sequenced"
    }

    fn base_url(&self) -> &str {
        "mock://sequenced"
    }

    fn auth_type(&self) -> ContractAuthType {
        ContractAuthType::None
    }

    fn env_vars(&self) -> Vec<&str> {
        vec![]
    }

    fn default_headers(&self) -> HashMap<String, String> {
        HashMap::new()
    }

    fn has_model(&self, _model: &str) -> bool {
        true
    }

    fn default_model(&self) -> &str {
        "mock-model"
    }

    fn fallback_models(&self) -> Vec<&str> {
        vec![]
    }

    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        if request.messages.iter().any(|message| {
            message.role == MessageRole::System && message.content.contains("You are the Librarian")
        }) {
            return Ok(CompletionResponse {
                content: librarian_test_response(&request),
                model: request.model,
                usage: TokenUsage::new(5, 8),
                reasoning: None,
                stop_reason: Some("stop".to_string()),
                tool_calls: vec![],
                provider_replay: None,
            });
        }

        let call_index = self.calls.fetch_add(1, Ordering::SeqCst);
        let mut content = if self.native_images {
            self.native_requests.lock().unwrap().push(request.clone());
            self.native_script.lock().unwrap().pop_front().expect("native image script exhausted")
        } else if call_index == 0 {
            self.orchestrator_content.clone()
        } else {
            self.coder_content.clone()
        };
        let tool_calls = if self.native_images {
            if let Ok(AgentTurnResponse::ToolRequest { tool_calls, .. }) = serde_json::from_str(&content) {
                content.clear();
                tool_calls.into_iter().enumerate().map(|(index, call)| NativeToolCall {
                    id: format!("image-{call_index}-{index}"), tool_name:call.tool_name, arguments:call.input,
                }).collect()
            } else { Vec::new() }
        } else { Vec::new() };

        Ok(CompletionResponse {
            content,
            model: request.model,
            usage: TokenUsage::new(11 + call_index as u32, 21 + call_index as u32),
            reasoning: None,
            stop_reason: Some("stop".to_string()),
            tool_calls,
            provider_replay: None,
        })
    }

    async fn stream(&self, _request: CompletionRequest) -> anyhow::Result<StreamingResponse> {
        Ok(StreamingResponse {
            content: String::new(),
            reasoning: None,
            done: true,
        })
    }

    async fn embeddings(&self, _texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(vec![])
    }

    async fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        Ok(vec![])
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

#[async_trait]
impl LLMProvider for ThreeStepProvider {
    fn name(&self) -> &str {
        "three-step"
    }

    fn display_name(&self) -> &str {
        "ThreeStep"
    }

    fn base_url(&self) -> &str {
        "mock://three-step"
    }

    fn auth_type(&self) -> ContractAuthType {
        ContractAuthType::None
    }

    fn env_vars(&self) -> Vec<&str> {
        vec![]
    }

    fn default_headers(&self) -> HashMap<String, String> {
        HashMap::new()
    }

    fn has_model(&self, _model: &str) -> bool {
        true
    }

    fn default_model(&self) -> &str {
        "mock-model"
    }

    fn fallback_models(&self) -> Vec<&str> {
        vec![]
    }

    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        if request.messages.iter().any(|message| {
            message.role == MessageRole::System && message.content.contains("You are the Librarian")
        }) {
            return Ok(CompletionResponse {
                content: librarian_test_response(&request),
                model: request.model,
                usage: TokenUsage::new(5, 8),
                reasoning: None,
                stop_reason: Some("stop".to_string()),
                tool_calls: vec![],
                provider_replay: None,
            });
        }

        let call_index = self.calls.fetch_add(1, Ordering::SeqCst);
        let content = match call_index {
            0 => self.first.clone(),
            1 => self.second.clone(),
            _ => self.third.clone(),
        };

        Ok(CompletionResponse {
            content,
            model: request.model,
            usage: TokenUsage::new(11 + call_index as u32, 21 + call_index as u32),
            reasoning: None,
            stop_reason: Some("stop".to_string()),
            tool_calls: vec![],
            provider_replay: None,
        })
    }

    async fn stream(&self, _request: CompletionRequest) -> anyhow::Result<StreamingResponse> {
        Ok(StreamingResponse {
            content: String::new(),
            reasoning: None,
            done: true,
        })
    }

    async fn embeddings(&self, _texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(vec![])
    }

    async fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        Ok(vec![])
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

fn librarian_test_response(request: &CompletionRequest) -> String {
    let user_message = request
        .messages
        .iter()
        .find(|message| message.role == MessageRole::User)
        .map(|message| message.content.as_str())
        .unwrap_or_default();

    if user_message.contains("Phase: preload")
        && user_message.contains("No librarian tools used yet.")
        && user_message.contains("Runtime memory store survey")
        && !user_message.contains("memory_search(")
    {
        return serde_json::json!({
                "type": "tool_request",
                "tool_calls": [
                    { "tool_name": "memory_search", "input": { "query": "Phoenix routing coder orchestrator", "limit": 5 } }
                ],
                "rationale": "Survey is preloaded; search memory for routing context."
            })
            .to_string();
    }

    if user_message.contains("Phase: preload")
        && user_message.contains("memory_search(")
        && user_message.contains("routing.md")
        && !user_message.contains("memory_read(")
    {
        return serde_json::json!({
            "type": "tool_request",
            "tool_calls": [
                { "tool_name": "memory_read", "input": { "path": "HOT/learnings/routing.md" } }
            ],
            "rationale": "Read the routing memory before injecting."
        })
        .to_string();
    }

    if user_message.contains("Phase: preload")
        && user_message.contains("memory_read(")
        && user_message.contains("Phoenix routes code tasks")
    {
        return serde_json::json!({
            "type": "final",
            "loaded_memory_paths": ["memory/HOT/learnings/routing.md"],
            "loaded_knowledge_paths": [],
            "saved_memory_paths": [],
            "omitted_items": [],
            "receipts": ["memory_search", "memory_read"],
            "context_budget_used": 1,
            "pruned_message_count": 0,
            "pruned_message_indices": [],
            "summary": "Preloaded routing memory."
        })
        .to_string();
    }

    if user_message.contains("Phase: prune")
        && user_message.contains("No librarian tools used yet.")
    {
        return serde_json::json!({
                "type": "tool_request",
                "tool_calls": [
                    { "tool_name": "session_tool_outputs", "input": { "min_output_chars": 120, "limit": 20 } }
                ],
                "rationale": "Inspect long tool outputs before pruning."
            })
            .to_string();
    }

    if user_message.contains("Phase: prune") && user_message.contains("session_tool_outputs") {
        return serde_json::json!({
                "type": "tool_request",
                "tool_calls": [
                    { "tool_name": "session_prune", "input": { "indices": [0], "max_tool_result_chars": 240 } }
                ],
                "rationale": "Prune the long tool output into a compact receipt."
            })
            .to_string();
    }

    if user_message.contains("Phase: prune")
        && user_message.contains("Pruned 1 tool-result message(s).")
    {
        return serde_json::json!({
            "type": "final",
            "loaded_memory_paths": [],
            "loaded_knowledge_paths": [],
            "saved_memory_paths": [],
            "omitted_items": [],
            "receipts": ["Session prune tool executed."],
            "context_budget_used": 0,
            "pruned_message_count": 1,
            "pruned_message_indices": [0],
            "summary": "Librarian pruned session tool chatter by tool request."
        })
        .to_string();
    }

    if user_message.contains("Phase: save")
        && user_message.contains("No librarian tools used yet.")
        && !user_message.contains("session_tail(")
    {
        return serde_json::json!({
                "type": "tool_request",
                "tool_calls": [
                    { "tool_name": "session_tail", "input": { "limit": 8, "include_tool_outputs": false } }
                ],
                "rationale": "Inspect recent session before deciding to save."
            })
            .to_string();
    }

    if user_message.contains("Phase: save")
        && user_message.contains("session_tail(")
        && !user_message.contains("Wrote memory/WARM/projects/PHOENIX_PROJECT.md")
    {
        let content = "# Phoenix Project\n\nObserved: Phoenix project structure uses a Rust CLI and hidden librarian runtime.\n";
        return serde_json::json!({
            "type": "tool_request",
            "tool_calls": [
                {
                    "tool_name": "memory_write",
                    "input": {
                        "path": "WARM/projects/PHOENIX_PROJECT.md",
                        "content": content,
                        "overwrite": true
                    }
                }
            ],
            "rationale": "Save stable project knowledge to a canonical project memory."
        })
        .to_string();
    }

    if user_message.contains("Phase: save")
        && user_message.contains("Wrote memory/WARM/projects/PHOENIX_PROJECT.md")
    {
        return serde_json::json!({
            "type": "final",
            "loaded_memory_paths": [],
            "loaded_knowledge_paths": [],
            "saved_memory_paths": ["memory/WARM/projects/PHOENIX_PROJECT.md"],
            "omitted_items": [],
            "receipts": ["Saved canonical Phoenix project memory."],
            "context_budget_used": 0,
            "pruned_message_count": 0,
            "pruned_message_indices": [],
            "summary": "Librarian saved one canonical project memory."
        })
        .to_string();
    }

    serde_json::json!({
        "type": "final",
        "loaded_memory_paths": [],
        "loaded_knowledge_paths": [],
        "saved_memory_paths": [],
        "omitted_items": ["No durable or relevant memory action was needed."],
        "receipts": ["Librarian inspected available context and skipped action."],
        "context_budget_used": 0,
        "pruned_message_count": 0,
        "pruned_message_indices": [],
        "summary": "Librarian completed without loading or saving memory."
    })
    .to_string()
}

#[tokio::test]
async fn executes_orchestrator_to_coder_first_slice() {
    let root_dir = tempdir().unwrap();
    let root = root_dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("HOT").join("learnings")).unwrap();
    std::fs::write(
        root.join("HOT").join("learnings").join("routing.md"),
        "# Routing\n\nPhoenix routes code tasks to coder.",
    )
    .unwrap();

    let runner = AgentRunner::new(root.clone(), Arc::new(MockProvider));
    let orchestrator = Orchestrator::new("mock-model", "Route work only.");
    let mut task = TaskEnvelope::new(
        "session-main",
        AgentTarget::Orchestrator,
        "Fix runtime wiring",
        "Update the Rust runtime slice for coder execution.",
    );
    task.context.push(ContextItem {
        label: "Priority".to_string(),
        content: "foundation".to_string(),
    });

    let execution = runner
        .execute_first_slice(&orchestrator, &task)
        .await
        .unwrap();

    assert_eq!(execution.main_session_id, "session-main");
    assert_eq!(execution.main_session_status, PersistenceStatus::Created);
    assert_eq!(execution.specialist_session_id, "session-main__coder");
    assert_eq!(
        execution.specialist_session_status,
        PersistenceStatus::Created
    );
    assert_eq!(execution.main_cache_status, PersistenceStatus::Created);
    assert_eq!(
        execution.specialist_cache_status,
        PersistenceStatus::Created
    );
    assert!(matches!(execution.decision.mode, DelegationMode::Handoff));
    assert!(matches!(
        execution.decision.target,
        AgentTarget::Specialist(SubAgentType::Coder)
    ));
    assert!(execution
        .specialist_prompt
        .user_prompt
        .contains("Loaded memories"));
    assert!(execution.specialist_outcome.summary.contains("## Result"));
    assert!(execution.outcome.summary.contains("## Result"));
    let provider_output = &execution
        .specialist_outcome
        .provider_response
        .as_ref()
        .unwrap()
        .output_text;
    assert!(provider_output.contains("\"final_markdown\""));
    assert_ne!(provider_output, &execution.specialist_outcome.summary);
    assert!(matches!(execution.outcome.agent, AgentTarget::Orchestrator));
    assert_eq!(execution.librarian_passes.len(), 6);
    assert!(execution
        .librarian_passes
        .iter()
        .any(|record| matches!(record.phase, LibrarianPhase::Prune)));
    assert!(matches!(
        execution.main_bundle.session_scope,
        SessionScope::Main
    ));
    assert!(matches!(
        execution.specialist_bundle.session_scope,
        SessionScope::Specialist(SubAgentType::Coder)
    ));
    let final_save = execution.librarian_passes.last().unwrap();
    let save_confirmed = final_save
        .saved_memory_paths
        .iter()
        .any(|path| path.contains("cognee://remember"));
    assert!(
        save_confirmed
            || final_save
                .receipts
                .iter()
                .any(|receipt| receipt.contains("DURABILITY NOT CONFIRMED")),
        "the save pass must report either confirmed storage or honest degradation: {:?}",
        final_save.receipts
    );
    assert_eq!(execution.specialist_outcome.tool_results.len(), 1);
    assert!(execution
        .specialist_outcome
        .artifacts
        .iter()
        .any(|artifact| {
            artifact.title == "Execution transcript"
                && artifact.body.contains("No tool calls were executed")
        }));
    assert_eq!(execution.outcome.tool_results.len(), 2);

    let session_root = root.join(".phoenix").join("sessions");
    assert!(session_root.join("session-main.json").exists());
    assert!(session_root.join("session-main__coder.json").exists());
    let cache_root = root.join(".phoenix").join("session_cache");
    assert!(cache_root.join("session-main.json").exists());
    assert!(cache_root.join("session-main__coder.json").exists());
}

#[test]
fn runner_memory_policy_defaults_enabled_and_can_be_disabled_explicitly() {
    let root_dir = tempdir().unwrap();
    let runner = AgentRunner::new(root_dir.path(), Arc::new(MockProvider));
    assert_eq!(runner.memory_policy, RunnerMemoryPolicy::Enabled);

    let runner = runner.with_memory_policy(RunnerMemoryPolicy::DisabledDiagnostic);
    assert_eq!(runner.memory_policy, RunnerMemoryPolicy::DisabledDiagnostic);
}

#[tokio::test]
async fn disabled_diagnostic_policy_keeps_six_passes_without_durable_io() {
    let home_parent = tempdir().unwrap();
    let phoenix_home = home_parent.path().join(".phoenix");
    std::fs::create_dir(&phoenix_home).unwrap();
    let memory_root = phoenix_home.join("memory");
    let workspace_dir = tempdir().unwrap();
    let _home_guard = crate::config::test_env::PhoenixHomeGuard::set_private(&phoenix_home);

    let runner = AgentRunner::with_workspace(
        memory_root.clone(),
        workspace_dir.path(),
        Arc::new(MockProvider),
    )
    .with_memory_policy(RunnerMemoryPolicy::DisabledDiagnostic);
    let orchestrator = Orchestrator::new("mock-model", "Route work only.");
    let task = TaskEnvelope::new(
        "scaffold-memory-disabled",
        AgentTarget::Orchestrator,
        "Verify scaffold isolation",
        "Return a deterministic success receipt without durable memory.",
    );

    let execution = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        runner.execute_first_slice(&orchestrator, &task),
    )
    .await
    .expect("disabled diagnostic memory must not wait on Cognee")
    .unwrap();

    assert_eq!(execution.librarian_passes.len(), 6);
    assert_eq!(
        execution
            .librarian_passes
            .iter()
            .map(|pass| pass.phase.clone())
            .collect::<Vec<_>>(),
        vec![
            LibrarianPhase::Preload,
            LibrarianPhase::Preload,
            LibrarianPhase::Prune,
            LibrarianPhase::Save,
            LibrarianPhase::Prune,
            LibrarianPhase::Save,
        ]
    );
    for pass in &execution.librarian_passes {
        assert!(pass.memory_paths.is_empty());
        assert!(pass.knowledge_paths.is_empty());
        assert!(pass.saved_memory_paths.is_empty());
        assert_eq!(pass.receipts.len(), 1);
        assert!(pass.receipts[0].contains("scaffold diagnostic"));
        assert!(pass.receipts[0].contains("intentionally disabled"));
    }
    assert!(execution.main_bundle.loaded_memory_paths.is_empty());
    assert!(execution.main_bundle.loaded_knowledge_paths.is_empty());
    assert!(execution.specialist_bundle.loaded_memory_paths.is_empty());
    assert!(execution
        .specialist_bundle
        .loaded_knowledge_paths
        .is_empty());
    assert_eq!(execution.main_session_status, PersistenceStatus::Created);
    assert_eq!(
        execution.specialist_session_status,
        PersistenceStatus::Created
    );
    assert_eq!(execution.main_cache_status, PersistenceStatus::Created);
    assert_eq!(
        execution.specialist_cache_status,
        PersistenceStatus::Created
    );
    assert!(execution.main_session_path.as_os_str().is_empty());
    assert!(execution.specialist_session_path.as_os_str().is_empty());
    assert!(execution.main_cache_path.as_os_str().is_empty());
    assert!(execution.specialist_cache_path.as_os_str().is_empty());

    assert!(
        !memory_root.exists(),
        "disabled scaffold diagnostics must not create a memory ledger"
    );
    assert!(
        !phoenix_home.join("cognee").exists(),
        "disabled scaffold diagnostics must not open a Cognee store"
    );
    assert!(
        !phoenix_home.join("sessions").exists(),
        "disabled scaffold diagnostics must not create persisted sessions"
    );
    assert!(
        !phoenix_home.join("session_cache").exists(),
        "disabled scaffold diagnostics must not create persisted session caches"
    );

    assert!(
        crate::runtime::session_digest::digest_idle_sessions()
            .await
            .unwrap()
            .is_empty(),
        "a nonpersistent diagnostic turn must be invisible to the digest sweep"
    );
    assert!(!phoenix_home.join("session_digest_state.json").exists());
}

#[tokio::test]
async fn rejects_non_orchestrator_task_for_first_slice() {
    let root_dir = tempdir().unwrap();
    let root = root_dir.path().to_path_buf();
    let runner = AgentRunner::new(root, Arc::new(MockProvider));
    let orchestrator = Orchestrator::new("mock-model", "Route work only.");
    let task = TaskEnvelope::new(
        "session-main",
        AgentTarget::Specialist(SubAgentType::Coder),
        "Direct coder task",
        "This bypasses orchestrator.",
    );

    let error = runner
        .execute_first_slice(&orchestrator, &task)
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("first runtime slice expects an orchestrator task"));
}

#[tokio::test]
async fn reuses_persisted_sessions_and_caches_across_turns() {
    let root_dir = tempdir().unwrap();
    let root = root_dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("HOT").join("learnings")).unwrap();
    std::fs::write(
        root.join("HOT").join("learnings").join("session.md"),
        "# Session\n\nPhoenix session continuity foundation.",
    )
    .unwrap();

    let runner = AgentRunner::new(root.clone(), Arc::new(MockProvider));
    let orchestrator = Orchestrator::new("mock-model", "Route work only.");

    let first_task = TaskEnvelope::new(
        "sticky-session",
        AgentTarget::Orchestrator,
        "First turn",
        "Establish session continuity.",
    );
    runner
        .execute_first_slice(&orchestrator, &first_task)
        .await
        .unwrap();

    let second_task = TaskEnvelope::new(
        "sticky-session",
        AgentTarget::Orchestrator,
        "Second turn",
        "Continue the same Phoenix session.",
    );
    let execution = runner
        .execute_first_slice(&orchestrator, &second_task)
        .await
        .unwrap();

    assert_eq!(execution.main_session_id, "sticky-session");
    assert_eq!(execution.main_session_status, PersistenceStatus::Resumed);
    assert_eq!(execution.specialist_session_id, "sticky-session__coder");
    assert_eq!(
        execution.specialist_session_status,
        PersistenceStatus::Resumed
    );
    assert_eq!(execution.main_cache_status, PersistenceStatus::Resumed);
    assert_eq!(
        execution.specialist_cache_status,
        PersistenceStatus::Resumed
    );

    let main_session_path = root
        .join(".phoenix")
        .join("sessions")
        .join("sticky-session.json");
    let specialist_session_path = root
        .join(".phoenix")
        .join("sessions")
        .join("sticky-session__coder.json");
    let main_session_json = std::fs::read_to_string(main_session_path).unwrap();
    let specialist_session_json = std::fs::read_to_string(specialist_session_path).unwrap();
    assert!(main_session_json.contains("Establish session continuity."));
    assert!(main_session_json.contains("Continue the same Phoenix session."));
    assert!(specialist_session_json.contains("\"type\": \"Talk\""));

    let main_cache_path = root
        .join(".phoenix")
        .join("session_cache")
        .join("sticky-session.json");
    let main_cache_json = std::fs::read_to_string(main_cache_path).unwrap();
    assert!(main_cache_json.contains("\"session_id\": \"sticky-session\""));
    assert!(main_cache_json.contains("\"loaded\""));
}

#[tokio::test]
async fn executes_tool_backed_orchestrator_to_coder_slice() {
    let memory_root_dir = tempdir().unwrap();
    let memory_root = memory_root_dir.path().to_path_buf();
    let workspace_root_dir = tempdir().unwrap();
    let workspace_root = workspace_root_dir.path().to_path_buf();
    std::fs::create_dir_all(memory_root.join("HOT").join("learnings")).unwrap();
    std::fs::write(
        memory_root.join("HOT").join("learnings").join("coder.md"),
        "# Coder\n\nPhoenix coder must inspect files before reporting.",
    )
    .unwrap();
    std::fs::create_dir_all(workspace_root.join("src")).unwrap();
    std::fs::write(
            workspace_root.join("Cargo.toml"),
            "[package]\nname = \"phoenix_tool_slice_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
    std::fs::write(
        workspace_root.join("src").join("lib.rs"),
        "pub fn ok() -> bool { true }\n",
    )
    .unwrap();

    let runner =
        AgentRunner::with_workspace(memory_root.clone(), workspace_root, Arc::new(MockProvider));
    let orchestrator = Orchestrator::new("mock-model", "Route work only.");
    let task = TaskEnvelope::new(
        "tool-session",
        AgentTarget::Orchestrator,
        "Inspect runtime",
        "Prove the coder can run Phoenix tools.",
    );

    let execution = runner
        .execute_tool_backed_slice(&orchestrator, &task)
        .await
        .unwrap();

    assert!(execution
        .specialist_outcome
        .summary
        .contains("tool-backed coder inspection"));
    assert!(execution
        .specialist_outcome
        .summary
        .contains("No source edits were attempted"));
    assert!(execution.specialist_outcome.provider_response.is_none());
    assert!(execution
        .specialist_outcome
        .tool_results
        .iter()
        .any(|result| result.tool_name == "bash" && result.success));
    assert!(execution
        .specialist_outcome
        .artifacts
        .iter()
        .any(|artifact| {
            artifact.title == "Execution transcript"
                && artifact.body.contains("Execution mode: real_tool_backed")
        }));
    assert_eq!(execution.librarian_passes.len(), 6);
    assert!(execution
        .librarian_passes
        .iter()
        .any(|record| matches!(record.phase, LibrarianPhase::Prune)));
}

#[tokio::test]
async fn librarian_save_can_create_canonical_project_memory_by_tool() {
    let root_dir = tempdir().unwrap();
    let root = root_dir.path().to_path_buf();
    let task = TaskEnvelope::new(
        "canonical-session",
        AgentTarget::Orchestrator,
        "Explain Phoenix",
        "Preserve stable project structure.",
    );
    let decision = OrchestratorDecision {
        mode: DelegationMode::Handoff,
        target: AgentTarget::Specialist(SubAgentType::Coder),
        rationale: "Coder inspected the project.".to_string(),
    };
    let outcome = AgentOutcome {
        completion: crate::runtime::OutcomeCompletion::Completed,
        agent: AgentTarget::Specialist(SubAgentType::Coder),
        summary: "Phoenix project structure uses a Rust CLI and hidden librarian runtime."
            .to_string(),
        artifacts: vec![],
        tool_results: vec![],
        provider_response: None,
    };
    let mut cache = SessionCache::new("canonical-session");

    let record = memory_hooks::save_phase(
        &root,
        &root,
        Arc::new(MockProvider),
        "mock-model",
        SessionScope::Specialist(SubAgentType::Coder),
        &task,
        &crate::session::Session::new_sub_agent_with_id(
            "canonical-session",
            SubAgentType::Coder,
            "mock-model",
            "",
        ),
        &decision,
        &outcome,
        &mut cache,
        None,
    )
    .await
    .unwrap();

    // Cognee-only save: a success marker exists only when persistence was
    // confirmed. An unavailable/no-Cognee test build stays retryable and gets
    // an explicit degradation receipt instead of a false "remembered" claim.
    let save_confirmed = record.saved_memory_paths == vec!["cognee://remember".to_string()];
    assert!(record
        .receipts
        .iter()
        .any(|receipt| receipt.contains("Phoenix memory")));
    assert_eq!(
        record
            .receipts
            .iter()
            .any(|receipt| receipt.contains("outcome stored")),
        save_confirmed
    );
    assert_eq!(
        record
            .receipts
            .iter()
            .any(|receipt| receipt.contains("DURABILITY NOT CONFIRMED")),
        !save_confirmed
    );
    assert!(!root.join("WARM/projects/PHOENIX_PROJECT.md").exists());
}

#[tokio::test]
async fn executes_provider_backed_slice_with_valid_orchestrator_and_coder_json() {
    let root_dir = tempdir().unwrap();
    let root = root_dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("HOT").join("learnings")).unwrap();
    std::fs::write(
        root.join("HOT").join("learnings").join("provider.md"),
        "# Provider\n\nProvider-backed Phoenix turns must stay observable.",
    )
    .unwrap();
    let provider = SequencedProvider::new(
            serde_json::json!({
                "type": "tool_request",
                "tool_calls": [
                    {
                        "tool_name": "talk",
                        "input": {
                            "to": "coder",
                            "subject": "Inspect provider loop",
                            "body": "Inspect the provider-backed runtime loop in phoenix_agent/src/runtime and report on how it executes tools and returns results.",
                            "mode": 1
                        }
                    }
                ],
                "rationale": "The task needs coder inspection."
            })
            .to_string(),
            serde_json::json!({
                "type": "final",
                "summary": "Provider-backed coder turn completed.",
                "final_markdown": "## Result\nProvider-backed coder turn completed.",
                "changes_made": [],
                "verification": ["No tools were available in provider_no_tools mode."],
                "execution_mode": "provider_tools_readonly",
                "tool_transcript": []
            })
            .to_string(),
        );
    let runner = AgentRunner::new(root.clone(), Arc::new(provider));
    let orchestrator = Orchestrator::new(
        "mock-model",
        crate::orchestrator::ORCHESTRATOR_SYSTEM_PROMPT,
    );
    let task = TaskEnvelope::new(
        "provider-session",
        AgentTarget::Orchestrator,
        "Inspect provider loop",
        "Report on the provider-backed loop.",
    );

    let execution = runner
        .execute_provider_backed_slice(&orchestrator, &task)
        .await
        .unwrap();

    assert_eq!(execution.orchestrator_parse.status, ParseStatus::Parsed);
    assert_eq!(execution.coder_parse.status, ParseStatus::Parsed);
    assert_eq!(
        execution.decision.rationale,
        "Orchestrator routed work to coder with talk: Inspect provider loop"
    );
    assert_eq!(execution.outcome.tool_results.len(), 1);
    assert_eq!(execution.outcome.tool_results[0].tool_name, "talk");
    // The talk reply block carries the editable founding persona (`Leo (coder)`),
    // not a bare role string — the user reads "who" produced the result.
    assert!(execution.outcome.tool_results[0]
        .output
        .contains("Leo (coder) result"));
    assert!(execution.outcome.provider_response.is_some());
    assert!(execution.specialist_outcome.provider_response.is_some());
    assert!(execution.outcome.summary.contains("Provider-backed coder"));
    assert!(root
        .join(".phoenix")
        .join("sessions")
        .join("provider-session.json")
        .exists());
}

#[tokio::test]
async fn legacy_native_image_requests_keep_explicit_refs_and_reset_next_turn() {
    let dir = tempdir().unwrap();
    let _home = crate::config::test_env::PhoenixHomeGuard::set_private(dir.path());
    for (name, color) in [("primary.png", [200, 10, 10]), ("ref.png", [10, 200, 10]), ("next.png", [10, 10, 200])] {
        image::RgbImage::from_pixel(24, 16, image::Rgb(color)).save(dir.path().join(name)).unwrap();
    }
    std::fs::write(dir.path().join("broken.png"), b"broken").unwrap();
    let call = |input: serde_json::Value| serde_json::json!({"type":"tool_request", "rationale":"inspect pixels",
        "tool_calls":[{"tool_name":"image_analyze","input":input}]}).to_string();
    let final_text = serde_json::json!({"type":"final", "summary":"inspected", "final_markdown":"The failed image could not be inspected.",
        "changes_made":[], "verification":["native images"], "execution_mode":"provider_tools_readonly", "tool_transcript":[]}).to_string();
    let mut scripted = SequencedProvider::new("unused", "unused");
    scripted.native_images = true;
    *scripted.native_script.lock().unwrap() = vec![
        serde_json::json!({"type":"tool_request","rationale":"inspect one image before replacing it", "tool_calls":[
            {"tool_name":"image_analyze","input":{"path":"primary.png","reference_paths":["ref.png"]}},
            {"tool_name":"image_analyze","input":{"path":"next.png"}}
        ]}).to_string(),
        call(serde_json::json!({"path":"next.png"})),
        call(serde_json::json!({"path":"broken.png"})),
        final_text.clone(), final_text,
    ].into();
    let provider = Arc::new(scripted);
    let runner = AgentRunner::with_workspace(dir.path().join("memory"), dir.path().to_path_buf(), provider.clone()).with_native_vision(true);
    let orchestrator = Orchestrator::new("mock-model", crate::orchestrator::ORCHESTRATOR_SYSTEM_PROMPT);
    let task = TaskEnvelope::new("legacy-native-images", AgentTarget::Orchestrator, "Inspect", "Inspect the requested file comparisons.");
    let result = runner.execute_provider_backed_slice(&orchestrator, &task).await.unwrap();
    // Both file inspections of the first batch run; neither is rejected.
    assert!(!result.outcome.tool_results.iter().any(|result| result.output.contains("first image remains attached")));
    let next_turn = TaskEnvelope::new("legacy-native-images", AgentTarget::Orchestrator, "New turn", "No reference selected this turn.");
    runner.execute_provider_backed_slice(&orchestrator, &next_turn).await.unwrap();
    let reference = crate::runtime::vision::screenshot_data_uri(&dir.path().join("ref.png")).await.unwrap();
    let requests = provider.native_requests.lock().unwrap();
    let newest_set = |r: &CompletionRequest| {
        let newest = r.messages.iter().rposition(|m| !m.images.is_empty());
        let reply = r.messages.iter().rposition(|m| m.role == crate::providers::MessageRole::Assistant);
        newest.filter(|&i| reply.is_none_or(|a| i > a)).map_or(0, |i| r.messages[i].images.len())
    };
    // Round 2 repeats the identical set already committed, so it is not re-sent.
    assert_eq!(requests.iter().map(newest_set).collect::<Vec<_>>(), vec![0,2,0,1,0]);
    for round in [1,2,3] {
        let images = requests[round].messages.iter().rev().find(|m| !m.images.is_empty()).unwrap();
        assert_eq!(images.images.last(), Some(&reference));
        assert!(images.content.contains("reference_paths"));
        let wire = crate::providers::openai_codex::OpenAICodexProvider::responses_input(&requests[round]);
        assert!(wire.iter().flat_map(|item| item["content"].as_array().into_iter().flatten()).any(|part| part["image_url"] == reference));
    }
    assert!(requests[3].messages.iter().rev().find(|m| !m.images.is_empty()).unwrap().content.contains("No current observation"));
    assert!(requests[1..].iter().all(|request| request.tools.iter().map(|t| &t.name).collect::<Vec<_>>() == requests[0].tools.iter().map(|t| &t.name).collect::<Vec<_>>()));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 5, "preparing/retaining pixels adds no provider round");
}

/// Provider that finishes the orchestrator turn via a native `final_answer`
/// tool call (the structured-output path) instead of a free-text JSON envelope.
struct FinalAnswerToolProvider;

#[async_trait]
impl LLMProvider for FinalAnswerToolProvider {
    fn name(&self) -> &str {
        "final-answer-tool"
    }
    fn display_name(&self) -> &str {
        "FinalAnswerTool"
    }
    fn base_url(&self) -> &str {
        "mock://final-answer"
    }
    fn auth_type(&self) -> ContractAuthType {
        ContractAuthType::None
    }
    fn env_vars(&self) -> Vec<&str> {
        vec![]
    }
    fn default_headers(&self) -> HashMap<String, String> {
        HashMap::new()
    }
    fn has_model(&self, _model: &str) -> bool {
        true
    }
    fn default_model(&self) -> &str {
        "mock-model"
    }
    fn fallback_models(&self) -> Vec<&str> {
        vec![]
    }

    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        if request.messages.iter().any(|message| {
            message.role == MessageRole::System && message.content.contains("You are the Librarian")
        }) {
            return Ok(CompletionResponse {
                content: librarian_test_response(&request),
                model: request.model,
                usage: TokenUsage::new(5, 8),
                reasoning: None,
                stop_reason: Some("stop".to_string()),
                tool_calls: vec![],
                provider_replay: None,
            });
        }

        Ok(CompletionResponse {
            content: String::new(),
            model: request.model,
            usage: TokenUsage::new(10, 20),
            reasoning: None,
            stop_reason: Some("tool_calls".to_string()),
            tool_calls: vec![NativeToolCall {
                id: "call_final".to_string(),
                tool_name: "final_answer".to_string(),
                arguments: serde_json::json!({
                    "summary": "Introduced Phoenix.",
                    "final_markdown": "## Hello\nI am Phoenix, the orchestrator.",
                    "changes_made": [],
                    "verification": []
                }),
            }],
            provider_replay: None,
        })
    }

    async fn stream(&self, _request: CompletionRequest) -> anyhow::Result<StreamingResponse> {
        Ok(StreamingResponse {
            content: String::new(),
            reasoning: None,
            done: true,
        })
    }

    async fn embeddings(&self, _texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(vec![])
    }

    async fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        Ok(vec![])
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

#[tokio::test]
async fn orchestrator_finishes_via_final_answer_tool_call() {
    let root_dir = tempdir().unwrap();
    let root = root_dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("HOT").join("learnings")).unwrap();

    let runner = AgentRunner::new(root.clone(), Arc::new(FinalAnswerToolProvider));
    let orchestrator = Orchestrator::new(
        "mock-model",
        crate::orchestrator::ORCHESTRATOR_SYSTEM_PROMPT,
    );
    let task = TaskEnvelope::new(
        "final-answer-session",
        AgentTarget::Orchestrator,
        "Who are you",
        "Introduce yourself in one line.",
    );

    let execution = runner
        .execute_provider_backed_slice(&orchestrator, &task)
        .await
        .unwrap();

    // Structured finish via the tool call must parse cleanly — no fallback path.
    assert_eq!(execution.orchestrator_parse.status, ParseStatus::Parsed);
    assert!(execution.outcome.summary.contains("Phoenix"));
    assert!(execution
        .outcome
        .artifacts
        .iter()
        .any(|artifact| artifact.body.contains("I am Phoenix")));
}

#[test]
fn final_response_from_tool_input_fills_defaults() {
    let response = final_response_from_tool_input(&serde_json::json!({
        "final_markdown": "## Done\nThe work is complete.",
        "changes_made": ["edited runner.rs", "   ", ""],
        "verification": ["cargo test → 167 passed"]
    }));
    // summary defaults to the first line of the markdown when omitted.
    assert_eq!(response.summary, "## Done");
    // blank array entries are dropped.
    assert_eq!(response.changes_made, vec!["edited runner.rs".to_string()]);
    assert_eq!(response.verification.len(), 1);
    assert_eq!(response.execution_mode, "provider_tools_final_answer");
}

#[tokio::test]
async fn provider_backed_slice_accepts_provider_native_orchestrator_text() {
    let root_dir = tempdir().unwrap();
    let root = root_dir.path().to_path_buf();
    // Both rounds return plain text: round 0 trips the native "no tool calls"
    // nudge, round 1 falls through to the plain-text wrap (direct final).
    let provider = SequencedProvider::new("not json", "not json");
    let runner = AgentRunner::new(root, Arc::new(provider));
    let orchestrator = Orchestrator::new(
        "mock-model",
        crate::orchestrator::ORCHESTRATOR_SYSTEM_PROMPT,
    );
    let task = TaskEnvelope::new(
        "fallback-session",
        AgentTarget::Orchestrator,
        "Fallback route",
        "Trigger fallback route.",
    );

    let execution = runner
        .execute_provider_backed_slice(&orchestrator, &task)
        .await
        .unwrap();

    // Chat Completions and Responses both permit terminal assistant text.
    // Phoenix enriches that text into its receipt shape instead of rejecting
    // a healthy provider response as an unstructured mesh turn.
    assert!(
        execution
            .orchestrator_parse
            .detail
            .contains("wrapped it into a runtime final response"),
        "provider-native text should become a runtime final"
    );
    assert!(matches!(
        execution.decision.target,
        AgentTarget::Orchestrator
    ));
    assert_eq!(execution.coder_parse.status, ParseStatus::NotApplicable);
    assert_eq!(execution.outcome.summary, "not json");
    // The native no-tool-calls nudge fires once (recording feedback) before the
    // plain-text wrap, so the wrapped final still carries a Verification artifact.
    assert!(execution
        .outcome
        .artifacts
        .iter()
        .any(|artifact| artifact.title == "Verification"));
}

#[tokio::test]
async fn provider_backed_slice_allows_direct_orchestrator_final() {
    let root_dir = tempdir().unwrap();
    let root = root_dir.path().to_path_buf();
    let provider = SequencedProvider::new(
        serde_json::json!({
            "type": "final",
            "summary": "Phoenix identity answered.",
            "final_markdown": "I am Phoenix.",
            "changes_made": [],
            "verification": [],
            "execution_mode": "provider_tools_readonly",
            "tool_transcript": []
        })
        .to_string(),
        "coder should not be called",
    );
    let runner = AgentRunner::new(root, Arc::new(provider));
    let orchestrator = Orchestrator::new(
        "mock-model",
        crate::orchestrator::ORCHESTRATOR_SYSTEM_PROMPT,
    );
    let task = TaskEnvelope::new(
        "direct-session",
        AgentTarget::Orchestrator,
        "Identity",
        "Who are you?",
    );

    let execution = runner
        .execute_provider_backed_slice(&orchestrator, &task)
        .await
        .unwrap();

    assert_eq!(execution.orchestrator_parse.status, ParseStatus::Parsed);
    assert!(matches!(execution.decision.mode, DelegationMode::StayLocal));
    assert!(matches!(
        execution.decision.target,
        AgentTarget::Orchestrator
    ));
    assert!(matches!(
        execution.specialist_outcome.agent,
        AgentTarget::Orchestrator
    ));
    assert_eq!(execution.coder_parse.status, ParseStatus::NotApplicable);
    assert!(execution.outcome.summary.contains("I am Phoenix."));
}

#[tokio::test]
async fn orchestrator_repairs_empty_response_after_tool_round() {
    let root_dir = tempdir().unwrap();
    let root = root_dir.path().to_path_buf();
    let provider = ThreeStepProvider::new(
        serde_json::json!({
            "type": "tool_request",
            "tool_calls": [
                {
                    "tool_name": "todo_write",
                    "input": {
                        "todos": [
                            { "task": "Gather evidence", "completed": false },
                            { "task": "Synthesize answer", "completed": false }
                        ]
                    }
                }
            ],
            "rationale": "Set visible task state before synthesis."
        })
        .to_string(),
        "",
        serde_json::json!({
            "type": "final",
            "summary": "Recovered after empty provider response.",
            "final_markdown": "Recovered after the empty provider response.",
            "changes_made": [],
            "verification": ["runtime repair prompt after empty orchestrator response"],
            "execution_mode": "provider_backed",
            "tool_transcript": []
        })
        .to_string(),
    );
    let runner = AgentRunner::new(root, Arc::new(provider));
    let orchestrator = Orchestrator::new(
        "mock-model",
        crate::orchestrator::ORCHESTRATOR_SYSTEM_PROMPT,
    );
    let task = TaskEnvelope::new(
        "empty-repair-session",
        AgentTarget::Orchestrator,
        "Recover",
        "Use a todo then answer.",
    );

    let execution = runner
        .execute_provider_backed_slice(&orchestrator, &task)
        .await
        .unwrap();

    assert_eq!(execution.orchestrator_parse.status, ParseStatus::Parsed);
    assert!(execution
        .orchestrator_parse
        .detail
        .contains("Provider output parsed"));
    assert!(execution
        .outcome
        .summary
        .contains("Recovered after the empty provider response."));
    assert!(execution
        .outcome
        .tool_results
        .iter()
        .any(|result| result.tool_name == "response_validation"
            && result.output.contains("returned empty content after")));
}

#[tokio::test]
async fn provider_backed_slice_preserves_coder_result_when_final_orchestrator_reply_is_malformed() {
    let root_dir = tempdir().unwrap();
    let root = root_dir.path().to_path_buf();
    let provider = ThreeStepProvider::new(
            serde_json::json!({
                "type": "tool_request",
                "tool_calls": [
                    {
                        "tool_name": "talk",
                        "input": {
                            "to": "coder",
                            "subject": "Inspect provider loop",
                            "body": "Inspect the provider-backed runtime loop in phoenix_agent/src/runtime and report on how it executes tools and returns results.",
                            "mode": 1
                        }
                    }
                ],
                "rationale": "The task needs coder inspection."
            })
            .to_string(),
            serde_json::json!({
                "type": "final",
                "summary": "Coder verified the delegated task.",
                "final_markdown": "## Result\nCoder verified the delegated task.",
                "changes_made": ["Checked the provider-backed path"],
                "verification": ["cargo test: PASS"],
                "execution_mode": "provider_tools_readonly",
                "tool_transcript": []
            })
            .to_string(),
            "not json",
        );
    let runner = AgentRunner::new(root, Arc::new(provider));
    let orchestrator = Orchestrator::new(
        "mock-model",
        crate::orchestrator::ORCHESTRATOR_SYSTEM_PROMPT,
    );
    let task = TaskEnvelope::new(
        "fallback-session",
        AgentTarget::Orchestrator,
        "Inspect provider loop",
        "Report on the provider-backed loop.",
    );

    let execution = runner
        .execute_provider_backed_slice(&orchestrator, &task)
        .await
        .unwrap();

    assert_eq!(
        execution.orchestrator_parse.status,
        ParseStatus::FallbackUsed
    );
    assert!(execution
        .orchestrator_parse
        .detail
        .contains("preserved the delegated specialist result"));
    assert_eq!(
        execution.outcome.summary,
        "## Result\nCoder verified the delegated task."
    );
    assert!(execution.outcome.artifacts.iter().any(|artifact| {
        artifact.title == "Verification"
            && artifact
                .body
                .contains("preserved the delegated specialist result")
    }));
    assert_eq!(
        execution.specialist_outcome.summary,
        "## Result\nCoder verified the delegated task."
    );
}

#[tokio::test]
async fn provider_backed_slice_preserves_coder_result_when_orchestrator_final_repeatedly_fails_validation(
) {
    let root_dir = tempdir().unwrap();
    let root = root_dir.path().to_path_buf();
    let provider = ThreeStepProvider::new(
            serde_json::json!({
                "type": "tool_request",
                "tool_calls": [
                    {
                        "tool_name": "talk",
                        "input": {
                            "to": "coder",
                            "subject": "Inspect provider loop",
                            "body": "Inspect the provider-backed runtime loop in phoenix_agent/src/runtime and report on how it executes tools and returns results.",
                            "mode": 1
                        }
                    }
                ],
                "rationale": "The task needs coder inspection."
            })
            .to_string(),
            serde_json::json!({
                "type": "final",
                "summary": "Coder verified the delegated task.",
                "final_markdown": "## Result\nCoder verified the delegated task.",
                "changes_made": ["Checked the provider-backed path"],
                "verification": ["cargo test: PASS"],
                "execution_mode": "provider_tools_readonly",
                "tool_transcript": []
            })
            .to_string(),
            serde_json::json!({
                "type": "final",
                "summary": "Done.",
                "final_markdown": "Package name: `REPLACE_ME`",
                "changes_made": [],
                "verification": [],
                "execution_mode": "provider_tools_readonly",
                "tool_transcript": []
            })
            .to_string(),
        );
    let runner = AgentRunner::new(root, Arc::new(provider));
    let orchestrator = Orchestrator::new(
        "mock-model",
        crate::orchestrator::ORCHESTRATOR_SYSTEM_PROMPT,
    );
    let task = TaskEnvelope::new(
        "validation-session",
        AgentTarget::Orchestrator,
        "Inspect provider loop",
        "Report on the provider-backed loop.",
    );

    let execution = runner
        .execute_provider_backed_slice(&orchestrator, &task)
        .await
        .unwrap();

    assert_eq!(
        execution.orchestrator_parse.status,
        ParseStatus::FallbackUsed
    );
    assert!(execution
        .orchestrator_parse
        .detail
        .contains("failed runtime validation after 3 consecutive rejection"));
    assert_eq!(
        execution.outcome.summary,
        "## Result\nCoder verified the delegated task."
    );
    assert!(execution
            .outcome
            .artifacts
            .iter()
            .any(|artifact| artifact.title == "Verification"
                && artifact
                    .body
                    .contains("preserved the delegated specialist result after repeated orchestrator validation failures")));
}

#[test]
fn specialist_reply_excerpt_preserves_late_findings() {
    let excerpt = outcome_excerpt(
        "Plan:\n\
             - Inspect workspace Cargo.toml files.\n\
             - Extract the `[package]` `name` value.\n\
             - Report the exact path and value without editing files.\n\
             \n\
             Diff summary:\n\
             - No files changed.\n\
             \n\
             Reported package metadata:\n\
             - File: `Cargo.toml`\n\
             - `name`: `phoenix_agent`",
    );

    assert!(excerpt.contains("Reported package metadata"));
    assert!(excerpt.contains("`name`: `phoenix_agent`"));
}

#[test]
fn malformed_envelope_does_not_leak_to_user() {
    // The exact failure from the live trace: a truncated tool_request
    // envelope (stray quote on end_line) must NOT be dumped raw to the user.
    let leaked = r#"{"type":"tool_request","tool_calls":[{"tool_name":"read","input":{"path":"src/x.rs","end_line":20"}}],"rationale":"Work Gate..."}"#;
    assert!(looks_like_json_envelope(leaked));
    let fallback = invalid_provider_json_fallback("Coder", leaked, "expected `,` or `}`");
    assert!(
        !fallback.final_markdown.contains("\"tool_calls\""),
        "raw envelope leaked: {}",
        fallback.final_markdown
    );
    assert!(!fallback.final_markdown.contains("tool_request"));
}

#[test]
fn malformed_envelope_salvages_final_markdown() {
    // If the broken envelope still carries a usable final_markdown, surface it.
    let raw = "{\"type\":\"final\",\"summary\":\"done\",\"final_markdown\":\"## Result\\nBuild passes, 166 tests.\",\"changes_made\":[}";
    let fallback = invalid_provider_json_fallback("Coder", raw, "trailing junk");
    assert!(fallback.final_markdown.contains("Build passes, 166 tests"));
    assert!(!fallback.final_markdown.contains("\"final_markdown\""));
}

#[test]
fn real_prose_fallback_is_preserved() {
    // Genuine prose (not an envelope) must still be preserved verbatim.
    let prose = "The crate version is 0.1.0 and the build is clean.";
    assert!(!looks_like_json_envelope(prose));
    let fallback = invalid_provider_json_fallback("Coder", prose, "no json");
    assert!(fallback.final_markdown.contains("0.1.0"));
}

#[tokio::test]
async fn provider_backed_slice_accepts_provider_native_coder_text() {
    let root_dir = tempdir().unwrap();
    let root = root_dir.path().to_path_buf();
    let provider = SequencedProvider::new(
            serde_json::json!({
                "type": "tool_request",
                "tool_calls": [
                    {
                        "tool_name": "talk",
                        "input": {
                            "to": "coder",
                            "subject": "Bad coder",
                            "body": "Trigger the coder parse-failure path and report back in detail on what the runtime did.",
                            "mode": 1
                        }
                    }
                ],
                "rationale": "The task needs coder inspection."
            })
            .to_string(),
            "not json",
        );
    let runner = AgentRunner::new(root, Arc::new(provider));
    let orchestrator = Orchestrator::new(
        "mock-model",
        crate::orchestrator::ORCHESTRATOR_SYSTEM_PROMPT,
    );
    let task = TaskEnvelope::new(
        "bad-coder-session",
        AgentTarget::Orchestrator,
        "Bad coder",
        "Trigger coder parse failure.",
    );

    let execution = runner
        .execute_provider_backed_slice(&orchestrator, &task)
        .await
        .unwrap();

    // Provider-native terminal text is valid; only unfinished promises are
    // rejected (pinned separately above). This is the compatibility path that
    // prevents Responses-style output from becoming a parse failure.
    assert!(
        execution
            .coder_parse
            .detail
            .contains("wrapped it into a runtime final response"),
        "provider-native specialist text should become a runtime final"
    );
}

#[tokio::test]
async fn coder_empty_response_gets_repair_attempt_before_fallback() {
    let root_dir = tempdir().unwrap();
    let root = root_dir.path().to_path_buf();
    let provider = SequencedProvider::new(
            serde_json::json!({
                "type": "tool_request",
                "tool_calls": [
                    {
                        "tool_name": "talk",
                        "input": {
                            "to": "coder",
                            "subject": "Empty coder",
                            "body": "Trigger the coder empty-response path and report back in valid Phoenix JSON.",
                            "mode": 1
                        }
                    }
                ],
                "rationale": "The task needs coder inspection."
            })
            .to_string(),
            "",
        );
    let runner = AgentRunner::new(root, Arc::new(provider));
    let orchestrator = Orchestrator::new(
        "mock-model",
        crate::orchestrator::ORCHESTRATOR_SYSTEM_PROMPT,
    );
    let task = TaskEnvelope::new(
        "empty-coder-session",
        AgentTarget::Orchestrator,
        "Empty coder",
        "Trigger coder empty response.",
    );

    let execution = runner
        .execute_provider_backed_slice(&orchestrator, &task)
        .await
        .unwrap();

    assert_eq!(execution.coder_parse.status, ParseStatus::FallbackUsed);
    assert!(execution
        .coder_parse
        .detail
        .contains("after 3 repair attempt(s)"));
    // An empty (reasoning-only) coder message gets the explicit
    // "write the envelope, don't return empty" nudge, not the generic
    // malformed-JSON feedback.
    assert!(execution
        .specialist_outcome
        .tool_results
        .iter()
        .any(|result| result.tool_name == "response_validation"
            && result.output.contains("Do not return empty content")));
}

#[test]
fn normalizes_todo_title_status_items_inside_todos() {
    let normalized = normalize_tool_input(
        "todo_write",
        serde_json::json!({
            "todos": [
                { "title": "Gather web evidence", "status": "done" },
                { "name": "Audit repo", "status": "in_progress" },
                "Synthesize answer"
            ]
        }),
    );

    assert_eq!(
        normalized,
        serde_json::json!({
            "todos": [
                { "task": "Gather web evidence", "completed": true },
                { "task": "Audit repo", "completed": false },
                { "task": "Synthesize answer", "completed": false }
            ]
        })
    );
}

#[test]
fn normalizes_talk_input_aliases_from_provider_output() {
    // Team model (2026-07-02): mode 2 (background handoff) is the DEFAULT for
    // every delegation when the model omits `mode`. Legacy `reply_expected` is
    // consumed (not allowed to override the default back to mode 1); `mode` is
    // the single knob. Explicit mode 1 is honored — see the test below.
    let normalized = normalize_tool_input(
        "talk",
        serde_json::json!({
            "target": "coder",
            "title": "Inspect project",
            "message": "Read the repo and report back.",
            "reply_expected": true
        }),
    );

    assert_eq!(
        normalized,
        serde_json::json!({
            "to": "coder",
            "subject": "Inspect project",
            "body": "Read the repo and report back.",
            "mode": 2
        })
    );
}

#[test]
fn explicit_talk_mode_1_is_preserved() {
    // Mode 1 (synchronous, caller blocks for the reply this turn) stays
    // available — the model emits it explicitly for the rare quick-blocking case.
    let normalized = normalize_tool_input(
        "talk",
        serde_json::json!({
            "to": "coder",
            "subject": "Quick check",
            "body": "Read one file.",
            "mode": 1
        }),
    );
    assert_eq!(normalized.get("mode").and_then(|v| v.as_u64()), Some(1));
}

#[test]
fn normalizes_talk_brief_field_into_body() {
    let normalized = normalize_tool_input(
        "talk",
        serde_json::json!({
            "target": "coder",
            "mode": 1,
            "brief": "Objective: audit phoenix_agent. Deliverable: reports/status.md. Verification: cargo test."
        }),
    );

    let body = normalized
        .get("body")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    assert!(body.contains("Objective: audit phoenix_agent"));
    assert_eq!(normalized.get("to").and_then(|v| v.as_str()), Some("coder"));
}
