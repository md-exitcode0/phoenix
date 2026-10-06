//! OpenAI provider — native tool calling via /v1/chat/completions

use anyhow::Context;
use async_trait::async_trait;
use reqwest::Client;
use std::collections::HashMap;
use std::time::Duration;

use super::contracts::{
    AuthType, CompletionRequest, CompletionResponse, LLMProvider, ModelInfo,
    NativeCompactionCapability, NativeCompactionRequest, NativeCompactionResult,
    ProviderResponseDialect, StreamingResponse,
};
use super::openai_codex::OpenAICodexProvider;
use super::openai_compat;
use super::providers_data;

pub struct OpenAIProvider {
    client: Client,
    api_key: String,
    base_url: String,
    /// Direct OpenAI profiles remain Chat Completions-compatible for general
    /// models and embeddings, while reasoning text models can opt into the
    /// first-party Responses/compaction path.  Keeping this as a model-aware
    /// delegate avoids making the entire `openai` provider pretend every model
    /// speaks Responses.
    responses: Option<OpenAICodexProvider>,
}

/// OpenAI model families that accept the chat-completions `reasoning_effort`
/// param. Everything else (gpt-4o and earlier, third-party models served
/// through OpenAI-compatible APIs) 400s on it.
fn is_openai_reasoning_model(model: &str) -> bool {
    let m = model.trim().to_ascii_lowercase();
    m.starts_with("gpt-5")
        || m.starts_with("gpt-6-")
        || m.starts_with("gpt-6.")
        || m.starts_with("o1")
        || m.starts_with("o3")
        || m.starts_with("o4")
}

impl OpenAIProvider {
    pub fn new(api_key: String) -> Self {
        Self::with_url_and_timeout(
            "https://api.openai.com".to_string(),
            api_key,
            Duration::from_secs(120),
        )
    }

    pub fn with_timeout(api_key: String, timeout: Duration) -> Self {
        Self::with_url_and_timeout("https://api.openai.com".to_string(), api_key, timeout)
    }

    pub fn with_url_and_timeout(base_url: String, api_key: String, timeout: Duration) -> Self {
        Self::with_url_and_timeout_and_native(base_url, api_key, timeout, false)
    }

    pub fn with_url_and_timeout_and_native(
        base_url: String,
        api_key: String,
        timeout: Duration,
        native_compaction_enabled: bool,
    ) -> Self {
        let client = super::apply_read_timeout(
            Client::builder().connect_timeout(Duration::from_secs(30)),
            timeout,
        )
            .build()
            .expect("OpenAI client build failed");
        let responses = native_compaction_enabled.then(|| {
            OpenAICodexProvider::for_openai_api(base_url.clone(), api_key.clone(), timeout, true)
        });
        Self {
            client,
            api_key,
            base_url,
            responses,
        }
    }

    fn responses_for_model(&self, model: &str) -> Option<&OpenAICodexProvider> {
        self.responses
            .as_ref()
            .filter(|provider| provider.native_compaction_capability(model).is_supported())
    }

    /// Build an endpoint URL, version-aware. Catalog base URLs already include
    /// the API version (e.g. `.../v1`, `.../openai/v1`, `.../v1/openai`); the
    /// bare default (`https://api.openai.com`) does not. Only prepend `/v1` when
    /// the base omits it — otherwise we'd hit `.../v1/v1/...` (a 404).
    fn endpoint_url(&self, path: &str) -> String {
        let base = self.base_url.trim_end_matches('/');
        if base.contains("/v1") {
            format!("{base}/{path}")
        } else {
            format!("{base}/v1/{path}")
        }
    }

    fn chat_url(&self) -> String {
        self.endpoint_url("chat/completions")
    }

    fn has_model_impl(&self, model: &str) -> bool {
        providers_data::get_provider("openai")
            .map(|p| p.has_model(model))
            .unwrap_or(false)
    }

    fn default_model_impl(&self) -> &str {
        "gpt-4o"
    }

    fn fallback_models_impl(&self) -> Vec<&str> {
        vec!["gpt-4o-mini", "gpt-4-turbo"]
    }

