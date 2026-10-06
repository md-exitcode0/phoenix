//! xAI Grok via the SuperGrok CLI proxy — the lane that makes a $30 SuperGrok
//! subscription actually drive Phoenix.
//!
//! `api.x.ai/v1` (the console pay-per-token API) tier-gates subscription OAuth
//! tokens: every call 403s `permission-denied`. Grok Build never touches it —
//! it calls `cli-chat-proxy.grok.com`, the subscriber proxy, with the SAME
//! OIDC token Phoenix's xAI OAuth already obtains (identical client id
//! `b1a00492…`, `grok-cli:access` scope). The endpoint is proven live with the
//! user's token; the only reason Phoenix used to 403 was wrong host + missing
//! headers.
//!
//! The wire, verbatim from the grok CLI's own embedded docs + a live 200:
//!   POST https://cli-chat-proxy.grok.com/v1/chat/completions   (standard
//!   OpenAI `messages`, SSE streaming only), with four headers —
//!     Authorization: Bearer <oidc-token>
//!     X-XAI-Token-Auth: xai-grok-cli          (validate as a CLI session)
//!     x-grok-model-override: <model>          (routes to the inference cluster)
//!     x-grok-client-version: <ver>            (server rejects "none" with 426)
//! The proxy's inference clusters are streaming-only, so every call streams and
//! we assemble the chunks (donor shape: OpenAI chat.completion.chunk).

use async_trait::async_trait;
use reqwest::Client;
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use super::contracts::{
    AuthType, CompletionRequest, CompletionResponse, LLMProvider, ModelInfo, StreamingResponse,
};
use super::{openai_compat, providers_data};

pub const PROXY_BASE_URL: &str = "https://cli-chat-proxy.grok.com/v1";
const PROXY_CHAT_URL: &str = "https://cli-chat-proxy.grok.com/v1/chat/completions";
const MAX_GROK_SSE_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Minimum client version the proxy accepts (it 426s `version (none)`). Kept a
/// hair ahead of the documented floor (0.1.202); the live grok CLI on this box
/// sends 0.2.93. If xAI tightens the floor again, bump this one constant.
const GROK_CLI_VERSION: &str = "0.2.93";

/// Marks only failures for which replaying the same request can legitimately
/// succeed: the response body transport stalled or broke before completion.
/// Parser/schema/provider errors intentionally use ordinary `anyhow::Error`
/// and therefore never enter the retry branch.
#[derive(Debug)]
struct TransientGrokStreamError(String);

impl fmt::Display for TransientGrokStreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for TransientGrokStreamError {}

fn transient_stream_error(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(TransientGrokStreamError(message.into()))
}

fn is_transient_stream_error(error: &anyhow::Error) -> bool {
    error.downcast_ref::<TransientGrokStreamError>().is_some()
}

fn is_transient_reqwest_error(error: &reqwest::Error) -> bool {
    error.is_connect()
        || error.is_timeout()
        || error.is_body()
        || (error.is_request() && !error.is_builder() && !error.is_redirect() && !error.is_decode())
}

fn is_retryable_http_status(status: reqwest::StatusCode) -> bool {
    status.is_server_error()
}

fn is_valid_terminal_finish_reason(reason: &str) -> bool {
    matches!(
        reason,
        "stop" | "length" | "tool_calls" | "function_call" | "content_filter"
    )
}

fn canonical_chat_url(base_url: &str) -> anyhow::Result<&'static str> {
    anyhow::ensure!(
        base_url == PROXY_BASE_URL,
        "Grok CLI credentials may only be sent to the canonical proxy endpoint {PROXY_BASE_URL}"
    );
    Ok(PROXY_CHAT_URL)
}

fn stream_error_preview(error: &Value) -> String {
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| error.as_str())
        .unwrap_or("provider returned a structured error frame");
    let message = super::bounded_error_preview(message);
    match error.get("code").and_then(Value::as_str) {
        Some(code) => {
            let combined = format!("{} (code: {})", message, super::bounded_error_preview(code));
            super::bounded_error_preview(&combined)
        }
        None => message,
    }
}

fn optional_string_field<'a>(
    value: &'a Value,
    field: &str,
    context: &str,
) -> anyhow::Result<Option<&'a str>> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => anyhow::bail!("Grok SSE {context} field `{field}` was not a string or null"),
    }
}

pub struct GrokCliProvider {
    client: Client,
    access_token: String,
    base_url: String,
}

impl GrokCliProvider {
    pub fn with_timeout(access_token: String, timeout: Duration) -> Self {
        Self::with_url_and_timeout(PROXY_BASE_URL.to_string(), access_token, timeout)
    }

