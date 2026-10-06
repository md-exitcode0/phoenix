//! OpenCode Zen provider implementation

use async_trait::async_trait;
use reqwest::Client;
use std::collections::HashMap;
use std::time::Duration;

use super::contracts::{
    AuthType, CompletionRequest, CompletionResponse, LLMProvider, MessageRole, ModelInfo,
    NativeToolCall, StreamingResponse, TokenUsage,
};
use super::openai_compat;
use super::providers_data;

const BASE_URL: &str = "https://opencode.ai/zen/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelRoute {
    Responses,
    Messages,
    ChatCompletions,
    GoogleGenerateContent,
}

pub struct OpenCodeProvider {
    client: Client,
    api_key: String,
}

impl OpenCodeProvider {
    pub fn new(api_key: String) -> Self {
        Self::with_timeout(api_key, Duration::from_secs(120))
    }

    pub fn with_timeout(api_key: String, timeout: Duration) -> Self {
        let client = super::apply_read_timeout(
            Client::builder().connect_timeout(Duration::from_secs(30)),
            timeout,
        )
            .build()
            .expect("OpenCode client build failed");
        Self { client, api_key }
    }

    fn has_model_impl(&self, model: &str) -> bool {
        let normalized = Self::normalize_model_id(model);
        let provider_data = providers_data::get_provider("opencode");
        provider_data
            .map(|p| p.has_model(&normalized))
            .unwrap_or(false)
    }

    fn default_model_impl(&self) -> &str {
        "claude-sonnet-4-6"
    }

    fn fallback_models_impl(&self) -> Vec<&str> {
        vec!["gpt-5.4-mini", "qwen3.6-plus", "minimax-m2.5-free"]
    }

    fn normalize_model_id(model: &str) -> String {
        model.trim().trim_start_matches("opencode/").to_string()
    }

    fn route_for_model(model: &str) -> ModelRoute {
        let normalized = Self::normalize_model_id(model);

        if normalized.starts_with("claude-") || normalized == "big-pickle" {
            return ModelRoute::Messages;
        }

        if normalized.starts_with("gemini-") {
            return ModelRoute::GoogleGenerateContent;
        }

        if normalized.starts_with("gpt-") {
            return ModelRoute::Responses;
        }

        ModelRoute::ChatCompletions
    }

    fn bearer_request(&self, url: &str) -> reqwest::RequestBuilder {
        self.client
            .post(url)
            .bearer_auth(&self.api_key)
            .header("Content-Type", "application/json")
    }

    async fn make_request(&self, request: &CompletionRequest) -> anyhow::Result<serde_json::Value> {
        super::retry::with_retry(|| self.dispatch_request(request)).await
    }

    async fn dispatch_request(
        &self,
        request: &CompletionRequest,
    ) -> anyhow::Result<serde_json::Value> {
        match Self::route_for_model(&request.model) {
            ModelRoute::Responses => self.make_responses_request(request).await,
            ModelRoute::Messages => self.make_messages_request(request).await,
            ModelRoute::ChatCompletions => self.make_chat_request(request).await,
            ModelRoute::GoogleGenerateContent => self.make_google_request(request).await,
        }
    }

