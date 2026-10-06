//! Provider connectivity check using Phoenix config.

use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::{json, Value};

use crate::config::PhoenixConfig;
use crate::debug_session;
use crate::providers::contracts::{ChatMessage, CompletionRequest};
use crate::providers::ProviderFactory;

const CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const CHECK_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Connectivity probes request only a few tokens. A larger response is a
/// broken/malicious upstream, not useful diagnostic data.
const CHECK_RESPONSE_MAX_BYTES: usize = 1024 * 1024;
const CHECK_ERROR_PREVIEW_CHARS: usize = 500;

fn check_body_preview(raw: &str) -> String {
    let preview: String = raw
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .take(CHECK_ERROR_PREVIEW_CHARS)
        .collect();
    if raw.chars().count() > CHECK_ERROR_PREVIEW_CHARS {
        format!("{preview}...[truncated]")
    } else {
        preview
    }
}

async fn read_check_response_text(response: reqwest::Response, label: &str) -> Result<String> {
    let status = response.status();
    crate::providers::read_response_text(response, CHECK_RESPONSE_MAX_BYTES, label)
        .await
        .with_context(|| format!("failed to read {label} (HTTP {status})"))
}

fn parse_check_json(raw: &str, status: reqwest::StatusCode, label: &str) -> Result<Value> {
    let body: Value = serde_json::from_str(raw).with_context(|| {
        format!(
            "{label} returned invalid JSON (HTTP {status}): {}",
            check_body_preview(raw)
        )
    })?;
    if status.is_success() && body.get("error").is_some_and(|error| !error.is_null()) {
        anyhow::bail!(
            "{label} reported an error despite HTTP {status}: {}",
            check_json_error_preview(&body, raw)
        );
    }
    Ok(body)
}

fn check_json_error_preview(body: &Value, raw: &str) -> String {
    let message = body
        .pointer("/error/message")
        .or_else(|| body.get("error"))
        .or_else(|| body.get("message"))
        .and_then(Value::as_str)
        .unwrap_or(raw);
    check_body_preview(message)
}

#[derive(Debug, Clone)]
pub struct CheckResult {
    pub provider_id: String,
    pub model_id: String,
    pub endpoint: String,
    pub auth_method: String,
    pub auth_source: String,
    pub response_preview: String,
}

pub async fn run_check(config: &PhoenixConfig) -> Result<CheckResult> {
    run_with_check_timeout(CHECK_TIMEOUT, run_check_inner(config)).await
}

async fn run_with_check_timeout<T>(
    timeout: std::time::Duration,
    future: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    match tokio::time::timeout(timeout, future).await {
        Ok(result) => result,
        Err(_) => anyhow::bail!(
            "provider connectivity check timed out after {}s",
            timeout.as_secs()
        ),
    }
}