    pub fn with_url_and_timeout(base_url: String, access_token: String, timeout: Duration) -> Self {
        // Idle-read timeout, not a total cap: a long reasoning generation streams
        // for minutes and a total `.timeout()` would kill the body mid-stream.
        let client = super::apply_read_timeout(
            Client::builder().connect_timeout(Duration::from_secs(30)),
            timeout,
        )
            // Never replay the bearer or Grok-specific headers to a redirect
            // target; redirects surface as deterministic 3xx provider errors.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("Grok CLI client build failed");
        Self {
            client,
            access_token,
            base_url,
        }
    }

    fn has_model_impl(&self, model: &str) -> bool {
        providers_data::get_provider("grok-cli")
            .map(|p| p.has_model(model))
            .unwrap_or(false)
    }

    fn build_body(&self, request: &CompletionRequest) -> Value {
        let messages: Vec<Value> = request
            .messages
            .iter()
            .map(openai_compat::serialize_message)
            .collect();
        // Streaming is mandatory — the proxy's inference clusters do not serve
        // non-streaming completions.
        let mut body = serde_json::json!({
            "model": request.model,
            "messages": messages,
            "stream": true,
            // OpenAI-compatible streaming servers omit usage unless the
            // caller explicitly asks for the terminal usage frame. Without
            // this the live Grok lane works but every burn/bench receipt lies
            // with `0 tokens`, making quota and volume planning impossible.
            "stream_options": {"include_usage": true},
        });
        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        // Native tool-calling is how Phoenix gets structured output uniformly;
        // `auto` (the shared default) lets the model finish via `final_answer`.
        openai_compat::attach_tools(&mut body, &request.tools);
        // Pass through caller extras, minus the runtime-only hints a
        // chat/completions server 400s on (Responses-API `reasoning`, JSON
        // `response_format` — Phoenix uses tool-calling for structure).
        for (key, value) in &request.extra_body {
            if matches!(
                key.as_str(),
                "response_format" | "reasoning" | "stream" | "stream_options"
            ) {
                continue;
            }
            body[key] = value.clone();
        }
        body
    }