    async fn make_responses_request(
        &self,
        request: &CompletionRequest,
    ) -> anyhow::Result<serde_json::Value> {
        // Same Responses-API multi-turn shapes as the codex client: assistant
        // tool calls become function_call items, tool results become
        // function_call_output items, system prompts ride in `instructions`.
        let input = super::openai_codex::OpenAICodexProvider::responses_input(request);
        let instructions = super::openai_codex::OpenAICodexProvider::instructions(request);

        let mut body = serde_json::json!({
            "model": Self::normalize_model_id(&request.model),
            "input": input,
        });
        if !instructions.is_empty() {
            body["instructions"] = serde_json::json!(instructions);
        }

        if let Some(max_tokens) = request.max_tokens {
            body["max_output_tokens"] = serde_json::json!(max_tokens);
        }

        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        for (key, value) in &request.extra_body {
            // `response_format` is a chat-completions param; Responses uses
            // text.format. `reasoning` IS valid here, so it passes through.
            if key == "response_format" {
                continue;
            }
            body[key] = value.clone();
        }

        if !request.tools.is_empty() {
            let tools: Vec<serde_json::Value> = request
                .tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    })
                })
                .collect();
            body["tools"] = serde_json::json!(tools);
            body["tool_choice"] = serde_json::json!("auto");
        }

        let resp = self
            .bearer_request(&format!("{BASE_URL}/responses"))
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let err_text =
                super::read_error_response(resp, "OpenCode Responses API error response").await?;
            anyhow::bail!(
                "OpenCode responses API error ({}): {}",
                status,
                super::bounded_error_preview(&err_text)
            );
        }

        super::read_json_response(resp, "OpenCode Responses API response").await
    }

    async fn make_messages_request(
        &self,
        request: &CompletionRequest,
    ) -> anyhow::Result<serde_json::Value> {
        let system_msg = request
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::System)
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("\n");

        // Anthropic wire format: tool_use/tool_result content blocks,
        // alternating-role merge — same serialization as the direct client.
        let messages = super::anthropic::AnthropicProvider::build_messages(request);

        let mut body = serde_json::json!({
            "model": Self::normalize_model_id(&request.model),
            "messages": messages,
        });

        body["max_tokens"] = serde_json::json!(request.max_tokens.unwrap_or(4096));

        if !system_msg.is_empty() {
            body["system"] = serde_json::json!(system_msg);
        }

        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        if !request.tools.is_empty() {
            body["tools"] = serde_json::json!(
                super::anthropic::AnthropicProvider::build_tool_definitions(&request.tools)
            );
            body["tool_choice"] = serde_json::json!({"type": "auto"});
        }

        for (key, value) in &request.extra_body {
            // Phoenix runtime hints in OpenAI shapes — the Messages API
            // rejects unknown top-level params.
            if key == "response_format" || key == "reasoning" {
                continue;
            }
            body[key] = value.clone();
        }

        let resp = self
            .bearer_request(&format!("{BASE_URL}/messages"))
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let err_text =
                super::read_error_response(resp, "OpenCode Messages API error response").await?;
            anyhow::bail!(
                "OpenCode messages API error ({}): {}",
                status,
                super::bounded_error_preview(&err_text)
            );
        }

        super::read_json_response(resp, "OpenCode Messages API response").await
    }

    async fn make_chat_request(
        &self,
        request: &CompletionRequest,
    ) -> anyhow::Result<serde_json::Value> {
        let messages: Vec<serde_json::Value> = request
            .messages
            .iter()
            .map(openai_compat::serialize_message)
            .collect();

        let mut body = serde_json::json!({
            "model": Self::normalize_model_id(&request.model),
            "messages": messages,
        });

        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }

        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        for (key, value) in &request.extra_body {
            // Responses-API `reasoning` object — strict chat-completions
            // backends 400 on unknown top-level params.
            if key == "reasoning" {
                continue;
            }
            body[key] = value.clone();
        }

        openai_compat::attach_tools(&mut body, &request.tools);

        let resp = self
            .bearer_request(&format!("{BASE_URL}/chat/completions"))
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let err_text =
                super::read_error_response(resp, "OpenCode chat API error response").await?;
            // Parse structured 429 errors for clear messaging
            if status.as_u16() == 429 {
                if let Ok(body) = serde_json::from_str::<serde_json::Value>(&err_text) {
                    let provider_name = body
                        .pointer("/error/metadata/provider_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    let is_byok = body
                        .pointer("/error/metadata/is_byok")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let raw_msg = body
                        .pointer("/error/metadata/raw")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Too many requests");
                    let provider_name = super::bounded_error_preview(provider_name);
                    let raw_msg = super::bounded_error_preview(raw_msg);
                    if is_byok {
                        anyhow::bail!(
                            "Rate limited by {} (BYOK). Your own API key hit the provider rate limit. Wait a moment and retry. Detail: {}",
                            provider_name, raw_msg
                        );
                    } else {
                        anyhow::bail!(
                            "Rate limited by {} (shared pool). The free/shared tier is congested. Wait 30-60s and retry, or switch to a BYOK model. Detail: {}",
                            provider_name, raw_msg
                        );
                    }
                }
                anyhow::bail!(
                    "OpenCode rate limit (429): {}",
                    super::bounded_error_preview(&err_text)
                );
            }
            anyhow::bail!(
                "OpenCode chat API error ({}): {}",
                status,
                super::bounded_error_preview(&err_text)
            );
        }

        super::read_json_response(resp, "OpenCode chat API response").await
    }

    async fn make_google_request(
        &self,
        request: &CompletionRequest,
    ) -> anyhow::Result<serde_json::Value> {
        // Gemini wire format: user/model roles, functionCall/functionResponse
        // parts, system prompt in systemInstruction — same serialization as
        // the direct Google client.
        let contents = super::google::GoogleProvider::build_contents(request);

        let mut body = serde_json::json!({
            "contents": contents,
        });

        let system_instruction = request
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::System || m.role == MessageRole::Developer)
            .map(|m| m.content.trim())
            .filter(|c| !c.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        if !system_instruction.is_empty() {
            body["systemInstruction"] = serde_json::json!({
                "parts": [{"text": system_instruction}]
            });
        }

        if let Some(max_tokens) = request.max_tokens {
            body["generationConfig"]["maxOutputTokens"] = serde_json::json!(max_tokens);
        }

        if let Some(temp) = request.temperature {
            body["generationConfig"]["temperature"] = serde_json::json!(temp);
        }

        let tool_declarations =
            super::google::GoogleProvider::build_tool_declarations(&request.tools);
        if !tool_declarations.is_empty() {
            body["tools"] = serde_json::json!(tool_declarations);
        }

        let model = Self::normalize_model_id(&request.model);
        let resp = self
            .bearer_request(&format!("{BASE_URL}/models/{model}:generateContent"))
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let err_text =
                super::read_error_response(resp, "OpenCode Gemini API error response").await?;
            anyhow::bail!(
                "OpenCode Gemini API error ({}): {}",
                status,
                super::bounded_error_preview(&err_text)
            );
        }

        super::read_json_response(resp, "OpenCode Gemini API response").await
    }

    fn parse_content(route: ModelRoute, json: &serde_json::Value) -> String {
        match route {
            ModelRoute::Responses => {
                if let Some(text) = json["output_text"].as_str() {
                    return text.to_string();
                }

                // Walk ALL output items — reasoning items come before the
                // message item, so "first item only" drops the actual text.
                json["output"]
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item["content"].as_array())
                            .flatten()
                            .filter(|part| {
                                matches!(part["type"].as_str(), Some("output_text") | None)
                            })
                            .filter_map(|part| part["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("")
                    })
                    .unwrap_or_default()
            }
            ModelRoute::Messages => json["content"]
                .as_array()
                .and_then(|items| items.first())
                .and_then(|item| item.get("text"))
                .and_then(|text| text.as_str())
                .unwrap_or("")
                .to_string(),
            ModelRoute::ChatCompletions => json["choices"]
                .as_array()
                .and_then(|items| items.first())
                .and_then(|item| item.get("message"))
                .and_then(|message| message.get("content"))
                .and_then(|text| text.as_str())
                .unwrap_or("")
                .to_string(),
            ModelRoute::GoogleGenerateContent => json["candidates"]
                .as_array()
                .and_then(|items| items.first())
                .and_then(|item| item.get("content"))
                .and_then(|content| content.get("parts"))
                .and_then(|parts| parts.as_array())
                .and_then(|parts| parts.first())
                .and_then(|part| part.get("text"))
                .and_then(|text| text.as_str())
                .unwrap_or("")
                .to_string(),
        }
    }

    fn parse_usage(route: ModelRoute, json: &serde_json::Value) -> TokenUsage {
        match route {
            ModelRoute::Responses => TokenUsage::new(
                json["usage"]["input_tokens"].as_u64().unwrap_or(0) as u32,
                json["usage"]["output_tokens"].as_u64().unwrap_or(0) as u32,
            ),
            ModelRoute::Messages => TokenUsage::new(
                json["usage"]["input_tokens"].as_u64().unwrap_or(0) as u32,
                json["usage"]["output_tokens"].as_u64().unwrap_or(0) as u32,
            ),
            ModelRoute::ChatCompletions => TokenUsage::new(
                json["usage"]["prompt_tokens"].as_u64().unwrap_or(0) as u32,
                json["usage"]["completion_tokens"].as_u64().unwrap_or(0) as u32,
            ),
            ModelRoute::GoogleGenerateContent => TokenUsage::new(
                json["usageMetadata"]["promptTokenCount"]
                    .as_u64()
                    .unwrap_or(0) as u32,
                json["usageMetadata"]["candidatesTokenCount"]
                    .as_u64()
                    .unwrap_or(0) as u32,
            ),
        }
    }

    fn parse_stop_reason(route: ModelRoute, json: &serde_json::Value) -> Option<String> {
        match route {
            ModelRoute::Responses => json["status"].as_str().map(String::from),
            ModelRoute::Messages => json["stop_reason"].as_str().map(String::from),
            ModelRoute::ChatCompletions => json["choices"]
                .as_array()
                .and_then(|items| items.first())
                .and_then(|item| item.get("finish_reason"))
                .and_then(|reason| reason.as_str())
                .map(String::from),
            ModelRoute::GoogleGenerateContent => json["candidates"]
                .as_array()
                .and_then(|items| items.first())
                .and_then(|item| item.get("finishReason"))
                .and_then(|reason| reason.as_str())
                .map(String::from),
        }
    }

    fn extract_tool_calls(route: ModelRoute, json: &serde_json::Value) -> Vec<NativeToolCall> {
        match route {
            ModelRoute::Responses => json["output"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|item| {
                    if item["type"].as_str() == Some("function_call") {
                        let call_id = item["call_id"]
                            .as_str()
                            .or_else(|| item["id"].as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = item["name"].as_str()?.to_string();
                        let args_str = item["arguments"].as_str().unwrap_or("{}");
                        let arguments =
                            serde_json::from_str(args_str).unwrap_or(serde_json::json!({}));
                        Some(NativeToolCall {
                            id: call_id,
                            tool_name: name,
                            arguments,
                        })
                    } else {
                        None
                    }
                })
                .collect(),
            ModelRoute::ChatCompletions => openai_compat::parse_tool_calls(json),
            _ => vec![],
        }
    }
}

