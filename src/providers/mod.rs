//! LLM Providers
//!
//! This module contains all LLM provider implementations and the provider registry.
//! Providers are configured via `~/.phoenix/config.toml` and environment variables.

use anyhow::Context;
use serde::de::DeserializeOwned;
use std::fmt;
use std::sync::OnceLock;
use std::time::Duration;

/// Provider replies are untrusted network input. A model response should be
/// nowhere near this large, but the generous ceiling leaves room for sizeable
/// embedding batches and tool payloads without allowing a broken/malicious
/// endpoint to grow the process until the OOM killer intervenes.
pub(crate) const MAX_PROVIDER_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
/// Error bodies are only diagnostic and are eventually rendered in a log or
/// UI. Keep their download ceiling much smaller than a successful response.
pub(crate) const MAX_PROVIDER_ERROR_BYTES: usize = 64 * 1024;
/// Even an error body inside the download ceiling should not become a giant
/// formatted `anyhow` string that is cloned through retry/fallback layers.
const MAX_PROVIDER_ERROR_PREVIEW_BYTES: usize = 8 * 1024;

/// `timeout_seconds = 0` means no provider idle-read deadline. This is the
/// production default for long autonomous work. A positive configured value
/// remains an explicit user limit, while connect timeouts and response-size
/// ceilings still bound broken endpoints and untrusted bodies.
pub(crate) fn apply_read_timeout(
    builder: reqwest::ClientBuilder,
    timeout: Duration,
) -> reqwest::ClientBuilder {
    if timeout.is_zero() {
        builder
    } else {
        builder.read_timeout(timeout)
    }
}

/// A typed limit error lets streaming providers distinguish deterministic
/// oversized replies from transient mid-stream transport failures. Retrying an
/// over-limit SSE response would merely download the attack a second time.
#[derive(Debug)]
pub(crate) struct ResponseBodyLimitError {
    label: String,
    limit: usize,
    advertised: bool,
}

impl fmt::Display for ResponseBodyLimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let source = if self.advertised {
            "advertised Content-Length"
        } else {
            "received body size"
        };
        write!(
            f,
            "{} exceeded the {} byte limit ({source})",
            self.label, self.limit
        )
    }
}

impl std::error::Error for ResponseBodyLimitError {}

pub(crate) fn response_limit_error(label: &str, limit: usize, advertised: bool) -> anyhow::Error {
    anyhow::Error::new(ResponseBodyLimitError {
        label: label.to_string(),
        limit,
        advertised,
    })
}

pub(crate) fn is_response_body_limit_error(error: &anyhow::Error) -> bool {
    error.downcast_ref::<ResponseBodyLimitError>().is_some()
}

/// Reject a known-oversized body before reading a byte. The streamed byte
/// accounting below remains authoritative because Content-Length can be
/// absent, compressed, incorrect, or deliberately forged.
pub(crate) fn ensure_response_content_length(
    response: &reqwest::Response,
    limit: usize,
    label: &str,
) -> anyhow::Result<()> {
    if let Some(length) = response.content_length() {
        if length > limit as u64 {
            return Err(response_limit_error(label, limit, true));
        }
    }
    Ok(())
}

/// Append one transport chunk without ever letting the accumulated body cross
/// its hard ceiling. This is shared by ordinary JSON/text reads and SSE readers.
pub(crate) fn extend_bounded_response_body(
    body: &mut Vec<u8>,
    chunk: &[u8],
    limit: usize,
    label: &str,
) -> anyhow::Result<()> {
    let observed = body
        .len()
        .checked_add(chunk.len())
        .ok_or_else(|| response_limit_error(label, limit, false))?;
    if observed > limit {
        return Err(response_limit_error(label, limit, false));
    }
    body.extend_from_slice(chunk);
    Ok(())
}

pub(crate) async fn read_response_bytes(
    mut response: reqwest::Response,
    limit: usize,
    label: &str,
) -> anyhow::Result<Vec<u8>> {
    ensure_response_content_length(&response, limit, label)?;
    let initial_capacity = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or(0)
        // Do not allocate tens of MiB merely because an untrusted header says
        // they are coming. Grow as verified chunks actually arrive.
        .min(limit)
        .min(64 * 1024);
    let mut body = Vec::with_capacity(initial_capacity);
    while let Some(chunk) = response
        .chunk()
        .await
        .with_context(|| format!("failed while reading {label}"))?
    {
        extend_bounded_response_body(&mut body, &chunk, limit, label)?;
    }
    Ok(body)
}

pub(crate) async fn read_response_text(
    response: reqwest::Response,
    limit: usize,
    label: &str,
) -> anyhow::Result<String> {
    let body = read_response_bytes(response, limit, label).await?;
    String::from_utf8(body).with_context(|| format!("{label} was not valid UTF-8"))
}

pub(crate) async fn read_json_response<T>(
    response: reqwest::Response,
    label: &str,
) -> anyhow::Result<T>
where
    T: DeserializeOwned,
{
    let text = read_response_text(response, MAX_PROVIDER_RESPONSE_BYTES, label).await?;
    serde_json::from_str(&text).with_context(|| format!("{label} was not valid JSON"))
}

pub(crate) async fn read_error_response(
    response: reqwest::Response,
    label: &str,
) -> anyhow::Result<String> {
    read_response_text(response, MAX_PROVIDER_ERROR_BYTES, label).await
}

