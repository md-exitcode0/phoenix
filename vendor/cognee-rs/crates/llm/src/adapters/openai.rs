//! OpenAI API adapter with JSON schema support for structured outputs.
//!
//! This adapter uses OpenAI's function calling or JSON mode to generate
//! structured outputs based on JSON schemas derived from Rust types.

use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{debug, instrument, warn};

#[allow(unused_imports)]
use cognee_utils::tracing_keys::{COGNEE_LLM_MODEL, COGNEE_LLM_PROVIDER};

use crate::error::{LlmError, LlmResult};
use crate::llm_trait::Llm;
use crate::transcriber::{Transcriber, TranscriptionOutput, validate_audio_format};
use crate::types::{GenerationOptions, GenerationResponse, Message, MessageRole, TokenUsage};

/// A HARD quota error is not transient. Each resolved adapter owns a breaker
/// shared by its clones, which scopes cooldown to that endpoint/account lane
/// instead of blocking unrelated providers or credentials process-wide.
const QUOTA_COOLDOWN_MS: u64 = 15 * 60 * 1_000;
const MAX_NETWORK_RETRIES: usize = 10;

/// Provider replies are untrusted. These mirror Phoenix's provider ceilings:
/// successful model output gets a generous bound, while diagnostic error
/// bodies are stopped much earlier and truncated again before formatting.
const MAX_LLM_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
const MAX_LLM_ERROR_BYTES: usize = 64 * 1024;
const MAX_ERROR_PREVIEW_BYTES: usize = 8 * 1024;
const MAX_CHAT_SSE_EVENTS: usize = 100_000;
const MAX_CHAT_SSE_TOOL_CALLS: usize = 1_024;

/// The CLI proxy rejects missing/stale versions with HTTP 426. Keep this in
/// sync with the maintained Phoenix Grok provider; documented floor: 0.1.202.
const GROK_CLI_CLIENT_VERSION: &str = "0.2.93";
const GROK_TOKEN_AUTH: &str = "xai-grok-cli";

fn response_size_error(label: &str, limit: usize, advertised: bool) -> LlmError {
    let source = if advertised {
        "advertised Content-Length"
    } else {
        "received body size"
    };
    LlmError::InvalidResponse(format!(
        "{label} exceeded the {limit} byte limit ({source})"
    ))
}

fn ensure_content_length_with_limit(
    length: Option<u64>,
    limit: usize,
    label: &str,
) -> LlmResult<()> {
    if length.is_some_and(|length| length > limit as u64) {
        return Err(response_size_error(label, limit, true));
    }
    Ok(())
}

fn extend_bounded_body(
    body: &mut Vec<u8>,
    chunk: &[u8],
    limit: usize,
    label: &str,
) -> LlmResult<()> {
    let observed = body
        .len()
        .checked_add(chunk.len())
        .ok_or_else(|| response_size_error(label, limit, false))?;
    if observed > limit {
        return Err(response_size_error(label, limit, false));
    }
    body.extend_from_slice(chunk);
    Ok(())
}

async fn read_bounded_response_text(
    mut response: reqwest::Response,
    limit: usize,
    label: &str,
) -> LlmResult<String> {
    ensure_content_length_with_limit(response.content_length(), limit, label)?;
    let initial_capacity = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or(0)
        .min(limit)
        .min(64 * 1024);
    let mut body = Vec::with_capacity(initial_capacity);
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        LlmError::NetworkError(format!(
            "failed while reading {label}: {}",
            bounded_error_preview(&error.to_string())
        ))
    })? {
        extend_bounded_body(&mut body, &chunk, limit, label)?;
    }
    String::from_utf8(body)
        .map_err(|error| LlmError::DeserializationError(format!("{label} was not UTF-8: {error}")))
}

fn bounded_error_preview(body: &str) -> String {
    if body.len() <= MAX_ERROR_PREVIEW_BYTES {
        return body.to_string();
    }
    const SUFFIX: &str = "\n… [provider error body truncated]";
    let mut end = MAX_ERROR_PREVIEW_BYTES.saturating_sub(SUFFIX.len());
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    let mut preview = String::with_capacity(MAX_ERROR_PREVIEW_BYTES);
    preview.push_str(&body[..end]);
    preview.push_str(SUFFIX);
    preview
}

/// Exact endpoint predicate for the privileged SuperGrok subscription wire.
/// Never infer this from a model name or hostname substring: those would leak
/// CLI-only authentication headers to an attacker-controlled compatible host.
fn is_grok_cli_proxy_base_url(base_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(base_url) else {
        return false;
    };
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case("cli-chat-proxy.grok.com"))
        && url.port_or_known_default() == Some(443)
        && url.path().trim_end_matches('/') == "/v1"
        && url.query().is_none()
        && url.fragment().is_none()
}

#[derive(Default)]
struct StreamFunctionCall {
    seen: bool,
    name: String,
    arguments: String,
}

impl StreamFunctionCall {
    fn push_name(&mut self, fragment: &str) {
        if fragment.is_empty() || fragment == self.name {
            return;
        }
        if fragment.starts_with(&self.name) {
            self.name.clear();
            self.name.push_str(fragment);
        } else {
            self.name.push_str(fragment);
        }
    }

    fn into_openai(self) -> LlmResult<OpenAIFunctionCall> {
        if self.name.is_empty() {
            return Err(LlmError::InvalidResponse(
                "OpenAI stream returned function arguments without a function name".to_string(),
            ));
        }
        if self.arguments.trim().is_empty() {
            return Err(LlmError::InvalidResponse(format!(
                "OpenAI stream ended before function `{}` supplied arguments",
                bounded_error_preview(&self.name)
            )));
        }
        let parsed_arguments: Value = serde_json::from_str(&self.arguments).map_err(|error| {
            structured_parse_error("streamed function arguments", &error, &self.arguments)
        })?;
        if !parsed_arguments.is_object() {
            return Err(LlmError::InvalidResponse(format!(
                "OpenAI stream function `{}` returned non-object arguments: {}",
                bounded_error_preview(&self.name),
                bounded_error_preview(&self.arguments)
            )));
        }
        Ok(OpenAIFunctionCall {
            name: self.name,
            arguments: self.arguments,
        })
    }
}

fn bounded_token_count(value: Option<u64>) -> u32 {
    value.unwrap_or_default().min(u32::MAX as u64) as u32
}

fn openai_usage_from_value(value: &Value) -> OpenAIUsage {
    let prompt_tokens = bounded_token_count(value.get("prompt_tokens").and_then(Value::as_u64));
    let completion_tokens =
        bounded_token_count(value.get("completion_tokens").and_then(Value::as_u64));
    let total_tokens = value
        .get("total_tokens")
        .and_then(Value::as_u64)
        .map(|count| bounded_token_count(Some(count)))
        .unwrap_or_else(|| prompt_tokens.saturating_add(completion_tokens));
    OpenAIUsage {
        prompt_tokens,
        completion_tokens,
        total_tokens,
    }
}

fn responses_usage_from_value(value: &Value) -> OpenAIUsage {
    let prompt_tokens = bounded_token_count(value.get("input_tokens").and_then(Value::as_u64));
    let completion_tokens = bounded_token_count(value.get("output_tokens").and_then(Value::as_u64));
    let total_tokens = value
        .get("total_tokens")
        .and_then(Value::as_u64)
        .map(|count| bounded_token_count(Some(count)))
        .unwrap_or_else(|| prompt_tokens.saturating_add(completion_tokens));
    OpenAIUsage {
        prompt_tokens,
        completion_tokens,
        total_tokens,
    }
}

fn validate_completed_responses_status(label: &str, status: Option<&str>) -> LlmResult<()> {
    let Some(status) = status.map(str::trim).filter(|status| !status.is_empty()) else {
        return Ok(());
    };
    if status == "completed" {
        return Ok(());
    }
    Err(LlmError::InvalidResponse(format!(
        "{label} had terminal status `{}` instead of `completed`",
        bounded_error_preview(status)
    )))
}

fn validate_completed_responses_item(item: &Value, context: &str) -> LlmResult<()> {
    validate_completed_responses_status(
        &format!("{context} item"),
        item.get("status").and_then(Value::as_str),
    )?;
    if matches!(
        item.get("type").and_then(Value::as_str),
        Some("function_call")
    ) {
        let missing_arguments = item
            .get("arguments")
            .and_then(Value::as_str)
            .map(|arguments| arguments.trim().is_empty())
            .unwrap_or(true);
        if missing_arguments {
            return Err(LlmError::InvalidResponse(format!(
                "{context} function call item was incomplete"
            )));
        }
    }
    Ok(())
}

/// Process-wide cap on concurrent LLM HTTP calls. Cloud providers enforce
/// small per-account concurrency limits — Ollama Cloud rejects the excess
/// with 429 `"too many concurrent requests"`, and cognify's summarize task
/// fans out up to 50 calls at once, which aborted every backlog indexing run
/// (live 2026-07-06). The permit is held for the whole call including its
/// retries, so a retry storm cannot re-saturate the provider. Override with
/// `COGNEE_LLM_MAX_CONCURRENCY`.
fn llm_concurrency() -> &'static tokio::sync::Semaphore {
    static SEM: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    SEM.get_or_init(|| {
        let permits = std::env::var("COGNEE_LLM_MAX_CONCURRENCY")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(4);
        tokio::sync::Semaphore::new(permits)
    })
}

/// Extra retry attempts allowed when the failure is a SOFT rate limit
/// (concurrency spike, not quota). These resolve as in-flight calls drain,
/// so they deserve a longer budget than generic network errors.
const RATE_LIMIT_RETRIES: usize = 6;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Does a 429 body describe exhausted quota (vs a transient rate spike)?
fn is_hard_quota_error(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("freeusagelimit")
        || lower.contains("free usage")
        || lower.contains("usage limit")
        || lower.contains("quota")
        || lower.contains("insufficient credit")
}

fn is_transient_http_status(status: u16) -> bool {
    matches!(status, 408 | 409 | 425 | 429 | 500..=599)
}

fn is_terminal_chat_finish_reason(reason: &str) -> bool {
    matches!(
        reason,
        "stop" | "length" | "tool_calls" | "function_call" | "content_filter"
    )
}

fn is_transient_llm_error(error: &LlmError) -> bool {
    matches!(
        error,
        LlmError::NetworkError(_) | LlmError::RateLimitExceeded(_) | LlmError::Timeout(_)
    )
}

fn is_explicitly_unsupported(error: &LlmError, fields: &[&str]) -> bool {
    let message = match error {
        LlmError::InvalidResponse(message)
        | LlmError::ApiError(message)
        | LlmError::FeatureNotSupported(message) => message.to_ascii_lowercase(),
        _ => return false,
    };
    fields.iter().any(|field| message.contains(field))
        && [
            "unsupported",
            "not supported",
            "does not support",
            "unknown field",
            "unrecognized",
            "unavailable",
        ]
        .iter()
        .any(|needle| message.contains(needle))
}

fn structured_parse_error(label: &str, error: &serde_json::Error, raw: &str) -> LlmError {
    LlmError::DeserializationError(format!(
        "Failed to deserialize {label}: {error}. Raw: {}",
        bounded_error_preview(raw)
    ))
}

