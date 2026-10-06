//! Scaffold provider — a built-in mock provider for deterministic runtime diagnostics.
//!
//! Returns pre-canned responses that exercise the full Phoenix runtime loop
//! (orchestrator → coder plus deterministic librarian pass receipts) without
//! needing a real LLM provider. CLI scaffold runs never open durable Cognee
//! memory or persist their synthetic outcomes there.

use std::collections::HashMap;

use async_trait::async_trait;

use crate::providers::{
    CompletionRequest, CompletionResponse, ContractAuthType, LLMProvider, MessageRole, ModelInfo,
    StreamingResponse, TokenUsage,
};

pub struct ScaffoldProvider;

#[async_trait]
impl LLMProvider for ScaffoldProvider {
    fn name(&self) -> &str {
        "scaffold"
    }

    fn display_name(&self) -> &str {
        "Scaffold"
    }

    fn base_url(&self) -> &str {
        "mock://phoenix-scaffold"
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
        "phoenix-scaffold-model"
    }

    fn fallback_models(&self) -> Vec<&str> {
        vec![]
    }

    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        if request.messages.iter().any(|message| {
            message.role == MessageRole::System && message.content.contains("You are the Librarian")
        }) {
            return Ok(CompletionResponse {
                content: scaffold_librarian_response(&request),
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
        let title = extract_field(&user_message, "Title").unwrap_or("Phoenix task");
        let user_request = extract_section(&user_message, "User request:")
            .unwrap_or("No user request found.")
            .lines()
            .next()
            .unwrap_or("No user request found.")
            .trim()
            .to_string();

        Ok(CompletionResponse {
            content: serde_json::json!({
                "summary": format!("Completed scaffold coder turn for {title}."),
                "final_markdown": format!(
                    "## Result\nScaffold coder completed the task.\n\n- Task: {}\n- Request: {}",
                    title, user_request
                ),
                "changes_made": [
                    "Produced a scaffold coder result payload.",
                    "Kept final Markdown separate from internal fields."
                ],
                "verification": [
                    "Scaffold mode executed without real tools."
                ],
                "execution_mode": "scaffold_no_tools",
                "tool_transcript": []
            })
            .to_string(),
            model: request.model,
            usage: TokenUsage::new(10, 24),
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

fn scaffold_librarian_response(request: &CompletionRequest) -> String {
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

fn extract_field<'a>(body: &'a str, label: &str) -> Option<&'a str> {
    let prefix = format!("{label}:");
    body.lines()
        .find_map(|line| line.strip_prefix(&prefix).map(str::trim))
}

fn extract_section<'a>(body: &'a str, heading: &str) -> Option<&'a str> {
    let start = body.find(heading)?;
    let rest = &body[start + heading.len()..];
    let trimmed = rest.trim_start_matches('\n');
    let end = trimmed.find("\n\n").unwrap_or(trimmed.len());
    Some(trimmed[..end].trim())
}
