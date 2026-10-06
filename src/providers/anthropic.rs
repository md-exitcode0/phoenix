//! Anthropic provider — native tool calling via /v1/messages

use async_trait::async_trait;
use reqwest::Client;
use std::collections::HashMap;
use std::time::Duration;

use super::contracts::{
    AuthType, ChatMessage, CompletionRequest, CompletionResponse, LLMProvider, MessageRole,
    ModelInfo, NativeCompactionCapability, NativeCompactionProvenance, NativeCompactionReplay,
    NativeCompactionRequest, NativeCompactionResult, NativeToolCall, ProviderResponseDialect,
    StreamingResponse, TokenUsage,
};
use super::providers_data;

const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com/v1";
const ANTHROPIC_COMPACTION_BETA: &str = "compact-2026-01-12";
const MIN_ANTHROPIC_COMPACTION_TRIGGER_TOKENS: u64 = 50_000;
const MAX_NATIVE_COMPACTION_WIRE_BYTES: usize = 20 * 1024 * 1024;
const MAX_NATIVE_COMPACTION_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const NO_TOOLS_COMPACTION_INSTRUCTION: &str =
    "Do not call or request any tools while writing this compaction summary. Respond with text only.";

pub struct AnthropicProvider {
    client: Client,
    api_key: String,
    native_compaction_enabled: bool,
}

impl AnthropicProvider {
    pub fn new(api_key: String) -> Self {
        Self::with_timeout(api_key, Duration::from_secs(120))
    }

    pub fn with_timeout(api_key: String, timeout: Duration) -> Self {
        Self::with_timeout_and_native(api_key, timeout, false)
    }

    pub fn with_timeout_and_native(
        api_key: String,
        timeout: Duration,
        native_compaction_enabled: bool,
    ) -> Self {
        let client = super::apply_read_timeout(
            Client::builder().connect_timeout(Duration::from_secs(30)),
            timeout,
        )
            .build()
            .expect("Anthropic client build failed");
        Self {
            client,
            api_key,
            native_compaction_enabled,
        }
    }

    fn has_model_impl(&self, model: &str) -> bool {
        providers_data::get_provider("anthropic")
            .map(|p| p.has_model(model))
            .unwrap_or(false)
    }

    fn default_model_impl(&self) -> &str {
        "claude-sonnet-4-20250514"
    }

    fn fallback_models_impl(&self) -> Vec<&str> {
        vec!["claude-haiku-4-5-20250514", "claude-flash-3.5-20250514"]
    }

