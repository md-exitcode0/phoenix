//! Ollama provider — local OpenAI-compatible API and Ollama Cloud (ollama.com).

use async_trait::async_trait;
use reqwest::Client;
use std::collections::HashMap;
use std::time::Duration;

use super::contracts::{
    AuthType, CompletionRequest, CompletionResponse, LLMProvider, MessageRole, ModelInfo,
    NativeToolCall, StreamingResponse, TokenUsage, ToolDefinition,
};
use super::openai_compat;
use super::providers_data;

pub struct OllamaProvider {
    client: Client,
    base_url: String,
    api_key: Option<String>,
    catalog_id: String,
}

impl OllamaProvider {
    pub fn new() -> Self {
        Self::with_url_and_timeout(
            "http://localhost:11434".to_string(),
            Duration::from_secs(120),
        )
    }

    pub fn with_url(base_url: String) -> Self {
        Self::with_url_and_timeout(base_url, Duration::from_secs(120))
    }

    pub fn with_url_and_timeout(base_url: String, timeout: Duration) -> Self {
        Self::with_url_auth_and_timeout(base_url, None, "ollama", timeout)
    }

    /// Hosted models at ollama.com (requires `OLLAMA_API_KEY`).
    pub fn with_cloud(api_key: String, timeout: Duration) -> Self {
        Self::with_url_auth_and_timeout(
            "https://ollama.com".to_string(),
            Some(api_key),
            "ollama-cloud",
            timeout,
        )
    }

    pub fn with_url_auth_and_timeout(
        base_url: String,
        api_key: Option<String>,
        catalog_id: &str,
        timeout: Duration,
    ) -> Self {
        let client = super::apply_read_timeout(
            Client::builder().connect_timeout(Duration::from_secs(30)),
            timeout,
        )
            .build()
            .expect("Ollama client build failed");
        Self {
            client,
            base_url,
            api_key,
            catalog_id: catalog_id.to_string(),
        }
    }

    fn has_model_impl(&self, model: &str) -> bool {
        providers_data::get_provider(&self.catalog_id)
            .map(|p| p.has_model(model))
            .unwrap_or(false)
    }

    fn default_model_impl(&self) -> &str {
        if self.catalog_id == "ollama-cloud" {
            "gpt-oss:20b"
        } else {
            "llama3.2"
        }
    }

    fn fallback_models_impl(&self) -> Vec<&str> {
        if self.catalog_id == "ollama-cloud" {
            vec!["gemma3:27b", "gpt-oss:120b"]
        } else {
            vec!["mistral", "qwen2.5"]
        }
    }

    fn display_name_impl(&self) -> &str {
        providers_data::get_provider(&self.catalog_id)
            .map(|p| p.name)
            .unwrap_or("Ollama")
    }