    async fn make_request(&self, request: &CompletionRequest) -> anyhow::Result<Value> {
        // Validate before constructing or sending a request carrying either the
        // bearer or Grok's privileged CLI headers. This intentionally rejects
        // custom endpoints and URL lookalikes rather than following redirects.
        let chat_url = canonical_chat_url(&self.base_url)?;
        let body = self.build_body(request);
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..2 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            let resp = match self
                .client
                .post(chat_url)
                .bearer_auth(&self.access_token)
                .header("Content-Type", "application/json")
                // The three headers the proxy requires beyond the bearer.
                .header("X-XAI-Token-Auth", "xai-grok-cli")
                .header("x-grok-model-override", request.model.as_str())
                .header("x-grok-client-version", GROK_CLI_VERSION)
                .json(&body)
                .send()
                .await
            {
                Ok(resp) => resp,
                Err(error) if is_transient_reqwest_error(&error) => {
                    last_err = Some(error.into());
                    continue; // connect/request timeout or transport blip
                }
                Err(error) => return Err(error.into()),
            };

            let status = resp.status();
            if !status.is_success() {
                let err_text =
                    match super::read_error_response(resp, "Grok CLI proxy error response").await {
                        Ok(text) => text,
                        Err(error) if super::is_response_body_limit_error(&error) => {
                            return Err(error);
                        }
                        Err(error) if is_retryable_http_status(status) => {
                            last_err = Some(error);
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                let err_preview = super::bounded_error_preview(&err_text);
                if status.as_u16() == 429 {
                    anyhow::bail!("Grok CLI proxy rate limit (429): {err_preview}");
                }
                // A 426 here means the client-version floor moved — surface it
                // actionably instead of as a raw upgrade nag.
                if status.as_u16() == 426 {
                    anyhow::bail!(
                        "Grok CLI proxy rejected the client version (426): xAI raised the \
                         minimum — bump GROK_CLI_VERSION in grok_cli.rs. Raw: {err_preview}"
                    );
                }
                let error = anyhow::anyhow!("Grok CLI proxy error ({status}): {err_preview}");
                if is_retryable_http_status(status) {
                    last_err = Some(error);
                    continue;
                }
                return Err(error); // deterministic redirect/client status
            }

            match self
                .read_stream_observed(resp, &request.model, request.session_id.as_deref(), MAX_GROK_SSE_RESPONSE_BYTES, request.stream_observer.as_ref())
                .await
            {
                Ok(value) => return Ok(value),
                Err(error) => {
                    if super::is_response_body_limit_error(&error) {
                        return Err(error);
                    }
                    if is_transient_stream_error(&error) {
                        last_err = Some(error);
                        continue;
                    }
                    return Err(error);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Grok CLI proxy request failed")))
    }

    /// Read the SSE body and assemble it into ONE OpenAI-shaped chat response
    /// object, so the shared `openai_compat` parsers handle the rest.
    async fn read_stream(
        &self,
        resp: reqwest::Response,
        requested_model: &str,
        session_id: Option<&str>,
    ) -> anyhow::Result<Value> {
        self.read_stream_with_limit(
            resp,
            requested_model,
            session_id,
            MAX_GROK_SSE_RESPONSE_BYTES,
        )
        .await
    }

    async fn read_stream_with_limit(
        &self,
        resp: reqwest::Response,
        requested_model: &str,
        session_id: Option<&str>,
        max_stream_bytes: usize,
    ) -> anyhow::Result<Value> {
        self.read_stream_observed(resp, requested_model, session_id, max_stream_bytes, None).await
    }

    async fn read_stream_observed(
        &self,
        mut resp: reqwest::Response,
        requested_model: &str,
        session_id: Option<&str>,
        max_stream_bytes: usize,
        observer: Option<&super::contracts::StreamObserver>,
    ) -> anyhow::Result<Value> {
        const HEARTBEAT_EVERY: Duration = Duration::from_secs(15);
        const LABEL: &str = "Grok SSE response";
        super::ensure_response_content_length(&resp, max_stream_bytes, LABEL)?;
        let started = std::time::Instant::now();
        let mut last_beat = started;
        let mut raw: Vec<u8> = Vec::new();
        loop {
            let chunk = resp.chunk().await.map_err(|error| {
                let message = format!(
                    "Grok stream body read failed after {} KB: {}",
                    raw.len() / 1024,
                    super::bounded_error_preview(&error.to_string())
                );
                if is_transient_reqwest_error(&error) {
                    transient_stream_error(message)
                } else {
                    anyhow::anyhow!(message)
                }
            })?;
            let Some(chunk) = chunk else {
                break;
            };
            super::extend_bounded_response_body(&mut raw, &chunk, max_stream_bytes, LABEL)?;
            if last_beat.elapsed() >= HEARTBEAT_EVERY {
                super::stream_progress_observed(
                    session_id,
                    observer,
                    &format!(
                        "  grok streaming… {} KB in {}s",
                        raw.len() / 1024,
                        started.elapsed().as_secs()
                    ),
                );
                last_beat = std::time::Instant::now();
            }
        }
        let body = decode_sse_body(raw)?;
        assemble_chat_stream(&body, requested_model)
    }
}

fn decode_sse_body(raw: Vec<u8>) -> anyhow::Result<String> {
    String::from_utf8(raw)
        .map(|body| body.replace("\r\n", "\n"))
        .map_err(|error| anyhow::anyhow!("Grok SSE response was not valid UTF-8: {error}"))
}

#[derive(Debug, Eq, PartialEq)]
struct SseDataEvent {
    index: usize,
    data: String,
}

struct SseDataEvents<'a> {
    lines: std::str::Split<'a, char>,
    event_data: String,
    has_data: bool,
    next_index: usize,
    finished: bool,
}

/// Parse the SSE framing before interpreting provider JSON. Per the SSE wire
/// format, repeated `data:` fields belong to one event and are joined with a
/// newline; a blank line dispatches the event. A final unterminated event is
/// retained so the completion-level truncation checks can diagnose it. Events
/// are yielded lazily to avoid amplifying a bounded response into a large event
/// vector when an adversarial stream contains millions of tiny frames.
fn sse_data_events(body: &str) -> SseDataEvents<'_> {
    SseDataEvents {
        lines: body.split('\n'),
        event_data: String::new(),
        has_data: false,
        next_index: 1,
        finished: false,
    }
}

impl<'a> Iterator for SseDataEvents<'a> {
    type Item = SseDataEvent;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        loop {
            let Some(raw_line) = self.lines.next() else {
                self.finished = true;
                if self.has_data {
                    self.has_data = false;
                    let event = SseDataEvent {
                        index: self.next_index,
                        data: std::mem::take(&mut self.event_data),
                    };
                    self.next_index += 1;
                    return Some(event);
                }
                return None;
            };
            // `decode_sse_body` normalizes CRLF, while stripping here also
            // keeps direct parser tests and callers correct for CRLF input.
            let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
            if line.is_empty() {
                if self.has_data {
                    self.has_data = false;
                    let event = SseDataEvent {
                        index: self.next_index,
                        data: std::mem::take(&mut self.event_data),
                    };
                    self.next_index += 1;
                    return Some(event);
                }
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = line.split_once(':').unwrap_or((line, ""));
            if field != "data" {
                continue;
            }
            let value = value.strip_prefix(' ').unwrap_or(value);
            if self.has_data {
                self.event_data.push('\n');
            }
            self.event_data.push_str(value);
            self.has_data = true;
        }
    }
}

/// Fold an OpenAI `chat.completion.chunk` SSE stream into a single non-streamed
/// `chat.completion`-shaped object (content, reasoning, tool calls, usage,
/// finish reason) that `openai_compat::parse_completion_response` understands.
fn assemble_chat_stream(body: &str, fallback_model: &str) -> anyhow::Result<Value> {
    const MAX_STREAM_TOOL_CALLS: usize = 1024;
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut finish_reason: Option<String> = None;
    let mut model = fallback_model.to_string();
    let mut usage: Option<Value> = None;
    let mut saw_json_event = false;
    let mut saw_choice = false;
    let mut saw_done = false;
    // Tool calls arrive split across chunks by `index`: id+name land once, the
    // arguments string streams in fragments that must be concatenated in order.
    let mut tool_acc: Vec<ToolAcc> = Vec::new();

    for event in sse_data_events(body) {
        let payload = event.data.trim();
        if saw_done {
            anyhow::bail!(
                "Grok SSE contained data after terminal [DONE] at event {}",
                event.index
            );
        }
        if payload.is_empty() {
            continue;
        }
        if payload == "[DONE]" {
            saw_done = true;
            continue;
        }
        let chunk = serde_json::from_str::<Value>(payload).map_err(|error| {
            anyhow::anyhow!(
                "Grok SSE event {} contained malformed JSON ({} bytes): {error}",
                event.index,
                event.data.len()
            )
        })?;
        if !chunk.is_object() {
            anyhow::bail!("Grok SSE data event {} was not an object", event.index);
        }
        saw_json_event = true;
        if let Some(err) = chunk.get("error") {
            anyhow::bail!(
                "Grok stream reported an error: {}",
                stream_error_preview(err)
            );
        }
        if let Some(m) = optional_string_field(&chunk, "model", "event")? {
            model = m.to_string();
        }
        if let Some(event_usage) = chunk.get("usage").filter(|value| !value.is_null()) {
            if !event_usage.is_object() {
                anyhow::bail!("Grok SSE event field `usage` was not an object or null");
            }
            usage = Some(event_usage.clone());
        }
        let choices = match chunk.get("choices") {
            None => continue,
            Some(Value::Array(choices)) => choices,
            Some(_) => anyhow::bail!("Grok SSE event field `choices` was not an array"),
        };
        let Some(choice) = choices.first() else {
            continue;
        };
        if !choice.is_object() {
            anyhow::bail!("Grok SSE completion choice was not an object");
        }
        saw_choice = true;
        if let Some(fr) = optional_string_field(choice, "finish_reason", "choice")? {
            if !is_valid_terminal_finish_reason(fr) {
                anyhow::bail!(
                    "Grok stream returned unknown finish reason `{}`",
                    super::bounded_error_preview(fr)
                );
            }
            finish_reason = Some(fr.to_string());
        }
        let delta = match choice.get("delta") {
            None | Some(Value::Null) => continue,
            Some(delta @ Value::Object(_)) => delta,
            Some(_) => anyhow::bail!("Grok SSE choice field `delta` was not an object or null"),
        };
        if let Some(c) = optional_string_field(delta, "content", "delta")? {
            content.push_str(c);
        }
        // The proxy streams chain-of-thought as `reasoning_content` (seen live).
        if let Some(r) = optional_string_field(delta, "reasoning_content", "delta")? {
            reasoning.push_str(r);
        } else if let Some(r) = optional_string_field(delta, "reasoning", "delta")? {
            reasoning.push_str(r);
        }
        let calls = match delta.get("tool_calls") {
            None | Some(Value::Null) => None,
            Some(Value::Array(calls)) => Some(calls),
            Some(_) => anyhow::bail!("Grok SSE delta field `tool_calls` was not an array or null"),
        };
        if let Some(calls) = calls {
            for call in calls {
                if !call.is_object() {
                    anyhow::bail!("Grok stream tool-call delta was not an object");
                }
                let wire_index = call["index"].as_u64().ok_or_else(|| {
                    anyhow::anyhow!(
                        "Grok stream tool-call index was missing or not an unsigned integer"
                    )
                })?;
                let idx = usize::try_from(wire_index)
                    .map_err(|_| anyhow::anyhow!("Grok stream tool-call index is out of range"))?;
                if idx >= MAX_STREAM_TOOL_CALLS {
                    anyhow::bail!(
                        "Grok stream tool-call index {idx} exceeds the {MAX_STREAM_TOOL_CALLS} call limit"
                    );
                }
                while tool_acc.len() <= idx {
                    tool_acc.push(ToolAcc::default());
                }
                let acc = &mut tool_acc[idx];
                acc.observed = true;
                if let Some(id) = optional_string_field(call, "id", "tool call")?
                    .filter(|value| !value.is_empty())
                {
                    acc.id = id.to_string();
                }
                let function = match call.get("function") {
                    None | Some(Value::Null) => None,
                    Some(function @ Value::Object(_)) => Some(function),
                    Some(_) => anyhow::bail!(
                        "Grok stream tool-call field `function` was not an object or null"
                    ),
                };
                if let Some(function) = function {
                    if let Some(name) = optional_string_field(function, "name", "function")?
                        .filter(|value| !value.is_empty())
                    {
                        acc.push_name(name);
                    }
                    if let Some(args) = optional_string_field(function, "arguments", "function")? {
                        acc.arguments.push_str(args);
                    }
                }
            }
        }
    }

    if !saw_json_event {
        anyhow::bail!("Grok SSE response contained no JSON events");
    }
    if !saw_choice {
        anyhow::bail!("Grok SSE response contained no completion choice");
    }
    if !saw_done && finish_reason.is_none() {
        anyhow::bail!(
            "Grok SSE response ended before [DONE] or a terminal finish reason — stream was truncated"
        );
    }

    let mut populated_tool_calls = 0usize;
    for (index, tool) in tool_acc.iter().enumerate() {
        if !tool.is_populated() {
            continue;
        }
        populated_tool_calls += 1;
        if tool.name.trim().is_empty() {
            anyhow::bail!("Grok stream tool call {index} had arguments but no function name");
        }
        if tool.arguments.trim().is_empty() {
            anyhow::bail!("Grok stream tool call {index} ended without function arguments");
        }
        let arguments: Value = serde_json::from_str(&tool.arguments).map_err(|error| {
            anyhow::anyhow!(
                "Grok stream tool call {index} ended with invalid JSON arguments ({} bytes): {error}",
                tool.arguments.len()
            )
        })?;
        if !arguments.is_object() {
            anyhow::bail!("Grok stream tool call {index} arguments were not a JSON object");
        }
    }
    if matches!(
        finish_reason.as_deref(),
        Some("tool_calls" | "function_call")
    ) && populated_tool_calls == 0
    {
        anyhow::bail!(
            "Grok stream ended with a function-call finish reason but no complete tool call"
        );
    }

    // Rebuild the non-streamed message shape the shared parser reads.
    let message_tool_calls: Vec<Value> = tool_acc
        .iter()
        .filter(|t| !t.name.is_empty())
        .enumerate()
        .map(|(i, t)| {
            serde_json::json!({
                "id": if t.id.is_empty() { format!("call_{}_{i}", t.name) } else { t.id.clone() },
                "type": "function",
                "function": { "name": t.name, "arguments": t.arguments },
            })
        })
        .collect();

    let mut message = serde_json::json!({ "role": "assistant" });
    message["content"] = if content.is_empty() {
        Value::Null
    } else {
        Value::String(content)
    };
    if !reasoning.trim().is_empty() {
        message["reasoning_content"] = Value::String(reasoning);
    }
    if !message_tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(message_tool_calls);
    }

    let mut out = serde_json::json!({
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason,
        }],
    });
    if let Some(u) = usage {
        out["usage"] = u;
    }
    Ok(out)
}