    async fn make_request(
        &self,
        request: &CompletionRequest,
        stream: bool,
    ) -> anyhow::Result<serde_json::Value> {
        super::retry::with_retry(|| self.send_request(request, stream)).await
    }

    async fn send_request(
        &self,
        request: &CompletionRequest,
        stream: bool,
    ) -> anyhow::Result<serde_json::Value> {
        let messages: Vec<serde_json::Value> = request
            .messages
            .iter()
            .map(openai_compat::serialize_message)
            .collect();

        let mut body = serde_json::json!({
            "model": request.model,
            "messages": messages,
            "stream": stream,
        });

        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }
        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        for (key, value) in &request.extra_body {
            // `response_format` and the Responses-API `reasoning` object are
            // Phoenix runtime hints; chat/completions servers (OpenAI, MiniMax,
            // Groq, …) reject unknown top-level params with a 400. OpenAI's own
            // reasoning models take the effort as `reasoning_effort` instead.
            if key == "response_format" || key == "reasoning" {
                continue;
            }
            body[key] = value.clone();
        }
        if let Some(effort) = request
            .extra_body
            .get("reasoning")
            .and_then(|r| r["effort"].as_str())
        {
            if self.base_url.contains("api.openai.com") && is_openai_reasoning_model(&request.model)
            {
                body["reasoning_effort"] = serde_json::json!(effort);
            }
        }
        // Session-keyed cache routing (donor: opencode) — OpenAI's automatic
        // prefix caching only pays off when a session's rounds land on the
        // same cache shard. Only on api.openai.com: compat servers behind
        // this provider may 400 on unknown params.
        if self.base_url.contains("api.openai.com") {
            openai_compat::attach_prompt_cache_key(&mut body, request.session_id.as_deref());
        }

        openai_compat::attach_tools(&mut body, &request.tools);

        let resp = self
            .client
            .post(self.chat_url())
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let err_text = super::read_error_response(resp, "OpenAI API error response").await?;
            if status.as_u16() == 429 {
                if let Ok(body) = serde_json::from_str::<serde_json::Value>(&err_text) {
                    let err_msg = body["error"]["message"].as_str().unwrap_or("Rate limited");
                    anyhow::bail!(
                        "OpenAI rate limit (429): {}. Wait a moment and retry.",
                        super::bounded_error_preview(err_msg)
                    );
                }
                anyhow::bail!(
                    "OpenAI rate limit (429): {}",
                    super::bounded_error_preview(&err_text)
                );
            }
            // Tier gate (xai SuperGrok et al.): the credential authenticates
            // but the plan tier has no API access. Raw 403 JSON in the chat
            // pane reads like a broken login, so the turn fails with one
            // actionable sentence; the raw body goes to the gateway log.
            if let Some(message) =
                super::tier_gate::map_response(status.as_u16(), &self.base_url, &err_text)
            {
                anyhow::bail!("{message}");
            }
            anyhow::bail!(
                "OpenAI API error ({}): {}",
                status,
                super::bounded_error_preview(&err_text)
            );
        }

        // A working call clears the tier-gate spam suppression for this host
        // (so a lane that gets re-gated later logs loud again).
        super::tier_gate::note_success(&self.base_url);
        super::read_json_response(resp, "OpenAI API response").await
    }
}

#[async_trait]
impl LLMProvider for OpenAIProvider {
    /// Speaks the OpenAI multipart-content wire format, so attached
    /// screenshot data URIs reach the model as real images.
    fn supports_native_images(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "openai"
    }
    fn display_name(&self) -> &str {
        "OpenAI"
    }
    fn base_url(&self) -> &str {
        &self.base_url
    }
    fn auth_type(&self) -> AuthType {
        AuthType::Bearer
    }
    fn env_vars(&self) -> Vec<&str> {
        vec!["OPENAI_API_KEY"]
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

    fn native_compaction_capability(&self, model: &str) -> NativeCompactionCapability {
        self.responses_for_model(model)
            .map(|provider| provider.native_compaction_capability(model))
            .unwrap_or(NativeCompactionCapability::Unsupported)
    }

    fn response_dialect(&self, model: &str) -> ProviderResponseDialect {
        if self.responses_for_model(model).is_some() {
            ProviderResponseDialect::OpenAiResponses
        } else {
            ProviderResponseDialect::Generic
        }
    }

    async fn compact_context(
        &self,
        request: NativeCompactionRequest,
    ) -> anyhow::Result<NativeCompactionResult> {
        let provider = self
            .responses_for_model(request.model())
            .context("OpenAI Responses compaction is unavailable for this model")?;
        provider.compact_context(request).await
    }

    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        if let Some(provider) = self.responses_for_model(&request.model) {
            return provider.complete(request).await;
        }
        let json = self.make_request(&request, false).await?;
        Ok(openai_compat::parse_completion_response(
            &json,
            &request.model,
        ))
    }