async fn run_check_inner(config: &PhoenixConfig) -> Result<CheckResult> {
    let factory = ProviderFactory::new();
    let resolved = factory.resolve_llm_profile(&config.profile.llm)?;

    let client = Client::builder()
        .connect_timeout(CHECK_CONNECT_TIMEOUT)
        .timeout(CHECK_TIMEOUT)
        .build()?;
    let preview = match resolved.provider_id.as_str() {
        "anthropic" => {
            check_anthropic(
                &client,
                &resolved.base_url,
                &resolved.model_id,
                resolved.auth.credential.as_deref().unwrap_or(""),
            )
            .await?
        }
        "google" => {
            check_google(
                &client,
                &resolved.base_url,
                &resolved.model_id,
                resolved.auth.credential.as_deref().unwrap_or(""),
            )
            .await?
        }
        "ollama" => {
            check_openai_compatible(&client, &resolved.base_url, &resolved.model_id, None).await?
        }
        "ollama-cloud" => {
            check_ollama_cloud(
                &client,
                &resolved.model_id,
                resolved.auth.credential.as_deref().unwrap_or(""),
            )
            .await?
        }
        "opencode" => {
            check_opencode(
                &client,
                &resolved.base_url,
                &resolved.model_id,
                resolved.auth.credential.as_deref().unwrap_or(""),
            )
            .await?
        }
        "openai-codex" => {
            check_openai_responses(
                &client,
                &resolved.base_url,
                &resolved.model_id,
                resolved.auth.credential.as_deref().unwrap_or(""),
            )
            .await?
        }
        "openai" | "openrouter" | "tokenrouter" | "deepseek" | "minimax-portal" => {
            check_openai_compatible(
                &client,
                &resolved.base_url,
                &resolved.model_id,
                resolved.auth.credential.as_deref(),
            )
            .await?
        }
        // Everything else: ask the provider itself instead of hand-rolling its
        // wire format. Every provider already implements `complete`, so this
        // reaches the same endpoint, auth and headers the agent loop uses —
        // which is the thing the check is actually asked to prove, and closer
        // to it than a bespoke arm that can drift from the real client.
        //
        // This used to `bail!`, and the bail took the WHOLE command down rather
        // than one lane with it: `run_check` resolves a single profile, so a
        // config whose main lane is an unlisted provider made `phoenix check`
        // unrunnable. That is exactly what happened once the live lane moved to
        // `grok-cli` — the one command the runbook says to run BEFORE any
        // hands-on agent test could not be run at all, on the day the lanes
        // were down.
        _ => {
            let provider = factory.build_llm_provider(&config.profile.llm)?;
            let mut request = CompletionRequest::new(
                &resolved.model_id,
                vec![ChatMessage::user("Reply with the single word: phoenix")],
            );
            request.max_tokens = Some(16);
            let content = provider.complete(request).await?.content;
            let preview = content.trim();
            if preview.is_empty() {
                // A 200 carrying no text still proves the lane is reachable and
                // authorized, which is what the check asks. Say so rather than
                // printing a blank and looking like a failure.
                "(connected; model returned no text)".to_string()
            } else {
                preview.chars().take(200).collect()
            }
        }
    };

    let auth_source = resolved.auth.source_summary();
    let auth_method = resolved.auth.method.clone();

    // #region agent log
    debug_session::log(
        "B",
        "check.rs:run_check",
        "check completed",
        serde_json::json!({
            "provider_id": resolved.provider_id,
            "model_id": resolved.model_id,
            "auth_source": auth_source,
            "auth_method": auth_method,
            "has_credential": resolved.auth.credential.as_ref().is_some_and(|c| !c.is_empty()),
        }),
    );
    // #endregion

    Ok(CheckResult {
        provider_id: resolved.provider_id,
        model_id: resolved.model_id,
        endpoint: resolved.base_url,
        auth_method,
        auth_source,
        response_preview: preview,
    })
}

/// OpenRouter free-tier 429 with shared upstream limits (not a missing Phoenix API key).
pub fn is_openrouter_free_tier_rate_limit(message: &str) -> bool {
    message.contains("429")
        && message.contains("is_byok")
        && (message.contains("is_byok\":false") || message.contains("is_byok\": false"))
}

pub fn explain_openrouter_rate_limit_error(model: &str, api_key_sent: bool) -> String {
    format!(
        "OpenRouter accepted your API key but rate-limited the free model '{model}' on shared upstream capacity \
         (metadata is_byok=false). Phoenix {} send Authorization with your OPENROUTER_API_KEY. \
         This is not fixed by re-exporting the env var alone — raise limits by adding provider keys at \
         https://openrouter.ai/settings/integrations, retry later, or choose a paid/non-free model."
        ,
        if api_key_sent { "did" } else { "did NOT" }
    )
}

/// Ollama Cloud uses the native `POST /api/chat` API (not OpenAI-compat for all models).
async fn check_ollama_cloud(client: &Client, model: &str, api_key: &str) -> Result<String> {
    let url = "https://ollama.com/api/chat";
    let api_key_sent = !api_key.is_empty();

    // #region agent log
    debug_session::log(
        "F",
        "check.rs:check_ollama_cloud",
        "sending ollama cloud check",
        serde_json::json!({
            "url": url,
            "model": model,
            "api_key_sent": api_key_sent,
        }),
    );
    // #endregion

    let resp = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&json!({
            "model": model,
            "messages": [{"role": "user", "content": "Reply with the single word: phoenix"}],
            "stream": false
        }))
        .send()
        .await?;

    let status = resp.status();
    let raw = read_check_response_text(resp, "Ollama Cloud check response").await?;

    if !status.is_success() {
        // #region agent log
        debug_session::log(
            "G",
            "check.rs:check_ollama_cloud",
            "ollama cloud HTTP error",
            serde_json::json!({
                "status": status.as_u16(),
                "model": model,
                "body_prefix": check_body_preview(&raw),
            }),
        );
        // #endregion
        if status.as_u16() == 401 {
            anyhow::bail!(
                "Ollama Cloud auth failed (401 Unauthorized). Check your OLLAMA_API_KEY."
            );
        }
        let detail = serde_json::from_str::<Value>(&raw)
            .ok()
            .map(|body| check_json_error_preview(&body, &raw))
            .unwrap_or_else(|| check_body_preview(&raw));
        anyhow::bail!("Ollama Cloud returned {status}: {detail}");
    }

    let body = parse_check_json(&raw, status, "Ollama Cloud")?;

    let text = body["message"]["content"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();
    Ok(text)
}