#[derive(Default)]
struct ToolAcc {
    observed: bool,
    id: String,
    name: String,
    arguments: String,
}

impl ToolAcc {
    fn push_name(&mut self, fragment: &str) {
        // Some OpenAI-compatible streams resend the full name while others split
        // it across deltas. Fold both forms without duplicating either fragment.
        if fragment == self.name {
            return;
        }
        if fragment.starts_with(&self.name) {
            self.name.clear();
            self.name.push_str(fragment);
        } else {
            self.name.push_str(fragment);
        }
    }

    fn is_populated(&self) -> bool {
        self.observed || !self.id.is_empty() || !self.name.is_empty() || !self.arguments.is_empty()
    }
}

#[async_trait]
impl LLMProvider for GrokCliProvider {
    fn supports_native_images(&self) -> bool {
        true
    }
    fn name(&self) -> &str {
        "grok-cli"
    }
    fn display_name(&self) -> &str {
        "xAI Grok (SuperGrok)"
    }
    fn base_url(&self) -> &str {
        &self.base_url
    }
    fn auth_type(&self) -> AuthType {
        AuthType::Bearer
    }
    fn env_vars(&self) -> Vec<&str> {
        vec!["XAI_OAUTH_TOKEN"]
    }
    fn default_headers(&self) -> HashMap<String, String> {
        HashMap::new()
    }
    fn has_model(&self, model: &str) -> bool {
        self.has_model_impl(model)
    }
    fn default_model(&self) -> &str {
        "grok-4.5"
    }
    fn fallback_models(&self) -> Vec<&str> {
        vec!["grok-composer-2.5-fast"]
    }

    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        let json = self.make_request(&request).await?;
        Ok(openai_compat::parse_completion_response(
            &json,
            &request.model,
        ))
    }

    async fn stream(&self, request: CompletionRequest) -> anyhow::Result<StreamingResponse> {
        // The transport already streams under the hood; surface the assembled
        // text (same contract the OpenAI provider's stream() gives).
        let json = self.make_request(&request).await?;
        let parsed = openai_compat::parse_completion_response(&json, &request.model);
        Ok(StreamingResponse {
            content: parsed.content,
            reasoning: parsed.reasoning,
            done: true,
        })
    }

    async fn embeddings(&self, _texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
        anyhow::bail!("grok-cli proxy does not serve embeddings")
    }

    async fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        let provider_data = providers_data::get_provider("grok-cli")
            .ok_or_else(|| anyhow::anyhow!("grok-cli provider data not found"))?;
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
                supports_vision: Some(false),
            })
            .collect())
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        let request = CompletionRequest::new(
            self.default_model(),
            vec![super::contracts::ChatMessage::user("Reply with: ok")],
        );
        Ok(self.complete(request).await.is_ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

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

    #[test]
    fn grok_stream_requests_terminal_usage_receipts() {
        let provider = GrokCliProvider::with_timeout("token".to_string(), Duration::from_secs(1));
        let mut request = CompletionRequest::new(
            "grok-4.5",
            vec![super::super::contracts::ChatMessage::user("hi")],
        );
        request.extra_body.insert("stream".into(), false.into());
        request.extra_body.insert(
            "stream_options".into(),
            serde_json::json!({"include_usage": false}),
        );
        let body = provider.build_body(&request);
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
    }

    #[test]
    fn grok_credentials_are_gated_to_the_exact_canonical_proxy_path() {
        assert_eq!(canonical_chat_url(PROXY_BASE_URL).unwrap(), PROXY_CHAT_URL);

        for lookalike in [
            "http://cli-chat-proxy.grok.com/v1",
            "https://cli-chat-proxy.grok.com/v1/",
            "https://cli-chat-proxy.grok.com:443/v1",
            "https://CLI-CHAT-PROXY.GROK.COM/v1",
            "https://cli-chat-proxy.grok.com/v1?next=https://evil.test",
            "https://cli-chat-proxy.grok.com/v1#evil",
            "https://cli-chat-proxy.grok.com/v1/../evil",
            "https://cli-chat-proxy.grok.com.evil.test/v1",
            "https://cli-chat-proxy.grok.com@evil.test/v1",
            "https://evil.test/https://cli-chat-proxy.grok.com/v1",
        ] {
            let error = canonical_chat_url(lookalike).unwrap_err();
            assert!(error.to_string().contains(PROXY_BASE_URL));
            assert!(
                !error.to_string().contains(lookalike),
                "the rejected URL must not be reflected into diagnostics"
            );
        }
    }

    #[tokio::test]
    async fn sse_reader_fails_when_chunked_body_crosses_limit() {
        let response = local_response(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n8\r\ndata: {}\r\n0\r\n\r\n"
                .to_vec(),
        )
        .await;
        let provider = GrokCliProvider::with_url_and_timeout(
            "http://127.0.0.1".to_string(),
            "token".to_string(),
            Duration::from_secs(1),
        );
        let error = provider
            .read_stream_with_limit(response, "model", None, 7)
            .await
            .unwrap_err();
        assert!(super::super::is_response_body_limit_error(&error));
        assert!(!is_transient_stream_error(&error));
    }

    #[test]
    fn assembles_content_and_reasoning_from_chunks() {
        let sse = "\
data: {\"model\":\"grok-4.5-build\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"Think\"}}]}\n\
\n\
data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"PO\"}}]}\n\
\n\
data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"NG\"},\"finish_reason\":\"stop\"}]}\n\
\n\
data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5}}\n\
\n\
data: [DONE]\n";
        let obj = assemble_chat_stream(sse, "grok-4.5").unwrap();
        let parsed = openai_compat::parse_completion_response(&obj, "grok-4.5");
        assert_eq!(parsed.content, "PONG");
        assert_eq!(parsed.reasoning.as_deref(), Some("Think"));
        assert_eq!(parsed.model, "grok-4.5-build");
        assert_eq!(parsed.stop_reason.as_deref(), Some("stop"));
        assert_eq!(parsed.usage.input_tokens, 3);
        assert_eq!(parsed.usage.output_tokens, 2);
        assert_eq!(obj["usage"]["total_tokens"], 5);
    }

    #[test]
    fn sse_events_join_data_lines_support_crlf_and_reject_data_after_done() {
        let sse = concat!(
            "event: message\r\n",
            "data: {\"model\":\"grok-4.5-build\",\"choices\":[\r\n",
            "data: {\"index\":0,\"delta\":{\"content\":\"PONG\"},\"finish_reason\":\"stop\"}\r\n",
            "data: ]}\r\n",
            "\r\n",
            ": heartbeat\r\n",
            "\r\n",
            "data: [DONE]\r\n",
            "\r\n",
        );
        let events: Vec<_> = sse_data_events(sse).collect();
        assert_eq!(events.len(), 2);
        assert!(events[0].data.contains("\n"));
        assert_eq!(events[1].data, "[DONE]");
        let object = assemble_chat_stream(sse, "grok-4.5").unwrap();
        assert_eq!(object["choices"][0]["message"]["content"], "PONG");

        let late = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"PONG\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
            "data: {\"choices\":[]}\n\n",
        );
        let error = assemble_chat_stream(late, "grok-4.5").unwrap_err();
        assert!(error.to_string().contains("after terminal [DONE]"));
        assert!(error.to_string().contains("event 3"));
        assert!(!is_transient_stream_error(&error));
    }

    #[test]
    fn assembles_tool_call_split_across_chunks() {
        // id+name in the first chunk, arguments streamed in fragments.
        let sse = "\
data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read_\",\"arguments\":\"{\\\"path\\\":\"}}]}}]}\n\
\n\
data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"file\",\"arguments\":\"\\\"a.rs\\\"}\"}}]}}]}\n\
\n\
data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\
\n\
data: [DONE]\n";
        let obj = assemble_chat_stream(sse, "grok-4.5").unwrap();
        let calls = openai_compat::parse_tool_calls(&obj);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool_name, "read_file");
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].arguments["path"], "a.rs");
    }

    #[test]
    fn stream_error_frame_surfaces_as_err() {
        let sse = "data: {\"error\":{\"message\":\"boom\"}}\n";
        let error = assemble_chat_stream(sse, "grok-4.5").unwrap_err();
        assert!(error.to_string().contains("boom"));

        let large = format!(
            "data: {{\"error\":{{\"message\":\"{}\"}}}}\n",
            "x".repeat(20_000)
        );
        let error = assemble_chat_stream(&large, "grok-4.5").unwrap_err();
        assert!(
            error.to_string().len() < 9_000,
            "provider error-frame preview must remain bounded"
        );
    }

    #[test]
    fn malformed_empty_and_non_utf8_streams_fail_closed() {
        for empty in ["", ": keepalive\n", "data: [DONE]\n"] {
            let error = assemble_chat_stream(empty, "grok-4.5").unwrap_err();
            assert!(
                error.to_string().contains("no JSON events"),
                "unexpected empty-stream error: {error:#}"
            );
        }

        let secret = "MALFORMED_PAYLOAD_MUST_NOT_BE_ECHOED";
        let malformed = format!("data: {{not-json {secret}\n\n");
        let error = assemble_chat_stream(&malformed, "grok-4.5").unwrap_err();
        assert!(error.to_string().contains("malformed JSON"));
        assert!(error.to_string().contains("bytes"));
        assert!(!error.to_string().contains(secret));

        let error = decode_sse_body(vec![b'd', b'a', b't', b'a', b':', b' ', 0xff]).unwrap_err();
        assert!(error.to_string().contains("not valid UTF-8"));
        assert!(!is_transient_stream_error(&error));
    }

    #[test]
    fn malformed_stream_schema_fails_closed_without_becoming_retryable() {
        let fixtures = [
            "data: []\n\ndata: [DONE]\n",
            r#"data: {"choices":{}}

data: [DONE]
"#,
            r#"data: {"choices":[[]]}

data: [DONE]
"#,
            r#"data: {"choices":[{"delta":{},"finish_reason":7}]}

data: [DONE]
"#,
            r#"data: {"choices":[{"delta":{"content":7},"finish_reason":"stop"}]}

data: [DONE]
"#,
            r#"data: {"choices":[{"delta":{"tool_calls":{}},"finish_reason":"tool_calls"}]}

data: [DONE]
"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":"0","function":{"name":"read_file","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}

data: [DONE]
"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":[]}]},"finish_reason":"tool_calls"}]}