#[async_trait]
impl LLMProvider for OpenCodeProvider {
    fn name(&self) -> &str {
        "opencode"
    }

    fn display_name(&self) -> &str {
        "OpenCode Zen"
    }

    fn base_url(&self) -> &str {
        BASE_URL
    }

    fn auth_type(&self) -> AuthType {
        AuthType::Bearer
    }

    fn env_vars(&self) -> Vec<&str> {
        vec!["OPENCODE_API_KEY"]
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
        let route = Self::route_for_model(&request.model);
        let json = self.make_request(&request).await?;

        // Messages/Gemini responses are parsed by the matching direct clients
        // (text blocks joined, tool_use/functionCall extracted, usage mapped).
        match route {
            ModelRoute::Messages => {
                return Ok(super::anthropic::AnthropicProvider::parse_response(
                    &json,
                    &request.model,
                ));
            }
            ModelRoute::GoogleGenerateContent => {
                return Ok(super::google::GoogleProvider::parse_response(
                    &json,
                    &request.model,
                ));
            }
            ModelRoute::Responses | ModelRoute::ChatCompletions => {}
        }

        let tool_calls = Self::extract_tool_calls(route, &json);
        let reasoning = match route {
            ModelRoute::ChatCompletions => super::openai_compat::parse_reasoning(&json),
            _ => None,
        };
        Ok(CompletionResponse {
            content: Self::parse_content(route, &json),
            model: json["model"].as_str().unwrap_or(&request.model).to_string(),
            usage: Self::parse_usage(route, &json),
            reasoning,
            stop_reason: Self::parse_stop_reason(route, &json),
            tool_calls,
            provider_replay: None,
        })
    }