    async fn stream(&self, request: CompletionRequest) -> anyhow::Result<StreamingResponse> {
        if let Some(provider) = self.responses_for_model(&request.model) {
            return provider.stream(request).await;
        }
        let json = self.make_request(&request, true).await?;
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

    async fn embeddings(&self, texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
        let resp = self
            .client
            .post(self.endpoint_url("embeddings"))
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({
                "model": "text-embedding-3-large",
                "input": texts,
            }))
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let err_text =
                super::read_error_response(resp, "OpenAI embeddings error response").await?;
            anyhow::bail!(
                "OpenAI embeddings error ({}): {}",
                status,
                super::bounded_error_preview(&err_text)
            );
        }

        let json: serde_json::Value =
            super::read_json_response(resp, "OpenAI embeddings response").await?;
        Ok(json["data"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        item["embedding"].as_array().map(|emb| {
                            emb.iter()
                                .filter_map(|v| v.as_f64().map(|f| f as f32))
                                .collect()
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        let provider_data = providers_data::get_provider("openai")
            .ok_or_else(|| anyhow::anyhow!("OpenAI provider data not found"))?;
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
                supports_vision: Some(m.id.contains("vision") || m.id.contains("gpt-4o")),
            })
            .collect())
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        let resp = self
            .client
            .post(self.chat_url())
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({
                "model": "gpt-4o-mini",
                "max_tokens": 5,
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .send()
            .await?;
        Ok(resp.status().is_success())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url_for(base: &str) -> String {
        OpenAIProvider::with_url_and_timeout(
            base.to_string(),
            "k".to_string(),
            Duration::from_secs(1),
        )
        .chat_url()
    }

    #[test]
    fn endpoint_url_does_not_double_v1_for_catalog_bases() {
        // Catalog base URLs already include the version → append only the path.
        assert_eq!(
            url_for("https://api.minimax.io/v1"),
            "https://api.minimax.io/v1/chat/completions"
        );
        assert_eq!(
            url_for("https://api.openai.com/v1"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            url_for("https://api.groq.com/openai/v1"),
            "https://api.groq.com/openai/v1/chat/completions"
        );
        assert_eq!(
            url_for("https://api.deepinfra.com/v1/openai"),
            "https://api.deepinfra.com/v1/openai/chat/completions"
        );
        // Bare base (no version) → add /v1.
        assert_eq!(
            url_for("https://api.openai.com"),
            "https://api.openai.com/v1/chat/completions"
        );
        // Trailing slash tolerated.
        assert_eq!(
            url_for("https://api.minimax.io/v1/"),
            "https://api.minimax.io/v1/chat/completions"
        );
    }

    #[test]
    fn reasoning_effort_only_for_openai_reasoning_models() {
        // The runtime attaches `reasoning: {effort}` (Responses-API shape) to
        // librarian calls; chat/completions servers must never receive it raw.
        assert!(is_openai_reasoning_model("gpt-5.4-mini"));
        assert!(is_openai_reasoning_model("gpt-6-astra"));
        assert!(is_openai_reasoning_model("gpt-6.1-sol"));
        assert!(is_openai_reasoning_model("gpt-6-astra-2026-09-03"));
        assert!(is_openai_reasoning_model("o3-mini"));
        assert!(!is_openai_reasoning_model("gpt-4o"));
        assert!(!is_openai_reasoning_model("MiniMax-M3"));
        assert!(!is_openai_reasoning_model("grok-4"));
    }
}