data: [DONE]
"#,
        ];
        for fixture in fixtures {
            let error = assemble_chat_stream(fixture, "grok-4.5").unwrap_err();
            assert!(!is_transient_stream_error(&error));
        }
    }

    #[test]
    fn stream_requires_done_or_a_real_terminal_finish_reason() {
        let truncated = r#"data: {"choices":[{"index":0,"delta":{"content":"partial"}}]}
"#;
        let error = assemble_chat_stream(truncated, "grok-4.5").unwrap_err();
        assert!(error.to_string().contains("stream was truncated"));

        let terminal_without_done = r#"data: {"choices":[{"index":0,"delta":{"content":"complete"},"finish_reason":"stop"}]}
"#;
        let object = assemble_chat_stream(terminal_without_done, "grok-4.5").unwrap();
        assert_eq!(object["choices"][0]["finish_reason"], "stop");

        let done_without_finish = r#"data: {"choices":[{"index":0,"delta":{"content":"complete"}}]}

data: [DONE]
"#;
        let object = assemble_chat_stream(done_without_finish, "grok-4.5").unwrap();
        assert!(object["choices"][0]["finish_reason"].is_null());

        let unknown_finish = r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"banana"}]}

data: [DONE]
"#;
        assert!(assemble_chat_stream(unknown_finish, "grok-4.5")
            .unwrap_err()
            .to_string()
            .contains("unknown finish reason"));
    }

    #[test]
    fn incomplete_structured_tool_calls_are_rejected() {
        let fixtures = [
            // Arguments without a function name.
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{}"}}]},"finish_reason":"tool_calls"}]}

data: [DONE]
"#,
            // Name without any arguments.
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"read_file"}}]},"finish_reason":"tool_calls"}]}

