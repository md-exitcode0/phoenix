//! OpenAI Codex provider — Responses API with native tool calling

use async_trait::async_trait;
use anyhow::Context;
use reqwest::Client;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

use super::contracts::{
    AuthType, ChatMessage, CompletionRequest, CompletionResponse, LLMProvider, MessageRole,
    ModelInfo, NativeCompactionCapability, NativeCompactionProvenance, NativeCompactionReplay,
    NativeCompactionRequest, NativeCompactionResult, NativeToolCall, ProviderResponseDialect,
    StreamingResponse, TokenUsage,
};
use super::providers_data;

const PUBLIC_OPENAI_RESPONSES_BASE_URL: &str = "https://api.openai.com/v1";
/// The first-party ChatGPT Codex origin used by the built-in subscription
/// provider.  This is intentionally an exact allow-list entry: an arbitrary
/// OpenAI-compatible origin must never inherit Codex's private route or
/// compaction semantics merely because it happens to expose `/responses`.
const CHATGPT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const MAX_NATIVE_COMPACTION_WIRE_BYTES: usize = 20 * 1024 * 1024;
const MAX_NATIVE_COMPACTION_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_CODEX_SSE_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

// The subscription route scopes usage to the account claim in the OAuth
// token. Decode only for routing; the server still validates the signature.
pub(crate) fn codex_account_id(token: &str) -> Option<String> {
    use base64::Engine;
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload).ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    claims.get("https://api.openai.com/auth")?.get("chatgpt_account_id")?
        .as_str().filter(|id| !id.is_empty()).map(str::to_string)
}

pub(crate) fn codex_oauth_identity(token: &str) -> Option<(String, String)> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(token.split('.').nth(1)?).ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    let subject = claims.get("sub")?.as_str().filter(|s| !s.is_empty())?;
    Some((subject.to_string(), codex_account_id(token)?))
}

/// Wire tiers for the subscription route. Fast retains the Priority wire
/// alias; Ultrafast is a separate tier and never changes reasoning effort.
pub(crate) fn codex_service_tier(model: &str, tier: &str) -> Option<&'static str> {
    match tier {
        "fast" | "priority" if matches!(model, "gpt-6.1-sol" | "gpt-6-astra" | "gpt-6-sol" | "gpt-6-luna" | "gpt-5.6-sol" | "gpt-5.6-terra" | "gpt-5.6-luna" | "gpt-5.5") => Some("priority"),
        "ultrafast" if matches!(model, "gpt-6.1-sol" | "gpt-6-astra") => Some("ultrafast"),
        _ => None,
    }
}

pub struct OpenAICodexProvider {
    client: Client,
    access_token: String,
    auth_profile: Option<(String, u64)>,
    base_url: String,
    /// The wire adapter is shared by the public OpenAI profile and the
    /// subscription Codex profile, but their provider identities must remain
    /// distinct in replay provenance and auth-route receipts.
    provider_id: &'static str,
    catalog_id: &'static str,
    display_name: &'static str,
    native_compaction_enabled: bool,
}

impl OpenAICodexProvider {
    pub fn with_url_and_timeout(base_url: String, access_token: String, timeout: Duration) -> Self {
        Self::with_url_and_timeout_and_native(base_url, access_token, timeout, false)
    }

    pub fn with_url_and_timeout_and_native(
        base_url: String,
        access_token: String,
        timeout: Duration,
        native_compaction_enabled: bool,
    ) -> Self {
        Self::with_identity(
            base_url,
            access_token,
            timeout,
            "openai-codex",
            "openai-codex",
            "OpenAI Codex",
            native_compaction_enabled,
        )
    }

    /// Build the Responses adapter for a direct OpenAI API-key profile.  It
    /// deliberately retains the `openai` provider identity; using the Codex
    /// identity here would make native replay/account provenance look like a
    /// ChatGPT subscription route.
    pub fn for_openai_api(
        base_url: String,
        api_key: String,
        timeout: Duration,
        native_compaction_enabled: bool,
    ) -> Self {
        Self::with_identity(
            base_url,
            api_key,
            timeout,
            "openai",
            "openai",
            "OpenAI",
            native_compaction_enabled,
        )
    }

    fn with_identity(
        base_url: String,
        access_token: String,
        timeout: Duration,
        provider_id: &'static str,
        catalog_id: &'static str,
        display_name: &'static str,
        native_compaction_enabled: bool,
    ) -> Self {
        // `timeout` is an IDLE-READ timeout, not a total-request cap. A long
        // generation (a full HTML page on a reasoning model) legitimately
        // streams for many minutes; with a total `.timeout()` the body read
        // dies mid-stream ("error decoding response body: operation timed
        // out") and the whole specialist turn is lost. As long as the server
        // keeps streaming chunks, the call stays alive; only true stalls are
        // killed.
        let client = super::apply_read_timeout(
            Client::builder().connect_timeout(Duration::from_secs(30)),
            timeout,
        )
            .build()
            .expect("OpenAI Codex client build failed");
        Self {
            client,
            access_token,
            auth_profile: None,
            base_url,
            provider_id,
            catalog_id,
            display_name,
            native_compaction_enabled,
        }
    }

    pub(crate) fn with_auth_profile(mut self, profile: Option<&str>, epoch: u64) -> Self {
        self.auth_profile = profile.map(|id| (id.to_string(), epoch));
        self
    }

    async fn current_access_token(&self, force_refresh: bool) -> anyhow::Result<String> {
        let Some((id, epoch)) = self.auth_profile.clone() else { return Ok(self.access_token.clone()); };
        tokio::task::spawn_blocking(move ||
            crate::config::auth_profile::resolve_bound_profile_secret(&id, epoch, force_refresh)
        ).await.context("OAuth credential worker failed")?
    }

    fn authenticated_post(&self, url: String, token: &str) -> reqwest::RequestBuilder {
        let mut request = self.client.post(url).bearer_auth(token);
        if self.is_chatgpt_codex_route() {
            if let Some(account_id) = codex_account_id(token) {
                request = request.header("chatgpt-account-id", account_id);
            }
        }
        request
    }

    /// The key that pins a request to a warm prompt-cache replica. The
    /// Codex backend routes by the `session_id` header; without it every
    /// turn lands on a cold replica and pays for the whole prefix again.
    fn cache_routing_key<'a>(body: &'a Value, session: Option<&'a str>) -> Option<&'a str> {
        body.get("prompt_cache_key")
            .and_then(Value::as_str)
            .or(session)
            .filter(|key| !key.is_empty())
    }

    fn responses_url(&self) -> String {
        format!("{}/responses", self.base_url.trim_end_matches('/'))
    }

    fn responses_compact_url(&self) -> String {
        format!("{}/responses/compact", self.base_url.trim_end_matches('/'))
    }

    fn is_public_openai_responses_route(&self) -> bool {
        self.base_url.trim_end_matches('/') == PUBLIC_OPENAI_RESPONSES_BASE_URL
    }

    fn is_chatgpt_codex_route(&self) -> bool {
        self.base_url.trim_end_matches('/') == CHATGPT_CODEX_BASE_URL
    }

    fn is_trusted_native_compaction_route(&self) -> bool {
        // The public API and the built-in ChatGPT Codex lane have distinct
        // auth contracts, but both intentionally use the same relative
        // `responses/compact` path.  No other OpenAI-shaped base URL is
        // trusted here.
        self.is_public_openai_responses_route()
            || (self.provider_id == "openai-codex" && self.is_chatgpt_codex_route())
    }

    fn supports_native_compaction_model(&self, model: &str) -> bool {
        // Compaction is a Responses/text operation. Use the curated catalog
        // to keep this model-aware (rather than inventing a short allow-list),
        // include Codex Pro and newer Codex models automatically, and leave
        // audio/realtime/transcription/TTS lanes on their existing behavior.
        // Unknown custom ids remain fail-closed. The catalog's `reasoning`
        // bit is deliberately not used as a compaction claim: reasoning and
        // Responses compaction are separate provider capabilities.
        let non_responses_name = model.to_ascii_lowercase();
        if [
            "audio",
            "realtime",
            "transcrib",
            "whisper",
            "tts",
            "embedding",
            "moderation",
        ]
        .iter()
        .any(|needle| non_responses_name.contains(needle))
        {
            return false;
        }
        providers_data::get_provider(self.catalog_id)
            .map(|provider| {
                provider
                    .models
                    .iter()
                    .any(|candidate| candidate.id == model)
            })
            .unwrap_or(false)
    }

    fn model_supports_reasoning(&self, model: &str) -> bool {
        providers_data::get_provider(self.catalog_id)
            .map(|provider| {
                provider
                    .models
                    .iter()
                    .find(|candidate| candidate.id == model)
                    .is_some_and(|candidate| candidate.reasoning)
            })
            .unwrap_or(false)
    }

    fn has_model_impl(&self, model: &str) -> bool {
        providers_data::get_provider(self.catalog_id)
            .map(|p| p.has_model(model))
            .unwrap_or(false)
    }

    fn default_model_impl(&self) -> &str {
        if self.provider_id == "openai" {
            "gpt-5.4"
        } else {
            "gpt-5.4-mini"
        }
    }
    fn fallback_models_impl(&self) -> Vec<&str> {
        if self.provider_id == "openai" {
            vec!["gpt-5.4-mini", "gpt-5.3-codex"]
        } else {
            vec!["gpt-5.3-codex", "gpt-5.2-codex"]
        }
    }

    /// Build the `input` array for the Responses API.
    ///
    /// The Responses API has a different multi-turn format than chat/completions:
    /// - User messages       → {"role":"user", "content":[{"type":"input_text","text":"..."}]}
    /// - Assistant text      → {"role":"assistant","content":[{"type":"output_text","text":"..."}]}
    /// - Assistant tool call → {"type":"function_call","call_id":"...","name":"...","arguments":"..."}
    /// - Tool result         → {"type":"function_call_output","call_id":"...","output":"..."}
    fn responses_items(messages: &[ChatMessage], include_instructions: bool) -> Vec<Value> {
        Self::responses_items_routed(messages, include_instructions, None, false)
    }