async fn check_openai_compatible(
    client: &Client,
    base_url: &str,
    model: &str,
    api_key: Option<&str>,
) -> Result<String> {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let mut req = client.post(&url).header("Content-Type", "application/json");
    let api_key_sent = api_key.is_some_and(|k| !k.is_empty());

    if let Some(key) = api_key {
        req = req.header("Authorization", format!("Bearer {}", key));
    }

    // #region agent log
    debug_session::log(
        "C",
        "check.rs:check_openai_compatible",
        "sending provider check request",
        serde_json::json!({
            "url": url,
            "model": model,
            "api_key_sent": api_key_sent,
        }),
    );
    // #endregion

    let resp = req
        .json(&json!({
            "model": model,
            "messages": [{"role": "user", "content": "Reply with the single word: phoenix"}],
            "max_tokens": 16,
            "stream": false
        }))
        .send()
        .await?;

    let status = resp.status();
    let raw = read_check_response_text(resp, "provider check response").await?;

    if !status.is_success() {
        if status.as_u16() == 401 {
            anyhow::bail!("Auth failed (401 Unauthorized). Check your API key for this provider.");
        }
        // Try JSON parse for structured error info (e.g. OpenRouter 429 BYOK)
        let body: Option<serde_json::Value> = serde_json::from_str(&raw).ok();
        if let Some(ref body) = body {
            let is_byok = body
                .pointer("/error/metadata/is_byok")
                .and_then(|v| v.as_bool());
            let user_id = body
                .get("user_id")
                .or_else(|| body.pointer("/error/user_id"))
                .and_then(|v| v.as_str());
            let provider_name = body
                .pointer("/error/metadata/provider_name")
                .and_then(|v| v.as_str());
            // #region agent log
            debug_session::log(
                "D",
                "check.rs:check_openai_compatible",
                "provider check HTTP error",
                serde_json::json!({
                    "status": status.as_u16(),
                    "model": model,
                    "api_key_sent": api_key_sent,
                    "is_byok": is_byok,
                    "user_id": user_id,
                    "provider_name": provider_name,
                }),
            );
            // #endregion
            if status.as_u16() == 429 && is_byok == Some(false) {
                anyhow::bail!(
                    "{} Raw: {}",
                    explain_openrouter_rate_limit_error(model, api_key_sent),
                    check_body_preview(&body.to_string())
                );
            }
        }
        let detail = body
            .as_ref()
            .map(|body| check_json_error_preview(body, &raw))
            .unwrap_or_else(|| check_body_preview(&raw));
        anyhow::bail!("Provider returned {status}: {detail}");
    }

    let body = parse_check_json(&raw, status, "provider")?;

    let text = body["choices"]
        .as_array()
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    Ok(text)
}

async fn check_opencode(
    client: &Client,
    base_url: &str,
    model: &str,
    api_key: &str,
) -> Result<String> {
    let normalized = model.trim().trim_start_matches("opencode/");

    if normalized.starts_with("claude-") || normalized == "big-pickle" {
        return check_anthropic_compatible_with_bearer(client, base_url, normalized, api_key).await;
    }

    if normalized.starts_with("gemini-") {
        return check_google_compatible_with_bearer(client, base_url, normalized, api_key).await;
    }

    if normalized.starts_with("gpt-") {
        return check_openai_responses(client, base_url, normalized, api_key).await;
    }

    check_openai_compatible(client, base_url, normalized, Some(api_key)).await
}