    async fn stream(&self, request: CompletionRequest) -> anyhow::Result<StreamingResponse> {
        let route = Self::route_for_model(&request.model);
        let json = self.make_request(&request).await?;

        Ok(StreamingResponse {
            content: Self::parse_content(route, &json),
            reasoning: None,
            done: true,
        })
    }

    async fn embeddings(&self, _texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
        anyhow::bail!("OpenCode Zen does not expose embeddings in the current Phoenix client")
    }

    async fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        let provider_data = providers_data::get_provider("opencode")
            .ok_or_else(|| anyhow::anyhow!("OpenCode provider data not found"))?;

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
                supports_tools: Some(true),
                supports_vision: Some(
                    m.id.starts_with("gpt-")
                        || m.id.starts_with("claude-")
                        || m.id.starts_with("gemini-"),
                ),
            })
            .collect())
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        let request = CompletionRequest {
            model: self.default_model().to_string(),
            messages: vec![super::contracts::ChatMessage::user(
                "Reply with the single word: phoenix",
            )],
            max_tokens: Some(8),
            temperature: None,
            stream: false,
            extra_body: HashMap::new(),
            session_id: None,
            stream_observer: None,
            tools: vec![],
            native_replay: None,
        };

        let response = self.complete(request).await?;
        Ok(!response.content.trim().is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::{ModelRoute, OpenCodeProvider};

    #[test]
    fn strips_opencode_prefix() {
        assert_eq!(
            OpenCodeProvider::normalize_model_id("opencode/gpt-5.5"),
            "gpt-5.5"
        );
    }

    #[test]
    fn routes_known_model_families() {
        assert_eq!(
            OpenCodeProvider::route_for_model("claude-sonnet-4-6"),
            ModelRoute::Messages
        );
        assert_eq!(
            OpenCodeProvider::route_for_model("gpt-5.5"),
            ModelRoute::Responses
        );
        assert_eq!(
            OpenCodeProvider::route_for_model("qwen3.6-plus"),
            ModelRoute::ChatCompletions
        );
        assert_eq!(
            OpenCodeProvider::route_for_model("gemini-3.1-pro"),
            ModelRoute::GoogleGenerateContent
        );
    }
}
