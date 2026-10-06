//! Google Gemini provider — native tool calling via generateContent API

use async_trait::async_trait;
use reqwest::Client;
use std::collections::HashMap;
use std::time::Duration;

use super::contracts::{
    AuthType, CompletionRequest, CompletionResponse, LLMProvider, MessageRole, ModelInfo,
    NativeToolCall, StreamingResponse, TokenUsage, ToolDefinition,
};
use super::providers_data;

pub struct GoogleProvider {
    client: Client,
    api_key: String,
    base_url: String,
    catalog_id: String,
}

/// Recover the function name from a synthesized `call_<name>_<idx>` id (also
/// tolerates the older `call_<name>` shape and raw names).
pub(crate) fn function_name_from_call_id(call_id: &str) -> &str {
    let stripped = call_id.strip_prefix("call_").unwrap_or(call_id);
    match stripped.rsplit_once('_') {
        Some((name, idx)) if !name.is_empty() && idx.chars().all(|c| c.is_ascii_digit()) => name,
        _ => stripped,
    }
}

impl GoogleProvider {
    pub fn new(api_key: String) -> Self {
        Self::with_timeout(api_key, Duration::from_secs(120))
    }

    pub fn with_timeout(api_key: String, timeout: Duration) -> Self {
        Self::with_url_auth_and_timeout(
            "https://generativelanguage.googleapis.com/v1beta".to_string(),
            api_key,
            "google",
            timeout,
        )
    }

    pub fn with_cli_oauth(base_url: String, token: String, timeout: Duration) -> Self {
        Self::with_url_auth_and_timeout(base_url, token, "google-gemini-cli", timeout)
    }

    fn with_url_auth_and_timeout(
        base_url: String,
        api_key: String,
        catalog_id: &str,
        timeout: Duration,
    ) -> Self {
        let client = super::apply_read_timeout(
            Client::builder().connect_timeout(Duration::from_secs(30)),
            timeout,
        )
            .build()
            .expect("Google client build failed");
        Self {
            client,
            api_key,
            base_url,
            catalog_id: catalog_id.to_string(),
        }
    }

    fn has_model_impl(&self, model: &str) -> bool {
        providers_data::get_provider(&self.catalog_id)
            .map(|p| p.has_model(model))
            .unwrap_or(false)
    }

    fn default_model_impl(&self) -> &str {
        "gemini-2.5-flash"
    }

    fn fallback_models_impl(&self) -> Vec<&str> {
        vec!["gemini-2.5-pro", "gemini-2.0-flash"]
    }

    fn generate_url(&self, model: &str) -> String {
        format!(
            "{}/models/{}:generateContent?key={}",
            self.base_url.trim_end_matches('/'),
            model,
            self.api_key
        )
    }

    pub(crate) fn build_contents(request: &CompletionRequest) -> Vec<serde_json::Value> {
        let mut contents = Vec::new();

        for m in &request.messages {
            match m.role {
                MessageRole::System | MessageRole::Developer => {}
                MessageRole::Tool => {
                    let tool_call_id = m.tool_call_id.as_deref().unwrap_or("");
                    contents.push(serde_json::json!({
                        "role": "user",
                        "parts": [{
                            "functionResponse": {
                                // Gemini matches responses to calls by FUNCTION
                                // name, not call id — recover it from the
                                // synthesized `call_<name>_<idx>` id.
                                "name": function_name_from_call_id(tool_call_id),
                                "response": serde_json::from_str::<serde_json::Value>(&m.content)
                                    .unwrap_or(serde_json::json!({"result": m.content}))
                            }
                        }]
                    }));
                }
                MessageRole::Assistant => {
                    if !m.tool_calls.is_empty() {
                        let mut parts: Vec<serde_json::Value> = Vec::new();
                        if !m.content.trim().is_empty() {
                            parts.push(serde_json::json!({"text": m.content}));
                        }
                        for tc in &m.tool_calls {
                            parts.push(serde_json::json!({
                                "functionCall": {
                                    "name": tc.tool_name,
                                    "args": tc.arguments
                                }
                            }));
                        }
                        contents.push(serde_json::json!({
                            "role": "model",
                            "parts": parts
                        }));
                    } else {
                        contents.push(serde_json::json!({
                            "role": "model",
                            "parts": [{"text": m.content}]
                        }));
                    }
                }
                _ => {
                    contents.push(serde_json::json!({
                        "role": "user",
                        "parts": [{"text": m.content}]
                    }));
                }
            }
        }

        contents
    }