data: [DONE]
"#,
            // Truncated argument JSON.
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"read_file","arguments":"{\"path\":"}}]},"finish_reason":"tool_calls"}]}

data: [DONE]
"#,
            // Syntactically valid JSON, but tool arguments must be an object.
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"read_file","arguments":"[]"}}]},"finish_reason":"tool_calls"}]}

data: [DONE]
"#,
            // Terminal call reason with no call payload.
            r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}

data: [DONE]
"#,
            // Merely opening an indexed tool-call slot is still incomplete.
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0}]},"finish_reason":"stop"}]}

data: [DONE]
"#,
        ];
        for fixture in fixtures {
            assert!(
                assemble_chat_stream(fixture, "grok-4.5").is_err(),
                "accepted incomplete tool-call stream: {fixture}"
            );
        }

        let secret = "TOOL_ARGUMENTS_MUST_NOT_BE_ECHOED";
        let malformed_arguments = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"read_file","arguments":"{\"secret\":\"TOOL_ARGUMENTS_MUST_NOT_BE_ECHOED\""}}]},"finish_reason":"tool_calls"}]}

data: [DONE]

"#;
        let error = assemble_chat_stream(malformed_arguments, "grok-4.5").unwrap_err();
        assert!(error.to_string().contains("invalid JSON arguments"));
        assert!(error.to_string().contains("tool call 0"));
        assert!(error.to_string().contains("bytes"));
        assert!(!error.to_string().contains(secret));
    }

    #[test]
    fn retry_policy_accepts_only_transport_markers_and_server_statuses() {
        assert!(is_retryable_http_status(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        ));
        assert!(is_retryable_http_status(
            reqwest::StatusCode::SERVICE_UNAVAILABLE
        ));
        for status in [
            reqwest::StatusCode::BAD_REQUEST,
            reqwest::StatusCode::UNAUTHORIZED,
            reqwest::StatusCode::FORBIDDEN,
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            reqwest::StatusCode::UPGRADE_REQUIRED,
            reqwest::StatusCode::PERMANENT_REDIRECT,
        ] {
            assert!(!is_retryable_http_status(status), "retried {status}");
        }

        let transient = transient_stream_error("body read reset");
        assert!(is_transient_stream_error(&transient));
        let deterministic = anyhow::anyhow!("malformed SSE");
        assert!(!is_transient_stream_error(&deterministic));
    }

    /// Live end-to-end against the real SuperGrok proxy, driven through the full
    /// provider path (body + 4 headers + SSE assembly + parse). Reads the token
    /// the same way Grok Build stores it (`~/.grok/auth.json`), so it proves the
    /// wire without needing Phoenix's own profile store.
    ///   cargo test --lib providers::grok_cli::tests::live -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_completion_and_tool_call_through_the_proxy() {
        use super::super::contracts::{ChatMessage, ToolDefinition};
        let home = std::env::var("HOME").unwrap();
        let raw = std::fs::read_to_string(format!("{home}/.grok/auth.json"))
            .expect("~/.grok/auth.json — run `grok login` first");
        let doc: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let token = doc
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .get("key")
            .and_then(|v| v.as_str())
            .expect("token key")
            .to_string();

        let provider = GrokCliProvider::with_timeout(token, Duration::from_secs(120));

        // 1) Plain completion.
        let req = CompletionRequest::new(
            "grok-4.5",
            vec![ChatMessage::user("Reply with exactly one word: PONG")],
        );
        let resp = provider
            .complete(req)
            .await
            .expect("live completion failed");
        eprintln!(
            "content={:?} model={} reasoning={:?}",
            resp.content,
            resp.model,
            resp.reasoning.is_some()
        );
        assert!(
            resp.content.to_uppercase().contains("PONG"),
            "got: {:?}",
            resp.content
        );

        // 2) Native tool-calling round-trips through the SSE assembler.
        let tool = ToolDefinition {
            name: "get_weather".to_string(),
            description: "Get weather for a city".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"],
            }),
        };
        let mut treq = CompletionRequest::new(
            "grok-4.5",
            vec![ChatMessage::user(
                "Call get_weather for Paris. Use the tool.",
            )],
        );
        treq.tools = vec![tool];
        let tresp = provider
            .complete(treq)
            .await
            .expect("live tool call failed");
        eprintln!("tool_calls={:?}", tresp.tool_calls);
        assert!(!tresp.tool_calls.is_empty(), "model did not call the tool");
        assert_eq!(tresp.tool_calls[0].tool_name, "get_weather");
    }
}
