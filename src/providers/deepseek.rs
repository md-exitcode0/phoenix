//! DeepSeek provider implementation

use async_trait::async_trait;
use reqwest::Client;
use std::collections::HashMap;
use std::time::Duration;

use super::contracts::{
    AuthType, CompletionRequest, CompletionResponse, LLMProvider, ModelInfo, StreamingResponse,
};
use super::openai_compat;
use super::providers_data;

pub struct DeepSeekProvider {
    client: Client,
    api_key: String,
}

impl DeepSeekProvider {
    pub fn new(api_key: String) -> Self {
        Self::with_timeout(api_key, Duration::from_secs(120))
    }

    pub fn with_timeout(api_key: String, timeout: Duration) -> Self {
        let client = super::apply_read_timeout(
            Client::builder().connect_timeout(Duration::from_secs(30)),
            timeout,
        )
            .build()
            .expect("DeepSeek client build failed");
        Self { client, api_key }
    }

    fn has_model_impl(&self, model: &str) -> bool {
        let provider_data = providers_data::get_provider("deepseek");
        provider_data.map(|p| p.has_model(model)).unwrap_or(false)
    }

    fn default_model_impl(&self) -> &str {
        "deepseek-chat"
    }

    fn fallback_models_impl(&self) -> Vec<&str> {
        vec!["deepseek-reasoner", "deepseek-v3"]
    }
}

#[async_trait]
impl LLMProvider for DeepSeekProvider {
    /// Speaks the OpenAI multipart-content wire format, so attached
    /// screenshot data URIs reach the model as real images.
    fn supports_native_images(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "deepseek"
    }

    fn display_name(&self) -> &str {
        "DeepSeek"
    }

    fn base_url(&self) -> &str {
        "https://api.deepseek.com/v1"
    }

    fn auth_type(&self) -> AuthType {
        AuthType::Bearer
    }

    fn env_vars(&self) -> Vec<&str> {
        vec!["DEEPSEEK_API_KEY"]
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
            // Responses-API `reasoning` object is a Phoenix runtime hint;
            // DeepSeek has no effort param and rejects unknown keys.
            if key == "reasoning" {
                continue;
            }
            body[key] = value.clone();
        }

        openai_compat::attach_tools(&mut body, &request.tools);

        let resp = self
            .client
            .post("https://api.deepseek.com/v1/chat/completions")
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let err_text = super::read_error_response(resp, "DeepSeek API error response").await?;
            anyhow::bail!(
                "DeepSeek API error ({}): {}",
                status,
                super::bounded_error_preview(&err_text)
            );
        }

        let json: serde_json::Value =
            super::read_json_response(resp, "DeepSeek API response").await?;
        Ok(openai_compat::parse_completion_response(
            &json,
            &request.model,
        ))
    }

    async fn stream(&self, request: CompletionRequest) -> anyhow::Result<StreamingResponse> {
        let json = self.complete(request).await?;

        Ok(StreamingResponse {
            content: json.content,
            reasoning: None,
            done: true,
        })
    }

    async fn embeddings(&self, texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
        let resp = self
            .client
            .post("https://api.deepseek.com/v1/embeddings")
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({
                "model": "deepseek-chat",
                "input": texts,
            }))
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let err_text =
                super::read_error_response(resp, "DeepSeek embeddings error response").await?;
            anyhow::bail!(
                "DeepSeek embeddings error ({}): {}",
                status,
                super::bounded_error_preview(&err_text)
            );
        }

        let json: serde_json::Value =
            super::read_json_response(resp, "DeepSeek embeddings response").await?;
        let embeddings: Vec<Vec<f32>> = json["data"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        item["embedding"].as_array().map(|emb| {
                            emb.iter()
                                .filter_map(|v| v.as_f64().map(|f| f as f32))
                                .collect::<Vec<_>>()
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(embeddings)
    }

    async fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        let provider_data = providers_data::get_provider("deepseek")
            .ok_or_else(|| anyhow::anyhow!("DeepSeek provider data not found"))?;

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
                supports_vision: Some(false),
            })
            .collect())
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        let resp = self
            .client
            .post("https://api.deepseek.com/v1/chat/completions")
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({
                "model": "deepseek-chat",
                "max_tokens": 5,
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .send()
            .await?;
        Ok(resp.status().is_success())
    }
}