    pub(crate) fn build_tool_declarations(tools: &[ToolDefinition]) -> Vec<serde_json::Value> {
        if tools.is_empty() {
            return vec![];
        }
        let declarations: Vec<serde_json::Value> = tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters
                })
            })
            .collect();
        vec![serde_json::json!({
            "functionDeclarations": declarations
        })]
    }

    pub(crate) fn parse_response(
        json: &serde_json::Value,
        fallback_model: &str,
    ) -> CompletionResponse {
        let candidates = json["candidates"].as_array();
        let content = candidates
            .and_then(|arr| arr.first())
            .and_then(|c| c["content"]["parts"].as_array())
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();

        let tool_calls: Vec<NativeToolCall> = candidates
            .and_then(|arr| arr.first())
            .and_then(|c| c["content"]["parts"].as_array())
            .map(|parts| {
                parts
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, p)| {
                        if let Some(fc) = p.get("functionCall") {
                            let name = fc["name"].as_str()?.to_string();
                            let args = fc.get("args").cloned().unwrap_or(serde_json::json!({}));
                            Some(NativeToolCall {
                                // Gemini has no call ids; synthesize unique ones
                                // (`function_name_from_call_id` reverses this).
                                id: format!("call_{}_{}", name, idx),
                                tool_name: name,
                                arguments: args,
                            })
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        let model = json["modelVersion"]
            .as_str()
            .or_else(|| {
                candidates
                    .and_then(|arr| arr.first())
                    .and_then(|c| c["modelVersion"].as_str())
            })
            .unwrap_or(fallback_model)
            .to_string();

        let input_tokens = json["usageMetadata"]["promptTokenCount"]
            .as_u64()
            .unwrap_or(0) as u32;
        let output_tokens = json["usageMetadata"]["candidatesTokenCount"]
            .as_u64()
            .unwrap_or(0) as u32;

        let stop_reason = candidates
            .and_then(|arr| arr.first())
            .and_then(|c| c["finishReason"].as_str())
            .map(|r| {
                if r == "STOP" && !tool_calls.is_empty() {
                    "tool_calls".to_string()
                } else {
                    r.to_string()
                }
            });

        CompletionResponse {
            content,
            model,
            usage: TokenUsage::new(input_tokens, output_tokens),
            reasoning: None,
            stop_reason,
            tool_calls,
            provider_replay: None,
        }
    }
}

#[async_trait]
impl LLMProvider for GoogleProvider {
    fn name(&self) -> &str {
        &self.catalog_id
    }

    fn display_name(&self) -> &str {
        providers_data::get_provider(&self.catalog_id)
            .map(|p| p.name)
            .unwrap_or("Google Gemini")
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn auth_type(&self) -> AuthType {
        AuthType::ApiKey
    }

    fn env_vars(&self) -> Vec<&str> {
        if self.catalog_id == "google-gemini-cli" {
            vec!["GEMINI_CLI_OAUTH_TOKEN"]
        } else {
            vec!["GOOGLE_API_KEY"]
        }
    }

    fn default_headers(&self) -> HashMap<String, String> {
        HashMap::new()
    }

    fn has_model(&self, model: &str) -> bool {
        self.has_model_impl(model)
    }

    fn default_model(&self) -> &str {
        self.default_model_impl()
    }

    fn fallback_models(&self) -> Vec<&str> {
        self.fallback_models_impl()
    }

    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        let system_instruction = request
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::System || m.role == MessageRole::Developer)
            .map(|m| m.content.trim())
            .filter(|c| !c.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");

        let contents = Self::build_contents(&request);

        let mut body = serde_json::json!({
            "contents": contents,
        });

        if !system_instruction.is_empty() {
            body["systemInstruction"] = serde_json::json!({
                "parts": [{"text": system_instruction}]
            });
        }

        if let Some(temp) = request.temperature {
            body["generationConfig"]["temperature"] = serde_json::json!(temp);
        }
        if let Some(max_tokens) = request.max_tokens {
            body["generationConfig"]["maxOutputTokens"] = serde_json::json!(max_tokens);
        }

        let tool_declarations = Self::build_tool_declarations(&request.tools);
        if !tool_declarations.is_empty() {
            body["tools"] = serde_json::json!(tool_declarations);
        }

        let url = self.generate_url(&request.model);
        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let err_text = super::read_error_response(resp, "Google API error response").await?;
            anyhow::bail!(
                "Google API error ({}): {}",
                status,
                super::bounded_error_preview(&err_text)
            );
        }

        let json: serde_json::Value =
            super::read_json_response(resp, "Google API response").await?;
        Ok(Self::parse_response(&json, &request.model))
    }

    async fn stream(&self, request: CompletionRequest) -> anyhow::Result<StreamingResponse> {
        let json = self.complete(request).await?;
        Ok(StreamingResponse {
            content: json.content,
            reasoning: None,
            done: true,
        })
    }

    async fn embeddings(&self, _texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
        anyhow::bail!("Google embeddings not yet supported in Phoenix")
    }

    async fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        let provider_data = providers_data::get_provider(&self.catalog_id)
            .ok_or_else(|| anyhow::anyhow!("Google provider data not found"))?;
        Ok(provider_data
            .models
            .iter()
            .map(|m| ModelInfo {
                id: m.id.to_string(),
                object: "model".to_string(),
                created: None,
                context_window: Some(m.context_window),
                input_cost_per_token: None,
                output_cost_per_token: None,
                supports_tools: Some(m.reasoning),
                supports_vision: Some(true),
            })
            .collect())
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        let url = self.generate_url("gemini-2.0-flash");
        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
                "generationConfig": {"maxOutputTokens": 5}
            }))
            .send()
            .await?;
        Ok(resp.status().is_success())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::contracts::ChatMessage;

    #[test]
    fn function_name_recovers_from_synthesized_ids() {
        assert_eq!(function_name_from_call_id("call_read_0"), "read");
        assert_eq!(
            function_name_from_call_id("call_memory_read_12"),
            "memory_read"
        );
        // Older shape without the index suffix.
        assert_eq!(
            function_name_from_call_id("call_memory_read"),
            "memory_read"
        );
        // Raw provider-issued ids pass through untouched.
        assert_eq!(function_name_from_call_id("abc123"), "abc123");
    }

    #[test]
    fn tool_result_round_trips_function_name() {
        let request = CompletionRequest {
            model: "gemini-2.5-flash".to_string(),
            messages: vec![
                ChatMessage::user("list files"),
                ChatMessage::assistant_tool_calls(
                    "",
                    vec![NativeToolCall {
                        id: "call_glob_0".to_string(),
                        tool_name: "glob".to_string(),
                        arguments: serde_json::json!({"pattern": "*.rs"}),
                    }],
                ),
                ChatMessage::tool_result("call_glob_0", "main.rs"),
            ],
            max_tokens: None,
            temperature: None,
            stream: false,
            extra_body: Default::default(),
            session_id: None,
            stream_observer: None,
            tools: vec![],
            native_replay: None,
        };
        let contents = GoogleProvider::build_contents(&request);
        assert_eq!(
            contents[1]["parts"][0]["functionCall"]["name"],
            serde_json::json!("glob")
        );
        // The functionResponse must carry the FUNCTION name, not the call id.
        assert_eq!(
            contents[2]["parts"][0]["functionResponse"]["name"],
            serde_json::json!("glob")
        );
    }

    #[test]
    fn parallel_calls_get_unique_ids() {
        let json = serde_json::json!({
            "candidates": [{
                "content": {"parts": [
                    {"functionCall": {"name": "read", "args": {"path": "a"}}},
                    {"functionCall": {"name": "read", "args": {"path": "b"}}}
                ]},
                "finishReason": "STOP"
            }]
        });
        let resp = GoogleProvider::parse_response(&json, "gemini-2.5-flash");
        assert_eq!(resp.tool_calls.len(), 2);
        assert_ne!(resp.tool_calls[0].id, resp.tool_calls[1].id);
        assert_eq!(resp.stop_reason.as_deref(), Some("tool_calls"));
    }
}