async fn check_anthropic(
    client: &Client,
    base_url: &str,
    model: &str,
    api_key: &str,
) -> Result<String> {
    let url = format!("{}/messages", base_url.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .json(&json!({
            "model": model,
            "max_tokens": 16,
            "messages": [{"role": "user", "content": "Reply with the single word: phoenix"}]
        }))
        .send()
        .await?;

    let status = resp.status();
    let raw = read_check_response_text(resp, "Anthropic check response").await?;
    let body = parse_check_json(&raw, status, "Anthropic")?;
    if !status.is_success() {
        anyhow::bail!(
            "Provider returned {status}: {}",
            check_json_error_preview(&body, &raw)
        );
    }

    let text = body["content"]
        .as_array()
        .and_then(|content| content.first())
        .and_then(|item| item.get("text"))
        .and_then(|text| text.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    Ok(text)
}

async fn check_anthropic_compatible_with_bearer(
    client: &Client,
    base_url: &str,
    model: &str,
    api_key: &str,
) -> Result<String> {
    let url = format!("{}/messages", base_url.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .json(&json!({
            "model": model,
            "max_tokens": 16,
            "messages": [{"role": "user", "content": "Reply with the single word: phoenix"}]
        }))
        .send()
        .await?;

    let status = resp.status();
    let raw = read_check_response_text(resp, "Anthropic-compatible check response").await?;
    let body = parse_check_json(&raw, status, "Anthropic-compatible provider")?;
    if !status.is_success() {
        anyhow::bail!(
            "Provider returned {status}: {}",
            check_json_error_preview(&body, &raw)
        );
    }

    let text = body["content"]
        .as_array()
        .and_then(|content| content.first())
        .and_then(|item| item.get("text"))
        .and_then(|text| text.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    Ok(text)
}

async fn check_google(
    client: &Client,
    base_url: &str,
    model: &str,
    api_key: &str,
) -> Result<String> {
    let url = format!(
        "{}/models/{}:generateContent",
        base_url.trim_end_matches('/'),
        model
    );
    let resp = client
        .post(&url)
        .header("x-goog-api-key", api_key)
        .header("Content-Type", "application/json")
        .json(&json!({
            "contents": [{
                "parts": [{"text": "Reply with the single word: phoenix"}]
            }]
        }))
        .send()
        .await?;

    let status = resp.status();
    let raw = read_check_response_text(resp, "Google check response").await?;
    let body = parse_check_json(&raw, status, "Google")?;
    if !status.is_success() {
        anyhow::bail!(
            "Provider returned {status}: {}",
            check_json_error_preview(&body, &raw)
        );
    }

    let text = body["candidates"]
        .as_array()
        .and_then(|candidates| candidates.first())
        .and_then(|candidate| candidate.get("content"))
        .and_then(|content| content.get("parts"))
        .and_then(|parts| parts.as_array())
        .and_then(|parts| parts.first())
        .and_then(|part| part.get("text"))
        .and_then(|text| text.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    Ok(text)
}

async fn check_google_compatible_with_bearer(
    client: &Client,
    base_url: &str,
    model: &str,
    api_key: &str,
) -> Result<String> {
    let url = format!(
        "{}/models/{}:generateContent",
        base_url.trim_end_matches('/'),
        model
    );
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&json!({
            "contents": [{
                "parts": [{"text": "Reply with the single word: phoenix"}]
            }]
        }))
        .send()
        .await?;

    let status = resp.status();
    let raw = read_check_response_text(resp, "Google-compatible check response").await?;
    let body = parse_check_json(&raw, status, "Google-compatible provider")?;
    if !status.is_success() {
        anyhow::bail!(
            "Provider returned {status}: {}",
            check_json_error_preview(&body, &raw)
        );
    }

    let text = body["candidates"]
        .as_array()
        .and_then(|candidates| candidates.first())
        .and_then(|candidate| candidate.get("content"))
        .and_then(|content| content.get("parts"))
        .and_then(|parts| parts.as_array())
        .and_then(|parts| parts.first())
        .and_then(|part| part.get("text"))
        .and_then(|text| text.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    Ok(text)
}

async fn check_openai_responses(
    client: &Client,
    base_url: &str,
    model: &str,
    api_key: &str,
) -> Result<String> {
    let url = format!("{}/responses", base_url.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&json!({
            "model": model,
            "instructions": "You are PhoenixAgent. Reply exactly as requested.",
            "input": [{
                "role": "user",
                "content": [{"type": "input_text", "text": "Reply with the single word: phoenix"}]
            }],
            "store": false,
            "stream": true
        }))
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let raw = read_check_response_text(resp, "Responses API check error").await?;
        let body = parse_check_json(&raw, status, "Responses API")?;
        anyhow::bail!(
            "Provider returned {status}: {}",
            check_json_error_preview(&body, &raw)
        );
    }

    let text = read_openai_responses_stream(resp).await?;

    Ok(text)
}

async fn read_openai_responses_stream(resp: reqwest::Response) -> Result<String> {
    let body = read_check_response_text(resp, "Responses API check stream").await?;
    let normalized = body.replace("\r\n", "\n");
    let mut output = String::new();
    let mut final_response: Option<Value> = None;

    for frame in normalized
        .split("\n\n")
        .filter(|frame| !frame.trim().is_empty())
    {
        handle_openai_responses_stream_frame(frame, &mut output, &mut final_response)?;
    }

    if output.is_empty() {
        if let Some(response) = final_response {
            output = extract_openai_responses_text(&response);
        }
    }

    Ok(output.trim().to_string())
}

fn handle_openai_responses_stream_frame(
    frame: &str,
    output: &mut String,
    final_response: &mut Option<Value>,
) -> Result<()> {
    let data = frame
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim_start)
        .collect::<Vec<_>>()
        .join("\n");

    if data.is_empty() || data.trim() == "[DONE]" {
        return Ok(());
    }

    let json = serde_json::from_str::<Value>(&data).with_context(|| {
        format!(
            "Responses API check stream contained invalid JSON: {}",
            check_body_preview(&data)
        )
    })?;

    let event_type = json.get("type").and_then(Value::as_str).unwrap_or_default();
    if event_type.contains("error") || json.get("error").is_some() {
        anyhow::bail!(
            "Responses API check stream returned an error: {}",
            check_json_error_preview(&json, &data)
        );
    }
    if event_type.contains("output_text.delta") {
        if let Some(delta) = json.get("delta").and_then(Value::as_str) {
            output.push_str(delta);
        }
        return Ok(());
    }

    if event_type.contains("output_text.done") {
        if let Some(text) = json.get("text").and_then(Value::as_str) {
            *output = text.to_string();
        }
        return Ok(());
    }

    if event_type.contains("completed") || event_type.contains("response.done") {
        if let Some(response) = json.get("response") {
            *final_response = Some(response.clone());
        }
    }
    Ok(())
}

fn extract_openai_responses_text(body: &Value) -> String {
    body["output_text"]
        .as_str()
        .map(str::to_string)
        .or_else(|| {
            body["output"].as_array().map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("content").and_then(Value::as_array))
                    .flatten()
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("")
            })
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connectivity_check_deadline_cancels_a_stalled_lane() {
        let outcome = run_with_check_timeout(
            std::time::Duration::ZERO,
            std::future::pending::<Result<()>>(),
        )
        .await;
        assert!(outcome
            .expect_err("stalled check must time out")
            .to_string()
            .contains("timed out"));
    }

    #[test]
    fn provider_error_previews_are_bounded_and_strip_nuls() {
        let raw = format!("secret\0{}", "x".repeat(CHECK_ERROR_PREVIEW_CHARS + 100));
        let preview = check_body_preview(&raw);
        assert!(!preview.contains('\0'));
        assert!(preview.ends_with("...[truncated]"));
        assert!(preview.chars().count() <= CHECK_ERROR_PREVIEW_CHARS + 14);
    }

    #[test]
    fn response_stream_rejects_malformed_and_error_events() {
        let mut output = String::new();
        let mut final_response = None;
        assert!(handle_openai_responses_stream_frame(
            "data: {not-json}",
            &mut output,
            &mut final_response
        )
        .is_err());
        assert!(handle_openai_responses_stream_frame(
            r#"data: {"type":"error","error":{"message":"lane failed"}}"#,
            &mut output,
            &mut final_response
        )
        .is_err());
    }

    #[test]
    fn check_json_parser_is_strict_and_status_aware() {
        let err = parse_check_json("not json", reqwest::StatusCode::BAD_GATEWAY, "fixture")
            .expect_err("invalid JSON must fail")
            .to_string();
        assert!(err.contains("502"));
        assert!(err.contains("not json"));

        let error_envelope = parse_check_json(
            r#"{"error":{"message":"logical failure"}}"#,
            reqwest::StatusCode::OK,
            "fixture",
        )
        .expect_err("HTTP 200 error envelopes must fail")
        .to_string();
        assert!(error_envelope.contains("logical failure"));
    }
}
