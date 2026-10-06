//! OpenRouter provider — native tool calling via OpenAI-compatible /chat/completions

use async_trait::async_trait;
use reqwest::Client;
use std::collections::HashMap;
use std::time::Duration;

use super::contracts::{
    AuthType, CompletionRequest, CompletionResponse, LLMProvider, ModelInfo, StreamingResponse,
};
use super::openai_compat;
use super::providers_data;
use crate::debug_session;

pub struct OpenRouterProvider {
    client: Client,
    api_key: String,
}

impl OpenRouterProvider {
    pub fn new(api_key: String) -> Self {
        Self::with_timeout(api_key, Duration::from_secs(120))
    }

    pub fn with_timeout(api_key: String, timeout: Duration) -> Self {
        let client = super::apply_read_timeout(
            Client::builder().connect_timeout(Duration::from_secs(30)),
            timeout,
        )
            .build()
            .expect("OpenRouter client build failed");
        Self { client, api_key }
    }

    fn has_model_impl(&self, model: &str) -> bool {
        providers_data::get_provider("openrouter")
            .map(|p| p.has_model(model))
            .unwrap_or(false)
    }

    fn default_model_impl(&self) -> &str {
        "anthropic/claude-sonnet-4.6"
    }

    fn fallback_models_impl(&self) -> Vec<&str> {
        vec!["openai/gpt-4o", "deepseek/deepseek-chat"]
    }

    async fn post_completions(&self, body: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        super::retry::with_retry(|| self.send_completions(&body)).await
    }

    async fn send_completions(
        &self,
        body: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let resp = self
            .client
            .post("https://openrouter.ai/api/v1/chat/completions")
            .bearer_auth(&self.api_key)
            .header("HTTP-Referer", "https://phoenix-agent.ai")
            .header("X-Title", "PhoenixAgent")
            .json(body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let err_text =
                super::read_error_response(resp, "OpenRouter API error response").await?;
            if let Ok(body) = serde_json::from_str::<serde_json::Value>(&err_text) {
                let is_byok = body
                    .pointer("/error/metadata/is_byok")
                    .and_then(|v| v.as_bool());
                let provider_name = body
                    .pointer("/error/metadata/provider_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let raw_msg = body
                    .pointer("/error/metadata/raw")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Too many requests");
                // #region agent log
                debug_session::log(
                    "E",
                    "openrouter.rs:post_completions",
                    "OpenRouter API error",
                    serde_json::json!({
                        "status": status.as_u16(),
                        "is_byok": is_byok,
                        "user_id": body.get("user_id").and_then(|v| v.as_str()),
                    }),
                );
                // #endregion
                if status.as_u16() == 429 {
                    let provider_name = super::bounded_error_preview(provider_name);
                    let raw_msg = super::bounded_error_preview(raw_msg);
                    if is_byok == Some(false) {
                        anyhow::bail!(
                            "Rate limited by {} (shared pool). Free tier is congested. Wait 30-60s and retry, or use a BYOK key. Detail: {}",
                            provider_name, raw_msg
                        );
                    } else {
                        anyhow::bail!(
                            "Rate limited by {} (BYOK). Your own API key hit the provider rate limit. Wait a moment and retry. Detail: {}",
                            provider_name, raw_msg
                        );
                    }
                }
            }
            anyhow::bail!(
                "OpenRouter API error ({}): {}",
                status,
                super::bounded_error_preview(&err_text)
            );
        }

        super::read_json_response(resp, "OpenRouter API response").await
    }
}

#[async_trait]
impl LLMProvider for OpenRouterProvider {
    /// Speaks the OpenAI multipart-content wire format, so attached
    /// screenshot data URIs reach the model as real images.
    fn supports_native_images(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "openrouter"
    }
    fn display_name(&self) -> &str {
        "OpenRouter"
    }
    fn base_url(&self) -> &str {
        "https://openrouter.ai/api/v1"
    }
    fn auth_type(&self) -> AuthType {
        AuthType::Bearer
    }
    fn env_vars(&self) -> Vec<&str> {
        vec!["OPENROUTER_API_KEY"]
    }

    fn default_headers(&self) -> HashMap<String, String> {
        let mut h = HashMap::new();
        h.insert(
            "HTTP-Referer".to_string(),
            "https://phoenix-agent.ai".to_string(),
        );
        h.insert("X-Title".to_string(), "PhoenixAgent".to_string());
        h
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
        let messages: Vec<serde_json::Value> = request
            .messages
            .iter()
            .map(openai_compat::serialize_message)
            .collect();

        let mut body = serde_json::json!({
            "model": request.model,
            "messages": messages,
        });

        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }
        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        for (key, value) in &request.extra_body {
            if key == "response_format" {
                continue;
            }
            body[key] = value.clone();
        }

        openai_compat::attach_tools(&mut body, &request.tools);

        let json = self.post_completions(body).await?;
        Ok(openai_compat::parse_completion_response(
            &json,
            &request.model,
        ))
    }

    async fn stream(&self, request: CompletionRequest) -> anyhow::Result<StreamingResponse> {
        let messages: Vec<serde_json::Value> = request
            .messages
            .iter()
            .map(openai_compat::serialize_message)
            .collect();

        let mut body = serde_json::json!({
            "model": request.model,
            "messages": messages,
            "stream": true,
        });
        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }
        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        let json = self.post_completions(body).await?;
        let content = json["choices"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|c| c["message"]["content"].as_str())
            .unwrap_or("")
            .to_string();

        Ok(StreamingResponse {
            content,
            reasoning: None,
            done: true,
        })
    }

    async fn embeddings(&self, _texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
        anyhow::bail!("OpenRouter does not support embeddings. Use OpenAI or another provider.")
    }

    async fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        let provider_data = providers_data::get_provider("openrouter")
            .ok_or_else(|| anyhow::anyhow!("OpenRouter provider data not found"))?;
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
                    m.id.contains("vision") || m.id.contains("claude") || m.id.contains("gpt-4o"),
                ),
            })
            .collect())
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        let body = serde_json::json!({
            "model": "openai/gpt-4o-mini",
            "max_tokens": 5,
            "messages": [{"role": "user", "content": "hi"}]
        });
        Ok(self.post_completions(body).await.is_ok())
    }
}