    fn tools_to_ollama_native(tools: &[ToolDefinition]) -> Vec<serde_json::Value> {
        tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters
                    }
                })
            })
            .collect()
    }

    fn parse_cloud_native_tool_calls(json: &serde_json::Value) -> Vec<NativeToolCall> {
        json["message"]["tool_calls"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .enumerate()
                    .filter_map(|(idx, tc)| {
                        let name = tc["function"]["name"].as_str()?.to_string();
                        let arguments = tc["function"]["arguments"].clone();
                        // Ollama issues no call ids; synthesize unique ones so
                        // results pair back to calls on later rounds.
                        let id = format!("call_{}_{}", name, idx);
                        Some(NativeToolCall {
                            id,
                            tool_name: name,
                            arguments,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Serialize messages for the native /api/chat endpoint. Assistant tool
    /// calls ride in `message.tool_calls` (arguments as an object) and tool
    /// results are `role: "tool"` messages carrying `tool_name` — Ollama pairs
    /// by name, not id.
    fn cloud_native_messages(request: &CompletionRequest) -> Vec<serde_json::Value> {
        request
            .messages
            .iter()
            .map(|m| {
                if !m.tool_calls.is_empty() {
                    let calls: Vec<serde_json::Value> = m
                        .tool_calls
                        .iter()
                        .map(|tc| {
                            serde_json::json!({
                                "function": {
                                    "name": tc.tool_name,
                                    "arguments": tc.arguments
                                }
                            })
                        })
                        .collect();
                    serde_json::json!({
                        "role": "assistant",
                        "content": m.content,
                        "tool_calls": calls
                    })
                } else if m.role == MessageRole::Tool {
                    serde_json::json!({
                        "role": "tool",
                        "content": m.content,
                        "tool_name": super::google::function_name_from_call_id(
                            m.tool_call_id.as_deref().unwrap_or("")
                        )
                    })
                } else {
                    serde_json::json!({
                        "role": m.role.to_string(),
                        "content": m.content,
                    })
                }
            })
            .collect()
    }

    /// Ollama's native channel for extended thinking is `message.thinking`.
    ///
    /// This used to be hard-coded `None` at the response site: the field was
    /// parsed (for the empty-content retry heuristic) and then thrown away. On
    /// a box whose every lane is ollama-cloud that meant `response.reasoning`
    /// was ALWAYS None, so `AgentThinking` was never emitted and no agent
    /// window, Mission Control row, or TUI line had anything to show — the
    /// whole reasoning pipeline downstream was correct and starved.
    fn native_reasoning(json: &serde_json::Value) -> Option<String> {
        json["message"]["thinking"]
            .as_str()
            .map(str::to_string)
            .filter(|thinking| !thinking.trim().is_empty())
    }

    async fn complete_cloud_native(
        &self,
        request: CompletionRequest,
    ) -> anyhow::Result<CompletionResponse> {
        let messages = Self::cloud_native_messages(&request);

        // Thinking models (deepseek-v4-flash, glm, qwen…) spend `num_predict`
        // on the reasoning channel FIRST: a verdict-class call (small
        // max_tokens, fixed rubric) got back `content: ""` with the whole
        // budget burned in `message.thinking` — which parsed as an Unknown
        // verdict and silently disabled every judge in the building. Small
        // caps ⇒ the caller wants an answer, not a think-aloud: disable
        // thinking up front, and salvage any other empty-content/thinking
        // response with one think-off retry.
        let mut disable_think = request.max_tokens.map(|m| m <= 512).unwrap_or(false);
        // Effort → think mapping (plan 018 per-lane efforts): Ollama has no
        // OpenAI-style reasoning_effort param; gpt-oss takes string levels
        // (think: "low"/"medium"/"high"), other thinking models only a bool.
        // A verdict-class call (disable_think) always wins over effort.
        let mut effort_think: Option<serde_json::Value> = request
            .extra_body
            .get("reasoning")
            .and_then(|r| r["effort"].as_str())
            .map(|effort| {
                if request.model.to_ascii_lowercase().contains("gpt-oss") {
                    let level = match effort {
                        "minimal" | "low" => "low",
                        "medium" => "medium",
                        _ => "high", // high | xhigh | max
                    };
                    serde_json::json!(level)
                } else {
                    serde_json::json!(true)
                }
            });
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let mut body = serde_json::json!({
                "model": request.model,
                "messages": messages,
                "stream": false,
            });
            if disable_think {
                body["think"] = serde_json::json!(false);
            } else if let Some(think) = &effort_think {
                body["think"] = think.clone();
            }
            if let Some(temp) = request.temperature {
                body["options"]["temperature"] = serde_json::json!(temp);
            }
            if let Some(max_tokens) = request.max_tokens {
                body["options"]["num_predict"] = serde_json::json!(max_tokens);
            }
            if !request.tools.is_empty() {
                body["tools"] = serde_json::json!(Self::tools_to_ollama_native(&request.tools));
            }

            let mut req = self
                .client
                .post(format!("{}/api/chat", self.base_url.trim_end_matches('/')))
                .json(&body);
            if let Some(key) = &self.api_key {
                req = req.bearer_auth(key);
            }
            let resp = req.send().await?;
            let status = resp.status();
            if !status.is_success() {
                let raw =
                    super::read_error_response(resp, "Ollama Cloud API error response").await?;
                // A model that rejects the `think` flag outright still works
                // without it — retry once flag-free rather than failing the
                // whole lane over an optimization. Covers both the disable
                // path and the effort-mapped path.
                if attempt == 1
                    && (disable_think || effort_think.is_some())
                    && raw.to_ascii_lowercase().contains("think")
                {
                    disable_think = false;
                    effort_think = None;
                    continue;
                }
                anyhow::bail!(
                    "Ollama Cloud API error ({}): {}",
                    status,
                    super::bounded_error_preview(&raw)
                );
            }
            let raw = super::read_response_text(
                resp,
                super::MAX_PROVIDER_RESPONSE_BYTES,
                "Ollama Cloud API response",
            )
            .await?;
            let json: serde_json::Value = serde_json::from_str(&raw)
                .map_err(|e| anyhow::anyhow!("Ollama Cloud response was not valid JSON: {e}"))?;

            let content = json["message"]["content"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let thinking = json["message"]["thinking"].as_str().unwrap_or("");
            let model = json["model"].as_str().unwrap_or(&request.model).to_string();
            let tool_calls = Self::parse_cloud_native_tool_calls(&json);
            if content.is_empty()
                && tool_calls.is_empty()
                && !thinking.is_empty()
                && !disable_think
                && attempt == 1
            {
                disable_think = true;
                continue;
            }
            // Ollama's native response reports REAL work, not request size:
            // `prompt_eval_count` = prompt tokens actually evaluated this call
            // (with server-side KV prefix reuse this is just the new suffix —
            // so a small number here IS the cache hit receipt), `eval_count` =
            // generated tokens. This lane hard-coded (0, 0) for weeks and the
            // primary lane's telemetry was blind.
            let input_tokens = json["prompt_eval_count"].as_u64().unwrap_or(0) as u32;
            let output_tokens = json["eval_count"].as_u64().unwrap_or(0) as u32;
            return Ok(CompletionResponse {
                content,
                model,
                usage: TokenUsage::new(input_tokens, output_tokens),
                reasoning: Self::native_reasoning(&json),
                stop_reason: if tool_calls.is_empty() {
                    json["done"].as_bool().map(|_| "stop".to_string())
                } else {
                    Some("tool_calls".to_string())
                },
                tool_calls,
                provider_replay: None,
            });
        }
    }

    async fn complete_openai_compat(
        &self,
        request: CompletionRequest,
    ) -> anyhow::Result<CompletionResponse> {
        let messages: Vec<serde_json::Value> = request
            .messages
            .iter()
            .map(openai_compat::serialize_message)
            .collect();

        let mut body = serde_json::json!({
            "model": request.model,
            "messages": messages,
            "stream": false,
        });

        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }

        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        openai_compat::attach_tools(&mut body, &request.tools);

        let mut req = self
            .client
            .post(format!(
                "{}/v1/chat/completions",
                self.base_url.trim_end_matches('/')
            ))
            .json(&body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().await?;

        let status = resp.status();
        if !status.is_success() {
            let err_text = super::read_error_response(resp, "Ollama API error response").await?;
            anyhow::bail!(
                "Ollama API error ({}): {}",
                status,
                super::bounded_error_preview(&err_text)
            );
        }

        let json: serde_json::Value =
            super::read_json_response(resp, "Ollama API response").await?;
        let mut result = openai_compat::parse_completion_response(&json, &request.model);

        // Ollama openai-compat may not set finish_reason; fall back to "done" field
        if result.stop_reason.is_none() {
            result.stop_reason = json["done"].as_bool().map(|_| "stop".to_string());
        }

        Ok(result)
    }
}

impl Default for OllamaProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LLMProvider for OllamaProvider {
    /// Speaks the OpenAI multipart-content wire format, so attached
    /// screenshot data URIs reach the model as real images.
    fn supports_native_images(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        &self.catalog_id
    }

    fn display_name(&self) -> &str {
        self.display_name_impl()
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn auth_type(&self) -> AuthType {
        if self.api_key.is_some() {
            AuthType::Bearer
        } else {
            AuthType::None
        }
    }

    fn env_vars(&self) -> Vec<&str> {
        if self.catalog_id == "ollama-cloud" {
            vec!["OLLAMA_API_KEY"]
        } else {
            vec![]
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
        if self.catalog_id == "ollama-cloud" {
            return self.complete_cloud_native(request).await;
        }
        self.complete_openai_compat(request).await
    }

    async fn stream(&self, request: CompletionRequest) -> anyhow::Result<StreamingResponse> {
        let json = self.complete(request).await?;

        Ok(StreamingResponse {
            content: json.content,
            reasoning: json.reasoning,
            done: true,
        })
    }

    async fn embeddings(&self, _texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
        anyhow::bail!("Ollama does not support embeddings via chat API")
    }

    async fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        let mut tags_req = self
            .client
            .get(format!("{}/api/tags", self.base_url.trim_end_matches('/')));
        if let Some(key) = &self.api_key {
            tags_req = tags_req.bearer_auth(key);
        }
        let resp = tags_req.send().await?;

        if !resp.status().is_success() {
            let provider_data = providers_data::get_provider(&self.catalog_id)
                .ok_or_else(|| anyhow::anyhow!("Ollama provider data not found"))?;
            return Ok(provider_data
                .models
                .iter()
                .map(|m| ModelInfo {
                    id: m.id.to_string(),
                    object: "model".to_string(),
                    created: None,
                    context_window: Some(m.context_window),
                    input_cost_per_token: None,
                    output_cost_per_token: None,
                    supports_tools: Some(false),
                    supports_vision: Some(m.id.contains("vision")),
                })
                .collect());
        }

        let json: serde_json::Value =
            super::read_json_response(resp, "Ollama model-list response").await?;

        let models: Vec<ModelInfo> = json["models"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|m| ModelInfo {
                        id: m["name"].as_str().unwrap_or("").to_string(),
                        object: "model".to_string(),
                        created: None,
                        context_window: m["size"]
                            .as_u64()
                            .map(|s| (s / 1_000_000_000) as u32 * 1_000_000),
                        input_cost_per_token: None,
                        output_cost_per_token: None,
                        supports_tools: Some(false),
                        supports_vision: Some(
                            m["name"]
                                .as_str()
                                .map(|n| n.contains("vision"))
                                .unwrap_or(false),
                        ),
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(models)
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        let model = self.default_model();
        let (url, body) = if self.catalog_id == "ollama-cloud" {
            (
                format!("{}/api/chat", self.base_url.trim_end_matches('/')),
                serde_json::json!({
                    "model": model,
                    "messages": [{"role": "user", "content": "hi"}],
                    "stream": false
                }),
            )
        } else {
            (
                format!(
                    "{}/v1/chat/completions",
                    self.base_url.trim_end_matches('/')
                ),
                serde_json::json!({
                    "model": model,
                    "max_tokens": 5,
                    "messages": [{"role": "user", "content": "hi"}],
                    "stream": false
                }),
            )
        };
        let mut req = self.client.post(url).json(&body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        Ok(req.send().await.is_ok())
    }
}

#[cfg(test)]
mod ollama_reasoning_tests {
    use super::*;

    /// The reasoning channel must survive the response parse. Regression for
    /// 2026-07-20: `complete_cloud_native` returned `reasoning: None`
    /// unconditionally, so on an all-ollama box NO agent ever emitted a
    /// thought and every reasoning surface in the app sat empty.
    #[test]
    fn native_thinking_becomes_reasoning() {
        // Shape of a real ollama.com /api/chat reply for a thinking model.
        let json: serde_json::Value = serde_json::from_str(
            r#"{
                "model": "glm-5.2:cloud",
                "message": {
                    "role": "assistant",
                    "thinking": "17 * 23 = 17*20 + 17*3 = 340 + 51 = 391.",
                    "content": "391"
                },
                "done": true,
                "prompt_eval_count": 12,
                "eval_count": 34
            }"#,
        )
        .unwrap();
        assert_eq!(
            OllamaProvider::native_reasoning(&json).as_deref(),
            Some("17 * 23 = 17*20 + 17*3 = 340 + 51 = 391.")
        );
    }

    #[test]
    fn absent_or_blank_thinking_is_none() {
        let no_field: serde_json::Value =
            serde_json::from_str(r#"{"message":{"content":"hi"}}"#).unwrap();
        assert_eq!(OllamaProvider::native_reasoning(&no_field), None);
        // A whitespace-only channel must not render an empty thinking box.
        let blank: serde_json::Value =
            serde_json::from_str(r#"{"message":{"thinking":"  \n ","content":"hi"}}"#).unwrap();
        assert_eq!(OllamaProvider::native_reasoning(&blank), None);
    }
}