/// OpenAI API adapter.
///
/// Supports structured output generation via:
/// - Strict JSON schema mode (response_format with type: "json_schema")
/// - Function calling (for GPT-4 and GPT-3.5-turbo)
/// - JSON mode (response_format with type: "json_object")
/// - JSON schema validation (via function parameters)
///
/// # Example
/// ```ignore
/// use cognee_llm::adapters::OpenAIAdapter;
/// use cognee_llm::Llm;
///
/// let adapter = OpenAIAdapter::new(
///     "gpt-4-turbo-preview",
///     "sk-...",
///     None, // Use default base URL
/// )?;
///
/// let result: MyStruct = adapter.create_structured_output(
///     "Extract information from this text",
///     "You are a helpful assistant",
///     None,
/// ).await?;
/// ```
#[derive(Clone)]
pub struct OpenAIAdapter {
    model: String,
    api_key: String,
    base_url: String,
    client: Client,
    /// Number of times to retry the HTTP request on transient network/server errors.
    network_retries: usize,
    /// Model name for audio transcription (e.g. `"whisper-1"`).
    transcription_model: String,
    /// OpenAI transport dialect. `chat_completions` is the compatibility
    /// default; `responses` supports Responses-only backends such as Codex.
    api_style: OpenAIApiStyle,
    /// Shared only by clones of this resolved endpoint/account adapter.
    quota_cooldown_until_ms: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum OpenAIApiStyle {
    #[default]
    ChatCompletions,
    Responses,
}

impl OpenAIAdapter {
    /// Default OpenAI API base URL
    pub const DEFAULT_BASE_URL: &'static str = "https://api.openai.com/v1";
    /// Legacy compatibility constant. Structured parse/schema failures are
    /// deterministic and now fail immediately; only transient transport/API
    /// failures consume a bounded retry budget.
    pub const DEFAULT_STRUCTURED_OUTPUT_RETRIES: usize = 5;
    /// Default retry attempts for transient network/server errors.
    pub const DEFAULT_NETWORK_RETRIES: usize = 3;

    /// Create a new OpenAI adapter.
    ///
    /// # Arguments
    /// * `model` - Model identifier (e.g., "gpt-4", "gpt-3.5-turbo")
    /// * `api_key` - OpenAI API key
    /// * `base_url` - Optional custom base URL (defaults to OpenAI's API)
    ///
    /// # Returns
    /// A new OpenAI adapter instance
    pub fn new(
        model: impl Into<String>,
        api_key: impl Into<String>,
        base_url: Option<String>,
    ) -> LlmResult<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            .build()
            .map_err(|e| LlmError::ConfigError(format!("Failed to create HTTP client: {e}")))?;

        let transcription_model =
            std::env::var("TRANSCRIPTION_MODEL").unwrap_or_else(|_| "whisper-1".to_string());

        // Strip a leading litellm-style "openai/" provider prefix. Python's
        // litellm accepts provider-qualified names (e.g. "openai/gpt-5-mini")
        // and strips the provider before calling the OpenAI-native API, which
        // itself rejects the prefix. Strip it here for parity so a
        // provider-qualified config value works against real OpenAI.
        let model: String = model.into();
        let model = model
            .strip_prefix("openai/")
            .map(str::to_string)
            .unwrap_or(model);

        Ok(Self {
            model,
            api_key: api_key.into(),
            // Normalise a trailing slash so request URLs built as
            // `{base_url}/chat/completions` never produce a double slash. The
            // Gemini OpenAI-compat base ends in `/v1beta/openai/`, and a
            // user-supplied endpoint may too; both would otherwise 404.
            base_url: base_url
                .map(|u| u.trim_end_matches('/').to_string())
                .unwrap_or_else(|| Self::DEFAULT_BASE_URL.to_string()),
            client,
            network_retries: Self::DEFAULT_NETWORK_RETRIES,
            transcription_model,
            api_style: OpenAIApiStyle::ChatCompletions,
            quota_cooldown_until_ms: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        })
    }

    /// Select `chat_completions` (default) or the OpenAI `responses` transport.
    /// This changes only the wire format; callers keep using the same `Llm`
    /// interface and structured-output helpers.
    pub fn with_api_style(mut self, style: &str) -> LlmResult<Self> {
        let api_style = match style.trim().to_ascii_lowercase().as_str() {
            "" | "chat" | "chat_completions" | "chat-completions" => {
                OpenAIApiStyle::ChatCompletions
            }
            "responses" | "response" => OpenAIApiStyle::Responses,
            other => {
                return Err(LlmError::ConfigError(format!(
                    "unsupported OpenAI API style `{}`; use `chat_completions` or `responses`",
                    bounded_error_preview(other)
                )));
            }
        };
        if self.is_grok_cli_proxy() && api_style != OpenAIApiStyle::ChatCompletions {
            return Err(LlmError::ConfigError(
                "cli-chat-proxy.grok.com supports streaming chat completions, not the Responses API"
                    .to_string(),
            ));
        }
        self.api_style = api_style;
        Ok(self)
    }

    /// Retained for source compatibility. Deterministic structured-output
    /// failures are never retried; configure HTTP retries separately.
    pub fn with_structured_output_retries(self, _retries: u32) -> Self {
        self
    }

    /// Configure retry attempts for transient network and server errors (HTTP 429, 5xx).
    ///
    /// Each retry uses exponential backoff starting at 1 s, doubling up to 30 s.
    pub fn with_network_retries(mut self, retries: u32) -> Self {
        self.network_retries = usize::try_from(retries)
            .unwrap_or(MAX_NETWORK_RETRIES)
            .min(MAX_NETWORK_RETRIES);
        self
    }

    /// Configure the model used for audio transcription (default: `"whisper-1"`).
    pub fn with_transcription_model(mut self, model: impl Into<String>) -> Self {
        self.transcription_model = model.into();
        self
    }

    /// Build the authorization header value
    fn auth_header(&self) -> String {
        format!("Bearer {}", self.api_key)
    }

    fn check_quota_cooldown_at(&self, now: u64) -> LlmResult<()> {
        let until = self
            .quota_cooldown_until_ms
            .load(std::sync::atomic::Ordering::Acquire);
        if now < until {
            return Err(LlmError::RateLimitExceeded(
                "provider quota exhausted for this endpoint/account — cooling down, call skipped"
                    .to_string(),
            ));
        }
        Ok(())
    }

    fn check_quota_cooldown(&self) -> LlmResult<()> {
        self.check_quota_cooldown_at(now_ms())
    }

    fn trip_quota_cooldown_at(&self, now: u64) -> bool {
        let until = now.saturating_add(QUOTA_COOLDOWN_MS);
        self.quota_cooldown_until_ms
            .fetch_max(until, std::sync::atomic::Ordering::AcqRel)
            <= now
    }