    /// Serialize tool definitions to the Anthropic tools format
    /// (`input_schema` instead of OpenAI's `parameters`).
    pub(crate) fn build_tool_definitions(
        tools: &[super::contracts::ToolDefinition],
    ) -> Vec<serde_json::Value> {
        tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters
                })
            })
            .collect()
    }

    /// Build the Anthropic messages array from a request.
    ///
    /// Anthropic format differs from OpenAI in two key ways:
    /// 1. Tool call results use `role: user` with a tool_result content block.
    /// 2. Assistant messages with tool calls use a content block array.
    fn build_messages_from(messages: &[ChatMessage]) -> Vec<serde_json::Value> {
        let mut out: Vec<serde_json::Value> = Vec::new();

        for m in messages.iter().filter(|m| m.role != MessageRole::System) {
            match m.role {
                MessageRole::Tool => {
                    // Tool result → role:user with tool_result block.
                    // If the previous message is also role:user, merge into its content array
                    // to satisfy Anthropic's alternating-role requirement.
                    let block = serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": m.tool_call_id.as_deref().unwrap_or(""),
                        "content": m.content
                    });
                    if let Some(last) = out.last_mut() {
                        if last["role"].as_str() == Some("user") {
                            if let Some(content) = last["content"].as_array_mut() {
                                content.push(block);
                                continue;
                            }
                        }
                    }
                    out.push(serde_json::json!({
                        "role": "user",
                        "content": [block]
                    }));
                }
                MessageRole::Assistant => {
                    if !m.tool_calls.is_empty() {
                        // Build a content array: text block (if any) + tool_use blocks
                        let mut content: Vec<serde_json::Value> = Vec::new();
                        if !m.content.trim().is_empty() {
                            content.push(serde_json::json!({"type": "text", "text": m.content}));
                        }
                        for tc in &m.tool_calls {
                            content.push(serde_json::json!({
                                "type": "tool_use",
                                "id": tc.id,
                                "name": tc.tool_name,
                                "input": tc.arguments
                            }));
                        }
                        out.push(serde_json::json!({
                            "role": "assistant",
                            "content": content
                        }));
                    } else {
                        out.push(serde_json::json!({
                            "role": "assistant",
                            "content": m.content
                        }));
                    }
                }
                _ => {
                    // User (and any other non-system roles)
                    out.push(serde_json::json!({
                        "role": "user",
                        "content": m.content
                    }));
                }
            }
        }

        out
    }

    pub(crate) fn build_messages(request: &CompletionRequest) -> Vec<serde_json::Value> {
        Self::build_messages_from(&request.messages)
    }

    fn system_message(messages: &[ChatMessage]) -> String {
        messages
            .iter()
            .filter(|message| message.role == MessageRole::System)
            .map(|message| message.content.trim())
            .filter(|content| !content.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    fn matches_model_alias_or_snapshot(model: &str, alias: &str) -> bool {
        model == alias
            || model.strip_prefix(alias).is_some_and(|suffix| {
                suffix.strip_prefix('-').is_some_and(|date| {
                    date.len() == 8 && date.bytes().all(|byte| byte.is_ascii_digit())
                })
            })
    }

    fn supports_native_compaction_model(model: &str) -> bool {
        [
            "claude-fable-5",
            "claude-mythos-5",
            "claude-mythos-preview",
            "claude-opus-5",
            "claude-sonnet-5",
            "claude-opus-4-6",
            "claude-opus-4-7",
            "claude-opus-4-8",
            "claude-sonnet-4-6",
        ]
        .iter()
        .any(|alias| Self::matches_model_alias_or_snapshot(model, alias))
    }

    fn matching_native_replay<'a>(
        &self,
        request: &'a CompletionRequest,
    ) -> Option<&'a super::contracts::NativeCompactionReplayInput> {
        let native = request.native_replay.as_ref()?;
        let session_id = request.session_id.as_deref()?;
        let replay = native.replay();
        let provenance = replay.provenance();
        native
            .matches_route(
                NativeCompactionCapability::AnthropicCompact,
                session_id,
                self.name(),
                self.base_url(),
                &request.model,
                provenance.account_scope(),
                provenance.auth_epoch(),
            )
            .then_some(native)
    }

    /// Explicit prompt-cache breakpoint on the LAST content block of the last
    /// message. This is Anthropic's incremental-conversation pattern: the
    /// breakpoint rolls forward each round, the previous round's prefix is
    /// found via the cache lookback window, and every re-sent transcript byte
    /// reads at ~10× discount instead of full price. Phoenix's renders are
    /// already byte-stable between compaction generations, so without this
    /// emission that stability was buying nothing on the Anthropic lane.
    pub(crate) fn apply_rolling_breakpoint(messages: &mut [serde_json::Value]) {
        let Some(last) = messages.last_mut() else {
            return;
        };
        match &mut last["content"] {
            serde_json::Value::String(text) => {
                let text = std::mem::take(text);
                last["content"] = serde_json::json!([{
                    "type": "text",
                    "text": text,
                    "cache_control": {"type": "ephemeral"}
                }]);
            }
            serde_json::Value::Array(blocks) => {
                if let Some(serde_json::Value::Object(block)) = blocks.last_mut() {
                    block.insert(
                        "cache_control".to_string(),
                        serde_json::json!({"type": "ephemeral"}),
                    );
                }
            }
            _ => {}
        }
    }

    async fn make_request(
        &self,
        request: &CompletionRequest,
        stream: bool,
    ) -> anyhow::Result<serde_json::Value> {
        let native = (self.native_compaction_capability(&request.model)
            == NativeCompactionCapability::AnthropicCompact)
            .then(|| self.matching_native_replay(request))
            .flatten();
        let (system_msg, messages) = if let Some(native) = native {
            let system = Self::system_message(native.native_messages_for_wire());
            let mut suffix = Self::build_messages_from(native.native_messages_for_wire());
            Self::apply_rolling_breakpoint(&mut suffix);
            let mut messages = vec![serde_json::json!({
                "role": "assistant",
                "content": native.replay().opaque_items_for_wire().to_vec(),
            })];
            messages.extend(suffix);
            (system, messages)
        } else {
            let system = Self::system_message(&request.messages);
            let mut messages = Self::build_messages(request);
            Self::apply_rolling_breakpoint(&mut messages);
            (system, messages)
        };

        let mut body = serde_json::json!({
            "model": request.model,
            "messages": messages,
            "stream": stream,
            "max_tokens": request.max_tokens.unwrap_or(8096),
        });

        if !system_msg.is_empty() {
            // System as a block array with a cache breakpoint: caching is
            // hierarchical (tools → system → messages), so this one breakpoint
            // caches the tool definitions AND the system prompt — the largest
            // byte-stable prefix — for every subsequent call in the session.
            body["system"] = serde_json::json!([{
                "type": "text",
                "text": system_msg,
                "cache_control": {"type": "ephemeral"}
            }]);
        }
        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        // Native tool definitions — Anthropic uses `input_schema` not `parameters`
        if !request.tools.is_empty() {
            body["tools"] = serde_json::json!(Self::build_tool_definitions(&request.tools));
            body["tool_choice"] = serde_json::json!({"type": "auto"});
        }

        for (key, value) in &request.extra_body {
            // Phoenix runtime hints in OpenAI shapes — the Messages API rejects
            // unknown top-level params.
            if key == "response_format" || key == "reasoning" {
                continue;
            }
            body[key] = value.clone();
        }

        let mut outbound = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body);
        if native.is_some() {
            outbound = outbound.header("anthropic-beta", ANTHROPIC_COMPACTION_BETA);
        }
        let resp = outbound.send().await?;

        let status = resp.status();
        if !status.is_success() {
            let err_text = super::read_error_response(resp, "Anthropic API error response").await?;
            if native.is_some() {
                anyhow::bail!(
                    "Anthropic API error ({status}) while replaying native context; response body omitted"
                );
            }
            anyhow::bail!(
                "Anthropic API error ({}): {}",
                status,
                super::bounded_error_preview(&err_text)
            );
        }

        super::read_json_response(resp, "Anthropic API response").await
    }

    fn native_compaction_route_matches(&self, request: &NativeCompactionRequest) -> bool {
        let route = request.route();
        self.native_compaction_capability(request.model())
            == NativeCompactionCapability::AnthropicCompact
            && route.capability() == NativeCompactionCapability::AnthropicCompact
            && route.provider() == self.name()
            && route.base_route().trim_end_matches('/') == self.base_url().trim_end_matches('/')
            && route.model() == request.model()
    }

    fn native_compaction_instructions(request: &NativeCompactionRequest) -> String {
        let requested = request.instructions().trim();
        if requested.is_empty() {
            format!(
                "Summarize the transcript with all state needed to continue the task. {NO_TOOLS_COMPACTION_INSTRUCTION}"
            )
        } else {
            format!("{requested}\n\n{NO_TOOLS_COMPACTION_INSTRUCTION}")
        }
    }

    fn native_compaction_usage(json: &serde_json::Value) -> anyhow::Result<TokenUsage> {
        let iterations = json
            .pointer("/usage/iterations")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                anyhow::anyhow!("Anthropic compaction response is missing usage.iterations")
            })?;
        anyhow::ensure!(
            iterations.iter().any(|iteration| {
                iteration.get("type").and_then(serde_json::Value::as_str) == Some("compaction")
            }),
            "Anthropic compaction response usage has no compaction iteration"
        );

        let mut input_tokens = 0_u64;
        let mut output_tokens = 0_u64;
        for iteration in iterations {
            let input = iteration
                .get("input_tokens")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| {
                    anyhow::anyhow!("Anthropic usage iteration is missing input_tokens")
                })?;
            let output = iteration
                .get("output_tokens")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| {
                    anyhow::anyhow!("Anthropic usage iteration is missing output_tokens")
                })?;
            input_tokens = input_tokens.checked_add(input).ok_or_else(|| {
                anyhow::anyhow!("Anthropic compaction input-token total overflow")
            })?;
            output_tokens = output_tokens.checked_add(output).ok_or_else(|| {
                anyhow::anyhow!("Anthropic compaction output-token total overflow")
            })?;
        }

        let input_tokens = u32::try_from(input_tokens)
            .map_err(|_| anyhow::anyhow!("Anthropic compaction input-token total exceeds u32"))?;
        let output_tokens = u32::try_from(output_tokens)
            .map_err(|_| anyhow::anyhow!("Anthropic compaction output-token total exceeds u32"))?;
        let mut usage = TokenUsage::new(input_tokens, output_tokens);
        usage.cache_creation_tokens = json["usage"]["cache_creation_input_tokens"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok());
        usage.cache_read_tokens = json["usage"]["cache_read_input_tokens"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok());
        Ok(usage)
    }

    async fn send_native_compaction(
        &self,
        body: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let encoded = serde_json::to_vec(body)?;
        anyhow::ensure!(
            encoded.len() <= MAX_NATIVE_COMPACTION_WIRE_BYTES,
            "Anthropic compaction request is {} bytes; maximum is {}",
            encoded.len(),
            MAX_NATIVE_COMPACTION_WIRE_BYTES
        );

        let response = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", ANTHROPIC_COMPACTION_BETA)
            .header("Content-Type", "application/json")
            .body(encoded)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            // The beta may echo part of the compaction block or transcript in
            // an error. Consume it under a hard bound without exposing it.
            let _ = super::read_error_response(response, "Anthropic compaction API error response")
                .await?;
            anyhow::bail!("Anthropic compaction API error ({status}); response body omitted");
        }

        let text = super::read_response_text(
            response,
            MAX_NATIVE_COMPACTION_RESPONSE_BYTES,
            "Anthropic compaction API response",
        )
        .await?;
        serde_json::from_str(&text).map_err(|error| {
            anyhow::anyhow!("Anthropic compaction API response was not valid JSON: {error}")
        })
    }

    pub(crate) fn parse_response(json: &serde_json::Value, model_hint: &str) -> CompletionResponse {
        let content_blocks = json["content"].as_array();

        // Text from text blocks
        let content = content_blocks
            .map(|arr| {
                arr.iter()
                    .filter(|b| b["type"].as_str() == Some("text"))
                    .filter_map(|b| b["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();

        // Tool calls from tool_use blocks
        let tool_calls = content_blocks
            .map(|arr| {
                arr.iter()
                    .filter_map(|b| {
                        if b["type"].as_str() == Some("tool_use") {
                            let id = b["id"].as_str()?.to_string();
                            let name = b["name"].as_str()?.to_string();
                            let arguments = b["input"].clone();
                            Some(NativeToolCall {
                                id,
                                tool_name: name,
                                arguments,
                            })
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        let model = json["model"].as_str().unwrap_or(model_hint).to_string();
        let input_tokens = json["usage"]["input_tokens"].as_u64().unwrap_or(0) as u32;
        let output_tokens = json["usage"]["output_tokens"].as_u64().unwrap_or(0) as u32;
        let stop_reason = json["stop_reason"].as_str().map(String::from);

        let mut usage = TokenUsage::new(input_tokens, output_tokens);
        // Cache-hit telemetry: both nonzero over a session = the breakpoints
        // are live; creation-only = the prefix is being rewritten every call
        // (a silent cache leak worth investigating).
        usage.cache_creation_tokens = json["usage"]["cache_creation_input_tokens"]
            .as_u64()
            .map(|v| v as u32);
        usage.cache_read_tokens = json["usage"]["cache_read_input_tokens"]
            .as_u64()
            .map(|v| v as u32);

        CompletionResponse {
            content,
            model,
            usage,
            reasoning: None,
            stop_reason,
            tool_calls,
            provider_replay: None,
        }
    }
}

#[async_trait]
impl LLMProvider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }
    fn display_name(&self) -> &str {
        "Anthropic"
    }
    fn base_url(&self) -> &str {
        ANTHROPIC_BASE_URL
    }
    fn auth_type(&self) -> AuthType {
        AuthType::ApiKey
    }
    fn env_vars(&self) -> Vec<&str> {
        vec!["ANTHROPIC_API_KEY"]
    }

    fn default_headers(&self) -> HashMap<String, String> {
        let mut headers = HashMap::new();
        headers.insert("anthropic-version".to_string(), "2023-06-01".to_string());
        headers
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
        if self.native_compaction_enabled && Self::supports_native_compaction_model(model) {
            NativeCompactionCapability::AnthropicCompact
        } else {
            NativeCompactionCapability::Unsupported
        }
    }

    fn response_dialect(&self, _model: &str) -> ProviderResponseDialect {
        ProviderResponseDialect::AnthropicMessages
    }

    async fn compact_context(
        &self,
        request: NativeCompactionRequest,
    ) -> anyhow::Result<NativeCompactionResult> {
        anyhow::ensure!(
            self.native_compaction_route_matches(&request),
            "Anthropic native compaction request does not match this provider route"
        );
        anyhow::ensure!(
            request.trigger_tokens() >= MIN_ANTHROPIC_COMPACTION_TRIGGER_TOKENS,
            "Anthropic native compaction requires a trigger of at least {MIN_ANTHROPIC_COMPACTION_TRIGGER_TOKENS} input tokens"
        );

        let system = Self::system_message(request.messages());
        let mut messages = Vec::new();
        if let Some(prior) = request.prior_replay() {
            anyhow::ensure!(
                prior.capability() == NativeCompactionCapability::AnthropicCompact,
                "Anthropic native compaction received an incompatible prior replay"
            );
            messages.push(serde_json::json!({
                "role": "assistant",
                "content": prior.opaque_items_for_wire().to_vec(),
            }));
        }
        messages.extend(Self::build_messages_from(request.messages()));
        anyhow::ensure!(
            !messages.is_empty(),
            "Anthropic native compaction has no Messages API input"
        );

        let mut body = serde_json::json!({
            "model": request.model(),
            "max_tokens": 8192,
            "messages": messages,
            "context_management": {
                "edits": [{
                    "type": "compact_20260112",
                    "trigger": {
                        "type": "input_tokens",
                        "value": request.trigger_tokens(),
                    },
                    "pause_after_compaction": true,
                    "instructions": Self::native_compaction_instructions(&request),
                }]
            }
        });
        if !system.is_empty() {
            body["system"] = serde_json::Value::String(system);
        }
        if !request.tools().is_empty() {
            // Preserve the tool schemas that shaped the transcript. Anthropic
            // warns that its internal summarizer can otherwise call one of
            // them, so the replacement instructions above explicitly forbid
            // tool use and the response validator rejects null summaries.
            body["tools"] = serde_json::json!(Self::build_tool_definitions(request.tools()));
        }

        let json = self.send_native_compaction(&body).await?;
        anyhow::ensure!(
            json.get("stop_reason").and_then(serde_json::Value::as_str) == Some("compaction"),
            "Anthropic native compaction did not pause at a compaction boundary"
        );
        let items = json
            .get("content")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("Anthropic compaction response is missing content"))?
            .clone();
        let summary = items
            .first()
            .and_then(|block| block.get("content"))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|content| !content.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("Anthropic compaction response has no usable compaction content")
            })?
            .to_owned();
        let usage = Self::native_compaction_usage(&json)?;
        let provenance = NativeCompactionProvenance::try_for_route(
            request.session_id(),
            request.route(),
            request.next_compaction_generation(),
            request.target_transcript_revision(),
        )?;
        let replay = NativeCompactionReplay::try_new(
            provenance,
            NativeCompactionCapability::AnthropicCompact,
            1,
            items,
        )?;
        let folded_input_messages = request.messages().len();
        NativeCompactionResult::try_new(
            &request,
            replay,
            Some(summary),
            usage,
            folded_input_messages,
        )
    }

    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        let model_hint = request.model.clone();
        let json = self.make_request(&request, false).await?;
        Ok(Self::parse_response(&json, &model_hint))
    }

    async fn stream(&self, request: CompletionRequest) -> anyhow::Result<StreamingResponse> {
        let model_hint = request.model.clone();
        let json = self.make_request(&request, true).await?;
        let resp = Self::parse_response(&json, &model_hint);
        Ok(StreamingResponse {
            content: resp.content,
            reasoning: None,
            done: true,
        })
    }

    async fn embeddings(&self, _texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
        anyhow::bail!("Anthropic does not support embeddings")
    }

    async fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        let provider_data = providers_data::get_provider("anthropic")
            .ok_or_else(|| anyhow::anyhow!("Anthropic provider data not found"))?;
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
                supports_vision: Some(m.id.contains("opus") || m.id.contains("sonnet")),
            })
            .collect())
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        let resp = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&serde_json::json!({
                "model": "claude-haiku-4-5-20250514",
                "max_tokens": 1,
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .send()
            .await?;
        Ok(resp.status().is_success())
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    #[test]
    fn rolling_breakpoint_lands_on_the_last_block() {
        // String content wraps into a cache-marked text block…
        let mut messages = vec![
            serde_json::json!({"role": "user", "content": "first"}),
            serde_json::json!({"role": "user", "content": "latest turn"}),
        ];
        AnthropicProvider::apply_rolling_breakpoint(&mut messages);
        assert!(
            messages[0]["content"].is_string(),
            "earlier messages untouched"
        );
        let blocks = messages[1]["content"].as_array().expect("wrapped");
        assert_eq!(blocks[0]["text"], "latest turn");
        assert_eq!(blocks[0]["cache_control"]["type"], "ephemeral");

        // …and array content gets the marker on its LAST block only.
        let mut messages = vec![serde_json::json!({
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": "a", "content": "r1"},
                {"type": "tool_result", "tool_use_id": "b", "content": "r2"}
            ]
        })];
        AnthropicProvider::apply_rolling_breakpoint(&mut messages);
        let blocks = messages[0]["content"].as_array().unwrap();
        assert!(blocks[0].get("cache_control").is_none());
        assert_eq!(blocks[1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn parse_response_carries_cache_telemetry() {
        let json = serde_json::json!({
            "content": [{"type": "text", "text": "hi"}],
            "model": "claude-sonnet-4-20250514",
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 12,
                "output_tokens": 3,
                "cache_creation_input_tokens": 4096,
                "cache_read_input_tokens": 90000
            }
        });
        let resp = AnthropicProvider::parse_response(&json, "m");
        assert_eq!(resp.usage.cache_creation_tokens, Some(4096));
        assert_eq!(resp.usage.cache_read_tokens, Some(90000));
    }
}