    /// `replay_route`: the route this request is going to. Opaque reasoning
    /// items are replayed only when they were minted on exactly this route —
    /// encrypted reasoning from another model or account is rejected upstream.
    fn responses_items_routed(
        messages: &[ChatMessage],
        include_instructions: bool,
        replay_route: Option<&str>,
        inline_system: bool,
    ) -> Vec<Value> {
        let mut items = Vec::new();
        for m in messages {
            match m.role {
                MessageRole::System | MessageRole::Developer if inline_system => {
                    if !m.content.trim().is_empty() {
                        items.push(serde_json::json!({
                            "role": "developer",
                            "content": [{"type": "input_text", "text": m.content}]
                        }));
                    }
                }
                MessageRole::System | MessageRole::Developer => {
                    if include_instructions && !m.content.trim().is_empty() {
                        let role = if m.role == MessageRole::Developer {
                            "developer"
                        } else {
                            "system"
                        };
                        items.push(serde_json::json!({
                            "role": role,
                            "content": [{"type": "input_text", "text": m.content}]
                        }));
                    }
                }
                MessageRole::User => {
                    let mut content = Vec::new();
                    if !m.content.trim().is_empty() {
                        content.push(serde_json::json!({"type":"input_text", "text":m.content}));
                    }
                    for image in &m.images {
                        content.push(serde_json::json!({"type":"input_image", "image_url":image}));
                    }
                    if !content.is_empty() {
                        items.push(serde_json::json!({
                            "role": "user",
                            "content": content
                        }));
                    }
                }
                MessageRole::Assistant => {
                    if let (Some(route), Some(replay)) = (replay_route, m.provider_replay.as_ref()) {
                        if replay.route == route {
                            items.extend(replay.items.iter().cloned());
                        }
                    }
                    if !m.tool_calls.is_empty() {
                        // Prose that accompanied the calls is the model's own
                        // working note (what it just observed, what it ruled
                        // out). Dropping it made every round start amnesiac.
                        if !m.content.trim().is_empty() {
                            items.push(serde_json::json!({
                                "role": "assistant",
                                "content": [{"type": "output_text", "text": m.content}]
                            }));
                        }
                        // One function_call item per tool call
                        for tc in &m.tool_calls {
                            items.push(serde_json::json!({
                                "type": "function_call",
                                "call_id": tc.id,
                                "name": tc.tool_name,
                                "arguments": serde_json::to_string(&tc.arguments)
                                    .unwrap_or_else(|_| "{}".to_string())
                            }));
                        }
                    } else if !m.content.trim().is_empty() {
                        items.push(serde_json::json!({
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": m.content}]
                        }));
                    }
                }
                MessageRole::Tool => {
                    let call_id = m.tool_call_id.as_deref().unwrap_or("");
                    items.push(serde_json::json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": m.content
                    }));
                }
            }
        }
        items
    }

    fn ensure_nonempty_responses_input(items: &mut Vec<Value>) {
        // Codex Responses rejects a request whose `input` is empty (e.g. only a
        // system prompt, or a turn that begins with reasoning). OpenClaw handles
        // this by injecting a single-space user input; we do the same so the
        // first round of a native turn never 400s on empty input.
        if items.is_empty() {
            items.push(serde_json::json!({
                "role": "user",
                "content": [{"type": "input_text", "text": " "}]
            }));
        }
    }

    pub(crate) fn responses_input(request: &CompletionRequest) -> Vec<Value> {
        let mut items = Self::responses_items(&request.messages, false);
        Self::ensure_nonempty_responses_input(&mut items);
        items
    }

    fn instructions_for_messages(messages: &[ChatMessage]) -> String {
        let s = messages
            .iter()
            .filter(|m| matches!(m.role, MessageRole::System | MessageRole::Developer))
            .map(|m| m.content.trim())
            .filter(|c| !c.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        if s.is_empty() {
            "You are PhoenixAgent. Answer clearly and follow the user's request.".to_string()
        } else {
            s
        }
    }

    pub(crate) fn instructions(request: &CompletionRequest) -> String {
        Self::instructions_for_messages(&request.messages)
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
                NativeCompactionCapability::ResponsesCompact,
                session_id,
                self.name(),
                self.base_url(),
                &request.model,
                provenance.account_scope(),
                provenance.auth_epoch(),
            )
            .then_some(native)
    }

    fn completion_wire_view(&self, request: &CompletionRequest) -> (String, Vec<Value>) {
        if self.native_compaction_capability(&request.model)
            == NativeCompactionCapability::ResponsesCompact
        {
            if let Some(native) = self.matching_native_replay(request) {
                let mut input = native.replay().opaque_items_for_wire().to_vec();
                input.extend(Self::responses_items(
                    native.native_messages_for_wire(),
                    false,
                ));
                Self::ensure_nonempty_responses_input(&mut input);
                return (
                    Self::instructions_for_messages(native.native_messages_for_wire()),
                    input,
                );
            }
        }

        // Only the LEADING system block becomes `instructions`. System notes
        // injected later (per-round guidance, audits, reminders) stay where
        // they are, as developer items: hoisting them into `instructions`
        // rewrote the very first bytes of the prompt whenever they changed,
        // so every round missed the prompt cache for the whole transcript.
        let lead = request
            .messages
            .iter()
            .take_while(|m| matches!(m.role, MessageRole::System | MessageRole::Developer))
            .count();
        let mut input = Self::responses_items_routed(
            &request.messages[lead..],
            false,
            Some(&self.replay_route(&request.model)),
            true,
        );
        Self::ensure_nonempty_responses_input(&mut input);
        (Self::instructions_for_messages(&request.messages[..lead]), input)
    }

    /// Identity of the route that mints and may replay encrypted reasoning.
    fn replay_route(&self, model: &str) -> String {
        let account = self
            .auth_profile
            .as_ref()
            .map(|(id, epoch)| format!("{id}@{epoch}"))
            .unwrap_or_else(|| "static".into());
        format!("{}|{}|{}", self.base_url.trim_end_matches('/'), model, account)
    }

    /// Reasoning items (with `encrypted_content`) from a finished response,
    /// reduced to the fields the stateless (`store:false`) protocol accepts.
    fn replayable_reasoning(json: &Value) -> Vec<Value> {
        let from_output: Vec<&Value> = json
            .get("output")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
            .collect();
        let source: Vec<&Value> = if from_output.is_empty() {
            json.get("streamed_reasoning")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .collect()
        } else {
            from_output
        };
        source
            .into_iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
            .filter_map(|item| {
                let encrypted = item.get("encrypted_content")?.as_str()?;
                Some(serde_json::json!({
                    "type": "reasoning",
                    "summary": item.get("summary").cloned().unwrap_or_else(|| serde_json::json!([])),
                    "encrypted_content": encrypted,
                }))
            })
            .collect()
    }

    /// One-shot vision call (screenshot grounding): prompt + image data URI in,
    /// caption text out. Same Responses endpoint and stream parsing as
    /// `complete`, with low reasoning effort — captioning is mechanical work.
    pub async fn complete_vision(
        &self,
        model: &str,
        prompt: &str,
        image_data_uri: &str,
    ) -> anyhow::Result<String> {
        let body = serde_json::json!({
            "model": model,
            "instructions": "You are the eyes of a browser-automation agent. Describe screenshots factually and compactly.",
            "input": [{
                "role": "user",
                "content": [
                    {"type": "input_text", "text": prompt},
                    {"type": "input_image", "image_url": image_data_uri}
                ]
            }],
            "store": false,
            "stream": true,
            "reasoning": {"effort": "low", "summary": "auto"},
        });
        let response = self.send_responses(&body, model, None).await?;
        let text = response
            .get("output_text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        anyhow::ensure!(!text.is_empty(), "vision call returned no output text");
        Ok(text)
    }

    /// One-shot text sidecar call (e.g. structuring a page extraction with a
    /// cheap model). Same endpoint/parsing as `complete_vision`, no image.
    pub async fn complete_sidecar_text(
        &self,
        model: &str,
        instructions: &str,
        prompt: &str,
    ) -> anyhow::Result<String> {
        let body = serde_json::json!({
            "model": model,
            "instructions": instructions,
            "input": [{
                "role": "user",
                "content": [{"type": "input_text", "text": prompt}]
            }],
            "store": false,
            "stream": true,
            "reasoning": {"effort": "low", "summary": "auto"},
        });
        let response = self.send_responses(&body, model, None).await?;
        let text = response
            .get("output_text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        anyhow::ensure!(!text.is_empty(), "sidecar call returned no output text");
        Ok(text)
    }

    async fn make_request(&self, request: &CompletionRequest) -> anyhow::Result<Value> {
        let (instructions, input) = self.completion_wire_view(request);
        let mut body = serde_json::json!({
            "model": request.model,
            "instructions": instructions,
            "input": input,
            "store": false,
            "stream": true,
        });
        // Session-keyed cache routing. Both backends accept `prompt_cache_key`;
        // the ChatGPT Codex backend additionally needs the `session_id` header
        // (sent in `send_responses_observed`) or it NEVER serves a cached
        // prefix — measured 2026-09-22: identical 14k-token prompt ×3 → 0
        // cached without it, 14,208 cached with it. History before the fix:
        // 592M input tokens at 32% cache.
        if let Some(key) = request.session_id.as_deref().filter(|k| !k.is_empty()) {
            body["prompt_cache_key"] = serde_json::json!(key);
        }
        // Stored-state continuation: the server holds the transcript; this
        // request's `input` carries only the new items (the mesh guarantees
        // that). Instructions are NOT inherited across responses, so they are
        // resent above either way.

        // Reasoning models (gpt-5.x) on the Responses API emit chain-of-thought
        // in a separate channel. Request a summary so Phoenix can surface it in
        // the CLI — without this, orchestrator/coder turns look like they "aren't
        // reasoning" because the only thing Phoenix ever saw was the (often
        // empty) final message.
        //
        // Default `effort: "high"`: the empty-message collapse is fixed by JSON
        // enforcement + retries (not by starving reasoning), and high effort
        // demonstrably produces better thinking, so we keep it. Merge any
        // caller-supplied `reasoning` (e.g. `/reasoning medium`) from extra_body
        // so it still wins.
        let mut reasoning = serde_json::json!({ "effort": "high", "summary": "detailed" });
        if let Some(Value::Object(extra)) = request.extra_body.get("reasoning") {
            for (k, v) in extra {
                reasoning[k] = v.clone();
            }
        }
        if self.model_supports_reasoning(&request.model) {
            body["reasoning"] = reasoning;
            body["include"] = serde_json::json!(["reasoning.encrypted_content"]);
        }
        // NOTE: we deliberately do NOT set `max_output_tokens`. The Codex
        // backend Phoenix talks to (ChatGPT-style endpoint, `{"detail":...}`
        // errors) rejects it: 400 "Unsupported parameter: max_output_tokens".
        // The public Responses API does accept it, but this endpoint does not,
        // so we leave output budgeting to the server default and rely on the
        // incomplete/empty detection below to surface a starved-output stall
        // instead of letting it look like the model did nothing.

        if !request.tools.is_empty() {
            let tools: Vec<Value> = request
                .tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "name": t.name,
                        "description": t.description,
                        // Our shared tool schemas contain true optional fields
                        // with Rust defaults. Responses auto-normalization can
                        // make them required (e.g. a spurious 1x1 image crop).
                        // Preserve their contract; tools still validate inputs.
                        "strict": false,
                        "parameters": t.parameters,
                    })
                })
                .collect();
            body["tools"] = serde_json::json!(tools);
            // `required`, not `auto`: in an agentic loop a reasoning model given
            // `auto` will monologue its plan as plain text and never call a tool
            // ("plans then stalls"). Forcing a tool call every turn makes it ACT;
            // finishing is itself a tool call (`final_answer`), so the loop can
            // always terminate. This is how OpenClaw/opencode drive agentic turns.
            body["tool_choice"] = serde_json::json!("required");
        }

        for (key, value) in &request.extra_body {
            // `reasoning` is merged above. `response_format`/`text.format` JSON
            // modes are intentionally NOT sent: Codex (like every provider the
            // donor agents drive) gets structured output from native tool-calling
            // (the forced `final_answer` tool), not provider JSON modes — that is
            // the only mechanism uniform across providers. This endpoint also
            // rejects some of these params outright (e.g. `max_output_tokens`).
            if key == "response_format" || key == "reasoning" {
                continue;
            }
            // Server-side context management emits an opaque compaction item
            // that must be persisted and replayed verbatim. Phoenix currently
            // uses the standalone `/responses/compact` transaction, whose
            // complete output is already durable; never mix the two protocols
            // by forwarding an unpersisted caller hint here.
            if key == "context_management" {
                continue;
            }
            body[key] = value.clone();
        }

        match self
            .send_responses_observed(&body, &request.model, request.session_id.as_deref(), request.stream_observer.as_ref())
            .await
        {
            // Replayed reasoning is an optimization, never a reason to fail a
            // turn: if the backend refuses it (rotated key, expired blob),
            // resend once without it.
            Err(error)
                if error.to_string().contains("(400")
                    && Self::strip_replayed_reasoning(&mut body) =>
            {
                tracing::warn!("codex rejected replayed reasoning, resending without it: {error}");
                self.send_responses_observed(&body, &request.model, request.session_id.as_deref(), request.stream_observer.as_ref())
                    .await
            }
            other => other,
        }
    }

    /// Remove replayed reasoning items from a request body; true if any were removed.
    fn strip_replayed_reasoning(body: &mut Value) -> bool {
        let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) else {
            return false;
        };
        let before = input.len();
        input.retain(|item| item.get("type").and_then(Value::as_str) != Some("reasoning"));
        before != input.len()
    }

    fn native_compaction_route_matches(&self, request: &NativeCompactionRequest) -> bool {
        let route = request.route();
        self.native_compaction_capability(request.model())
            == NativeCompactionCapability::ResponsesCompact
            && route.capability() == NativeCompactionCapability::ResponsesCompact
            && route.provider() == self.name()
            && route.base_route().trim_end_matches('/') == self.base_url.trim_end_matches('/')
            && route.model() == request.model()
    }

    fn native_compaction_input(request: &NativeCompactionRequest) -> Vec<Value> {
        let mut input = request
            .prior_replay()
            .map(|replay| replay.opaque_items_for_wire().to_vec())
            .unwrap_or_default();
        // Unlike a normal Responses request, the compact endpoint's top-level
        // `instructions` is the replacement-summary directive. Preserve the
        // original Phoenix system/developer messages as real input items.
        input.extend(Self::responses_items(request.messages(), true));
        Self::ensure_nonempty_responses_input(&mut input);
        input
    }

    async fn send_native_compaction(&self, body: &Value) -> anyhow::Result<Value> {
        let encoded = serde_json::to_vec(body)?;
        anyhow::ensure!(
            encoded.len() <= MAX_NATIVE_COMPACTION_WIRE_BYTES,
            "OpenAI compact request is {} bytes; maximum is {}",
            encoded.len(),
            MAX_NATIVE_COMPACTION_WIRE_BYTES
        );

        let token = self.current_access_token(false).await?;
        let response = self.authenticated_post(self.responses_compact_url(), &token)
            .header("Content-Type", "application/json")
            .body(encoded)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            // Compact errors can quote opaque encrypted input items. Consume
            // the bounded body, but never propagate it into logs or UI errors.
            let _ =
                super::read_error_response(response, "OpenAI compact API error response").await?;
            anyhow::bail!("OpenAI compact API error ({status}); response body omitted");
        }

        let text = super::read_response_text(
            response,
            MAX_NATIVE_COMPACTION_RESPONSE_BYTES,
            "OpenAI compact API response",
        )
        .await?;
        serde_json::from_str(&text).map_err(|error| {
            anyhow::anyhow!("OpenAI compact API response was not valid JSON: {error}")
        })
    }

    /// POST one prepared Responses body and read the streamed result.
    ///
    /// Transport failures (connect timeout, mid-stream read timeout, reset
    /// connections) get ONE retry after a short backoff — the request body is
    /// fully prepared and idempotent from our side, and losing a whole mesh
    /// turn to a transient network blip is far more expensive than a repeat
    /// call. API-level errors (4xx/5xx with a body) are NOT retried here.
    async fn send_responses(
        &self,
        body: &Value,
        model: &str,
        session: Option<&str>,
    ) -> anyhow::Result<Value> {
        self.send_responses_observed(body, model, session, None).await
    }

    async fn send_responses_observed(
        &self,
        body: &Value,
        model: &str,
        session: Option<&str>,
        observer: Option<&super::contracts::StreamObserver>,
    ) -> anyhow::Result<Value> {
        let contains_native_replay =
            body.get("input")
                .and_then(Value::as_array)
                .is_some_and(|items| {
                    items.iter().any(|item| {
                        matches!(
                            item.get("type").and_then(Value::as_str),
                            Some("compaction" | "compaction_summary")
                        )
                    })
                });
        dump_request_body(body, session);
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..3 {
            if attempt > 0 {
                tracing::warn!(
                    "codex transport error, retrying ({}): {}",
                    attempt,
                    last_err.as_ref().map(|e| e.to_string()).unwrap_or_default()
                );
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            let token = self.current_access_token(false).await?;
            let mut post = self.authenticated_post(self.responses_url(), &token)
                .header("Content-Type", "application/json");
            if let Some(key) = Self::cache_routing_key(body, session) {
                post = post.header("session_id", key);
            }
            let resp = match post
                .json(body)
                .send()
                .await
            {
                Ok(resp) => resp,
                Err(e) => {
                    last_err = Some(e.into());
                    continue;
                }
            };

            let status = resp.status();
            if !status.is_success() {
                let err_text =
                    super::read_error_response(resp, "OpenAI Codex API error response").await?;
                if status.as_u16() == 401 && attempt == 0 && self.auth_profile.is_some() {
                    let refreshed = self.current_access_token(true).await?;
                    if refreshed != token {
                        last_err = Some(anyhow::anyhow!("OAuth access token refreshed after rejection"));
                        continue;
                    }
                }
                if (status.as_u16() == 429 || status.is_server_error())
                    && !err_text.contains("usage_limit_reached") && attempt < 2 {
                    last_err = Some(anyhow::anyhow!(
                        "OpenAI Codex API error ({}): {}",
                        status,
                        super::bounded_error_preview(&err_text)
                    ));
                    continue;
                }
                if contains_native_replay {
                    anyhow::bail!(
                        "OpenAI Codex API error ({status}) while replaying native context; response body omitted"
                    );
                }
                anyhow::bail!(
                    "OpenAI Codex API error ({}): {}",
                    status,
                    super::bounded_error_preview(&err_text)
                );
            }

            match Self::read_stream_observed(resp, model, session, MAX_CODEX_SSE_RESPONSE_BYTES, observer).await {
                Ok(value) => return Ok(value),
                Err(e) => {
                    if super::is_response_body_limit_error(&e) {
                        return Err(e);
                    }
                    // Body-read failures are transport-class too (the timeout
                    // that motivated this retry fires here, mid-stream).
                    last_err = Some(if contains_native_replay {
                        anyhow::anyhow!(
                            "OpenAI Codex response failed while replaying native context; details omitted"
                        )
                    } else {
                        e
                    });
                    continue;
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("codex request failed")))
    }

    async fn read_stream(
        resp: reqwest::Response,
        requested_model: &str,
        session: Option<&str>,
    ) -> anyhow::Result<Value> {
        Self::read_stream_with_limit(resp, requested_model, session, MAX_CODEX_SSE_RESPONSE_BYTES)
            .await
    }

    async fn read_stream_with_limit(
        resp: reqwest::Response,
        requested_model: &str,
        session: Option<&str>,
        max_stream_bytes: usize,
    ) -> anyhow::Result<Value> {
        Self::read_stream_observed(resp, requested_model, session, max_stream_bytes, None).await
    }

    async fn read_stream_observed(
        mut resp: reqwest::Response,
        requested_model: &str,
        session: Option<&str>,
        max_stream_bytes: usize,
        observer: Option<&super::contracts::StreamObserver>,
    ) -> anyhow::Result<Value> {
        // Read the SSE body chunk-by-chunk instead of one buffered `.text()`.
        // A heartbeat every 15s proves a long generation is alive. The client's
        // idle-read timeout still catches a stalled connection and the byte
        // limit bounds memory, but a healthy stream has no total wall-clock cap.
        const HEARTBEAT_EVERY: Duration = Duration::from_secs(15);
        const LABEL: &str = "OpenAI Codex SSE response";
        super::ensure_response_content_length(&resp, max_stream_bytes, LABEL)?;
        let started = std::time::Instant::now();
        let mut last_beat = started;
        let mut raw: Vec<u8> = Vec::new();
        let mut output_text = String::new();
        let mut reasoning_text = String::new();
        let mut final_response: Option<Value> = None;
        let mut stream_error: Option<String> = None;
        let mut streamed_calls: Vec<Value> = Vec::new();
        let mut streamed_reasoning: Vec<Value> = Vec::new();
        let mut frame_start = 0;
        let mut scan = 0;
        loop {
            let chunk = resp.chunk().await?;
            let Some(chunk) = chunk else {
                break;
            };
            super::extend_bounded_response_body(&mut raw, &chunk, max_stream_bytes, LABEL)?;
            // Scan each byte once, retaining incomplete frames across network
            // chunks (including split UTF-8 and CRLF delimiters). Dispatch
            // complete frames now, not after the HTTP response closes.
            while scan < raw.len() {
                scan += 1;
                if raw[..scan].ends_with(b"\n\n") || raw[..scan].ends_with(b"\r\n\r\n") {
                    let frame = std::str::from_utf8(&raw[frame_start..scan])
                        .context("OpenAI Codex SSE response was not valid UTF-8")?;
                    Self::collect_reasoning_item(frame, &mut streamed_reasoning);
                    Self::handle_sse_frame_observed(frame, session, &mut output_text,
                        &mut reasoning_text, &mut final_response, &mut streamed_calls,
                        &mut stream_error, observer);
                    frame_start = scan;
                }
            }
            if last_beat.elapsed() >= HEARTBEAT_EVERY {
                let progress = format!(
                        "  model streaming… {} KB in {}s",
                        raw.len() / 1024,
                        started.elapsed().as_secs()
                    );
                super::stream_progress_observed(session, observer, &progress);
                last_beat = std::time::Instant::now();
            }
        }
        let raw_len = raw.len();
        // Preserve compatibility with a final frame lacking a blank line.
        if frame_start < raw.len() {
            let frame = std::str::from_utf8(&raw[frame_start..])
                .context("OpenAI Codex SSE response was not valid UTF-8")?;
            Self::collect_reasoning_item(frame, &mut streamed_reasoning);
            Self::handle_sse_frame_observed(
                frame,
                session,
                &mut output_text,
                &mut reasoning_text,
                &mut final_response,
                &mut streamed_calls,
                &mut stream_error,
                observer,
            );
        }

        let mut response = Self::assemble_stream_response(
            requested_model,
            output_text,
            reasoning_text,
            final_response,
            streamed_calls,
            stream_error,
            raw_len,
        )?;
        if !streamed_reasoning.is_empty() {
            response["streamed_reasoning"] = Value::Array(streamed_reasoning);
        }
        Ok(response)
    }

    /// The ChatGPT Codex backend delivers output items only as
    /// `response.output_item.done` events — its `response.completed` carries
    /// an empty `output`. Keep the reasoning items so they can be replayed.
    fn collect_reasoning_item(frame: &str, items: &mut Vec<Value>) {
        const MAX_REPLAY_ITEMS: usize = 64;
        if !frame.contains("output_item.done") || !frame.contains("encrypted_content") {
            return;
        }
        for line in frame.lines() {
            let Some(data) = line.strip_prefix("data:") else { continue };
            let Ok(json) = serde_json::from_str::<Value>(data.trim()) else { continue };
            if let Some(item) = json.get("item") {
                if item.get("type").and_then(Value::as_str) == Some("reasoning")
                    && items.len() < MAX_REPLAY_ITEMS
                {
                    items.push(item.clone());
                }
            }
        }
    }

    /// Turn the parsed stream into the response value — or an error. An in-band
    /// `error` / `response.failed` event, or a stream that ended without ANY
    /// usable content, must surface as Err so callers can react (the mesh
    /// resends the full transcript when a stored-state continuation is
    /// rejected). Fabricating an empty "completed" here made those turns die
    /// silently: no answer, no tool calls, 0 tokens.
    fn assemble_stream_response(
        requested_model: &str,
        mut output_text: String,
        mut reasoning_text: String,
        final_response: Option<Value>,
        streamed_calls: Vec<Value>,
        stream_error: Option<String>,
        raw_len: usize,
    ) -> anyhow::Result<Value> {
        if let Some(err) = stream_error {
            anyhow::bail!(
                "codex stream reported an error: {}",
                super::bounded_error_preview(&err)
            );
        }

        // If streaming text was empty, try extracting from the final response object
        if output_text.is_empty() {
            if let Some(ref r) = final_response {
                output_text = Self::extract_text(r);
            }
        }
        // Same fallback for reasoning summaries (some servers only send them on
        // the final object, not as deltas).
        if reasoning_text.trim().is_empty() {
            if let Some(ref r) = final_response {
                reasoning_text = Self::extract_reasoning(r);
            }
        }

        if final_response.is_none() && output_text.trim().is_empty() && streamed_calls.is_empty() {
            anyhow::bail!(
                "codex stream ended without a completed response or any output ({} KB received)",
                raw_len / 1024
            );
        }

        let mut response = final_response.unwrap_or_else(
            || serde_json::json!({"model": requested_model, "status": "completed", "usage": {}}),
        );
        response["output_text"] = serde_json::json!(output_text);
        response["reasoning_text"] = serde_json::json!(reasoning_text);
        if !streamed_calls.is_empty() {
            response["streamed_function_calls"] = serde_json::json!(streamed_calls);
        }
        Ok(response)
    }

    fn handle_sse_frame(
        frame: &str,
        session: Option<&str>,
        output_text: &mut String,
        reasoning_text: &mut String,
        final_response: &mut Option<Value>,
        streamed_calls: &mut Vec<Value>,
        stream_error: &mut Option<String>,
    ) {
        Self::handle_sse_frame_observed(frame, session, output_text, reasoning_text, final_response, streamed_calls, stream_error, None);
    }

    fn handle_sse_frame_observed(
        frame: &str,
        session: Option<&str>,
        output_text: &mut String,
        reasoning_text: &mut String,
        final_response: &mut Option<Value>,
        streamed_calls: &mut Vec<Value>,
        stream_error: &mut Option<String>,
        observer: Option<&super::contracts::StreamObserver>,
    ) {
        const MAX_STREAM_TOOL_CALLS: usize = 1024;
        const MAX_ASSEMBLED_FIELD_BYTES: usize = super::MAX_PROVIDER_RESPONSE_BYTES;
        if stream_error.is_some() {
            return;
        }
        let data = frame
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");

        if data.is_empty() || data.trim() == "[DONE]" {
            return;
        }
        let Ok(json) = serde_json::from_str::<Value>(&data) else {
            return;
        };

        let event_type = json.get("type").and_then(Value::as_str).unwrap_or_default();

        if event_type.contains("output_text.delta") {
            if let Some(delta) = json.get("delta").and_then(Value::as_str) {
                if output_text
                    .len()
                    .checked_add(delta.len())
                    .is_none_or(|length| length > MAX_ASSEMBLED_FIELD_BYTES)
                {
                    *stream_error = Some(format!(
                        "assembled output text exceeded the {MAX_ASSEMBLED_FIELD_BYTES} byte limit"
                    ));
                    return;
                }
                if let Some(observer) = observer {
                    observer.emit("text", delta);
                }
                output_text.push_str(delta);
            }
            return;
        }
        if event_type.contains("output_text.done") {
            if let Some(text) = json.get("text").and_then(Value::as_str) {
                if text.len() > MAX_ASSEMBLED_FIELD_BYTES {
                    *stream_error = Some(format!(
                        "assembled output text exceeded the {MAX_ASSEMBLED_FIELD_BYTES} byte limit"
                    ));
                    return;
                }
                *output_text = text.to_string();
            }
            return;
        }
        // Reasoning summary stream (gpt-5.x): surface the model's thinking so the
        // CLI shows orchestrator/coder reasoning the way it shows the librarian's.
        if event_type.contains("reasoning_summary_text.delta") {
            if let Some(delta) = json.get("delta").and_then(Value::as_str) {
                if reasoning_text
                    .len()
                    .checked_add(delta.len())
                    .is_none_or(|length| length > MAX_ASSEMBLED_FIELD_BYTES)
                {
                    *stream_error = Some(format!(
                        "assembled reasoning text exceeded the {MAX_ASSEMBLED_FIELD_BYTES} byte limit"
                    ));
                    return;
                }
                if let Some(observer) = observer {
                    observer.emit("thinking", delta);
                }
                reasoning_text.push_str(delta);
            }
            return;
        }
        if event_type.contains("reasoning_summary_text.done") {
            if let Some(text) = json.get("text").and_then(Value::as_str) {
                if !text.trim().is_empty() {
                    if text.len() > MAX_ASSEMBLED_FIELD_BYTES {
                        *stream_error = Some(format!(
                            "assembled reasoning text exceeded the {MAX_ASSEMBLED_FIELD_BYTES} byte limit"
                        ));
                        return;
                    }
                    *reasoning_text = text.to_string();
                }
            }
            return;
        }
        // Native tool-call streaming (Responses API). A function call arrives as:
        //   output_item.added   → item {type:function_call, call_id, id, name}
        //   function_call_arguments.delta → delta (the arguments JSON, streamed)
        //   output_item.done    → item with the complete `arguments` string
        if event_type.contains("output_item.added") {
            if let Some(item) = json.get("item") {
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    if streamed_calls.len() >= MAX_STREAM_TOOL_CALLS {
                        *stream_error = Some(format!(
                            "streamed tool calls exceeded the {MAX_STREAM_TOOL_CALLS} call limit"
                        ));
                        return;
                    }
                    streamed_calls.push(serde_json::json!({
                        "item_id": item.get("id"),
                        "output_index": json.get("output_index"),
                        "call_id": item.get("call_id").and_then(Value::as_str)
                            .or_else(|| item.get("id").and_then(Value::as_str))
                            .unwrap_or(""),
                        "name": item.get("name").and_then(Value::as_str).unwrap_or(""),
                        "arguments": item.get("arguments").and_then(Value::as_str).unwrap_or(""),
                    }));
                }
            }
            return;
        }
        if event_type.contains("function_call_arguments.delta") {
            if let Some(delta) = json.get("delta").and_then(Value::as_str) {
                let item_id=json.get("item_id").and_then(Value::as_str);
                let output_index=json.get("output_index").and_then(Value::as_u64);
                let target=if item_id.is_some()||output_index.is_some() {
                    let matches=streamed_calls.iter().enumerate().filter(|(_,call)|
                        item_id.is_none_or(|id|call["item_id"].as_str()==Some(id)) &&
                        output_index.is_none_or(|index|call["output_index"].as_u64()==Some(index)))
                        .map(|(index,_)|index).collect::<Vec<_>>();
                    if matches.len()==1 {Some(matches[0])} else {None}
                } else if streamed_calls.len()==1 {Some(0)} else {None};
                let Some(target)=target else {
                    *stream_error=Some("tool argument delta has no unambiguous matching call".into());
                    return;
                };
                if let Some(last) = streamed_calls.get_mut(target) {
                    if let Some(Value::String(arguments)) = last.get_mut("arguments") {
                        if arguments
                            .len()
                            .checked_add(delta.len())
                            .is_none_or(|length| length > MAX_ASSEMBLED_FIELD_BYTES)
                        {
                            *stream_error = Some(format!(
                                "streamed tool-call arguments exceeded the {MAX_ASSEMBLED_FIELD_BYTES} byte limit"
                            ));
                            return;
                        }
                        arguments.push_str(delta);
                    }
                }
            }
            return;
        }
        if event_type.contains("output_item.done") {
            if let Some(item) = json.get("item") {
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    // The done event carries the complete item; prefer its values.
                    let call = serde_json::json!({
                        "item_id": item.get("id"),
                        "output_index": json.get("output_index"),
                        "call_id": item.get("call_id").and_then(Value::as_str)
                            .or_else(|| item.get("id").and_then(Value::as_str))
                            .unwrap_or(""),
                        "name": item.get("name").and_then(Value::as_str).unwrap_or(""),
                        "arguments": item.get("arguments").and_then(Value::as_str).unwrap_or(""),
                    });
                    // Replace the matching streamed entry (started by output_item.added)
                    // if present; otherwise this is our first sight of the call.
                    let id = call["call_id"].as_str().unwrap_or("");
                    if let Some(existing) = streamed_calls
                        .iter_mut()
                        .find(|c| c["call_id"].as_str() == Some(id))
                    {
                        if call["arguments"].as_str().is_some_and(|a| !a.is_empty()) {
                            *existing = call;
                        }
                    } else {
                        if streamed_calls.len() >= MAX_STREAM_TOOL_CALLS {
                            *stream_error = Some(format!(
                                "streamed tool calls exceeded the {MAX_STREAM_TOOL_CALLS} call limit"
                            ));
                            return;
                        }
                        streamed_calls.push(call);
                    }
                }
            }
            return;
        }
        if event_type.contains("completed") || event_type.contains("response.done") {
            if let Some(response) = json.get("response") {
                *final_response = Some(response.clone());
            }
            return;
        }
        // An incomplete response (e.g. max_output_tokens) still carries partial
        // output and real usage — keep it; status_details() reports the reason.
        if event_type == "response.incomplete" {
            if let Some(response) = json.get("response") {
                *final_response = Some(response.clone());
            }
            return;
        }
        // In-band failures: `response.failed` (e.g. a rejected request
        // after an account switch lands here) and bare `error` events.
        if event_type == "response.failed" {
            let msg = json
                .pointer("/response/error/message")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| json.to_string());
            *stream_error = Some(msg);
            return;
        }
        if event_type == "error" {
            let msg = json
                .get("message")
                .or_else(|| json.pointer("/error/message"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| json.to_string());
            *stream_error = Some(msg);
        }
    }

    /// Pull reasoning-summary text out of the final Responses object, scanning
    /// `output[]` items of type `reasoning` and concatenating their summaries.
    fn extract_reasoning(json: &Value) -> String {
        if let Some(text) = json.get("reasoning_text").and_then(Value::as_str) {
            if !text.trim().is_empty() {
                return text.trim().to_string();
            }
        }
        json.get("output")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|item| item["type"].as_str() == Some("reasoning"))
            .flat_map(|item| {
                item["summary"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|s| s["text"].as_str())
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string()
    }

    /// `(status, incomplete_reason)` from the final Responses object.
    fn status_details(json: &Value) -> (Option<String>, Option<String>) {
        let status = json
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_string);
        let reason = json
            .get("incomplete_details")
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str)
            .map(str::to_string);
        (status, reason)
    }

    fn extract_text(json: &Value) -> String {
        // Try the convenience field we set after streaming
        if let Some(text) = json.get("output_text").and_then(Value::as_str) {
            return text.trim().to_string();
        }
        // Fall back to scanning output items for text content
        json.get("output")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|item| item["type"].as_str() == Some("message"))
            .flat_map(|item| {
                item["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|c| c["type"].as_str() == Some("output_text"))
                    .filter_map(|c| c["text"].as_str())
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
            .join("")
            .trim()
            .to_string()
    }

    /// Extract native tool calls. Prefer calls captured from the stream (the
    /// Codex backend does not reliably repeat them in the final object); fall
    /// back to scanning the final `output[]` array for other Responses endpoints.
    fn extract_function_calls(json: &Value) -> anyhow::Result<Vec<NativeToolCall>> {
        let source = json.get("streamed_function_calls")
            .filter(|v| v.as_array().is_some_and(|a| !a.is_empty()))
            .or_else(|| json.get("output"));
        let mut calls=Vec::new();
        let mut seen=std::collections::HashSet::new();
        for item in source.and_then(Value::as_array).into_iter().flatten() {
            if item["type"].as_str()!=Some("function_call") &&
                !(item.get("call_id").is_some() && item.get("name").is_some()) {continue;}
            let id=item["call_id"].as_str().or_else(||item["id"].as_str())
                .filter(|id|!id.trim().is_empty()).context("provider tool call is missing its identity")?;
            anyhow::ensure!(seen.insert(id),"provider response contains duplicate tool call identities");
            let name=item["name"].as_str().filter(|name|!name.trim().is_empty())
                .context("provider tool call is missing its name")?;
            let raw=item["arguments"].as_str().context("provider tool arguments must be a JSON string")?;
            // Do not fabricate defaults or echo argument contents, which may
            // contain credentials or private file data, into provider errors.
            let arguments:Value=serde_json::from_str(raw)
                .map_err(|_|anyhow::anyhow!("provider tool call contains malformed argument JSON"))?;
            anyhow::ensure!(arguments.is_object(),"provider tool arguments must decode to an object");
            calls.push(NativeToolCall{id:id.into(),tool_name:name.into(),arguments});
        }
        Ok(calls)
    }

    fn usage(json: &Value) -> TokenUsage {
        let usage = json.get("usage").unwrap_or(&Value::Null);
        let input_tokens = usage
            .get("input_tokens")
            .or_else(|| usage.get("prompt_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        let output_tokens = usage
            .get("output_tokens")
            .or_else(|| usage.get("completion_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        let mut parsed = TokenUsage::new(input_tokens, output_tokens);
        // Responses API cache telemetry (donor: opencode openai-responses):
        // `input_tokens_details.cached_tokens`, chat-shape fallback included.
        // Same sanity guard as the compat parser — cached ≤ input or ignore.
        parsed.cache_read_tokens = usage
            .pointer("/input_tokens_details/cached_tokens")
            .or_else(|| usage.pointer("/prompt_tokens_details/cached_tokens"))
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n <= input_tokens);
        parsed
    }
}

#[async_trait]
impl LLMProvider for OpenAICodexProvider {
    fn supports_native_images(&self) -> bool {
        true
    }
    fn name(&self) -> &str {
        self.provider_id
    }
    fn display_name(&self) -> &str {
        self.display_name
    }
    fn base_url(&self) -> &str {
        &self.base_url
    }
    fn auth_type(&self) -> AuthType {
        if self.provider_id == "openai" {
            AuthType::Bearer
        } else {
            AuthType::Bearer
        }
    }
    fn env_vars(&self) -> Vec<&str> {
        if self.provider_id == "openai" {
            vec!["OPENAI_API_KEY"]
        } else {
            vec!["OPENAI_OAUTH_TOKEN"]
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

    fn native_compaction_capability(&self, model: &str) -> NativeCompactionCapability {
        if self.native_compaction_enabled
            && self.is_trusted_native_compaction_route()
            && self.supports_native_compaction_model(model)
        {
            NativeCompactionCapability::ResponsesCompact
        } else {
            NativeCompactionCapability::Unsupported
        }
    }

    fn response_dialect(&self, _model: &str) -> ProviderResponseDialect {
        ProviderResponseDialect::OpenAiResponses
    }

    async fn compact_context(
        &self,
        request: NativeCompactionRequest,
    ) -> anyhow::Result<NativeCompactionResult> {
        anyhow::ensure!(
            self.native_compaction_route_matches(&request),
            "OpenAI native compaction request does not match this provider route"
        );

        // `/responses/compact` is intrinsically stateless and does not accept
        // the normal Responses `store` field. Normal replay requests remain
        // explicitly `store: false` in `make_request` above.
        let body = serde_json::json!({
            "model": request.model(),
            "instructions": request.instructions(),
            "input": Self::native_compaction_input(&request),
        });
        let json = self.send_native_compaction(&body).await?;
        anyhow::ensure!(
            json.get("object").and_then(Value::as_str) == Some("response.compaction"),
            "OpenAI compact API returned an unexpected object type"
        );
        let output = json
            .get("output")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("OpenAI compact API response is missing output"))?;

        let provenance = NativeCompactionProvenance::try_for_route(
            request.session_id(),
            request.route(),
            request.next_compaction_generation(),
            request.target_transcript_revision(),
        )?;
        // The complete output array is the provider's canonical next context
        // window. Validation may reject it, but it must never be filtered,
        // reordered, parsed into a lossy shape, or reduced to one item.
        let replay = NativeCompactionReplay::try_new(
            provenance,
            NativeCompactionCapability::ResponsesCompact,
            1,
            output,
        )?;
        let folded_input_messages = request.messages().len();
        NativeCompactionResult::try_new(
            &request,
            replay,
            None,
            Self::usage(&json),
            folded_input_messages,
        )
    }

    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        let json = self.make_request(&request).await?;
        let tool_calls = Self::extract_function_calls(&json)?;
        let model = json
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(&request.model)
            .to_string();
        let (status, incomplete_reason) = Self::status_details(&json);
        let content = Self::extract_text(&json);
        let mut reasoning = Self::extract_reasoning(&json);

        // A reasoning model can finish a turn having produced ONLY hidden
        // reasoning — no visible message, no tool call ("plans then stalls").
        // We keep returning the (empty) content so the runtime's existing
        // empty-content recovery / repair path still runs, but we make the stall
        // VISIBLE in the reasoning channel instead of letting it look like the
        // model did nothing. (We can't raise the output budget — this endpoint
        // rejects max_output_tokens — so surfacing the stall is the lever we have.)
        if content.trim().is_empty() && tool_calls.is_empty() {
            let reason = incomplete_reason
                .as_deref()
                .or(status.as_deref())
                .unwrap_or("no message returned");
            let note = format!(
                "[Phoenix] Codex returned no message and no tool call (status: {reason}); \
                 the reasoning model likely stalled or exhausted its output budget."
            );
            reasoning = if reasoning.trim().is_empty() {
                note
            } else {
                format!("{reasoning}\n\n{note}")
            };
        }
        let reasoning = (!reasoning.trim().is_empty()).then_some(reasoning);

        Ok(CompletionResponse {
            content,
            model,
            usage: Self::usage(&json),
            reasoning,
            stop_reason: status,
            tool_calls,
            provider_replay: {
                let items = Self::replayable_reasoning(&json);
                (!items.is_empty()).then(|| super::contracts::ProviderReplay {
                    route: self.replay_route(&request.model),
                    items,
                })
            },
        })
    }

    async fn stream(&self, request: CompletionRequest) -> anyhow::Result<StreamingResponse> {
        let response = self.complete(request).await?;
        Ok(StreamingResponse {
            content: response.content,
            reasoning: response.reasoning,
            done: true,
        })
    }

    async fn embeddings(&self, _texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
        anyhow::bail!("OpenAI Codex provider does not support embeddings in Phoenix yet")
    }

    async fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        let provider_data = providers_data::get_provider(self.catalog_id)
            .ok_or_else(|| anyhow::anyhow!("{} provider data not found", self.display_name))?;
        Ok(provider_data
            .models
            .iter()
            .map(|model| ModelInfo {
                id: model.id.to_string(),
                object: "model".to_string(),
                created: None,
                context_window: Some(model.context_window),
                input_cost_per_token: None,
                output_cost_per_token: None,
                supports_tools: Some(true),
                supports_vision: Some(true),
            })
            .collect())
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        let mut request = CompletionRequest::new(
            self.default_model_impl(),
            vec![super::contracts::ChatMessage::user("Reply with: ok")],
        );
        request.max_tokens = Some(8);
        Ok(self.complete(request).await.is_ok())
    }
}

/// Diagnostic: with `PHOENIX_DUMP_REQUESTS=<dir>`, write every outgoing
/// Responses body there so context composition can be measured from the real
/// wire payload instead of estimated. Off unless the variable is set.
fn dump_request_body(body: &Value, session: Option<&str>) {
    let Some(dir) = std::env::var_os("PHOENIX_DUMP_REQUESTS") else { return };
    let dir = std::path::PathBuf::from(dir);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let session: String = session
        .unwrap_or("none")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .take(80)
        .collect();
    let path = dir.join(format!("{stamp}-{session}.json"));
    if let Ok(bytes) = serde_json::to_vec(body) {
        let _ = std::fs::write(path, bytes);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn codex_speed_wire_tiers_keep_fast_and_ultrafast_distinct() {
        for model in ["gpt-6.1-sol", "gpt-6-astra"] {
            assert_eq!(super::codex_service_tier(model, "fast"), Some("priority"));
            assert_eq!(super::codex_service_tier(model, "ultrafast"), Some("ultrafast"));
            assert_eq!(super::codex_service_tier(model, "standard"), None);
        }
        assert_eq!(super::codex_service_tier("gpt-6-luna", "ultrafast"), None);
        assert_eq!(super::codex_service_tier("gpt-4.1-mini", "fast"), None);
    }
    #[test]
    fn codex_requests_scope_the_account_on_the_subscription_origin_only() {
        use base64::Engine;
        let payload = serde_json::json!({"sub":"test-user","https://api.openai.com/auth":{"chatgpt_account_id":"test-account"}});
        let token = format!("header.{}.signature", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string()));
        for (url, expected) in [(CHATGPT_CODEX_BASE_URL, Some("test-account")), (PUBLIC_OPENAI_RESPONSES_BASE_URL, None)] {
            let provider = OpenAICodexProvider::with_url_and_timeout(url.into(), token.clone(), Duration::from_secs(2));
            let request = provider.authenticated_post(provider.responses_url(), &token).build().unwrap();
            assert_eq!(request.headers().get("chatgpt-account-id").map(|v| v.to_str().unwrap()), expected);
        }
        assert!(codex_account_id("invalid-token").is_none());
    }

    #[test]
    fn reasoning_replays_only_on_its_route_and_prose_survives_tool_calls() {
        use crate::providers::contracts::{ChatMessage, NativeToolCall, ProviderReplay};
        let mut reply = ChatMessage::assistant_tool_calls(
            "13_banana_single.jpg is a studio shot; rejected.",
            vec![NativeToolCall { id: "c1".into(), tool_name: "look".into(), arguments: serde_json::json!({}) }],
        );
        reply.provider_replay = Some(ProviderReplay {
            route: "r1".into(),
            items: vec![serde_json::json!({"type":"reasoning","summary":[],"encrypted_content":"x"})],
        });
        let messages = [reply];
        let kinds = |route| -> Vec<String> {
            OpenAICodexProvider::responses_items_routed(&messages, false, route, false)
                .iter()
                .map(|item| item.get("type").and_then(|t| t.as_str()).unwrap_or("message").to_string())
                .collect()
        };
        assert_eq!(kinds(Some("r1")), ["reasoning", "message", "function_call"]);
        assert_eq!(kinds(Some("other-account")), ["message", "function_call"]);
        assert_eq!(kinds(None), ["message", "function_call"]);
    }

    #[test]
    fn streamed_reasoning_items_are_collected_for_replay() {
        let mut items = Vec::new();
        OpenAICodexProvider::collect_reasoning_item(
            r#"data: {"type":"response.output_item.done","item":{"type":"reasoning","id":"rs_1","summary":[],"encrypted_content":"blob"}}"#,
            &mut items,
        );
        OpenAICodexProvider::collect_reasoning_item(
            r#"data: {"type":"response.output_item.done","item":{"type":"function_call","call_id":"c"}}"#,
            &mut items,
        );
        assert_eq!(items.len(), 1);
        let replay = OpenAICodexProvider::replayable_reasoning(&serde_json::json!({"output": [], "streamed_reasoning": items}));
        assert_eq!(replay, vec![serde_json::json!({"type":"reasoning","summary":[],"encrypted_content":"blob"})]);
        let mut body = serde_json::json!({"input": [replay[0].clone(), {"type":"function_call"}]});
        assert!(OpenAICodexProvider::strip_replayed_reasoning(&mut body));
        assert_eq!(body["input"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn cache_routing_key_prefers_the_body_key_then_the_session() {
        let keyed = serde_json::json!({"prompt_cache_key": "agent-theo"});
        assert_eq!(OpenAICodexProvider::cache_routing_key(&keyed, Some("s")), Some("agent-theo"));
        let bare = serde_json::json!({});
        assert_eq!(OpenAICodexProvider::cache_routing_key(&bare, Some("s")), Some("s"));
        assert_eq!(OpenAICodexProvider::cache_routing_key(&bare, Some("")), None);
        assert_eq!(OpenAICodexProvider::cache_routing_key(&bare, None), None);
    }

    #[tokio::test]
    #[ignore = "real authorized Codex request with a synthetic image"]
    async fn live_astra_native_image_through_normal_completion() {
        use crate::config::auth_profile::{load_auth_profile_store, extract_profile_secret};
        let store = load_auth_profile_store().unwrap();
        let mut ids = store.profiles_for_provider("openai-codex");
        ids.sort();
        let token = extract_profile_secret(&store.profiles[ids.first().expect("connected Codex account")]).unwrap();
        let provider = super::OpenAICodexProvider::with_url_and_timeout(
            super::CHATGPT_CODEX_BASE_URL.into(), token, std::time::Duration::from_secs(90));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("color-blocks.png");
        image::RgbImage::from_fn(192, 96, |x, _| if x < 96 { image::Rgb([240, 0, 0]) } else { image::Rgb([0, 0, 240]) })
            .save(&path).unwrap();
        let pixels = crate::runtime::vision::screenshot_data_uri(&path).await.unwrap();
        let mut request = CompletionRequest::new("gpt-6-astra", vec![ChatMessage::user_with_images(
            "Identify the two solid regions in the supplied image. Return only JSON with left and right as lowercase basic color names.", vec![pixels])]);
        request.extra_body.insert("reasoning".into(), serde_json::json!({"effort":"low"}));
        let started = std::time::Instant::now();
        let response = provider.complete(request).await.unwrap_or_else(|_| panic!("native-image provider request failed; raw account response suppressed"));
        let answer: serde_json::Value = serde_json::from_str(response.content.trim()).expect("JSON image answer");
        assert_eq!(answer, serde_json::json!({"left":"red","right":"blue"}));
        println!("ASTRA_NATIVE_IMAGE_OK normal_completion=true sidecar=false elapsed_ms={} input_tokens={}", started.elapsed().as_millis(), response.usage.input_tokens);
    }

    #[test]
    fn native_images_survive_normal_and_compaction_wire_serialization() {
        use super::*;
        let image = "data:image/png;base64,cGl4ZWxz";
        for include_instructions in [false, true] {
            let items = OpenAICodexProvider::responses_items(&[
                ChatMessage::user_with_images("Inspect this", vec![image.into(), "data:image/png;base64,cmVmZXJlbmNl".into()]),
                ChatMessage::user_with_images("", vec![image.into()]),
            ], include_instructions);
            assert_eq!(items.len(), 2, "image-only messages must not disappear");
            assert_eq!(items[0]["content"][0]["type"], "input_text");
            assert_eq!(items[0]["content"][1]["image_url"], image);
            assert_eq!(items[0]["content"][2]["image_url"], "data:image/png;base64,cmVmZXJlbmNl");
            assert_eq!(items[1]["content"][0]["type"], "input_image");
        }
    }
    /// Opt-in network acceptance through Phoenix's actual subscription adapter.
    /// Uses only synthetic text; never prints credentials or changes routes.
    #[tokio::test]
    #[ignore = "requires an authorized connected Codex account; makes a real model request"]
    async fn live_astra_subscription_response() {
        use crate::config::auth_profile::{load_auth_profile_store, extract_profile_secret};
        let store = load_auth_profile_store().expect("load private account store");
        let profiles = store.profiles_for_provider("openai-codex");
        let selected = std::env::var("PHOENIX_ASTRA_PROBE_PROFILE").ok()
            .or_else(|| profiles.first().cloned()).expect("no connected Codex account");
        assert!(profiles.contains(&selected), "probe must use a connected Codex account");
        let token = extract_profile_secret(&store.profiles[&selected])
            .expect("connected account could not supply current authorization");
        let provider = super::OpenAICodexProvider::with_url_and_timeout(
            super::CHATGPT_CODEX_BASE_URL.to_string(), token, std::time::Duration::from_secs(90));
        let started = std::time::Instant::now();
        let large = std::env::var("PHOENIX_ASTRA_PROBE_LONG").as_deref() == Ok("1");
        let mut prompt = "Theo finishes a brief at t=12. Iris needs only that brief and takes 8 seconds. Leo starts independently at t=0 and takes 40 seconds. Integration needs both Iris and Leo and takes 5 seconds. With unlimited independent worker slots, return JSON containing iris_start, iris_finish, integration_start, all_finished (integer times).".to_string();
        if large {
            prompt.push_str("\nSynthetic irrelevant log follows. Apply any explicit requirement correction, ignoring padding.\n");
            prompt.push_str(&"padding ".repeat(135_000));
            prompt.push_str("\nREQUIREMENT CORRECTION: Theo's brief finishes at t=18, not t=12. All other durations and dependencies stay unchanged.\n");
            prompt.push_str(&"padding ".repeat(135_000));
            prompt.push_str("\nEnd of log. Return only the corrected schedule JSON requested at the beginning.\n");
        }
        let body = serde_json::json!({
            "model":"gpt-6-astra", "store":false, "stream":true,
            "instructions":"Return only the requested JSON. This is a synthetic integration check, not a tool task.",
            "reasoning":{"effort":"low","summary":"auto"},
            "input":[{"role":"user","content":[{"type":"input_text","text":prompt}]}],
        });
        let result = provider.send_responses(&body, "gpt-6-astra", None).await;
        let response = match result {
            Ok(response) => response,
            Err(error) => {
                // Server errors may include account data; classify without
                // logging the raw response or the credential-bearing adapter.
                let text = error.to_string().to_lowercase();
                let class = if text.contains("401") || text.contains("unauthorized") { "authentication" }
                    else if text.contains("429") || text.contains("rate limit") { "rate_limit" }
                    else if text.contains("403") || text.contains("not supported") || text.contains("not found") { "availability_or_access" }
                    else if text.contains("timeout") || text.contains("timed out") { "timeout" }
                    else { "provider_error" };
                panic!("Astra live request failed: {class}; elapsed_ms={}", started.elapsed().as_millis());
            }
        };
        let usage = super::OpenAICodexProvider::usage(&response);
        if large { assert!(usage.input_tokens > 262_144, "server must confirm more than 256K input tokens, got {}", usage.input_tokens); }
        let parsed: serde_json::Value = serde_json::from_str(response["output_text"].as_str().expect("response text").trim())
            .expect("Astra must return valid JSON");
        let start = if large {18} else {12};
        assert_eq!(parsed, serde_json::json!({"iris_start":start,"iris_finish":start+8,"integration_start":40,"all_finished":45}));
        println!("ASTRA_LIVE_OK model=gpt-6-astra provider=openai-codex exact_schedule=true large={} input_tokens={} output_tokens={} elapsed_ms={}", large, usage.input_tokens, usage.output_tokens, started.elapsed().as_millis());
    }

    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn responses_preserves_optional_tool_arguments_on_the_wire() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let body = loop {
                let mut chunk = [0u8;4096];
                let n = socket.read(&mut chunk).await.unwrap();
                assert!(n > 0); bytes.extend_from_slice(&chunk[..n]);
                assert!(bytes.len() < 128*1024);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers=std::str::from_utf8(&bytes[..end]).unwrap();
                    let length:usize=headers.lines().find_map(|line|line.split_once(':').filter(|(name,_)|name.eq_ignore_ascii_case("content-length")).map(|(_,value)|value.trim().parse().unwrap())).unwrap();
                    if bytes.len() >= end+4+length { break serde_json::from_slice::<Value>(&bytes[end+4..end+4+length]).unwrap(); }
                }
            };
            let response="data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{}}}\n\n";
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).as_bytes()).await.unwrap();
            body
        });
        let provider=OpenAICodexProvider::with_url_and_timeout(format!("http://{address}"),"local-test-token".into(),Duration::from_secs(5));
        let mut request=CompletionRequest::new("gpt-5.6-sol",vec![ChatMessage::user("Inspect the full image")]);
        request.tools=crate::tools::tool_definitions_for_agent(&["image_analyze".into()]);
        provider.make_request(&request).await.unwrap();
        let body=server.await.unwrap();
        assert_eq!(body["tools"][0]["strict"],false);
        assert_eq!(body["tools"][0]["parameters"],request.tools[0].parameters);
        assert_eq!(body["tools"][0]["parameters"]["required"],serde_json::json!(["path"]));
        assert_eq!(body["tools"][0]["parameters"]["properties"]["crop"]["type"],serde_json::json!(["object","null"]));
        assert_eq!(body["tool_choice"],"required");
    }

    async fn local_response(raw_response: Vec<u8>) -> reqwest::Response {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            let _ = socket.read(&mut request).await;
            socket.write_all(&raw_response).await.unwrap();
            socket.shutdown().await.unwrap();
        });
        reqwest::Client::new()
            .get(format!("http://{address}/"))
            .send()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn request_owned_streams_deliver_before_eof_without_cross_talk() {
        async fn held_response(text: &str) -> (reqwest::Response, tokio::sync::oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let frame = format!("data: {}\r\n\r\n", serde_json::json!({"type":"response.output_text.delta", "delta":text}));
            let ending = b"data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{}}}\n\n";
            let (release, released) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0u8; 2048];
                socket.read(&mut request).await.unwrap();
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", frame.len() + ending.len()).as_bytes()).await.unwrap();
                // Fragment every UTF-8 byte and CRLF delimiter at transport
                // writes. The last response frame remains withheld.
                for byte in frame.as_bytes() {
                    socket.write_all(&[*byte]).await.unwrap();
                    tokio::task::yield_now().await;
                }
                if released.await.is_ok() {
                    socket.write_all(ending).await.unwrap();
                }
            });
            let response = reqwest::Client::new().get(format!("http://{address}/")).send().await.unwrap();
            (response, release, server)
        }
        let (a, release_a, server_a) = held_response("Iris 🦋").await;
        let (b, release_b, server_b) = held_response("Theo").await;
        let (tx_a, mut rx_a) = tokio::sync::mpsc::unbounded_channel();
        let (tx_b, mut rx_b) = tokio::sync::mpsc::unbounded_channel();
        let observer_a = crate::providers::contracts::StreamObserver::new(move |kind, text| { let _ = tx_a.send((kind.to_string(), text.to_string())); });
        let observer_b = crate::providers::contracts::StreamObserver::new(move |kind, text| { let _ = tx_b.send((kind.to_string(), text.to_string())); });
        let mut request = CompletionRequest::new("model", vec![]);
        request.stream_observer = Some(observer_a);
        assert!(serde_json::to_value(&request).unwrap().get("stream_observer").is_none());
        let observer_a = request.clone().stream_observer.unwrap();
        let task_a = tokio::spawn(async move { OpenAICodexProvider::read_stream_observed(a, "model", Some("observer-same-room"), 4096, Some(&observer_a)).await });
        let task_b = tokio::spawn(async move { OpenAICodexProvider::read_stream_observed(b, "model", Some("observer-same-room"), 4096, Some(&observer_b)).await });
        let first_a = tokio::time::timeout(Duration::from_secs(2), rx_a.recv()).await.expect("Iris output must arrive before EOF").unwrap();
        let first_b = tokio::time::timeout(Duration::from_secs(2), rx_b.recv()).await.expect("Theo output must arrive before EOF").unwrap();
        assert_eq!(first_a, ("text".to_string(), "Iris 🦋".to_string()));
        assert_eq!(first_b, ("text".to_string(), "Theo".to_string()));
        assert!(!task_a.is_finished() && !task_b.is_finished());
        release_a.send(()).unwrap();
        assert_eq!(task_a.await.unwrap().unwrap()["output_text"], "Iris 🦋");
        assert!(!task_b.is_finished());
        release_b.send(()).unwrap();
        assert_eq!(task_b.await.unwrap().unwrap()["output_text"], "Theo");
        server_a.await.unwrap();
        server_b.await.unwrap();
        assert!(rx_a.try_recv().is_err() && rx_b.try_recv().is_err(), "frames must not replay at EOF");
    }

    #[tokio::test]
    async fn sse_reader_fails_when_chunked_body_crosses_limit() {
        let response = local_response(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n8\r\ndata: {}\r\n0\r\n\r\n"
                .to_vec(),
        )
        .await;
        let error = OpenAICodexProvider::read_stream_with_limit(response, "model", None, 7)
            .await
            .unwrap_err();
        assert!(super::super::is_response_body_limit_error(&error));
    }

    #[test]
    fn codex_stream_keeps_a_bounded_response_size() {
        assert_eq!(MAX_CODEX_SSE_RESPONSE_BYTES, 4 * 1024 * 1024);
    }

    #[test]
    fn extract_reasoning_reads_summary_items() {
        let json = serde_json::json!({
            "output": [
                {"type": "reasoning", "summary": [
                    {"type": "summary_text", "text": "First I check the project map."},
                    {"type": "summary_text", "text": "Then I read the runner."}
                ]},
                {"type": "message", "content": [
                    {"type": "output_text", "text": "done"}
                ]}
            ]
        });
        let reasoning = OpenAICodexProvider::extract_reasoning(&json);
        assert!(reasoning.contains("project map"));
        assert!(reasoning.contains("read the runner"));
    }

    #[test]
    fn status_details_reads_incomplete_reason() {
        let json = serde_json::json!({
            "status": "incomplete",
            "incomplete_details": {"reason": "max_output_tokens"}
        });
        let (status, reason) = OpenAICodexProvider::status_details(&json);
        assert_eq!(status.as_deref(), Some("incomplete"));
        assert_eq!(reason.as_deref(), Some("max_output_tokens"));
    }

    #[test]
    fn handle_sse_frame_captures_reasoning_summary_delta() {
        let mut out = String::new();
        let mut reasoning = String::new();
        let mut final_response = None;
        let mut calls = Vec::new();
        let mut stream_error = None;
        OpenAICodexProvider::handle_sse_frame(
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"thinking...\"}",
            None,
            &mut out,
            &mut reasoning,
            &mut final_response,
            &mut calls,
            &mut stream_error,
        );
        assert_eq!(reasoning, "thinking...");
        assert!(out.is_empty());
    }

    #[test]
    fn streamed_function_call_is_captured_and_extracted() {
        let mut out = String::new();
        let mut reasoning = String::new();
        let mut final_response = None;
        let mut calls = Vec::new();
        let frames = [
            r#"data: {"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_1","name":"web_search"}}"#,
            r#"data: {"type":"response.function_call_arguments.delta","delta":"{\"query\":\"top "}"#,
            r#"data: {"type":"response.function_call_arguments.delta","delta":"repos\"}"}"#,
            r#"data: {"type":"response.output_item.done","item":{"type":"function_call","call_id":"call_1","name":"web_search","arguments":"{\"query\":\"top repos\"}"}}"#,
        ];
        let mut stream_error = None;
        for f in frames {
            OpenAICodexProvider::handle_sse_frame(
                f,
                None,
                &mut out,
                &mut reasoning,
                &mut final_response,
                &mut calls,
                &mut stream_error,
            );
        }
        assert_eq!(calls.len(), 1);
        // Build the response object the way read_stream does, then extract.
        let response = serde_json::json!({ "streamed_function_calls": calls });
        let extracted = OpenAICodexProvider::extract_function_calls(&response).unwrap();
        assert_eq!(extracted.len(), 1);
        assert_eq!(extracted[0].tool_name, "web_search");
        assert_eq!(extracted[0].id, "call_1");
        assert_eq!(extracted[0].arguments["query"], "top repos");
    }

    #[test]
    fn malformed_tool_arguments_reject_the_entire_response_without_defaults() {
        for bad in [serde_json::json!("{secret-value"),serde_json::json!("null"),
            serde_json::json!("[]"),serde_json::json!("42"),Value::Null,serde_json::json!({})] {
            let response=serde_json::json!({"output":[
                {"type":"function_call","call_id":"first","name":"read","arguments":"{}"},
                {"type":"function_call","call_id":"second","name":"read","arguments":bad}
            ]});
            let error=OpenAICodexProvider::extract_function_calls(&response).unwrap_err().to_string();
            assert!(!error.contains("secret-value"));
        }
        let empty=serde_json::json!({"output":[{"type":"function_call","call_id":"valid","name":"computer_screenshot","arguments":"{}"}]});
        assert_eq!(OpenAICodexProvider::extract_function_calls(&empty).unwrap()[0].arguments,serde_json::json!({}));
    }

    #[test]
    fn tool_call_identity_is_required_and_unique() {
        for bad in [
            serde_json::json!({"type":"function_call","name":"read","arguments":"{}"}),
            serde_json::json!({"type":"function_call","call_id":"first","name":"read","arguments":"{}"}),
            serde_json::json!({"type":"function_call","call_id":"other","arguments":"{}"})
        ] {
            assert!(OpenAICodexProvider::extract_function_calls(&serde_json::json!({"streamed_function_calls":[
                {"call_id":"first","name":"read","arguments":"{}"},bad
            ]})).is_err());
        }
    }

    #[test]
    fn interleaved_argument_deltas_stay_with_their_call() {
        let (_,_,error,calls)=parse_frames(&[
            r#"data: {"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"item_a","call_id":"call_a","name":"read"}}"#,
            r#"data: {"type":"response.output_item.added","output_index":2,"item":{"type":"function_call","id":"item_b","call_id":"call_b","name":"read"}}"#,
            r#"data: {"type":"response.function_call_arguments.delta","item_id":"item_a","output_index":0,"delta":"{\"path\":\"first"}"#,
            r#"data: {"type":"response.function_call_arguments.delta","output_index":2,"delta":"{\"path\":\"second\"}"}"#,
            r#"data: {"type":"response.function_call_arguments.delta","item_id":"item_a","delta":".txt\"}"}"#,
        ]);
        assert!(error.is_none(),"{error:?}");
        let extracted=OpenAICodexProvider::extract_function_calls(&serde_json::json!({"streamed_function_calls":calls})).unwrap();
        assert_eq!(extracted[0].id,"call_a");
        assert_eq!(extracted[0].arguments["path"],"first.txt");
        assert_eq!(extracted[1].id,"call_b");
        assert_eq!(extracted[1].arguments["path"],"second");
    }

    #[test]
    fn unidentified_parallel_argument_delta_fails_without_mutating_calls() {
        let (_,_,error,calls)=parse_frames(&[
            r#"data: {"type":"response.output_item.added","item":{"type":"function_call","id":"a","call_id":"a","name":"read"}}"#,
            r#"data: {"type":"response.output_item.added","item":{"type":"function_call","id":"b","call_id":"b","name":"read"}}"#,
            r#"data: {"type":"response.function_call_arguments.delta","delta":"wrong"}"#,
        ]);
        assert!(error.unwrap().contains("unambiguous"));
        assert!(calls.iter().all(|call|call["arguments"]==""));
    }

    #[test]
    fn unknown_or_conflicting_delta_identity_never_falls_back_to_last_call() {
        for delta in [
            r#"data: {"type":"response.function_call_arguments.delta","item_id":"missing","delta":"wrong"}"#,
            r#"data: {"type":"response.function_call_arguments.delta","item_id":"a","output_index":7,"delta":"wrong"}"#,
        ] {
            let (_,_,error,calls)=parse_frames(&[
                r#"data: {"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"a","call_id":"call_a","name":"read"}}"#,
                delta,
            ]);
            assert!(error.unwrap().contains("unambiguous"));
            assert_eq!(calls[0]["arguments"],"");
        }
    }

    fn parse_frames(frames: &[&str]) -> (String, Option<Value>, Option<String>, Vec<Value>) {
        let mut out = String::new();
        let mut reasoning = String::new();
        let mut final_response = None;
        let mut calls = Vec::new();
        let mut stream_error = None;
        for f in frames {
            OpenAICodexProvider::handle_sse_frame(
                f,
                None,
                &mut out,
                &mut reasoning,
                &mut final_response,
                &mut calls,
                &mut stream_error,
            );
        }
        (out, final_response, stream_error, calls)
    }

    #[test]
    fn response_failed_event_becomes_stream_error() {
        let (_, final_response, stream_error, _) = parse_frames(&[
            r#"data: {"type":"response.failed","response":{"status":"failed","error":{"code":"previous_response_not_found","message":"Previous response not found"}}}"#,
        ]);
        assert!(final_response.is_none());
        assert_eq!(stream_error.as_deref(), Some("Previous response not found"));
    }

    #[test]
    fn bare_error_event_becomes_stream_error() {
        let (_, _, stream_error, _) = parse_frames(&[
            r#"data: {"type":"error","code":"rate_limit_exceeded","message":"Rate limit reached"}"#,
        ]);
        assert_eq!(stream_error.as_deref(), Some("Rate limit reached"));
    }

    #[test]
    fn incomplete_response_is_kept_as_final() {
        let (_, final_response, stream_error, _) = parse_frames(&[
            r#"data: {"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"usage":{"input_tokens":10,"output_tokens":5}}}"#,
        ]);
        assert!(stream_error.is_none());
        assert_eq!(
            final_response.unwrap()["incomplete_details"]["reason"],
            "max_output_tokens"
        );
    }

    #[test]
    fn assemble_errors_on_stream_error_instead_of_fake_success() {
        let err = OpenAICodexProvider::assemble_stream_response(
            "gpt-5.4",
            String::new(),
            String::new(),
            None,
            Vec::new(),
            Some("Previous response not found".into()),
            1024,
        )
        .unwrap_err();
        assert!(err.to_string().contains("Previous response not found"));
    }

    #[test]
    fn assemble_errors_when_stream_ends_with_nothing() {
        let err = OpenAICodexProvider::assemble_stream_response(
            "gpt-5.4",
            String::new(),
            String::new(),
            None,
            Vec::new(),
            None,
            435_000,
        )
        .unwrap_err();
        assert!(err.to_string().contains("without a completed response"));
    }

    #[test]
    fn assemble_keeps_text_even_without_completed_event() {
        let response = OpenAICodexProvider::assemble_stream_response(
            "gpt-5.4",
            "partial answer".into(),
            String::new(),
            None,
            Vec::new(),
            None,
            2048,
        )
        .unwrap();
        assert_eq!(response["output_text"], "partial answer");
        assert_eq!(response["status"], "completed");
    }

    #[test]
    fn cache_usage_preserves_explicit_zero_and_rejects_overflow() {
        for details in ["input_tokens_details", "prompt_tokens_details"] {
            for (value, expected) in [
                (serde_json::json!(0), Some(0)),
                (serde_json::json!(64), Some(64)),
                (serde_json::json!(128), Some(128)),
                (serde_json::json!(129), None),
                (serde_json::json!(4_294_967_360u64), None),
                (serde_json::json!(-1), None),
                (serde_json::json!("0"), None),
                (serde_json::Value::Null, None),
            ] {
                let mut response = serde_json::json!({"usage":{"input_tokens":128,"output_tokens":9}});
                response["usage"][details] = serde_json::json!({"cached_tokens":value});
                let usage = OpenAICodexProvider::usage(&response);
                assert_eq!(usage.cache_read_tokens, expected);
                assert_eq!(usage.input_tokens, 128);
                assert_eq!(usage.output_tokens, 9);
            }
        }
        let missing = OpenAICodexProvider::usage(&serde_json::json!({"usage":{"input_tokens":128,"output_tokens":9}}));
        assert_eq!(missing.cache_read_tokens, None, "unknown is not a cache miss");
    }

    #[test]
    fn native_compaction_is_explicit_and_route_scoped() {
        let public = OpenAICodexProvider::for_openai_api(
            PUBLIC_OPENAI_RESPONSES_BASE_URL.to_string(),
            "key".to_string(),
            Duration::from_secs(1),
            true,
        );
        assert!(public
            .native_compaction_capability("gpt-5.5")
            .is_supported());
        // Catalog-driven gating covers newer Pro/Codex ids without a hand
        // maintained short list. Non-Responses lanes remain fail-closed.
        assert!(public
            .native_compaction_capability("gpt-5.4-pro")
            .is_supported());
        assert!(public.native_compaction_capability("gpt-4o").is_supported());
        assert_eq!(
            public.native_compaction_capability("gpt-realtime-2.1"),
            NativeCompactionCapability::Unsupported
        );

        let private = OpenAICodexProvider::with_url_and_timeout_and_native(
            CHATGPT_CODEX_BASE_URL.to_string(),
            "oauth".to_string(),
            Duration::from_secs(1),
            true,
        );
        assert!(private
            .native_compaction_capability("gpt-5.2-codex")
            .is_supported());

        let arbitrary = OpenAICodexProvider::with_url_and_timeout_and_native(
            "https://proxy.example/v1".to_string(),
            "key".to_string(),
            Duration::from_secs(1),
            true,
        );
        assert_eq!(
            arbitrary.native_compaction_capability("gpt-5.5"),
            NativeCompactionCapability::Unsupported
        );
        assert_eq!(
            OpenAICodexProvider::with_url_and_timeout(
                CHATGPT_CODEX_BASE_URL.to_string(),
                "oauth".to_string(),
                Duration::from_secs(1),
            )
            .native_compaction_capability("gpt-5.5"),
            NativeCompactionCapability::Unsupported
        );
    }
}