    async fn acquire_llm_permit_and_recheck<'a>(
        &self,
        semaphore: &'a tokio::sync::Semaphore,
    ) -> LlmResult<tokio::sync::SemaphorePermit<'a>> {
        let permit = semaphore
            .acquire()
            .await
            .expect("llm concurrency semaphore is never closed");
        self.check_quota_cooldown()?;
        Ok(permit)
    }

    fn is_grok_cli_proxy(&self) -> bool {
        is_grok_cli_proxy_base_url(&self.base_url)
    }

    fn api_url(&self) -> String {
        match self.api_style {
            OpenAIApiStyle::ChatCompletions => format!("{}/chat/completions", self.base_url),
            OpenAIApiStyle::Responses => format!("{}/responses", self.base_url),
        }
    }

    /// Convert the caller's request to its selected wire dialect. Grok's CLI
    /// inference clusters are streaming-only, so the exact proxy endpoint
    /// always overrides a caller-supplied `stream: false`.
    fn wire_request_body(&self, request_body: Value) -> LlmResult<Value> {
        let mut body = match self.api_style {
            OpenAIApiStyle::ChatCompletions => request_body,
            OpenAIApiStyle::Responses => self.responses_request(request_body),
        };
        if self.is_grok_cli_proxy() {
            if self.api_style != OpenAIApiStyle::ChatCompletions {
                return Err(LlmError::ConfigError(
                    "Grok CLI proxy requires chat_completions transport".to_string(),
                ));
            }
            let object = body.as_object_mut().ok_or_else(|| {
                LlmError::InvalidResponse(
                    "Grok CLI chat-completions request body must be a JSON object".to_string(),
                )
            })?;
            object.insert("stream".to_string(), Value::Bool(true));

            // The subscription proxy rejects both JSON response-format modes
            // and Responses-style reasoning controls. Structured extraction
            // rides native chat tools or an explicit JSON-only prompt instead.
            object.remove("response_format");
            object.remove("reasoning");

            if let Some(functions) = object.remove("functions") {
                if object.contains_key("tools") {
                    return Err(LlmError::InvalidResponse(
                        "Grok CLI request cannot contain both `functions` and `tools`".to_string(),
                    ));
                }
                let functions = functions.as_array().ok_or_else(|| {
                    LlmError::InvalidResponse(
                        "Grok CLI legacy `functions` must be an array".to_string(),
                    )
                })?;
                object.insert(
                    "tools".to_string(),
                    Value::Array(
                        functions
                            .iter()
                            .cloned()
                            .map(|function| json!({"type": "function", "function": function}))
                            .collect(),
                    ),
                );
            }

            if let Some(function_call) = object.remove("function_call") {
                let tool_choice = match function_call {
                    Value::String(choice) => Value::String(choice),
                    Value::Object(call) => {
                        let name = call.get("name").and_then(Value::as_str).ok_or_else(|| {
                            LlmError::InvalidResponse(
                                "Grok CLI forced function call is missing `name`".to_string(),
                            )
                        })?;
                        json!({"type": "function", "function": {"name": name}})
                    }
                    Value::Null => Value::Null,
                    _ => {
                        return Err(LlmError::InvalidResponse(
                            "Grok CLI `function_call` has an unsupported shape".to_string(),
                        ));
                    }
                };
                if !tool_choice.is_null() {
                    object.insert("tool_choice".to_string(), tool_choice);
                }
            }
        }
        Ok(body)
    }

    fn build_api_request(&self, url: &str, request_body: &Value) -> reqwest::RequestBuilder {
        let mut request = self
            .client
            .post(url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json");
        if self.is_grok_cli_proxy() {
            let request_model = request_body
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or(&self.model);
            request = request
                .header("X-XAI-Token-Auth", GROK_TOKEN_AUTH)
                .header("x-grok-model-override", request_model)
                .header("x-grok-client-version", GROK_CLI_CLIENT_VERSION);
        }
        request.json(request_body)
    }

    /// Whether to request non-thinking mode for local Qwen OpenAI-compatible endpoints.
    fn should_disable_thinking(&self) -> bool {
        self.model.to_lowercase().starts_with("qwen") && !self.base_url.contains("api.openai.com")
    }

    /// True for OpenAI reasoning-model families (`gpt-5*`, `o1*`, `o3*`, `o4*`)
    /// that reject `temperature`/`top_p`/`frequency_penalty`/`presence_penalty`
    /// overrides and require `max_completion_tokens` in place of `max_tokens`.
    ///
    /// Gated on the official `api.openai.com` base URL so custom OpenAI-compatible
    /// proxies (Ollama, vLLM, …) keep accepting legacy parameters even when the
    /// configured model name happens to match a reasoning-family prefix.
    fn is_reasoning_model(&self) -> bool {
        if !self.base_url.contains("api.openai.com") {
            return false;
        }
        let m = self.model.to_lowercase();
        m.starts_with("gpt-5") || m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4")
    }

    /// Insert `max_tokens` (or `max_completion_tokens` on reasoning models) into a
    /// request body if `value` is `Some`.
    fn write_max_tokens(&self, body: &mut Value, value: Option<u32>) {
        if let Some(v) = value {
            let key = if self.is_reasoning_model() {
                "max_completion_tokens"
            } else {
                "max_tokens"
            };
            body[key] = json!(v);
        }
    }

    /// Call the configured OpenAI API dialect, retrying on transient network/server errors.
    ///
    /// Retries up to `self.network_retries` times with exponential backoff (1 s, 2 s, 4 s …
    /// capped at 30 s) on:
    /// - Network-level failures (connection refused, timeout, etc.)
    /// - HTTP 408/409/425/429
    /// - HTTP 5xx (server errors)
    ///
    /// Every other HTTP status and every response parse/schema failure returns
    /// immediately; retry counts are capped even if configuration is hostile.
    #[instrument(
        name = "llm.api_call",
        level = "info",
        skip(self, request_body),
        fields(
            url = tracing::field::Empty,
            cognee.llm.model = self.model.as_str(),
            cognee.llm.provider = "openai",
        ),
    )]
    async fn call_api(&self, request_body: Value) -> LlmResult<OpenAIResponse> {
        // Quota cooldown: fail fast before joining the queue. The same scope
        // is rechecked after acquiring a permit and before every retry.
        self.check_quota_cooldown()?;
        let url = self.api_url();
        let url_preview = bounded_error_preview(&url);
        let request_body = self.wire_request_body(request_body)?;
        tracing::Span::current().record("url", url_preview.as_str());
        let debug_enabled = std::env::var("COGNEE_DEBUG_LLM_REQUEST")
            .map(|v| cognee_utils::parse_env_bool(&v))
            .unwrap_or(false);

        if debug_enabled {
            let pretty_request = serde_json::to_string_pretty(&request_body)
                .unwrap_or_else(|_| request_body.to_string());
            eprintln!(
                "\n[COGNEE_DEBUG_LLM_REQUEST] POST {url_preview}\n{}\n",
                bounded_error_preview(&pretty_request)
            );
        }

        // Serialize against the process-wide concurrency cap. Held across
        // retries so backoff sleeps also hold the fan-out down.
        let _permit = self
            .acquire_llm_permit_and_recheck(llm_concurrency())
            .await?;

        let mut last_error = LlmError::NetworkError("No attempt made".to_string());
        let mut attempt: usize = 0;

        loop {
            // Soft rate limits get a longer, slower ladder than generic
            // network errors: they resolve as other calls drain, and giving
            // up after 3 quick tries aborted whole cognify runs.
            let rate_limited = matches!(last_error, LlmError::RateLimitExceeded(_));
            let budget = if rate_limited {
                self.network_retries.max(RATE_LIMIT_RETRIES)
            } else {
                self.network_retries
            };
            if attempt > budget {
                break;
            }
            debug!(attempt, "LLM API attempt");
            if attempt > 0 {
                let base_ms: u64 = if rate_limited { 5_000 } else { 1_000 };
                let delay_ms = (base_ms * 2u64.saturating_pow(attempt as u32 - 1)).min(60_000);
                warn!(
                    attempt,
                    budget,
                    delay_ms,
                    error = %last_error,
                    "LLM request failed, retrying",
                );
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
            self.check_quota_cooldown()?;
            attempt += 1;

            let response = match self.build_api_request(&url, &request_body).send().await {
                Ok(r) => r,
                Err(e) => {
                    let error = LlmError::NetworkError(bounded_error_preview(&e.to_string()));
                    if e.is_builder() || e.is_redirect() {
                        return Err(error);
                    }
                    last_error = error;
                    continue;
                }
            };

            let status = response.status();

            if !status.is_success() {
                let error_body = match read_bounded_response_text(
                    response,
                    MAX_LLM_ERROR_BYTES,
                    "OpenAI API error response",
                )
                .await
                {
                    Ok(body) => body,
                    Err(error) if is_transient_llm_error(&error) => {
                        last_error = error;
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                let error_preview = bounded_error_preview(&error_body);

                // A hard quota 429 (free tier exhausted, credits gone) is not
                // transient — trip only this endpoint/account adapter lane.
                if status.as_u16() == 429 && is_hard_quota_error(&error_body) {
                    if self.trip_quota_cooldown_at(now_ms()) {
                        warn!(
                            cooldown_secs = QUOTA_COOLDOWN_MS / 1000,
                            "LLM provider quota exhausted — pausing this endpoint/account lane for the cooldown window"
                        );
                    }
                    return Err(LlmError::RateLimitExceeded(error_preview));
                }

                let err = match status.as_u16() {
                    401 => LlmError::AuthenticationError(error_preview),
                    429 => LlmError::RateLimitExceeded(error_preview),
                    400 => LlmError::InvalidResponse(format!("Bad request: {error_preview}")),
                    _ => LlmError::ApiError(format!("HTTP {status}: {error_preview}")),
                };

                if !is_transient_http_status(status.as_u16()) {
                    return Err(err);
                }

                last_error = err;
                continue;
            }

            let response_body = match read_bounded_response_text(
                response,
                MAX_LLM_RESPONSE_BYTES,
                "OpenAI API response",
            )
            .await
            {
                Ok(body) => body,
                Err(error) if is_transient_llm_error(&error) => {
                    last_error = error;
                    continue;
                }
                Err(error) => return Err(error),
            };

            if debug_enabled {
                eprintln!(
                    "\n[COGNEE_DEBUG_LLM_RESPONSE] POST {url_preview}\n{}\n",
                    bounded_error_preview(&response_body)
                );
            }

            return match self.api_style {
                OpenAIApiStyle::ChatCompletions => self.parse_chat_completions_body(&response_body),
                OpenAIApiStyle::Responses => self.parse_responses_body(&response_body),
            };
        }

        Err(LlmError::MaxRetriesExceeded(format!(
            "LLM request failed after {attempt} attempt(s): {last_error}"
        )))
    }

    fn parse_chat_completions_body(&self, raw: &str) -> LlmResult<OpenAIResponse> {
        self.parse_chat_completions_body_with_limit(raw, MAX_LLM_RESPONSE_BYTES)
    }

    fn parse_chat_completions_body_with_limit(
        &self,
        raw: &str,
        limit: usize,
    ) -> LlmResult<OpenAIResponse> {
        if raw.len() > limit {
            return Err(response_size_error(
                "OpenAI chat-completions response",
                limit,
                false,
            ));
        }

        let first_nonempty = raw
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with(':'));
        let is_sse = first_nonempty
            .is_some_and(|line| line.starts_with("event:") || line.starts_with("data:"));
        if is_sse {
            return self.parse_chat_completions_sse(raw, limit);
        }

        serde_json::from_str::<OpenAIResponse>(raw).map_err(|error| {
            LlmError::DeserializationError(format!(
                "Failed to parse response: {error}. Raw body: {}",
                bounded_error_preview(raw)
            ))
        })
    }

    /// Fold a bounded OpenAI chat-completions event stream into the legacy
    /// response shape consumed by Cognee. Cognee requests one forced function,
    /// so its response type has one `function_call`; when a modern `tool_calls`
    /// stream arrives, the first populated call is normalized into that slot.
    fn parse_chat_completions_sse(&self, raw: &str, limit: usize) -> LlmResult<OpenAIResponse> {
        if raw.len() > limit {
            return Err(response_size_error(
                "OpenAI chat-completions SSE response",
                limit,
                false,
            ));
        }

        let mut id: Option<String> = None;
        let mut created: Option<i64> = None;
        let mut model: Option<String> = None;
        let mut choice_index: Option<u32> = None;
        let mut role: Option<String> = None;
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut finish_reason: Option<String> = None;
        let mut legacy_function = StreamFunctionCall::default();
        let mut tool_calls: Vec<StreamFunctionCall> = Vec::new();
        let mut usage: Option<OpenAIUsage> = None;
        let mut event_count = 0usize;
        let mut saw_json_event = false;
        let mut saw_choice = false;
        let mut saw_done = false;
        let mut saw_terminal_finish = false;

        for line in raw.lines().map(str::trim) {
            let Some(payload) = line.strip_prefix("data:").map(str::trim) else {
                continue;
            };
            if payload.is_empty() {
                return Err(LlmError::InvalidResponse(
                    "OpenAI chat-completions SSE contained an empty `data:` event".to_string(),
                ));
            }
            if saw_done {
                return Err(LlmError::InvalidResponse(
                    "OpenAI chat-completions SSE contained data after `[DONE]`".to_string(),
                ));
            }
            if payload == "[DONE]" {
                saw_done = true;
                continue;
            }
            event_count = event_count.saturating_add(1);
            if event_count > MAX_CHAT_SSE_EVENTS {
                return Err(LlmError::InvalidResponse(format!(
                    "OpenAI chat-completions SSE exceeded the {MAX_CHAT_SSE_EVENTS} event limit"
                )));
            }
            let chunk = serde_json::from_str::<Value>(payload).map_err(|error| {
                LlmError::DeserializationError(format!(
                    "Malformed OpenAI chat-completions SSE `data:` JSON: {error}. Payload: {}",
                    bounded_error_preview(payload)
                ))
            })?;
            saw_json_event = true;

            if let Some(error) = chunk.get("error") {
                return Err(LlmError::ApiError(format!(
                    "OpenAI chat-completions stream reported an error: {}",
                    bounded_error_preview(&error.to_string())
                )));
            }
            if let Some(value) = chunk.get("id").and_then(Value::as_str) {
                id = Some(value.to_string());
            }
            if let Some(value) = chunk.get("created").and_then(Value::as_i64) {
                created = Some(value);
            }
            if let Some(value) = chunk.get("model").and_then(Value::as_str) {
                model = Some(value.to_string());
            }
            if let Some(value) = chunk.get("usage").filter(|value| !value.is_null()) {
                usage = Some(openai_usage_from_value(value));
            }

            let Some(choices) = chunk.get("choices").and_then(Value::as_array) else {
                continue;
            };
            let Some(choice) = choices
                .iter()
                .find(|choice| choice.get("index").and_then(Value::as_u64) == Some(0))
                .or_else(|| choices.first())
            else {
                continue;
            };
            if saw_terminal_finish {
                return Err(LlmError::InvalidResponse(
                    "OpenAI chat-completions SSE contained another choice after its terminal finish"
                        .to_string(),
                ));
            }
            saw_choice = true;
            if choice_index.is_none() {
                choice_index = Some(
                    choice
                        .get("index")
                        .and_then(Value::as_u64)
                        .unwrap_or(0)
                        .min(u32::MAX as u64) as u32,
                );
            }
            if let Some(value) = choice.get("finish_reason").and_then(Value::as_str) {
                if !is_terminal_chat_finish_reason(value) {
                    return Err(LlmError::InvalidResponse(format!(
                        "OpenAI chat-completions SSE returned invalid finish reason `{}`",
                        bounded_error_preview(value)
                    )));
                }
                finish_reason = Some(value.to_string());
                saw_terminal_finish = true;
            }

            let Some(delta) = choice.get("delta").or_else(|| choice.get("message")) else {
                continue;
            };
            if let Some(value) = delta.get("role").and_then(Value::as_str) {
                role = Some(value.to_string());
            }
            if let Some(value) = delta.get("content").and_then(Value::as_str) {
                content.push_str(value);
            }
            if let Some(value) = delta
                .get("reasoning_content")
                .or_else(|| delta.get("reasoning"))
                .and_then(Value::as_str)
            {
                reasoning.push_str(value);
            }
            if let Some(function) = delta.get("function_call") {
                legacy_function.seen = true;
                if let Some(value) = function.get("name").and_then(Value::as_str) {
                    legacy_function.push_name(value);
                }
                if let Some(value) = function.get("arguments").and_then(Value::as_str) {
                    legacy_function.arguments.push_str(value);
                }
            }
            for call in delta
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let index_u64 = call.get("index").and_then(Value::as_u64).unwrap_or(0);
                let index = usize::try_from(index_u64).map_err(|_| {
                    LlmError::InvalidResponse(
                        "OpenAI stream tool-call index is out of range".to_string(),
                    )
                })?;
                if index >= MAX_CHAT_SSE_TOOL_CALLS {
                    return Err(LlmError::InvalidResponse(format!(
                        "OpenAI stream tool-call index {index} exceeds the {MAX_CHAT_SSE_TOOL_CALLS} call limit"
                    )));
                }
                while tool_calls.len() <= index {
                    tool_calls.push(StreamFunctionCall::default());
                }
                let accumulator = &mut tool_calls[index];
                accumulator.seen = true;
                if let Some(value) = call
                    .get("function")
                    .and_then(|function| function.get("name"))
                    .and_then(Value::as_str)
                {
                    accumulator.push_name(value);
                }
                if let Some(value) = call
                    .get("function")
                    .and_then(|function| function.get("arguments"))
                    .and_then(Value::as_str)
                {
                    accumulator.arguments.push_str(value);
                }
            }
        }

        if !saw_json_event || !saw_choice {
            return Err(LlmError::InvalidResponse(
                "OpenAI chat-completions SSE contained no completion choice".to_string(),
            ));
        }
        if !saw_done && !saw_terminal_finish {
            return Err(LlmError::InvalidResponse(
                "OpenAI chat-completions SSE ended without `[DONE]` or a terminal finish reason"
                    .to_string(),
            ));
        }

        let mut completed_tools = tool_calls
            .into_iter()
            .filter(|call| call.seen)
            .map(StreamFunctionCall::into_openai)
            .collect::<LlmResult<Vec<_>>>()?;
        if legacy_function.seen && !completed_tools.is_empty() {
            return Err(LlmError::InvalidResponse(
                "OpenAI stream mixed legacy `function_call` and modern `tool_calls`".to_string(),
            ));
        }
        let function_call = if legacy_function.seen {
            Some(legacy_function.into_openai()?)
        } else if completed_tools.is_empty() {
            None
        } else {
            Some(completed_tools.remove(0))
        };
        if matches!(
            finish_reason.as_deref(),
            Some("tool_calls" | "function_call")
        ) && function_call.is_none()
        {
            return Err(LlmError::InvalidResponse(
                "OpenAI stream declared a structured-call finish without a complete call"
                    .to_string(),
            ));
        }
        if function_call.is_some()
            && matches!(finish_reason.as_deref(), Some("length" | "content_filter"))
        {
            return Err(LlmError::InvalidResponse(
                "OpenAI structured call was cut off by its terminal finish reason".to_string(),
            ));
        }

        Ok(OpenAIResponse {
            id: id.unwrap_or_else(|| "chatcmpl-stream".to_string()),
            object: "chat.completion".to_string(),
            created: created.unwrap_or_default(),
            model: model.unwrap_or_else(|| self.model.clone()),
            choices: vec![OpenAIChoice {
                index: choice_index.unwrap_or_default(),
                message: OpenAIMessage {
                    role: role.unwrap_or_else(|| "assistant".to_string()),
                    content: (!content.is_empty()).then_some(content),
                    reasoning: (!reasoning.is_empty()).then_some(reasoning),
                    function_call,
                },
                finish_reason: finish_reason.or_else(|| Some("stop".to_string())),
            }],
            usage,
        })
    }

    /// Translate the adapter's existing Chat Completions-shaped request into
    /// Responses input. This keeps every Cognee extraction path (plain text,
    /// JSON schema, and forced function output) working on either transport.
    fn responses_request(&self, chat: Value) -> Value {
        let messages = chat
            .get("messages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut instructions = Vec::new();
        let mut input = Vec::new();
        for message in messages {
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user");
            let content = message
                .get("content")
                .cloned()
                .unwrap_or(Value::String(String::new()));
            if role == "system" || role == "developer" {
                if let Some(text) = content.as_str().filter(|s| !s.trim().is_empty()) {
                    instructions.push(text.to_string());
                }
                continue;
            }
            let output_role = if role == "assistant" {
                "assistant"
            } else {
                "user"
            };
            let item_type = if output_role == "assistant" {
                "output_text"
            } else {
                "input_text"
            };
            let blocks = match content {
                Value::String(text) => json!([{ "type": item_type, "text": text }]),
                other => other,
            };
            input.push(json!({ "role": output_role, "content": blocks }));
        }
        if input.is_empty() {
            input.push(json!({
                "role": "user",
                "content": [{"type": "input_text", "text": " "}]
            }));
        }

        let mut body = json!({
            "model": chat.get("model").cloned().unwrap_or_else(|| json!(self.model)),
            "instructions": if instructions.is_empty() {
                Value::String("Return the requested result accurately.".to_string())
            } else {
                Value::String(instructions.join("\n\n"))
            },
            "input": input,
            "store": false,
            "stream": self.base_url.contains("chatgpt.com"),
        });

        if let Some(functions) = chat.get("functions").and_then(Value::as_array) {
            body["tools"] = Value::Array(
                functions
                    .iter()
                    .map(|f| json!({
                        "type": "function",
                        "name": f.get("name").cloned().unwrap_or(Value::String(String::new())),
                        "description": f.get("description").cloned().unwrap_or(Value::String(String::new())),
                        "parameters": f.get("parameters").cloned().unwrap_or_else(|| json!({"type":"object"})),
                    }))
                    .collect(),
            );
            if let Some(name) = chat
                .get("function_call")
                .and_then(|v| v.get("name"))
                .and_then(Value::as_str)
            {
                body["tool_choice"] = json!({"type": "function", "name": name});
            }
        }
        if let Some(format) = chat.get("response_format") {
            let responses_format =
                if format.get("type").and_then(Value::as_str) == Some("json_schema") {
                    format
                        .get("json_schema")
                        .cloned()
                        .map(|mut schema| {
                            schema["type"] = json!("json_schema");
                            schema
                        })
                        .unwrap_or_else(|| format.clone())
                } else {
                    format.clone()
                };
            body["text"] = json!({"format": responses_format});
        }
        // The public API accepts max_output_tokens. The ChatGPT subscription
        // Codex backend rejects it, so mirror Phoenix's native Codex transport
        // and leave that backend on its server-side output budget.
        if self.base_url.contains("api.openai.com") {
            if let Some(max) = chat
                .get("max_completion_tokens")
                .or_else(|| chat.get("max_tokens"))
            {
                body["max_output_tokens"] = max.clone();
            }
        }
        body
    }

    fn parse_responses_body(&self, raw: &str) -> LlmResult<OpenAIResponse> {
        if raw.len() > MAX_LLM_RESPONSE_BYTES {
            return Err(response_size_error(
                "OpenAI Responses API response",
                MAX_LLM_RESPONSE_BYTES,
                false,
            ));
        }
        // Responses SSE starts with an `event:` line on the ChatGPT/Codex
        // transport, while some compatible servers start directly with
        // `data:`. Treat either framing as SSE; feeding `event:
        // response.created` to serde_json was the reason Phoenix Memory
        // indexing failed even though the provider returned a valid stream.
        let first_nonempty = raw
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with(':'));
        let is_sse = first_nonempty
            .is_some_and(|line| line.starts_with("event:") || line.starts_with("data:"));
        let value: Value = if is_sse {
            let mut completed: Option<Value> = None;
            let mut items = Vec::new();
            let mut streamed_text = String::new();
            let mut saw_json_event = false;
            let mut saw_done = false;
            let mut saw_completed = false;
            let mut event_count = 0usize;
            for line in raw.lines().map(str::trim) {
                let Some(payload) = line.strip_prefix("data:").map(str::trim) else {
                    continue;
                };
                if payload.is_empty() {
                    return Err(LlmError::InvalidResponse(
                        "Responses SSE contained an empty `data:` event".to_string(),
                    ));
                }
                if saw_done {
                    return Err(LlmError::InvalidResponse(
                        "Responses SSE contained data after `[DONE]`".to_string(),
                    ));
                }
                if payload == "[DONE]" {
                    saw_done = true;
                    continue;
                }
                if saw_completed {
                    return Err(LlmError::InvalidResponse(
                        "Responses SSE contained data after `response.completed`".to_string(),
                    ));
                }
                event_count = event_count.saturating_add(1);
                if event_count > MAX_CHAT_SSE_EVENTS {
                    return Err(LlmError::InvalidResponse(format!(
                        "Responses SSE exceeded the {MAX_CHAT_SSE_EVENTS} event limit"
                    )));
                }
                let event = serde_json::from_str::<Value>(payload).map_err(|error| {
                    LlmError::DeserializationError(format!(
                        "Malformed Responses SSE `data:` JSON: {error}. Payload: {}",
                        bounded_error_preview(payload)
                    ))
                })?;
                saw_json_event = true;
                match event.get("type").and_then(Value::as_str) {
                    Some("response.completed") => {
                        saw_completed = true;
                        completed = event.get("response").cloned();
                    }
                    Some("response.output_item.done") => {
                        if let Some(item) = event.get("item") {
                            items.push(item.clone());
                        }
                    }
                    Some("response.output_text.delta") => {
                        if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                            streamed_text.push_str(delta);
                        }
                    }
                    Some("response.failed" | "error") => {
                        return Err(LlmError::ApiError(format!(
                            "Responses stream reported an error: {}",
                            bounded_error_preview(&event.to_string())
                        )));
                    }
                    Some("response.incomplete") => {
                        return Err(LlmError::InvalidResponse(format!(
                            "Responses stream ended incomplete: {}",
                            bounded_error_preview(&event.to_string())
                        )));
                    }
                    _ => {}
                }
            }
            if !saw_json_event {
                return Err(LlmError::InvalidResponse(
                    "Responses SSE contained no JSON events".to_string(),
                ));
            }
            if !saw_done && completed.is_none() {
                return Err(LlmError::InvalidResponse(
                    "Responses SSE ended without `[DONE]` or `response.completed`".to_string(),
                ));
            }
            let mut response = completed.unwrap_or_else(|| {
                json!({
                    "id": "response", "model": self.model, "usage": {}
                })
            });
            if !items.is_empty() {
                // Subscription Codex streams function calls as output-item
                // events and may omit them from response.completed.
                for item in &items {
                    validate_completed_responses_item(item, "Responses SSE output")?;
                }
                response["output"] = Value::Array(items);
            }
            if !streamed_text.is_empty() {
                response["output_text"] = Value::String(streamed_text);
            }
            response
        } else {
            serde_json::from_str(raw).map_err(|e| {
                LlmError::DeserializationError(format!(
                    "Failed to parse Responses API response: {e}. Raw body: {}",
                    bounded_error_preview(raw)
                ))
            })?
        };
        validate_completed_responses_status(
            "Responses response",
            value.get("status").and_then(Value::as_str),
        )?;
        let mut content = String::new();
        let mut function_call = None;
        for item in value
            .get("output")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            validate_completed_responses_item(item, "Responses final output")?;
            match item.get("type").and_then(Value::as_str) {
                Some("function_call") => {
                    let name = item
                        .get("name")
                        .and_then(Value::as_str)
                        .filter(|name| !name.is_empty())
                        .ok_or_else(|| {
                            LlmError::InvalidResponse(
                                "Responses function call is missing its name".to_string(),
                            )
                        })?;
                    let arguments = item
                        .get("arguments")
                        .and_then(Value::as_str)
                        .filter(|arguments| !arguments.trim().is_empty())
                        .ok_or_else(|| {
                            LlmError::InvalidResponse(format!(
                                "Responses function `{}` is missing arguments",
                                bounded_error_preview(name)
                            ))
                        })?;
                    let parsed_arguments: Value =
                        serde_json::from_str(arguments).map_err(|error| {
                            structured_parse_error(
                                "Responses function arguments",
                                &error,
                                arguments,
                            )
                        })?;
                    if !parsed_arguments.is_object() {
                        return Err(LlmError::InvalidResponse(format!(
                            "Responses function `{}` returned non-object arguments: {}",
                            bounded_error_preview(name),
                            bounded_error_preview(arguments)
                        )));
                    }
                    function_call = Some(OpenAIFunctionCall {
                        name: name.to_string(),
                        arguments: arguments.to_string(),
                    });
                }
                Some("message") => {
                    for block in item
                        .get("content")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        if matches!(
                            block.get("type").and_then(Value::as_str),
                            Some("output_text" | "text")
                        ) {
                            if let Some(text) = block.get("text").and_then(Value::as_str) {
                                content.push_str(text);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if content.is_empty() {
            content = value
                .get("output_text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
        }
        let usage = value.get("usage").map(responses_usage_from_value);
        Ok(OpenAIResponse {
            id: value
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("response")
                .to_string(),
            object: "chat.completion".to_string(),
            created: value
                .get("created_at")
                .and_then(Value::as_i64)
                .unwrap_or_default(),
            model: value
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or(&self.model)
                .to_string(),
            choices: vec![OpenAIChoice {
                index: 0,
                message: OpenAIMessage {
                    role: "assistant".to_string(),
                    content: (!content.is_empty()).then_some(content),
                    reasoning: None,
                    function_call,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage,
        })
    }

    /// Convert our Message type to OpenAI's format
    fn convert_messages(messages: &[Message]) -> Vec<Value> {
        messages
            .iter()
            .map(|msg| {
                json!({
                    "role": match msg.role {
                        MessageRole::System => "system",
                        MessageRole::User => "user",
                        MessageRole::Assistant => "assistant",
                    },
                    "content": msg.content
                })
            })
            .collect()
    }

    /// Convert JSON Schema to an example JSON with placeholder values
    /// This is clearer for LLMs than showing the full schema
    fn schema_to_example(schema: &Value) -> String {
        fn create_example(value: &Value, definitions: Option<&Value>) -> Value {
            match value {
                Value::Object(obj) => {
                    // Handle $ref references
                    if let Some(ref_str) = obj.get("$ref").and_then(|v| v.as_str())
                        && let Some(def_name) = ref_str.strip_prefix("#/definitions/")
                        && let Some(defs) = definitions
                        && let Some(def) = defs.get(def_name)
                    {
                        return create_example(def, definitions);
                    }

                    // Get the type of this field
                    let type_val = obj.get("type");

                    // Handle arrays
                    if let Some(Value::String(t)) = type_val
                        && t == "array"
                    {
                        if let Some(items) = obj.get("items") {
                            // Return array with one example item
                            return json!([create_example(items, definitions)]);
                        }
                        return json!([]);
                    }

                    // Handle objects with properties
                    if let Some(props) = obj.get("properties")
                        && let Value::Object(props_obj) = props
                    {
                        let mut result = serde_json::Map::new();
                        for (key, val) in props_obj {
                            result.insert(key.clone(), create_example(val, definitions));
                        }
                        return Value::Object(result);
                    }

                    // Handle primitive types
                    if let Some(Value::String(t)) = type_val {
                        return match t.as_str() {
                            "string" => json!("example"),
                            "number" | "integer" => json!(0),
                            "boolean" => json!(false),
                            _ => json!(null),
                        };
                    }

                    // Handle union types (e.g., ["string", "null"])
                    if let Some(Value::Array(types)) = type_val {
                        for t in types {
                            if let Value::String(type_str) = t
                                && type_str != "null"
                            {
                                return match type_str.as_str() {
                                    "string" => json!("example"),
                                    "number" | "integer" => json!(0),
                                    "boolean" => json!(false),
                                    _ => json!(null),
                                };
                            }
                        }
                    }

                    json!(null)
                }
                _ => value.clone(),
            }
        }

        let definitions = schema.get("definitions");
        let example = create_example(schema, definitions);

        serde_json::to_string_pretty(&example).unwrap_or_else(|_| "{}".to_string())
    }

    fn apply_structured_options(&self, body: &mut Value, options: &GenerationOptions) {
        if !self.is_reasoning_model()
            && let Some(temperature) = options.temperature
        {
            body["temperature"] = json!(temperature);
        }
        self.write_max_tokens(body, options.max_tokens);
        if self.should_disable_thinking() {
            body["think"] = json!(false);
            body["reasoning"] = json!({"effort": "none"});
        }
    }

    fn parse_structured_response(
        &self,
        response: &OpenAIResponse,
        label: &str,
    ) -> LlmResult<Value> {
        let choice = response
            .choices
            .first()
            .ok_or_else(|| LlmError::InvalidResponse(format!("No choices in {label} response")))?;
        if let Some(function_call) = &choice.message.function_call {
            return serde_json::from_str(&function_call.arguments).map_err(|error| {
                structured_parse_error(
                    &format!("{label} function-call arguments"),
                    &error,
                    &function_call.arguments,
                )
            });
        }
        if let Some(content) = choice.message.content.as_deref() {
            return serde_json::from_str(content)
                .map_err(|error| structured_parse_error(label, &error, content));
        }
        Err(LlmError::InvalidResponse(format!(
            "{label} response contained neither function arguments nor JSON content"
        )))
    }
}

/// Rewrite a `schemars`-generated JSON schema so it satisfies OpenAI's
/// **strict** structured-output requirements.
///
/// OpenAI's `response_format: {type: "json_schema", strict: true}` rejects any
/// schema where an object lacks `"additionalProperties": false` or whose
/// `"required"` array does not list *every* declared property. `schemars`
/// (0.8, draft-07) emits neither guarantee — optional (`Option<T>`) fields are
/// omitted from `required` and `additionalProperties` is left unset. Sending
/// that raw schema produces a deterministic 400; it must not be retried or
/// silently treated as a transient provider failure.
///
/// This walks the schema (including `definitions`/`$defs`, `properties`,
/// `items`, and the `anyOf`/`allOf`/`oneOf` combinators) and, for every object
/// that declares `properties`, forces `additionalProperties: false` and sets
/// `required` to the full set of property keys. The `Value` is cloned and
/// returned unchanged for non-object schemas.
fn to_strict_schema(schema: &Value) -> Value {
    fn walk(value: &mut Value) {
        match value {
            Value::Object(obj) => {
                if let Some(Value::Object(props)) = obj.get("properties") {
                    // Every declared property must be required under strict mode.
                    let keys: Vec<Value> = props.keys().map(|k| Value::String(k.clone())).collect();
                    obj.insert("required".to_string(), Value::Array(keys));
                    obj.insert("additionalProperties".to_string(), Value::Bool(false));
                }
                for (_k, v) in obj.iter_mut() {
                    walk(v);
                }
            }
            Value::Array(items) => {
                for v in items.iter_mut() {
                    walk(v);
                }
            }
            _ => {}
        }
    }

    let mut out = schema.clone();
    walk(&mut out);
    out
}

#[async_trait]
impl Llm for OpenAIAdapter {
    async fn generate(
        &self,
        messages: Vec<Message>,
        options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        let opts = options.unwrap_or_default();

        let mut request_body = json!({
            "model": self.model,
            "messages": Self::convert_messages(&messages),
        });

        // Add optional parameters. Reasoning models (gpt-5*/o1*/o3*/o4*)
        // reject sampling overrides and only accept `max_completion_tokens`.
        if !self.is_reasoning_model() {
            if let Some(temp) = opts.temperature {
                request_body["temperature"] = json!(temp);
            }
            if let Some(top_p) = opts.top_p {
                request_body["top_p"] = json!(top_p);
            }
            if let Some(freq_penalty) = opts.frequency_penalty {
                request_body["frequency_penalty"] = json!(freq_penalty);
            }
            if let Some(pres_penalty) = opts.presence_penalty {
                request_body["presence_penalty"] = json!(pres_penalty);
            }
        }
        self.write_max_tokens(&mut request_body, opts.max_tokens);
        if let Some(stop) = opts.stop
            && !stop.is_empty()
        {
            request_body["stop"] = json!(stop);
        }

        if self.should_disable_thinking() {
            request_body["think"] = json!(false);
            request_body["reasoning"] = json!({"effort": "none"});
        }

        let response = self.call_api(request_body).await?;

        // Extract the first choice
        let choice = response
            .choices
            .first()
            .ok_or_else(|| LlmError::InvalidResponse("No choices in response".to_string()))?;

        Ok(GenerationResponse {
            content: choice.message.content.clone().unwrap_or_default(),
            model: response.model,
            finish_reason: choice.finish_reason.clone(),
            usage: response.usage.map(|u| TokenUsage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
            }),
        })
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        messages: Vec<Message>,
        json_schema: &Value,
        options: Option<GenerationOptions>,
    ) -> LlmResult<Value> {
        let opts = options.unwrap_or_default();
        let schema = json_schema;
        let converted_messages = Self::convert_messages(&messages);
        let mut response_format_supported = !self.is_grok_cli_proxy();

        // Grok's subscription proxy explicitly rejects `response_format`, so
        // never probe it. Other OpenAI-compatible endpoints keep strict JSON
        // schema first; only an explicit capability rejection can select the
        // tool/prompt alternatives below. Parse/schema failures never retry.
        if !self.is_grok_cli_proxy() {
            let mut strict_schema_request = json!({
                "model": self.model,
                "messages": converted_messages.clone(),
                "response_format": {
                    "type": "json_schema",
                    "json_schema": {
                        "name": "extract_structured_data",
                        "schema": to_strict_schema(schema),
                        "strict": true
                    }
                }
            });
            self.apply_structured_options(&mut strict_schema_request, &opts);
            match self.call_api(strict_schema_request.clone()).await {
                Ok(strict_response) => {
                    return self.parse_structured_response(&strict_response, "strict JSON schema");
                }
                Err(error)
                    if is_explicitly_unsupported(
                        &error,
                        &["response_format", "json_schema", "json schema"],
                    ) =>
                {
                    response_format_supported = false;
                    warn!(error = %error, "strict JSON schema is explicitly unsupported; using function/tool extraction");
                }
                Err(error) => return Err(error),
            }
        }

        // Exact Grok requests are normalized by `wire_request_body` from these
        // legacy fields into native `tools` + forced `tool_choice`, while every
        // other provider retains its established function-calling wire.
        let mut request_body = json!({
            "model": self.model,
            "messages": converted_messages.clone(),
            "functions": [{
                "name": "extract_structured_data",
                "description": "Extract structured data from the input",
                "parameters": schema
            }],
            "function_call": {"name": "extract_structured_data"}
        });
        self.apply_structured_options(&mut request_body, &opts);

        match self.call_api(request_body).await {
            Ok(response) => return self.parse_structured_response(&response, "function/tool"),
            Err(error)
                if is_explicitly_unsupported(
                    &error,
                    &["functions", "function_call", "tools", "tool_choice"],
                ) =>
            {
                warn!(error = %error, "function/tool extraction is explicitly unsupported; using a JSON-only prompt");
            }
            Err(error) => return Err(error),
        }

        // Portable last resort for endpoints that explicitly reject tools.
        // Grok and providers that rejected response_format get prompt-only JSON;
        // otherwise retain the established json_object hint.
        let mut json_messages = converted_messages;
        let example = Self::schema_to_example(schema);
        let instruction = format!(
            "Extract the information above and return ONLY one valid JSON object. Use this structure as the template (with actual values):\n{example}"
        );
        if let Some(last_msg) = json_messages.last_mut()
            && last_msg["role"] == "user"
        {
            let original_content = last_msg["content"].as_str().unwrap_or("");
            last_msg["content"] = json!(format!("{original_content}\n\n{instruction}"));
        } else {
            json_messages.push(json!({"role": "user", "content": instruction}));
        }

        let mut json_request = json!({
            "model": self.model,
            "messages": json_messages,
        });
        if response_format_supported {
            json_request["response_format"] = json!({"type": "json_object"});
        }
        self.apply_structured_options(&mut json_request, &opts);
        let response = self.call_api(json_request).await?;
        self.parse_structured_response(&response, "JSON prompt")
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_function_calling(&self) -> bool {
        true
    }

    fn max_context_length(&self) -> u32 {
        // Context lengths for common OpenAI models
        match self.model.as_str() {
            m if m.starts_with("gpt-4-turbo") => 128_000,
            m if m.starts_with("gpt-4-32k") => 32_768,
            m if m.starts_with("gpt-4") => 8_192,
            m if m.starts_with("gpt-3.5-turbo-16k") => 16_384,
            m if m.starts_with("gpt-3.5-turbo") => 4_096,
            _ => 4_096, // Conservative default
        }
    }

    async fn transcribe_image(
        &self,
        image_bytes: &[u8],
        mime_type: &str,
        options: Option<GenerationOptions>,
    ) -> LlmResult<String> {
        use base64::Engine as _;

        if !mime_type.starts_with("image/") {
            return Err(LlmError::InvalidResponse(format!(
                "Expected image/* MIME type, got: {mime_type}"
            )));
        }

        let b64 = base64::engine::general_purpose::STANDARD.encode(image_bytes);
        let data_uri = format!("data:{mime_type};base64,{b64}");

        let vision_model = std::env::var("LLM_VISION_MODEL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.model.clone());

        let max_tokens = options.as_ref().and_then(|o| o.max_tokens).unwrap_or(300);

        let mut request_body = json!({
            "model": vision_model,
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": "What's in this image?" },
                    { "type": "image_url", "image_url": { "url": data_uri } }
                ]
            }],
        });
        self.write_max_tokens(&mut request_body, Some(max_tokens));

        let response = self.call_api(request_body).await?;

        let choice = response.choices.first().ok_or_else(|| {
            LlmError::InvalidResponse("No choices in vision response".to_string())
        })?;

        choice.message.content.clone().ok_or_else(|| {
            LlmError::InvalidResponse("Vision response contained no content".to_string())
        })
    }

    fn supports_vision(&self) -> bool {
        let m = self.model.to_lowercase();
        m.contains("gpt-4")
            || m.contains("gpt-5")
            || m.contains("vision")
            || m.contains("o1")
            || m.contains("o3")
            || m.contains("o4")
            || m.contains("llava")
            || m.contains("moondream")
            || m.contains("llama-3.2-vision")
            || m.contains("gemma3")
    }
}

// ---------------------------------------------------------------------------
// Whisper transcription support
// ---------------------------------------------------------------------------

/// Response from the OpenAI Whisper `verbose_json` endpoint.
#[derive(Debug, Deserialize)]
struct WhisperResponse {
    text: String,
    language: Option<String>,
    duration: Option<f32>,
}

/// Map a validated audio format extension to its MIME type.
fn audio_mime_type(format: &str) -> &'static str {
    match format {
        "mp3" | "mpeg" | "mpga" => "audio/mpeg",
        "mp4" | "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "webm" => "audio/webm",
        // validate_audio_format ensures only the above values reach here
        _ => "application/octet-stream",
    }
}

impl OpenAIAdapter {
    /// Call the Whisper transcription API with the same retry logic as `call_api`.
    #[instrument(
        name = "llm.transcription_api_call",
        level = "info",
        skip(self, form),
        fields(
            url = tracing::field::Empty,
            cognee.llm.model = self.transcription_model.as_str(),
            cognee.llm.provider = "openai",
        ),
    )]
    async fn call_transcription_api(
        &self,
        form: reqwest::multipart::Form,
    ) -> LlmResult<WhisperResponse> {
        let url = format!("{}/audio/transcriptions", self.base_url);
        let url_preview = bounded_error_preview(&url);
        tracing::Span::current().record("url", url_preview.as_str());

        // We cannot clone a multipart Form, so the first attempt uses the
        // original form and retries are not possible for the multipart body.
        // However, we keep the retry loop for network errors that occur
        // *before* the body is consumed (connection refused, DNS failure).
        // For simplicity and matching the guide's design, we rebuild the form
        // if needed by storing the bytes. But since `Form` doesn't support
        // Clone, we perform a single attempt with the form and rely on the
        // caller to retry externally if needed.
        //
        // Actually, the simplest approach is to send the form once and
        // handle retries at a higher level. But the guide says to mirror
        // call_api's retry. Since reqwest::multipart::Form is not Clone,
        // we accept `form` by value and do a single-shot request here,
        // while the `transcribe_audio` impl handles retry by rebuilding
        // the form on each attempt.

        let response = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .multipart(form)
            .send()
            .await
            .map_err(|error| {
                let preview = bounded_error_preview(&error.to_string());
                if error.is_builder() || error.is_redirect() {
                    LlmError::ConfigError(preview)
                } else {
                    LlmError::NetworkError(preview)
                }
            })?;

        let status = response.status();

        if !status.is_success() {
            let error_body = read_bounded_response_text(
                response,
                MAX_LLM_ERROR_BYTES,
                "OpenAI transcription error response",
            )
            .await?;
            let error_preview = bounded_error_preview(&error_body);

            if status.as_u16() == 429 && is_hard_quota_error(&error_body) {
                self.trip_quota_cooldown_at(now_ms());
                return Err(LlmError::RateLimitExceeded(error_preview));
            }

            let error = match status.as_u16() {
                401 => LlmError::AuthenticationError(error_preview),
                429 => LlmError::RateLimitExceeded(error_preview),
                400 => LlmError::InvalidResponse(format!("Bad request: {error_preview}")),
                _ => LlmError::ApiError(format!("HTTP {status}: {error_preview}")),
            };
            return if is_transient_http_status(status.as_u16())
                && !matches!(&error, LlmError::RateLimitExceeded(_))
            {
                Err(LlmError::NetworkError(bounded_error_preview(
                    &error.to_string(),
                )))
            } else {
                Err(error)
            };
        }

        let response_body = read_bounded_response_text(
            response,
            MAX_LLM_RESPONSE_BYTES,
            "OpenAI transcription response",
        )
        .await?;

        serde_json::from_str::<WhisperResponse>(&response_body).map_err(|e| {
            LlmError::DeserializationError(format!(
                "Failed to parse Whisper response: {e}. Raw body: {}",
                bounded_error_preview(&response_body)
            ))
        })
    }

    /// Build a `reqwest::multipart::Form` for a Whisper transcription request.
    fn build_transcription_form(
        &self,
        audio: &[u8],
        format: &str,
        language_hint: Option<&str>,
        prompt_hint: Option<&str>,
    ) -> LlmResult<reqwest::multipart::Form> {
        let mime = audio_mime_type(format);
        let filename = format!("audio.{format}");

        let file_part = reqwest::multipart::Part::bytes(audio.to_vec())
            .file_name(filename)
            .mime_str(mime)
            .map_err(|e| {
                LlmError::ConfigError(format!("Failed to set MIME type on multipart part: {e}"))
            })?;

        let mut form = reqwest::multipart::Form::new()
            .part("file", file_part)
            .text("model", self.transcription_model.clone())
            .text("response_format", "verbose_json");

        if let Some(lang) = language_hint {
            form = form.text("language", lang.to_string());
        }
        if let Some(prompt) = prompt_hint {
            form = form.text("prompt", prompt.to_string());
        }

        Ok(form)
    }
}

#[async_trait]
impl Transcriber for OpenAIAdapter {
    async fn transcribe_audio(
        &self,
        audio: &[u8],
        format: &str,
        language_hint: Option<&str>,
        prompt_hint: Option<&str>,
    ) -> LlmResult<TranscriptionOutput> {
        // Normalize and validate before any network I/O.
        let format_lower = format.to_ascii_lowercase();
        validate_audio_format(&format_lower)?;
        self.check_quota_cooldown()?;

        let mut last_error = LlmError::NetworkError("No attempt made".to_string());

        for attempt in 0..=self.network_retries {
            debug!(attempt, "Transcription API attempt");
            if attempt > 0 {
                let delay_ms = (1_000u64 * 2u64.saturating_pow(attempt as u32 - 1)).min(30_000);
                warn!(
                    attempt,
                    network_retries = self.network_retries,
                    delay_ms,
                    error = %last_error,
                    "Transcription request failed, retrying",
                );
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
            self.check_quota_cooldown()?;

            let form =
                self.build_transcription_form(audio, &format_lower, language_hint, prompt_hint)?;

            match self.call_transcription_api(form).await {
                Ok(resp) => {
                    return Ok(TranscriptionOutput {
                        text: resp.text,
                        language: resp.language,
                        duration: resp.duration,
                    });
                }
                Err(e) => {
                    if self.check_quota_cooldown().is_err() || !is_transient_llm_error(&e) {
                        return Err(e);
                    }
                    last_error = e;
                    continue;
                }
            }
        }

        Err(LlmError::MaxRetriesExceeded(format!(
            "Transcription request failed after {} attempt(s): {}",
            self.network_retries + 1,
            last_error
        )))
    }

    fn transcription_model(&self) -> &str {
        &self.transcription_model
    }
}

// OpenAI API response types
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIResponse {
    id: String,
    object: String,
    created: i64,
    model: String,
    choices: Vec<OpenAIChoice>,
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIChoice {
    index: u32,
    message: OpenAIMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIMessage {
    role: String,
    content: Option<String>,
    reasoning: Option<String>,
    function_call: Option<OpenAIFunctionCall>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct OpenAIUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "test code — panics are acceptable"
    )]
    use super::*;

    #[test]
    fn test_openai_provider_prefix_is_stripped() {
        // litellm-style "openai/<model>" must be sent as bare "<model>".
        let adapter = OpenAIAdapter::new("openai/gpt-5-mini", "test-key", None).unwrap();
        assert_eq!(adapter.model(), "gpt-5-mini");
        // Non-openai provider prefixes (custom endpoints) are left intact.
        let adapter = OpenAIAdapter::new("ollama/llama3", "test-key", None).unwrap();
        assert_eq!(adapter.model(), "ollama/llama3");
    }

    #[test]
    fn test_openai_adapter_creation() {
        let adapter = OpenAIAdapter::new("gpt-4", "test-key", None);
        assert!(adapter.is_ok());

        let adapter = adapter.unwrap();
        assert_eq!(adapter.model(), "gpt-4");
        assert_eq!(adapter.base_url, OpenAIAdapter::DEFAULT_BASE_URL);
        assert_eq!(OpenAIAdapter::DEFAULT_STRUCTURED_OUTPUT_RETRIES, 5);
    }

    #[test]
    fn responses_parser_accepts_event_first_sse() {
        let adapter = OpenAIAdapter::new("gpt-5", "test-key", None)
            .unwrap()
            .with_api_style("responses")
            .unwrap();
        let raw = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"indexed\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-5\",\"output\":[],\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}\n\n",
            "data: [DONE]\n"
        );

        let parsed = adapter.parse_responses_body(raw).unwrap();
        assert_eq!(parsed.id, "resp_1");
        assert_eq!(
            parsed.choices[0].message.content.as_deref(),
            Some("indexed")
        );
        let usage = parsed.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 3);
        assert_eq!(usage.completion_tokens, 1);
    }

    #[test]
    fn grok_proxy_predicate_rejects_lookalike_origins() {
        assert!(is_grok_cli_proxy_base_url(
            "https://cli-chat-proxy.grok.com/v1"
        ));
        assert!(is_grok_cli_proxy_base_url(
            "https://CLI-CHAT-PROXY.GROK.COM:443/v1/"
        ));

        for base_url in [
            "http://cli-chat-proxy.grok.com/v1",
            "https://cli-chat-proxy.grok.com.evil.example/v1",
            "https://evil.example/cli-chat-proxy.grok.com/v1",
            "https://user@cli-chat-proxy.grok.com/v1",
            "https://cli-chat-proxy.grok.com:444/v1",
            "https://cli-chat-proxy.grok.com/v1/chat/completions",
            "https://cli-chat-proxy.grok.com/v1?redirect=evil",
            "https://api.x.ai/v1",
        ] {
            assert!(
                !is_grok_cli_proxy_base_url(base_url),
                "accepted unsafe Grok proxy lookalike: {base_url}"
            );
        }
    }

    #[test]
    fn grok_request_forces_streaming_and_attaches_cli_headers() {
        let adapter = OpenAIAdapter::new(
            "grok-4.5",
            "subscription-token",
            Some("https://cli-chat-proxy.grok.com/v1/".to_string()),
        )
        .unwrap();
        let body = adapter
            .wire_request_body(json!({
                "model": "grok-4.5-fast",
                "messages": [{"role": "user", "content": "hello"}],
                "stream": false,
            }))
            .unwrap();
        assert_eq!(body.get("stream"), Some(&Value::Bool(true)));

        let request = adapter
            .build_api_request(&adapter.api_url(), &body)
            .build()
            .unwrap();
        assert_eq!(
            request.url().as_str(),
            "https://cli-chat-proxy.grok.com/v1/chat/completions"
        );
        assert_eq!(
            request
                .headers()
                .get("authorization")
                .unwrap()
                .to_str()
                .unwrap(),
            "Bearer subscription-token"
        );
        assert_eq!(
            request
                .headers()
                .get("x-xai-token-auth")
                .unwrap()
                .to_str()
                .unwrap(),
            GROK_TOKEN_AUTH
        );
        assert_eq!(
            request
                .headers()
                .get("x-grok-model-override")
                .unwrap()
                .to_str()
                .unwrap(),
            "grok-4.5-fast"
        );
        assert_eq!(
            request
                .headers()
                .get("x-grok-client-version")
                .unwrap()
                .to_str()
                .unwrap(),
            GROK_CLI_CLIENT_VERSION
        );
        let version: Vec<u32> = GROK_CLI_CLIENT_VERSION
            .split('.')
            .map(|part| part.parse().unwrap())
            .collect();
        assert!(version.as_slice() >= &[0_u32, 1, 202][..]);

        let encoded: Value = serde_json::from_slice(
            request
                .body()
                .and_then(reqwest::Body::as_bytes)
                .expect("JSON request body is buffered"),
        )
        .unwrap();
        assert_eq!(encoded.get("stream"), Some(&Value::Bool(true)));

        let structured = adapter
            .wire_request_body(json!({
                "model": "grok-4.5",
                "messages": [{"role": "user", "content": "extract"}],
                "response_format": {"type": "json_schema", "json_schema": {"schema": {"type": "object"}}},
                "reasoning": {"effort": "none"},
                "functions": [{
                    "name": "extract_structured_data",
                    "description": "extract",
                    "parameters": {"type": "object", "properties": {"value": {"type": "string"}}}
                }],
                "function_call": {"name": "extract_structured_data"}
            }))
            .unwrap();
        for unsupported in ["response_format", "reasoning", "functions", "function_call"] {
            assert!(
                structured.get(unsupported).is_none(),
                "Grok wire leaked unsupported field {unsupported}"
            );
        }
        assert_eq!(
            structured["tools"][0]["function"]["name"],
            "extract_structured_data"
        );
        assert_eq!(
            structured["tool_choice"]["function"]["name"],
            "extract_structured_data"
        );
        assert_eq!(structured["stream"], true);

        let prompt_json = adapter
            .wire_request_body(json!({
                "model": "grok-4.5",
                "messages": [{"role": "user", "content": "Return ONLY valid JSON."}],
                "response_format": {"type": "json_object"}
            }))
            .unwrap();
        assert!(prompt_json.get("response_format").is_none());
        assert_eq!(
            prompt_json["messages"][0]["content"],
            "Return ONLY valid JSON."
        );
    }

    #[test]
    fn generic_compatible_host_gets_no_grok_wire_changes() {
        let adapter = OpenAIAdapter::new(
            "grok-4.5",
            "ordinary-key",
            Some("https://grok-compatible.example/v1".to_string()),
        )
        .unwrap();
        let body = adapter
            .wire_request_body(json!({"model": "grok-4.5", "stream": false}))
            .unwrap();
        assert_eq!(body.get("stream"), Some(&Value::Bool(false)));

        let request = adapter
            .build_api_request(&adapter.api_url(), &body)
            .build()
            .unwrap();
        for header in [
            "x-xai-token-auth",
            "x-grok-model-override",
            "x-grok-client-version",
        ] {
            assert!(request.headers().get(header).is_none());
        }
    }

    #[test]
    fn grok_cli_proxy_rejects_responses_transport() {
        let error = OpenAIAdapter::new(
            "grok-4.5",
            "subscription-token",
            Some("https://cli-chat-proxy.grok.com/v1".to_string()),
        )
        .unwrap()
        .with_api_style("responses")
        .err()
        .expect("Grok CLI Responses transport must be rejected");
        assert!(error.to_string().contains("chat completions"));
    }

    #[test]
    fn grok_chat_sse_parser_collects_text_reasoning_and_usage() {
        let adapter = OpenAIAdapter::new(
            "grok-4.5",
            "test-key",
            Some("https://cli-chat-proxy.grok.com/v1".to_string()),
        )
        .unwrap();
        let raw = concat!(
            "data: {\"id\":\"chatcmpl_1\",\"created\":42,\"model\":\"grok-4.5-build\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"reasoning_content\":\"Think \"}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"PO\"}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"NG\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5}}\n\n",
            "data: [DONE]\n"
        );

        let response = adapter.parse_chat_completions_body(raw).unwrap();
        assert_eq!(response.id, "chatcmpl_1");
        assert_eq!(response.model, "grok-4.5-build");
        assert_eq!(response.created, 42);
        assert_eq!(response.choices[0].message.content.as_deref(), Some("PONG"));
        assert_eq!(
            response.choices[0].message.reasoning.as_deref(),
            Some("Think ")
        );
        assert_eq!(response.choices[0].finish_reason.as_deref(), Some("stop"));
        let usage = response.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 3);
        assert_eq!(usage.completion_tokens, 2);
        assert_eq!(usage.total_tokens, 5);
    }

    #[test]
    fn grok_chat_sse_parser_normalizes_tool_and_function_calls() {
        let adapter = OpenAIAdapter::new(
            "grok-4.5",
            "test-key",
            Some("https://cli-chat-proxy.grok.com/v1".to_string()),
        )
        .unwrap();
        let tool_stream = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"ext\",\"arguments\":\"{\\\"key\\\":\"}}]}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"ract\",\"arguments\":\"\\\"value\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n",
            "data: [DONE]\n"
        );
        let response = adapter.parse_chat_completions_body(tool_stream).unwrap();
        let function = response.choices[0].message.function_call.as_ref().unwrap();
        assert_eq!(function.name, "extract");
        assert_eq!(function.arguments, "{\"key\":\"value\"}");

        let function_stream = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"function_call\":{\"name\":\"save\",\"arguments\":\"{\"}}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"function_call\":{\"arguments\":\"}\"}},\"finish_reason\":\"function_call\"}]}\n",
            "data: [DONE]\n"
        );
        let response = adapter
            .parse_chat_completions_body(function_stream)
            .unwrap();
        let function = response.choices[0].message.function_call.as_ref().unwrap();
        assert_eq!(function.name, "save");
        assert_eq!(function.arguments, "{}");
    }

    #[test]
    fn chat_sse_errors_and_body_limits_fail_closed() {
        let adapter = OpenAIAdapter::new("gpt-4", "test-key", None).unwrap();
        let stream_error = adapter
            .parse_chat_completions_body(
                "data: {\"error\":{\"message\":\"subscription expired\"}}\n",
            )
            .unwrap_err();
        assert!(stream_error.to_string().contains("subscription expired"));

        assert!(ensure_content_length_with_limit(Some(9), 8, "fixture").is_err());
        let mut body = b"1234".to_vec();
        assert!(extend_bounded_body(&mut body, b"56789", 8, "fixture").is_err());
        assert_eq!(body, b"1234");
        assert!(
            adapter
                .parse_chat_completions_body_with_limit("data: {}\n", 4)
                .is_err()
        );
    }

    #[test]
    fn chat_sse_requires_well_formed_events_and_terminal_evidence() {
        let adapter = OpenAIAdapter::new("gpt-4", "test-key", None).unwrap();
        for (raw, expected) in [
            ("data: {not-json}\n", "Malformed"),
            ("data:\n", "empty `data:`"),
            ("data: [DONE]\n", "no completion choice"),
            (
                "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}]}\n",
                "without `[DONE]`",
            ),
            (
                "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"mystery\"}]}\n",
                "invalid finish reason",
            ),
            (
                "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\ndata: [DONE]\ndata: {}\n",
                "after `[DONE]`",
            ),
        ] {
            let error = adapter.parse_chat_completions_body(raw).unwrap_err();
            assert!(
                error.to_string().contains(expected),
                "expected `{expected}` in `{error}`"
            );
        }

        let terminal_without_done = adapter
            .parse_chat_completions_body(
                "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"complete\"},\"finish_reason\":\"stop\"}]}\n",
            )
            .unwrap();
        assert_eq!(
            terminal_without_done.choices[0].message.content.as_deref(),
            Some("complete")
        );
    }

    #[test]
    fn chat_sse_rejects_incomplete_structured_calls() {
        let adapter = OpenAIAdapter::new("gpt-4", "test-key", None).unwrap();
        for raw in [
            concat!(
                "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"extract\",\"arguments\":\"{\\\"value\\\":\"}}]}}]}\n",
                "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n",
                "data: [DONE]\n"
            ),
            concat!(
                "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{}\"}}]}}]}\n",
                "data: [DONE]\n"
            ),
            concat!(
                "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n",
                "data: [DONE]\n"
            ),
        ] {
            assert!(adapter.parse_chat_completions_body(raw).is_err());
        }
    }

    #[test]
    fn responses_sse_rejects_malformed_or_truncated_streams() {
        let adapter = OpenAIAdapter::new("gpt-5", "test-key", None)
            .unwrap()
            .with_api_style("responses")
            .unwrap();
        assert!(adapter.parse_responses_body("data: {bad}\n").is_err());
        assert!(
            adapter
                .parse_responses_body(
                    "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n"
                )
                .is_err()
        );
        assert!(adapter.parse_responses_body("data: [DONE]\n").is_err());
    }

    #[test]
    fn responses_sse_rejects_data_after_completion() {
        let adapter = OpenAIAdapter::new("gpt-5", "test-key", None)
            .unwrap()
            .with_api_style("responses")
            .unwrap();
        let raw = concat!(
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-5\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"late\"}\n",
            "data: [DONE]\n"
        );
        let error = adapter.parse_responses_body(raw).unwrap_err();
        assert!(error.to_string().contains("after `response.completed`"));
    }

    #[test]
    fn responses_sse_rejects_response_incomplete_events() {
        let adapter = OpenAIAdapter::new("gpt-5", "test-key", None)
            .unwrap()
            .with_api_style("responses")
            .unwrap();
        let raw = concat!(
            "data: {\"type\":\"response.incomplete\",\"response\":{\"id\":\"resp_1\",\"status\":\"incomplete\"}}\n",
            "data: [DONE]\n"
        );
        let error = adapter.parse_responses_body(raw).unwrap_err();
        assert!(error.to_string().contains("ended incomplete"));
    }

    #[test]
    fn responses_reject_incomplete_function_items_and_terminal_statuses() {
        let adapter = OpenAIAdapter::new("gpt-5", "test-key", None)
            .unwrap()
            .with_api_style("responses")
            .unwrap();

        let final_incomplete = json!({
            "id": "resp_1",
            "status": "incomplete",
            "model": "gpt-5",
            "output": [],
            "usage": {"input_tokens": 1, "output_tokens": 0}
        })
        .to_string();
        let error = adapter.parse_responses_body(&final_incomplete).unwrap_err();
        assert!(error.to_string().contains("status `incomplete`"));

        let sse_incomplete_function = concat!(
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"status\":\"incomplete\",\"name\":\"extract\",\"arguments\":\"\"}}\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_2\",\"status\":\"completed\",\"model\":\"gpt-5\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n",
            "data: [DONE]\n"
        );
        let error = adapter
            .parse_responses_body(sse_incomplete_function)
            .unwrap_err();
        assert!(error.to_string().contains("status `incomplete`"));

        let sse_missing_arguments = concat!(
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"status\":\"completed\",\"name\":\"extract\",\"arguments\":\"\"}}\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_3\",\"status\":\"completed\",\"model\":\"gpt-5\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n",
            "data: [DONE]\n"
        );
        let error = adapter
            .parse_responses_body(sse_missing_arguments)
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("function call item was incomplete")
        );
    }

    #[test]
    fn structured_output_retry_setter_is_source_compatible_but_deterministic() {
        let adapter = OpenAIAdapter::new("gpt-4", "test-key", None)
            .unwrap()
            .with_structured_output_retries(5);
        assert_eq!(adapter.model(), "gpt-4");

        let adapter = OpenAIAdapter::new("gpt-4", "test-key", None)
            .unwrap()
            .with_structured_output_retries(0);
        assert_eq!(adapter.model(), "gpt-4");
    }

    #[test]
    fn retry_classification_is_transient_only_and_bounded() {
        for status in [408, 409, 425, 429, 500, 502, 503, 504, 599] {
            assert!(is_transient_http_status(status), "status {status}");
        }
        for status in [200, 400, 401, 402, 403, 404, 405, 413, 422, 426, 451] {
            assert!(!is_transient_http_status(status), "status {status}");
        }
        let adapter = OpenAIAdapter::new("gpt-4", "test-key", None)
            .unwrap()
            .with_network_retries(u32::MAX);
        assert_eq!(adapter.network_retries, MAX_NETWORK_RETRIES);
    }

    #[test]
    fn quota_breaker_is_shared_by_clones_but_isolated_between_accounts() {
        let now = 10_000;
        let first = OpenAIAdapter::new(
            "gpt-4",
            "account-a",
            Some("https://api.example/v1".to_string()),
        )
        .unwrap();
        let first_clone = first.clone();
        let second_account = OpenAIAdapter::new(
            "gpt-4",
            "account-b",
            Some("https://api.example/v1".to_string()),
        )
        .unwrap();
        let other_endpoint = OpenAIAdapter::new(
            "gpt-4",
            "account-a",
            Some("https://other.example/v1".to_string()),
        )
        .unwrap();

        assert!(first.trip_quota_cooldown_at(now));
        assert!(first.check_quota_cooldown_at(now + 1).is_err());
        assert!(first_clone.check_quota_cooldown_at(now + 1).is_err());
        assert!(second_account.check_quota_cooldown_at(now + 1).is_ok());
        assert!(other_endpoint.check_quota_cooldown_at(now + 1).is_ok());
    }

    #[tokio::test]
    async fn queued_call_rechecks_quota_after_acquiring_its_permit() {
        let adapter = OpenAIAdapter::new("gpt-4", "queued-account", None).unwrap();
        let semaphore = tokio::sync::Semaphore::new(1);
        let held = semaphore.acquire().await.unwrap();

        let waiting = adapter.acquire_llm_permit_and_recheck(&semaphore);
        let tripping = async {
            tokio::task::yield_now().await;
            adapter.trip_quota_cooldown_at(now_ms());
            drop(held);
        };
        let (result, ()) = tokio::join!(waiting, tripping);
        assert!(matches!(result, Err(LlmError::RateLimitExceeded(_))));
    }

    #[test]
    fn structured_diagnostics_bound_large_invalid_arguments() {
        let raw = "x".repeat(MAX_ERROR_PREVIEW_BYTES * 4);
        let parse_error = serde_json::from_str::<Value>(&raw).unwrap_err();
        let rendered = structured_parse_error("fixture", &parse_error, &raw).to_string();
        assert!(rendered.contains("truncated"));
        assert!(rendered.len() <= MAX_ERROR_PREVIEW_BYTES + 512);
    }

    #[test]
    fn test_openai_adapter_custom_base_url() {
        let adapter = OpenAIAdapter::new(
            "gpt-4",
            "test-key",
            Some("https://custom.api.com/v1".to_string()),
        );
        assert!(adapter.is_ok());

        let adapter = adapter.unwrap();
        assert_eq!(adapter.base_url, "https://custom.api.com/v1");
    }

    #[test]
    fn test_base_url_trailing_slash_is_normalized() {
        // The Gemini OpenAI-compat base ends in `/`; without normalisation the
        // request URL would be `.../openai//chat/completions` and 404.
        let adapter = OpenAIAdapter::new(
            "gemini-2.0-flash",
            "test-key",
            Some("https://generativelanguage.googleapis.com/v1beta/openai/".to_string()),
        )
        .unwrap();
        assert_eq!(
            adapter.base_url,
            "https://generativelanguage.googleapis.com/v1beta/openai"
        );
    }

    #[test]
    fn test_is_reasoning_model_matches_openai_families() {
        let cases = [
            ("gpt-5", true),
            ("gpt-5-mini", true),
            ("gpt-5-2025-06-01", true),
            ("o1", true),
            ("o1-mini", true),
            ("o3", true),
            ("o3-mini", true),
            ("o4-mini", true),
            ("GPT-5-Mini", true),
            ("gpt-4o-mini", false),
            ("gpt-4-turbo", false),
            ("gpt-3.5-turbo", false),
            ("o-foo", false),
        ];
        for (model, expected) in cases {
            let adapter = OpenAIAdapter::new(model, "test-key", None).unwrap();
            assert_eq!(
                adapter.is_reasoning_model(),
                expected,
                "is_reasoning_model({model})"
            );
        }
    }

    #[test]
    fn test_is_reasoning_model_skipped_for_custom_base_url() {
        // Custom OpenAI-compatible endpoints (Ollama, vLLM, …) may have
        // model names that look like reasoning families but still accept
        // legacy sampling parameters — the gate is conservative.
        let adapter = OpenAIAdapter::new(
            "gpt-5-mini",
            "test-key",
            Some("http://localhost:11434/v1".to_string()),
        )
        .unwrap();
        assert!(!adapter.is_reasoning_model());
    }

    #[test]
    fn test_write_max_tokens_renames_key_for_reasoning_models() {
        let mut body = json!({"model": "gpt-5-mini"});
        let reasoning = OpenAIAdapter::new("gpt-5-mini", "test-key", None).unwrap();
        reasoning.write_max_tokens(&mut body, Some(2048));
        assert!(body.get("max_tokens").is_none());
        assert_eq!(body["max_completion_tokens"], 2048);

        let mut body = json!({"model": "gpt-4o-mini"});
        let classic = OpenAIAdapter::new("gpt-4o-mini", "test-key", None).unwrap();
        classic.write_max_tokens(&mut body, Some(2048));
        assert_eq!(body["max_tokens"], 2048);
        assert!(body.get("max_completion_tokens").is_none());

        // None leaves body untouched.
        let mut body = json!({"model": "gpt-5-mini"});
        reasoning.write_max_tokens(&mut body, None);
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("max_completion_tokens").is_none());
    }

    #[test]
    fn test_message_conversion() {
        let messages = vec![
            Message {
                role: MessageRole::System,
                content: "You are helpful".to_string(),
            },
            Message {
                role: MessageRole::User,
                content: "Hello".to_string(),
            },
        ];

        let converted = OpenAIAdapter::convert_messages(&messages);
        assert_eq!(converted.len(), 2);
        assert_eq!(converted[0]["role"], "system");
        assert_eq!(converted[0]["content"], "You are helpful");
        assert_eq!(converted[1]["role"], "user");
        assert_eq!(converted[1]["content"], "Hello");
    }

    #[test]
    fn test_context_length() {
        let adapter = OpenAIAdapter::new("gpt-4-turbo-preview", "key", None).unwrap();
        assert_eq!(adapter.max_context_length(), 128_000);

        let adapter = OpenAIAdapter::new("gpt-4", "key", None).unwrap();
        assert_eq!(adapter.max_context_length(), 8_192);

        let adapter = OpenAIAdapter::new("gpt-3.5-turbo-16k", "key", None).unwrap();
        assert_eq!(adapter.max_context_length(), 16_384);
    }

    #[test]
    fn test_supports_vision_gpt4o() {
        let adapter = OpenAIAdapter::new("gpt-4o", "key", None).unwrap();
        assert!(adapter.supports_vision());
    }

    #[test]
    fn test_supports_vision_gpt4_turbo() {
        let adapter = OpenAIAdapter::new("gpt-4-turbo", "key", None).unwrap();
        assert!(adapter.supports_vision());
    }

    #[test]
    fn test_supports_vision_gpt4o_mini() {
        let adapter = OpenAIAdapter::new("gpt-4o-mini", "key", None).unwrap();
        assert!(adapter.supports_vision());
    }

    #[test]
    fn test_supports_vision_gpt35_is_false() {
        let adapter = OpenAIAdapter::new("gpt-3.5-turbo", "key", None).unwrap();
        assert!(!adapter.supports_vision());
    }

    #[test]
    fn test_supports_vision_llava() {
        let adapter = OpenAIAdapter::new("llava:13b", "key", None).unwrap();
        assert!(adapter.supports_vision());
    }

    #[test]
    fn test_supports_vision_o1() {
        let adapter = OpenAIAdapter::new("o1-preview", "key", None).unwrap();
        assert!(adapter.supports_vision());
    }

    #[test]
    fn test_supports_vision_gemma3() {
        let adapter = OpenAIAdapter::new("gemma3:12b", "key", None).unwrap();
        assert!(adapter.supports_vision());
    }

    #[tokio::test]
    async fn transcribe_image_rejects_non_image_mime() {
        let adapter = OpenAIAdapter::new("gpt-4o", "fake-key", None).unwrap();
        let result = adapter
            .transcribe_image(b"not-an-image", "text/plain", None)
            .await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), LlmError::InvalidResponse(_)),
            "Expected InvalidResponse for non-image MIME type"
        );
    }

    #[test]
    fn test_transcription_model_default() {
        // Clear the env var to test the default value.
        // SAFETY: This test is single-threaded and no other thread reads
        // TRANSCRIPTION_MODEL concurrently.
        unsafe { std::env::remove_var("TRANSCRIPTION_MODEL") };
        let adapter = OpenAIAdapter::new("gpt-4", "key", None).unwrap();
        assert_eq!(adapter.transcription_model(), "whisper-1");
    }

    #[test]
    fn test_transcription_model_custom() {
        let adapter = OpenAIAdapter::new("gpt-4", "key", None)
            .unwrap()
            .with_transcription_model("whisper-large-v3");
        assert_eq!(adapter.transcription_model(), "whisper-large-v3");
    }

    #[test]
    fn test_audio_mime_type_mapping() {
        assert_eq!(audio_mime_type("mp3"), "audio/mpeg");
        assert_eq!(audio_mime_type("mpeg"), "audio/mpeg");
        assert_eq!(audio_mime_type("mpga"), "audio/mpeg");
        assert_eq!(audio_mime_type("mp4"), "audio/mp4");
        assert_eq!(audio_mime_type("m4a"), "audio/mp4");
        assert_eq!(audio_mime_type("wav"), "audio/wav");
        assert_eq!(audio_mime_type("webm"), "audio/webm");
    }

    #[test]
    fn test_to_strict_schema_marks_all_required_and_closes_objects() {
        // Mirrors the schemars-0.8 shape: an optional field omitted from
        // `required`, nested object behind `definitions`/`$ref`, and no
        // `additionalProperties` set anywhere.
        let schema = json!({
            "type": "object",
            "properties": {
                "nodes": { "type": "array", "items": { "$ref": "#/definitions/Node" } }
            },
            "required": ["nodes"],
            "definitions": {
                "Node": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "type": { "type": "string" },
                        "description": { "type": ["string", "null"] }
                    },
                    "required": ["name", "type"]
                }
            }
        });

        let strict = to_strict_schema(&schema);

        // Root object closed + all props required.
        assert_eq!(strict["additionalProperties"], json!(false));
        assert_eq!(strict["required"], json!(["nodes"]));

        // Nested object inside definitions: every property now required
        // (including the previously-optional `description`) and closed.
        let node = &strict["definitions"]["Node"];
        assert_eq!(node["additionalProperties"], json!(false));
        let mut req: Vec<String> = node["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        req.sort();
        assert_eq!(req, vec!["description", "name", "type"]);
    }
}