/// Preserve enough of a provider error to be actionable while bounding every
/// formatted/logged error. The returned string is always valid UTF-8 and never
/// exceeds `MAX_PROVIDER_ERROR_PREVIEW_BYTES` bytes.
pub(crate) fn bounded_error_preview(body: &str) -> String {
    bounded_error_preview_with_limit(body, MAX_PROVIDER_ERROR_PREVIEW_BYTES)
}

fn bounded_error_preview_with_limit(body: &str, limit: usize) -> String {
    if body.len() <= limit {
        return body.to_string();
    }
    const SUFFIX: &str = "\n… [provider error body truncated]";
    if limit <= SUFFIX.len() {
        let mut end = limit;
        while end > 0 && !SUFFIX.is_char_boundary(end) {
            end -= 1;
        }
        return SUFFIX[..end].to_string();
    }
    let mut end = limit - SUFFIX.len();
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    let mut preview = String::with_capacity(limit);
    preview.push_str(&body[..end]);
    preview.push_str(SUFFIX);
    preview
}

/// Optional hook for live stream-progress lines ("codex streaming... 120 KB").
/// The gateway daemon installs its logger here so long provider generations
/// show signs of life in the gateway terminal/log instead of looking hung.
static STREAM_PROGRESS_HOOK: OnceLock<Box<dyn Fn(Option<&str>, &str) + Send + Sync>> =
    OnceLock::new();

pub fn set_stream_progress_hook(hook: Box<dyn Fn(Option<&str>, &str) + Send + Sync>) {
    let _ = STREAM_PROGRESS_HOOK.set(hook);
}

pub(crate) fn stream_progress_for(session_id: Option<&str>, line: &str) {
    match STREAM_PROGRESS_HOOK.get() {
        Some(hook) => hook(session_id, line),
        None => tracing::info!("{line}"),
    }
}

pub(crate) fn stream_progress_observed(
    session_id: Option<&str>, observer: Option<&contracts::StreamObserver>, line: &str,
) {
    if let Some(observer) = observer {
        observer.emit("progress", line);
        tracing::info!("{line}");
    } else {
        stream_progress_for(session_id, line);
    }
}

pub mod anthropic;
pub mod contracts;
pub mod deepseek;
pub mod factory;
pub mod fallback;
pub mod google;
pub mod grok_cli;
pub mod live_catalogs;
pub mod model_id;
pub mod ollama;
pub mod openai;
pub mod openai_codex;
pub mod openai_compat;
pub mod opencode;
pub mod openrouter;
pub mod openrouter_catalog;
pub mod providers_data;
pub mod registry;
pub mod retry;
pub mod scaffold;
pub mod tier_gate;
pub mod validate;

pub use contracts::{
    AuthType as ContractAuthType, ChatMessage, CompletionRequest, CompletionResponse, LLMProvider,
    MessageRole, ModelInfo, NativeCompactionCapability, NativeCompactionProvenance,
    NativeCompactionReplay, NativeCompactionReplayInput, NativeCompactionRequest,
    NativeCompactionResult, NativeCompactionRoute, ProviderResponseDialect, ProviderResponseRoute,
    RoutedCompletionResponse, RoutedStreamingResponse, StreamingResponse, TokenUsage,
};
pub use factory::ProviderFactory;

#[cfg(test)]
mod response_body_tests {
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

    #[tokio::test]
    async fn rejects_oversized_content_length_before_body_read() {
        let response = local_response(
            b"HTTP/1.1 200 OK\r\nContent-Length: 9999\r\nConnection: close\r\n\r\n".to_vec(),
        )
        .await;
        let error = read_response_bytes(response, 8, "test response")
            .await
            .unwrap_err();
        assert!(is_response_body_limit_error(&error));
        assert!(error.to_string().contains("Content-Length"));
    }

    #[tokio::test]
    async fn rejects_chunked_body_when_stream_crosses_cap() {
        let response = local_response(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\n1234\r\n4\r\n5678\r\n0\r\n\r\n"
                .to_vec(),
        )
        .await;
        let error = read_response_bytes(response, 7, "chunked test response")
            .await
            .unwrap_err();
        assert!(is_response_body_limit_error(&error));
        assert!(error.to_string().contains("received body size"));
    }

    #[tokio::test]
    async fn invalid_utf8_and_json_are_errors() {
        let invalid_utf8 = local_response(
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n\xff\xfe".to_vec(),
        )
        .await;
        let utf8_error = read_response_text(invalid_utf8, 8, "UTF-8 test")
            .await
            .unwrap_err();
        assert!(utf8_error.to_string().contains("not valid UTF-8"));

        let invalid_json = local_response(
            b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nnot json".to_vec(),
        )
        .await;
        let json_error = read_json_response::<serde_json::Value>(invalid_json, "JSON test")
            .await
            .unwrap_err();
        assert!(json_error.to_string().contains("not valid JSON"));
    }

    #[test]
    fn error_preview_is_bounded_and_respects_utf8_boundaries() {
        let body = format!("{}tail", "🧪".repeat(32));
        let preview = bounded_error_preview_with_limit(&body, 41);
        assert!(preview.len() <= 41);
        assert!(preview.contains("truncated"));
        assert!(std::str::from_utf8(preview.as_bytes()).is_ok());
    }
}
