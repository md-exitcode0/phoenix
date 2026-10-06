//! Minimal Model Context Protocol client over Streamable HTTP — the transport
//! Composio's managed "For You" server speaks (`connect.composio.dev/mcp`).
//!
//! One endpoint, JSON-RPC over POST, a session established by `initialize` and
//! carried in the `Mcp-Session-Id` header. Responses come back as either
//! `application/json` or an SSE `event: message\ndata: {…}` stream, so both are
//! handled. Stateless from our side (re-initialize per call) — any app-level
//! workflow continuity Composio needs is threaded through the tool ARGUMENTS
//! (its own `session_id`), not the MCP transport session.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

const PROTOCOL_VERSION: &str = "2025-06-18";
const MAX_ENDPOINT_BYTES: usize = 8 * 1024;
const MAX_HEADERS: usize = 64;
const MAX_HEADER_NAME_BYTES: usize = 256;
const MAX_HEADER_VALUE_BYTES: usize = 16 * 1024;
const MAX_HEADER_BYTES: usize = 128 * 1024;
const MAX_SESSION_ID_BYTES: usize = 4 * 1024;
const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
const MAX_ARGUMENT_BYTES: usize = 1024 * 1024;
const MAX_HTTP_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_JSONRPC_MESSAGES: usize = 1_024;
const MAX_SSE_LINE_BYTES: usize = 2 * 1024 * 1024;
const MAX_TOOLS: usize = 1_024;
const MAX_TOOL_NAME_BYTES: usize = 256;
const MAX_TOOL_DESCRIPTION_BYTES: usize = 64 * 1024;
const MAX_TOOL_SCHEMA_BYTES: usize = 512 * 1024;
const MAX_CONTENT_ITEMS: usize = 256;
const MAX_CONTENT_ITEM_BYTES: usize = 2 * 1024 * 1024;
const MAX_RESULT_TEXT_BYTES: usize = 4 * 1024 * 1024;
const MAX_URI_BYTES: usize = 16 * 1024;
const MAX_MIME_BYTES: usize = 128;
const MAX_BLOB_ENCODED_BYTES: usize = 8 * 1024 * 1024;
const MAX_BLOB_DECODED_BYTES: usize = 6 * 1024 * 1024;
const MAX_STDIO_STDOUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_STDIO_STDERR_BYTES: usize = 256 * 1024;
const MAX_STDIO_LINE_BYTES: usize = 2 * 1024 * 1024;
const MAX_STDIO_ARGS: usize = 256;
const MAX_STDIO_ENV: usize = 128;
const MAX_STDIO_CONFIG_BYTES: usize = 512 * 1024;
const STDIO_POLL_INTERVAL: Duration = Duration::from_millis(5);
const STDIO_EXIT_GRACE: Duration = Duration::from_millis(100);

fn http() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(90))
        .build()
        .context("MCP: failed to build HTTP client")
}

pub fn first_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn append_jsonrpc_value(messages: &mut Vec<Value>, value: Value) -> Result<()> {
    match value {
        Value::Array(values) => {
            for value in values {
                append_jsonrpc_value(messages, value)?;
            }
        }
        Value::Object(_) => messages.push(value),
        _ => bail!("MCP: JSON-RPC payload was not an object"),
    }
    if messages.len() > MAX_JSONRPC_MESSAGES {
        bail!("MCP: response contained more than {MAX_JSONRPC_MESSAGES} JSON-RPC messages");
    }
    Ok(())
}

/// Decode a Streamable-HTTP MCP body into its bounded JSON-RPC messages.
/// An SSE event can contain multiple `data:` lines; distinct events are parsed
/// independently rather than concatenated into invalid JSON.
fn parse_jsonrpc_messages(body: &str) -> Result<Vec<Value>> {
    if body.len() > MAX_HTTP_BODY_BYTES {
        bail!("MCP: response exceeded the {MAX_HTTP_BODY_BYTES}-byte limit");
    }
    let trimmed = body.trim_start();
    let mut messages = Vec::new();
    if trimmed.starts_with("event:") || trimmed.starts_with("data:") || trimmed.starts_with(':') {
        let mut data = String::new();
        let flush = |data: &mut String, messages: &mut Vec<Value>| -> Result<()> {
            if data.trim().is_empty() {
                data.clear();
                return Ok(());
            }
            let value: Value = serde_json::from_str(data.trim()).with_context(|| {
                format!("MCP: SSE data was not JSON-RPC: {}", first_chars(data, 200))
            })?;
            append_jsonrpc_value(messages, value)?;
            data.clear();
            Ok(())
        };
        for raw_line in body.split('\n') {
            let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
            if line.len() > MAX_SSE_LINE_BYTES {
                bail!("MCP: SSE line exceeded the {MAX_SSE_LINE_BYTES}-byte limit");
            }
            if line.is_empty() {
                flush(&mut data, &mut messages)?;
            } else if let Some(payload) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(payload.strip_prefix(' ').unwrap_or(payload));
            } else if line.starts_with("event:")
                || line.starts_with("id:")
                || line.starts_with("retry:")
                || line.starts_with(':')
            {
                // SSE metadata/comment.
            } else {
                bail!("MCP: malformed SSE field: {}", first_chars(line, 80));
            }
        }
        flush(&mut data, &mut messages)?;
    } else {
        let value: Value = serde_json::from_str(trimmed).with_context(|| {
            format!("MCP: response was not JSON-RPC: {}", first_chars(body, 200))
        })?;
        append_jsonrpc_value(&mut messages, value)?;
    }
    if messages.is_empty() {
        bail!("MCP: response contained no JSON-RPC message");
    }
    Ok(messages)
}

#[cfg(test)]
fn parse_jsonrpc_body(body: &str) -> Result<Value> {
    let mut messages = parse_jsonrpc_messages(body)?;
    if messages.len() == 1 {
        Ok(messages.pop().expect("length checked"))
    } else {
        Ok(Value::Array(messages))
    }
}

fn json_preview(value: &Value, chars: usize) -> String {
    serde_json::to_string(value)
        .map(|raw| first_chars(&raw, chars))
        .unwrap_or_else(|_| "<unprintable JSON>".to_string())
}

fn response_for_id(body: &str, id: i64, operation: &str) -> Result<Value> {
    let messages = parse_jsonrpc_messages(body)?;
    let mut matching = messages
        .into_iter()
        .filter(|message| message.get("id").and_then(Value::as_i64) == Some(id));
    let message = matching
        .next()
        .with_context(|| format!("MCP {operation}: no response for JSON-RPC id {id}"))?;
    if matching.next().is_some() {
        bail!("MCP {operation}: duplicate response for JSON-RPC id {id}");
    }
    if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        bail!("MCP {operation}: response did not declare JSON-RPC 2.0");
    }
    if let Some(error) = message.get("error") {
        bail!(
            "MCP {operation} returned an error: {}",
            json_preview(error, 600)
        );
    }
    message
        .get("result")
        .cloned()
        .with_context(|| format!("MCP {operation}: response omitted both result and error"))
}

/// File extension for an MCP blob's declared MIME type.
fn ext_for_mime(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" | "image/svg" => "svg",
        "application/pdf" => "pdf",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/wav" | "audio/x-wav" => "wav",
        _ => "bin",
    }
}

fn validate_blob_signature(bytes: &[u8], mime: &str) -> Result<()> {
    let valid = match mime {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" | "image/jpg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP",
        "image/svg+xml" | "image/svg" => {
            let text = std::str::from_utf8(bytes).unwrap_or("");
            let lower = text.to_ascii_lowercase();
            (lower.trim_start().starts_with("<svg")
                || (lower.trim_start().starts_with("<?xml") && lower.contains("<svg")))
                && !lower.contains("<script")
                && !lower.contains("<!doctype")
                && !lower.contains("<!entity")
                && !lower.contains("onload=")
                && !lower.contains("javascript:")
                && !lower.contains("url(http")
                && !lower.contains("url(file")
        }
        "application/pdf" => bytes.starts_with(b"%PDF-"),
        "audio/mpeg" | "audio/mp3" => {
            bytes.starts_with(b"ID3")
                || (bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] & 0xe0 == 0xe0)
        }
        "audio/wav" | "audio/x-wav" => {
            bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WAVE"
        }
        "application/octet-stream" => true,
        _ => true,
    };
    if !valid {
        bail!("MCP blob did not match its declared MIME type {mime}");
    }
    Ok(())
}

/// Spill one base64 blob to `<phoenix_home>/attachments` and describe it.
///
/// MCP servers return screenshots as `{type:"image", data:"<base64>",
/// mimeType:"image/png"}`. Inlining that into the transcript is doubly wrong:
/// the model cannot SEE a base64 string (it is text, not an image block), and
/// one real screenshot is ~400KB of base64 ≈ 100k+ tokens of pure noise
/// dropped into the agent's context. So the bytes go to a FILE and the agent
/// gets a path with a media-appropriate next step. Saving an audio/document
/// blob does not mean that the model heard it or saw rendered document pages.
fn spill_blob(kind: &str, data: &str, mime: &str) -> Result<String> {
    if !matches!(kind, "image" | "audio" | "resource") {
        bail!("unsupported MCP blob kind {kind}");
    }
    if mime.is_empty() || mime.len() > MAX_MIME_BYTES || !mime.is_ascii() {
        bail!("MCP blob has an invalid MIME type");
    }
    let mime = mime
        .split_once(';')
        .map_or(mime, |(essence, _)| essence)
        .trim()
        .to_ascii_lowercase();
    if mime.is_empty() {
        bail!("MCP blob has an empty MIME type");
    }
    if kind == "image" && !mime.starts_with("image/") {
        bail!("MCP image block declared non-image MIME type {mime}");
    }
    if kind == "audio" && !mime.starts_with("audio/") {
        bail!("MCP audio block declared non-audio MIME type {mime}");
    }
    if kind == "image"
        && !matches!(
            mime.as_str(),
            "image/png"
                | "image/jpeg"
                | "image/jpg"
                | "image/gif"
                | "image/webp"
                | "image/svg+xml"
                | "image/svg"
        )
    {
        bail!("unsupported MCP image MIME type {mime}");
    }
    if kind == "audio"
        && !matches!(
            mime.as_str(),
            "audio/mpeg" | "audio/mp3" | "audio/wav" | "audio/x-wav"
        )
    {
        bail!("unsupported MCP audio MIME type {mime}");
    }
    let bytes = base64_decode(data).context("MCP blob contained invalid or oversized base64")?;
    validate_blob_signature(&bytes, &mime)?;
    let dir = crate::config::phoenix_home().join("attachments");
    crate::config::private_io::prepare_phoenix_directory(&dir)
        .context("prepare private MCP attachment directory")?;
    // Content-addressed: the same reference fetched twice is one private file,
    // and its stable path remains useful across turns.
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(&bytes);
    let digest = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let path = dir.join(format!("mcp-{digest}.{}", ext_for_mime(&mime)));
    crate::config::private_io::atomic_write_private_if_missing(&path, &bytes)
        .with_context(|| format!("save MCP {kind} privately"))?;
    verify_private_blob(&path, &bytes)?;
    let kb = bytes.len().div_ceil(1024);
    let guidance = match mime.as_str() {
        "image/png" | "image/jpeg" | "image/jpg" | "image/gif" | "image/webp" =>
            "open it with image_analyze to actually look at it",
        "image/svg+xml" | "image/svg" =>
            "read the SVG source or render it in the managed browser before visual review",
        "application/pdf" =>
            "use read for document text; inspect rendered pages separately to judge visual layout",
        mime if mime.starts_with("audio/") =>
            "audio bytes saved, not listened to or transcribed; content requires an audio-capable tool",
        _ => "binary bytes saved, not decoded; use a tool that supports this format",
    };
    Ok(format!(
        "[{kind} saved: {} ({mime}, {kb} KB) — {guidance}]",
        path.display()
    ))
}

fn verify_private_blob(path: &std::path::Path, expected: &[u8]) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("verify private MCP attachment {}", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("inspect MCP attachment {}", path.display()))?;
    if !metadata.is_file() || metadata.len() != expected.len() as u64 {
        bail!(
            "refusing unsafe or mismatched MCP attachment {}",
            path.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
            || metadata.permissions().mode() & 0o077 != 0
        {
            bail!(
                "refusing non-private MCP attachment {} (owner/link/mode mismatch)",
                path.display()
            );
        }
    }
    let mut actual = Vec::with_capacity(expected.len());
    std::io::Read::by_ref(&mut file)
        .take(MAX_BLOB_DECODED_BYTES as u64 + 1)
        .read_to_end(&mut actual)
        .with_context(|| format!("read MCP attachment {}", path.display()))?;
    if actual != expected {
        bail!("refusing mismatched MCP attachment {}", path.display());
    }
    Ok(())
}

/// Minimal standard-alphabet base64 decoder (padding optional, whitespace
/// tolerated) — MCP blobs only ever use this alphabet.
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    if input.len() > MAX_BLOB_ENCODED_BYTES {
        return None;
    }
    let compact: Vec<u8> = input
        .bytes()
        .filter(|byte| !matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
        .collect();
    if compact.len() > MAX_BLOB_ENCODED_BYTES || compact.len() % 4 == 1 {
        return None;
    }
    let decoded_upper_bound = compact.len().div_ceil(4).checked_mul(3)?;
    if decoded_upper_bound > MAX_BLOB_DECODED_BYTES + 2 {
        return None;
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&compact)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(&compact))
        .ok()?;
    (decoded.len() <= MAX_BLOB_DECODED_BYTES).then_some(decoded)
}

/// An MCP tool result's `content` array carries `text`, `image`, `audio` and
/// embedded `resource` blocks; flatten it to text the agent can act on. Binary
/// blocks are spilled to disk and referenced by path (see [`spill_blob`]) —
/// never inlined.
fn push_bounded_part(parts: &mut Vec<String>, total: &mut usize, part: String) -> Result<()> {
    if part.len() > MAX_CONTENT_ITEM_BYTES {
        bail!("MCP content item exceeded the {MAX_CONTENT_ITEM_BYTES}-byte limit");
    }
    if part.is_empty() {
        return Ok(());
    }
    let separator = usize::from(!parts.is_empty());
    *total = total
        .checked_add(separator)
        .and_then(|value| value.checked_add(part.len()))
        .context("MCP result text length overflow")?;
    if *total > MAX_RESULT_TEXT_BYTES {
        bail!("MCP result text exceeded the {MAX_RESULT_TEXT_BYTES}-byte limit");
    }
    parts.push(part);
    Ok(())
}

fn extract_content_text(result: &Value) -> Result<String> {
    if result.get("content").is_some() && !result.get("content").is_some_and(Value::is_array) {
        bail!("MCP result content was not an array");
    }
    if let Some(items) = result.get("content").and_then(Value::as_array) {
        if items.len() > MAX_CONTENT_ITEMS {
            bail!("MCP result contained more than {MAX_CONTENT_ITEMS} content items");
        }
        let mut parts = Vec::with_capacity(items.len());
        let mut total = 0usize;
        for item in items {
            let item_type = item
                .get("type")
                .and_then(Value::as_str)
                .context("MCP content item omitted its string type")?;
            let mime = item
                .get("mimeType")
                .and_then(Value::as_str)
                .unwrap_or("application/octet-stream");
            let part = match item_type {
                "text" => item
                    .get("text")
                    .and_then(Value::as_str)
                    .context("MCP text content item omitted its text")?
                    .to_string(),
                kind @ ("image" | "audio") => {
                    let data = item
                        .get("data")
                        .and_then(Value::as_str)
                        .with_context(|| format!("MCP {kind} content item omitted its data"))?;
                    spill_blob(kind, data, mime)?
                }
                "resource" => {
                    let resource = item
                        .get("resource")
                        .context("MCP resource content item omitted its resource")?;
                    let mime = resource
                        .get("mimeType")
                        .and_then(Value::as_str)
                        .unwrap_or(mime);
                    if let Some(text) = resource.get("text").and_then(Value::as_str) {
                        text.to_string()
                    } else if let Some(blob) = resource.get("blob").and_then(Value::as_str) {
                        let uri = resource.get("uri").and_then(Value::as_str).unwrap_or("");
                        if uri.len() > MAX_URI_BYTES {
                            bail!("MCP resource URI exceeded the {MAX_URI_BYTES}-byte limit");
                        }
                        format!("{} {uri}", spill_blob("resource", blob, mime)?)
                            .trim_end()
                            .to_string()
                    } else {
                        let encoded = serde_json::to_string(resource)
                            .context("serialize MCP embedded resource")?;
                        if encoded.len() > MAX_CONTENT_ITEM_BYTES {
                            bail!("MCP embedded resource exceeded its size limit");
                        }
                        encoded
                    }
                }
                "resource_link" => {
                    let uri = item
                        .get("uri")
                        .and_then(Value::as_str)
                        .context("MCP resource link omitted its URI")?;
                    if uri.len() > MAX_URI_BYTES {
                        bail!("MCP resource URI exceeded the {MAX_URI_BYTES}-byte limit");
                    }
                    format!("[resource: {uri}]")
                }
                _ => {
                    let encoded = serde_json::to_string(item)
                        .context("serialize unknown MCP content item")?;
                    if encoded.len() > MAX_CONTENT_ITEM_BYTES {
                        bail!("unknown MCP content item exceeded its size limit");
                    }
                    encoded
                }
            };
            push_bounded_part(&mut parts, &mut total, part)?;
        }
        if !parts.is_empty() {
            return Ok(parts.join("\n"));
        }
    }
    let text = serde_json::to_string_pretty(result).context("serialize MCP tool result")?;
    if text.len() > MAX_RESULT_TEXT_BYTES {
        bail!("MCP result exceeded the {MAX_RESULT_TEXT_BYTES}-byte text limit");
    }
    Ok(text)
}

async fn post(
    client: &reqwest::Client,
    endpoint: &str,
    headers: &[(String, String)],
    session: Option<&str>,
    payload: Value,
) -> Result<reqwest::Response> {
    validate_http_request(endpoint, headers, session, &payload)?;
    let body = serde_json::to_vec(&payload).context("MCP: serialize request")?;
    let mut req = client
        .post(endpoint)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", PROTOCOL_VERSION);
    for (key, value) in headers {
        req = req.header(key, value);
    }
    if let Some(sid) = session {
        req = req.header("Mcp-Session-Id", sid);
    }
    req.body(body).send().await.context("MCP: request failed")
}

fn validate_http_request(
    endpoint: &str,
    headers: &[(String, String)],
    session: Option<&str>,
    payload: &Value,
) -> Result<()> {
    if endpoint.is_empty() || endpoint.len() > MAX_ENDPOINT_BYTES {
        bail!("MCP endpoint must be 1..={MAX_ENDPOINT_BYTES} bytes");
    }
    let parsed = reqwest::Url::parse(endpoint).context("MCP endpoint is not a valid URL")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        bail!("MCP endpoint must use http or https");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() || parsed.fragment().is_some() {
        bail!("MCP endpoint must not contain credentials or a fragment");
    }
    if headers.len() > MAX_HEADERS {
        bail!("MCP request has more than {MAX_HEADERS} headers");
    }
    let mut header_bytes = 0usize;
    let mut header_names = BTreeSet::new();
    for (name, value) in headers {
        if name.is_empty() || name.len() > MAX_HEADER_NAME_BYTES {
            bail!("MCP header name has an invalid length");
        }
        if value.len() > MAX_HEADER_VALUE_BYTES {
            bail!("MCP header value exceeds the {MAX_HEADER_VALUE_BYTES}-byte limit");
        }
        reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .context("MCP header name is invalid")?;
        reqwest::header::HeaderValue::from_str(value).context("MCP header value is invalid")?;
        let normalized_name = name.to_ascii_lowercase();
        if !header_names.insert(normalized_name.clone()) {
            bail!("MCP request contains duplicate header {name}");
        }
        if matches!(
            normalized_name.as_str(),
            "accept"
                | "connection"
                | "content-length"
                | "content-type"
                | "host"
                | "mcp-protocol-version"
                | "mcp-session-id"
                | "transfer-encoding"
        ) {
            bail!("MCP caller may not override transport header {name}");
        }
        header_bytes = header_bytes
            .checked_add(name.len() + value.len())
            .context("MCP header size overflow")?;
    }
    if header_bytes > MAX_HEADER_BYTES {
        bail!("MCP headers exceed the {MAX_HEADER_BYTES}-byte aggregate limit");
    }
    if let Some(session) = session {
        if session.is_empty()
            || session.len() > MAX_SESSION_ID_BYTES
            || session.chars().any(char::is_control)
        {
            bail!("MCP session id is invalid or oversized");
        }
    }
    let encoded = serde_json::to_vec(payload).context("MCP: serialize request for sizing")?;
    if encoded.len() > MAX_REQUEST_BYTES {
        bail!("MCP request exceeds the {MAX_REQUEST_BYTES}-byte limit");
    }
    Ok(())
}

async fn read_http_body(mut response: reqwest::Response, operation: &str) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|size| size > u64::try_from(MAX_HTTP_BODY_BYTES).unwrap_or(u64::MAX))
    {
        bail!("MCP {operation}: response exceeds the {MAX_HTTP_BODY_BYTES}-byte limit");
    }
    let mut body = Vec::with_capacity(64 * 1024);
    while let Some(chunk) = response
        .chunk()
        .await
        .with_context(|| format!("MCP {operation}: response body read failed"))?
    {
        let new_len = body
            .len()
            .checked_add(chunk.len())
            .context("MCP response length overflow")?;
        if new_len > MAX_HTTP_BODY_BYTES {
            bail!("MCP {operation}: response exceeded the {MAX_HTTP_BODY_BYTES}-byte limit");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn status_error(operation: &str, status: reqwest::StatusCode, body: &[u8]) -> anyhow::Error {
    let preview = String::from_utf8_lossy(body);
    anyhow::anyhow!(
        "MCP {operation} failed ({status}): {}",
        first_chars(&preview, 400)
    )
}

fn validate_tool_name(tool: &str) -> Result<()> {
    if tool.trim().is_empty()
        || tool.len() > MAX_TOOL_NAME_BYTES
        || tool.chars().any(char::is_control)
    {
        bail!("MCP tool name is empty, oversized, or contains control characters");
    }
    Ok(())
}

fn validate_arguments(arguments: &Value) -> Result<()> {
    if !arguments.is_object() {
        bail!("MCP tool arguments must be a JSON object");
    }
    let encoded = serde_json::to_vec(arguments).context("serialize MCP tool arguments")?;
    if encoded.len() > MAX_ARGUMENT_BYTES {
        bail!("MCP tool arguments exceed the {MAX_ARGUMENT_BYTES}-byte limit");
    }
    Ok(())
}

fn parse_tool_list(result: Value, include_schema: bool) -> Result<Vec<(String, String, Value)>> {
    let result = result
        .as_object()
        .context("MCP tools/list result was not an object")?;
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .context("MCP tools/list result omitted its tools array")?;
    if result
        .get("nextCursor")
        .is_some_and(|cursor| !cursor.is_null())
    {
        bail!("MCP tools/list pagination is not supported; refusing a partial tool list");
    }
    if tools.len() > MAX_TOOLS {
        bail!("MCP tools/list returned more than {MAX_TOOLS} tools");
    }
    let mut parsed = Vec::with_capacity(tools.len());
    for tool in tools {
        let object = tool
            .as_object()
            .context("MCP tools/list contained a non-object tool")?;
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .context("MCP tool omitted its name")?;
        validate_tool_name(name)?;
        let description = match object.get("description") {
            Some(Value::String(value)) => value.trim(),
            Some(_) => bail!("MCP tool {name} had a non-string description"),
            None => "",
        };
        if description.len() > MAX_TOOL_DESCRIPTION_BYTES {
            bail!("MCP tool {name} description exceeded its size limit");
        }
        let schema = object
            .get("inputSchema")
            .cloned()
            .context("MCP tool omitted its inputSchema")?;
        if !schema.is_object() {
            bail!("MCP tool {name} inputSchema was not an object");
        }
        let encoded = serde_json::to_vec(&schema).context("serialize MCP input schema")?;
        if encoded.len() > MAX_TOOL_SCHEMA_BYTES {
            bail!("MCP tool {name} inputSchema exceeded its size limit");
        }
        let schema = if include_schema { schema } else { Value::Null };
        parsed.push((name.to_string(), description.to_string(), schema));
    }
    Ok(parsed)
}

/// Establish a session and return its `Mcp-Session-Id` (if the server issues
/// one). Sends the `notifications/initialized` follow-up the spec requires.
async fn open_session(
    client: &reqwest::Client,
    endpoint: &str,
    headers: &[(String, String)],
) -> Result<Option<String>> {
    let init = post(
        client,
        endpoint,
        headers,
        None,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "phoenix", "version": "1" }
            }
        }),
    )
    .await?;
    let status = init.status();
    let session_header = init.headers().get("mcp-session-id").cloned();
    let body = read_http_body(init, "initialize").await?;
    if !status.is_success() {
        return Err(status_error("initialize", status, &body));
    }
    let body = std::str::from_utf8(&body).context("MCP initialize response was not UTF-8")?;
    let result = response_for_id(body, 1, "initialize")?;
    let result_object = result
        .as_object()
        .context("MCP initialize result was not an object")?;
    if result_object
        .get("protocolVersion")
        .is_some_and(|version| version.as_str() != Some(PROTOCOL_VERSION))
    {
        bail!("MCP initialize selected an unsupported protocol version");
    }
    let session = session_header
        .map(|value| {
            let value = value
                .to_str()
                .context("MCP initialize returned a non-text session id")?;
            if value.is_empty()
                || value.len() > MAX_SESSION_ID_BYTES
                || value.chars().any(char::is_control)
            {
                bail!("MCP initialize returned an invalid or oversized session id");
            }
            Ok(value.to_string())
        })
        .transpose()?;
    let initialized = post(
        client,
        endpoint,
        headers,
        session.as_deref(),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    )
    .await?;
    let initialized_status = initialized.status();
    let initialized_body = read_http_body(initialized, "notifications/initialized").await?;
    if !initialized_status.is_success() {
        return Err(status_error(
            "notifications/initialized",
            initialized_status,
            &initialized_body,
        ));
    }
    if !initialized_body.iter().all(u8::is_ascii_whitespace) {
        let body = std::str::from_utf8(&initialized_body)
            .context("MCP notifications/initialized response was not UTF-8")?;
        for message in parse_jsonrpc_messages(body)? {
            if let Some(error) = message.get("error") {
                bail!(
                    "MCP notifications/initialized returned an error: {}",
                    json_preview(error, 600)
                );
            }
        }
    }
    Ok(session)
}

/// Call one tool and return its text content. Initializes a fresh session,
/// invokes `tools/call`, and flattens the result.
pub async fn call_tool(
    endpoint: &str,
    headers: &[(String, String)],
    tool: &str,
    arguments: Value,
) -> Result<String> {
    let (text, is_error) = call_tool_lenient(endpoint, headers, tool, arguments).await?;
    if is_error {
        bail!("MCP tool {tool} reported failure: {text}");
    }
    Ok(text)
}

/// Like `call_tool`, but a tool-level `isError` result is returned as
/// `(text, true)` instead of bailing — some servers flag PARTIAL successes
/// as errors (e.g. Composio schema lookups where most slugs resolved), and
/// the payload is still the answer. Transport/JSON-RPC failures still bail.
pub async fn call_tool_lenient(
    endpoint: &str,
    headers: &[(String, String)],
    tool: &str,
    arguments: Value,
) -> Result<(String, bool)> {
    validate_tool_name(tool)?;
    validate_arguments(&arguments)?;
    let client = http()?;
    let session = open_session(&client, endpoint, headers).await?;
    let resp = post(
        &client,
        endpoint,
        headers,
        session.as_deref(),
        json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": tool, "arguments": arguments }
        }),
    )
    .await?;
    let status = resp.status();
    let body = read_http_body(resp, &format!("tools/call {tool}")).await?;
    if !status.is_success() {
        return Err(status_error(&format!("tools/call {tool}"), status, &body));
    }
    let body = std::str::from_utf8(&body).context("MCP tools/call response was not UTF-8")?;
    let result = response_for_id(body, 2, &format!("tools/call {tool}"))?;
    if !result.is_object() {
        bail!("MCP tools/call {tool} result was not an object");
    }
    let is_error = match result.get("isError") {
        Some(Value::Bool(value)) => *value,
        Some(_) => bail!("MCP tools/call {tool} returned a non-boolean isError"),
        None => false,
    };
    Ok((extract_content_text(&result)?, is_error))
}

/// List the tools a server exposes (name + FULL description). Composio's
/// managed server embeds live per-consumer state in these descriptions (the
/// connected-apps list), so callers get the whole text, not a first line.
pub async fn list_tools(
    endpoint: &str,
    headers: &[(String, String)],
) -> Result<Vec<(String, String)>> {
    let client = http()?;
    let session = open_session(&client, endpoint, headers).await?;
    let resp = post(
        &client,
        endpoint,
        headers,
        session.as_deref(),
        json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" }),
    )
    .await?;
    let status = resp.status();
    let body = read_http_body(resp, "tools/list").await?;
    if !status.is_success() {
        return Err(status_error("tools/list", status, &body));
    }
    let body = std::str::from_utf8(&body).context("MCP tools/list response was not UTF-8")?;
    let result = response_for_id(body, 3, "tools/list")?;
    Ok(parse_tool_list(result, false)?
        .into_iter()
        .map(|(name, description, _)| (name, description))
        .collect())
}

/// Like `list_tools`, but also returns each tool's `inputSchema` — the shape
/// the generic MCP discovery (`mcp_servers`) needs to show arg hints. Kept
/// separate so the Composio call sites (which key off descriptions alone)
/// stay untouched.
pub async fn list_tools_with_schema(
    endpoint: &str,
    headers: &[(String, String)],
) -> Result<Vec<(String, String, Value)>> {
    let client = http()?;
    let session = open_session(&client, endpoint, headers).await?;
    let resp = post(
        &client,
        endpoint,
        headers,
        session.as_deref(),
        json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" }),
    )
    .await?;
    let status = resp.status();
    let body = read_http_body(resp, "tools/list").await?;
    if !status.is_success() {
        return Err(status_error("tools/list", status, &body));
    }
    let body = std::str::from_utf8(&body).context("MCP tools/list response was not UTF-8")?;
    let result = response_for_id(body, 3, "tools/list")?;
    parse_tool_list(result, true)
}

// ── stdio transport ───────────────────────────────────────────────────
// Most MCP servers in the wild (and T3MP3ST) speak JSON-RPC over a child
// process's stdin/stdout, one message per line, rather than Streamable HTTP.
// A local server is launched per operation (initialize → op → exit) — the
// same stateless model as the HTTP path; these servers do a clean handshake
// per connection, and short-lived processes keep the daemon free of child
// lifecycle/zombie management.

use std::io::{Read, Write};
use std::process::{ExitStatus, Stdio};

/// How to launch a local (stdio) MCP server.
#[derive(Debug, Clone)]
pub struct StdioServer {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: BTreeMap<String, String>,
}

/// Run one JSON-RPC exchange against a freshly spawned stdio MCP server: send
/// `initialize`, the `notifications/initialized` follow-up, then `request`,
/// and return the parsed `result` for `request`'s id. The whole exchange is
/// bounded by `timeout` so a wedged server can never hang a turn.
async fn stdio_exchange(
    server: &StdioServer,
    request: Value,
    request_id: i64,
    timeout: Duration,
) -> Result<Value> {
    if timeout.is_zero() {
        bail!("local MCP timeout must be greater than zero");
    }
    validate_stdio_server(server)?;
    validate_stdio_request(&request, request_id)?;
    let deadline = Instant::now()
        .checked_add(timeout)
        .context("local MCP timeout was too large")?;
    let server = server.clone();
    let desktop_scope = crate::tools::isolated_desktop::current_scope();
    tokio::task::spawn_blocking(move || {
        crate::tools::isolated_desktop::with_scope(desktop_scope, || {
            stdio_exchange_blocking(&server, request, request_id, timeout, deadline)
        })
    })
    .await
    .context("local MCP exchange worker failed")?
}

fn validate_stdio_server(server: &StdioServer) -> Result<()> {
    if server.command.trim().is_empty()
        || server.command.len() > MAX_ENDPOINT_BYTES
        || server.command.contains('\0')
    {
        bail!("local MCP command is empty, oversized, or contains NUL");
    }
    if server.args.len() > MAX_STDIO_ARGS {
        bail!("local MCP command has more than {MAX_STDIO_ARGS} arguments");
    }
    if server.env.len() > MAX_STDIO_ENV {
        bail!("local MCP command has more than {MAX_STDIO_ENV} environment entries");
    }
    let mut total = server.command.len();
    for argument in &server.args {
        if argument.len() > MAX_ARGUMENT_BYTES || argument.contains('\0') {
            bail!("local MCP command argument is oversized or contains NUL");
        }
        total = total
            .checked_add(argument.len())
            .context("local MCP command size overflow")?;
    }
    if let Some(cwd) = &server.cwd {
        if cwd.is_empty() || cwd.len() > MAX_ENDPOINT_BYTES || cwd.contains('\0') {
            bail!("local MCP working directory is invalid or oversized");
        }
        total = total
            .checked_add(cwd.len())
            .context("local MCP command size overflow")?;
    }
    for (key, value) in &server.env {
        if key.is_empty()
            || key.len() > MAX_HEADER_NAME_BYTES
            || key.chars().any(|character| matches!(character, '=' | '\0'))
            || value.len() > MAX_HEADER_VALUE_BYTES
            || value.contains('\0')
        {
            bail!("local MCP environment entry is invalid or oversized");
        }
        total = total
            .checked_add(key.len() + value.len())
            .context("local MCP command size overflow")?;
    }
    if total > MAX_STDIO_CONFIG_BYTES {
        bail!("local MCP launch configuration exceeds {MAX_STDIO_CONFIG_BYTES} bytes");
    }
    Ok(())
}

fn validate_stdio_request(request: &Value, request_id: i64) -> Result<()> {
    if request_id == 1 {
        bail!("local MCP request id 1 is reserved for initialize");
    }
    let object = request
        .as_object()
        .context("local MCP request was not a JSON object")?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || object.get("id").and_then(Value::as_i64) != Some(request_id)
        || object.get("method").and_then(Value::as_str).is_none()
    {
        bail!("local MCP request had invalid JSON-RPC framing");
    }
    let encoded = serde_json::to_vec(request).context("serialize local MCP request")?;
    if encoded.len() > MAX_REQUEST_BYTES {
        bail!("local MCP request exceeds the {MAX_REQUEST_BYTES}-byte limit");
    }
    Ok(())
}

struct StdioChildGuard {
    child: std::process::Child,
    pgid: i32,
    cleaned: bool,
}

impl StdioChildGuard {
    fn cleanup(&mut self) -> Result<StdioCleanup> {
        let cleanup = terminate_stdio_process_group(&mut self.child, self.pgid)?;
        self.cleaned = true;
        Ok(cleanup)
    }
}

impl Drop for StdioChildGuard {
    fn drop(&mut self) {
        if !self.cleaned {
            let _ = terminate_stdio_process_group(&mut self.child, self.pgid);
        }
    }
}

#[derive(Debug)]
struct StdioCleanup {
    status: ExitStatus,
    descendants: usize,
    leader_was_live: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StdioStop {
    ResponseReady,
    ChildExited,
    TimedOut,
    StdoutLimit,
    StderrLimit,
}

fn stdio_exchange_blocking(
    server: &StdioServer,
    request: Value,
    request_id: i64,
    timeout: Duration,
    deadline: Instant,
) -> Result<Value> {
    if Instant::now() >= deadline {
        bail!(
            "local MCP server `{}` timed out before launch",
            server.command
        );
    }
    let mut command = std::process::Command::new(&server.command);
    command
        .args(&server.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    if let Some(cwd) = &server.cwd {
        command.current_dir(cwd);
    }
    for (key, value) in &server.env {
        command.env(key, value);
    }
    // Server-defined environment is applied first. The agent's private
    // display/session fence is deliberately last, so an MCP registration can
    // never override DISPLAY, XAUTHORITY, Wayland, or host DBus activation and
    // put a GUI window onto the user's desktop.
    if let Some(desktop) = crate::tools::isolated_desktop::current_environment()? {
        desktop.apply_to_command(&mut command);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to launch local MCP server `{}`", server.command))?;
    let pgid = match i32::try_from(child.id()) {
        Ok(pgid) => pgid,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error).context("local MCP child pid did not fit i32");
        }
    };
    let mut guard = StdioChildGuard {
        child,
        pgid,
        cleaned: false,
    };
    let stdin = guard
        .child
        .stdin
        .take()
        .context("MCP stdio: no stdin handle")?;
    let mut stdout = guard
        .child
        .stdout
        .take()
        .context("MCP stdio: no stdout handle")?;
    let mut stderr = guard
        .child
        .stderr
        .take()
        .context("MCP stdio: no stderr handle")?;

    set_stdio_nonblocking(&stdin).context("make MCP stdin nonblocking")?;
    set_stdio_nonblocking(&stdout).context("make MCP stdout nonblocking")?;
    set_stdio_nonblocking(&stderr).context("make MCP stderr nonblocking")?;

    // The three-message opener: initialize, the required initialized note,
    // then the caller's request. Each is one line (no embedded newlines).
    let init = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "phoenix", "version": "1" }
        }
    });
    let mut wire = Vec::new();
    serde_json::to_writer(&mut wire, &init).context("serialize MCP initialize frame")?;
    wire.push(b'\n');
    let initialize_wire_len = wire.len();
    for line in [
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        request,
    ] {
        serde_json::to_writer(&mut wire, &line).context("serialize MCP stdio frame")?;
        wire.push(b'\n');
    }
    if wire.len() > MAX_REQUEST_BYTES + 1024 {
        bail!("MCP stdio request sequence exceeded its size limit");
    }

    let mut stdin_offset = 0usize;
    let mut stdin = Some(stdin);
    let mut stdout_pending = Vec::new();
    let mut stdout_total = 0usize;
    let mut stderr_bytes = Vec::with_capacity(MAX_STDIO_STDERR_BYTES.min(16 * 1024));
    let mut stderr_total = 0usize;
    let mut init_seen = false;
    let mut response = None;
    let mut response_seen_at = None;
    let mut stdout_eof = false;

    let loop_result: Result<StdioStop> = 'exchange: loop {
        let stdout_state = drain_stdio_pipe(
            &mut stdout,
            &mut stdout_pending,
            &mut stdout_total,
            MAX_STDIO_STDOUT_BYTES,
            true,
        );
        let stderr_state = drain_stdio_pipe(
            &mut stderr,
            &mut stderr_bytes,
            &mut stderr_total,
            MAX_STDIO_STDERR_BYTES,
            true,
        );
        let stdout_state = match stdout_state {
            Ok(state) => state,
            Err(error) => break Err(error.context("read local MCP stdout")),
        };
        let stderr_state = match stderr_state {
            Ok(state) => state,
            Err(error) => break Err(error.context("read local MCP stderr")),
        };
        stdout_eof |= stdout_state.eof;
        if stdout_state.overflow {
            break Ok(StdioStop::StdoutLimit);
        }
        if stderr_state.overflow {
            break Ok(StdioStop::StderrLimit);
        }
        if let Err(error) = process_stdio_lines(
            &mut stdout_pending,
            stdout_eof,
            request_id,
            stdin_offset == wire.len(),
            &mut init_seen,
            &mut response,
        ) {
            break Err(error);
        }

        if let Some(writer) = stdin.as_mut() {
            // MCP requires the initialize result before the initialized
            // notification or operation. Keep the later frames staged until
            // that result has been parsed and validated.
            let write_limit = if init_seen {
                wire.len()
            } else {
                initialize_wire_len
            };
            while stdin_offset < write_limit {
                match writer.write(&wire[stdin_offset..write_limit]) {
                    Ok(0) => {
                        break 'exchange Err(anyhow::anyhow!(
                            "local MCP stdin closed before the request was written"
                        ))
                    }
                    Ok(written) => stdin_offset += written,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) => {
                        break 'exchange Err(error).context("write local MCP request");
                    }
                }
            }
            if stdin_offset == wire.len() {
                stdin.take();
            }
        }

        let child_exited = match stdio_child_exited_without_reap(&mut guard.child, pgid) {
            Ok(exited) => exited,
            Err(error) => break Err(error.context("poll local MCP child")),
        };
        if response.is_some() && init_seen {
            let seen = *response_seen_at.get_or_insert_with(Instant::now);
            if child_exited || seen.elapsed() >= STDIO_EXIT_GRACE {
                break Ok(StdioStop::ResponseReady);
            }
        }
        if child_exited {
            break Ok(StdioStop::ChildExited);
        }
        if Instant::now() >= deadline {
            break Ok(StdioStop::TimedOut);
        }
        std::thread::sleep(STDIO_POLL_INTERVAL);
    };

    // The leader remains unreaped until this point on Linux, reserving the
    // child-owned PGID while every same-group descendant is terminated.
    let cleanup = guard.cleanup().with_context(|| {
        format!(
            "local MCP server `{}` process cleanup was not confirmed",
            server.command
        )
    })?;

    // Capture only finite kernel-buffered bytes after all writers are gone.
    let final_stdout = drain_stdio_pipe(
        &mut stdout,
        &mut stdout_pending,
        &mut stdout_total,
        MAX_STDIO_STDOUT_BYTES,
        true,
    )?;
    let final_stderr = drain_stdio_pipe(
        &mut stderr,
        &mut stderr_bytes,
        &mut stderr_total,
        MAX_STDIO_STDERR_BYTES,
        true,
    )?;
    stdout_eof |= final_stdout.eof;
    if final_stdout.overflow {
        bail!(
            "local MCP server `{}` stdout exceeded the {MAX_STDIO_STDOUT_BYTES}-byte limit",
            server.command
        );
    }
    if final_stderr.overflow {
        bail!(
            "local MCP server `{}` stderr exceeded the {MAX_STDIO_STDERR_BYTES}-byte limit",
            server.command
        );
    }
    process_stdio_lines(
        &mut stdout_pending,
        stdout_eof,
        request_id,
        stdin_offset == wire.len(),
        &mut init_seen,
        &mut response,
    )?;

    let stop = loop_result?;
    match stop {
        StdioStop::TimedOut => bail!(
            "local MCP server `{}` timed out after {:.3}s; its process group was terminated and reaped",
            server.command,
            timeout.as_secs_f64()
        ),
        StdioStop::StdoutLimit => bail!(
            "local MCP server `{}` stdout exceeded the {MAX_STDIO_STDOUT_BYTES}-byte limit; its process group was terminated and reaped",
            server.command
        ),
        StdioStop::StderrLimit => bail!(
            "local MCP server `{}` stderr exceeded the {MAX_STDIO_STDERR_BYTES}-byte limit; its process group was terminated and reaped",
            server.command
        ),
        StdioStop::ResponseReady | StdioStop::ChildExited => {}
    }
    if cleanup.descendants > 0 {
        bail!(
            "local MCP server `{}` left {} background descendant(s); they were terminated and reaped",
            server.command,
            cleanup.descendants
        );
    }
    if !cleanup.status.success()
        && !(cleanup.leader_was_live && stdio_status_was_cleanup_signal(&cleanup.status))
    {
        let stderr = String::from_utf8_lossy(&stderr_bytes);
        bail!(
            "local MCP server `{}` exited with {}{}",
            server.command,
            cleanup.status,
            if stderr.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", first_chars(stderr.trim(), 400))
            }
        );
    }
    if stdin_offset != wire.len() {
        bail!(
            "local MCP server `{}` closed before reading the request",
            server.command
        );
    }
    if !init_seen {
        bail!(
            "local MCP server `{}` never completed initialize",
            server.command
        );
    }
    response.with_context(|| {
        let stderr = String::from_utf8_lossy(&stderr_bytes);
        if stderr.trim().is_empty() {
            format!(
                "local MCP server `{}` closed without answering",
                server.command
            )
        } else {
            format!(
                "local MCP server `{}` closed without answering: {}",
                server.command,
                first_chars(stderr.trim(), 400)
            )
        }
    })
}

#[derive(Debug, Clone, Copy)]
struct PipeDrain {
    eof: bool,
    overflow: bool,
}

fn drain_stdio_pipe(
    pipe: &mut impl Read,
    retained: &mut Vec<u8>,
    total: &mut usize,
    cap: usize,
    retain: bool,
) -> Result<PipeDrain> {
    let mut buffer = [0u8; 8192];
    let mut eof = false;
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) => {
                eof = true;
                break;
            }
            Ok(read) => {
                *total = total
                    .checked_add(read)
                    .context("local MCP stream length overflow")?;
                if retain {
                    let remaining = cap.saturating_sub(retained.len());
                    retained.extend_from_slice(&buffer[..read.min(remaining)]);
                }
                if *total > cap {
                    return Ok(PipeDrain {
                        eof,
                        overflow: true,
                    });
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => return Err(error).context("read bounded local MCP stream"),
        }
    }
    Ok(PipeDrain {
        eof,
        overflow: false,
    })
}

fn process_stdio_lines(
    pending: &mut Vec<u8>,
    eof: bool,
    request_id: i64,
    request_sent: bool,
    init_seen: &mut bool,
    response: &mut Option<Value>,
) -> Result<()> {
    while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
        if newline > MAX_STDIO_LINE_BYTES {
            bail!("local MCP stdout line exceeded the {MAX_STDIO_LINE_BYTES}-byte limit");
        }
        let mut remaining = pending.split_off(newline + 1);
        std::mem::swap(pending, &mut remaining);
        remaining.truncate(newline);
        if remaining.last() == Some(&b'\r') {
            remaining.pop();
        }
        let line = std::str::from_utf8(&remaining).context("local MCP stdout was not UTF-8")?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let message: Value = serde_json::from_str(line).with_context(|| {
            format!(
                "local MCP stdout contained non-JSON protocol data: {}",
                first_chars(line, 160)
            )
        })?;
        process_stdio_message(message, request_id, request_sent, init_seen, response)?;
    }
    if pending.len() > MAX_STDIO_LINE_BYTES {
        bail!("local MCP stdout line exceeded the {MAX_STDIO_LINE_BYTES}-byte limit");
    }
    if eof && !pending.is_empty() {
        bail!("local MCP stdout ended with an unterminated JSON-RPC frame");
    }
    Ok(())
}

fn process_stdio_message(
    message: Value,
    request_id: i64,
    request_sent: bool,
    init_seen: &mut bool,
    response: &mut Option<Value>,
) -> Result<()> {
    let object = message
        .as_object()
        .context("local MCP JSON-RPC frame was not an object")?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        bail!("local MCP response did not declare JSON-RPC 2.0");
    }
    let Some(id) = object.get("id") else {
        if object.get("method").and_then(Value::as_str).is_none() {
            bail!("local MCP notification omitted its method");
        }
        return Ok(());
    };
    let id = id
        .as_i64()
        .context("local MCP response id was not an integer")?;
    let operation = if id == 1 {
        "initialize"
    } else if id == request_id {
        "request"
    } else {
        return Ok(());
    };
    if let Some(error) = object.get("error") {
        bail!(
            "local MCP {operation} returned an error: {}",
            json_preview(error, 600)
        );
    }
    let result = object
        .get("result")
        .cloned()
        .with_context(|| format!("local MCP {operation} omitted both result and error"))?;
    if id == 1 {
        if *init_seen {
            bail!("local MCP server returned duplicate initialize responses");
        }
        let init = result
            .as_object()
            .context("local MCP initialize result was not an object")?;
        if let Some(version) = init.get("protocolVersion") {
            if version.as_str() != Some(PROTOCOL_VERSION) {
                bail!("local MCP server selected an unsupported protocol version");
            }
        }
        *init_seen = true;
    } else {
        if !request_sent {
            bail!("local MCP server answered before the operation was sent");
        }
        if response.is_some() {
            bail!("local MCP server returned duplicate responses for id {request_id}");
        }
        *response = Some(result);
    }
    Ok(())
}

#[cfg(unix)]
fn set_stdio_nonblocking(stream: &impl std::os::fd::AsRawFd) -> Result<()> {
    let fd = stream.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error()).context("read local MCP pipe flags");
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error()).context("set local MCP pipe nonblocking");
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_stdio_nonblocking<T>(_stream: &T) -> Result<()> {
    Ok(())
}

#[cfg(target_os = "linux")]
fn stdio_child_exited_without_reap(_child: &mut std::process::Child, pid: i32) -> Result<bool> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result < 0 {
        return Err(std::io::Error::last_os_error()).context("waitid local MCP child");
    }
    Ok(info.si_signo == libc::SIGCHLD)
}

#[cfg(not(target_os = "linux"))]
fn stdio_child_exited_without_reap(child: &mut std::process::Child, _pid: i32) -> Result<bool> {
    Ok(child.try_wait().context("poll local MCP child")?.is_some())
}

#[cfg(target_os = "linux")]
fn live_stdio_group_members(pgid: i32) -> Result<Vec<i32>> {
    let mut members = Vec::new();
    for entry in std::fs::read_dir("/proc").context("scan /proc for local MCP process group")? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("read /proc entry for local MCP child"),
        };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        let stat = match std::fs::read_to_string(entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) || error.raw_os_error() == Some(libc::ESRCH) =>
            {
                // procfs can report ESRCH when a process exits after its
                // entry was enumerated/opened. It is no longer a live member.
                continue;
            }
            Err(error) => return Err(error).context("read local MCP /proc stat"),
        };
        let Some(after_name) = stat.rsplit_once(") ").map(|(_, rest)| rest) else {
            continue;
        };
        let mut fields = after_name.split_whitespace();
        let state = fields
            .next()
            .and_then(|field| field.as_bytes().first().copied());
        let _parent_pid = fields.next();
        let member_pgid = fields.next().and_then(|field| field.parse::<i32>().ok());
        if member_pgid == Some(pgid) && !matches!(state, Some(b'Z' | b'X')) {
            members.push(pid);
        }
    }
    Ok(members)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn live_stdio_group_members(pgid: i32) -> Result<Vec<i32>> {
    if unsafe { libc::kill(-pgid, 0) } == 0 {
        Ok(vec![pgid])
    } else {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(Vec::new())
        } else {
            Err(error).context("probe local MCP process group")
        }
    }
}

#[cfg(unix)]
fn signal_stdio_group(pgid: i32, signal: i32) -> Result<()> {
    if pgid <= 1 || pgid == unsafe { libc::getpgrp() } {
        bail!("refusing to signal unsafe local MCP process group {pgid}");
    }
    let actual = unsafe { libc::getpgid(pgid) };
    if actual != pgid {
        let error = std::io::Error::last_os_error();
        if actual == -1 && error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        bail!("local MCP process group ownership changed: pid={pgid}, pgid={actual}");
    }
    if unsafe { libc::kill(-pgid, signal) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error).context("signal local MCP process group")
    }
}

#[cfg(unix)]
fn terminate_stdio_process_group(
    child: &mut std::process::Child,
    pgid: i32,
) -> Result<StdioCleanup> {
    let initial = live_stdio_group_members(pgid)?;
    let descendants = initial.iter().filter(|pid| **pid != pgid).count();
    let leader_was_live = initial.contains(&pgid);
    if !initial.is_empty() {
        signal_stdio_group(pgid, libc::SIGTERM)?;
        let term_deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < term_deadline && !live_stdio_group_members(pgid)?.is_empty() {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !live_stdio_group_members(pgid)?.is_empty() {
            signal_stdio_group(pgid, libc::SIGKILL)?;
            let kill_deadline = Instant::now() + Duration::from_millis(500);
            while Instant::now() < kill_deadline && !live_stdio_group_members(pgid)?.is_empty() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    let remaining = live_stdio_group_members(pgid)?;
    if !remaining.is_empty() {
        bail!(
            "local MCP process group still has live members: {}",
            remaining
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(StdioCleanup {
        status: child.wait().context("reap local MCP leader")?,
        descendants,
        leader_was_live,
    })
}

#[cfg(unix)]
fn stdio_status_was_cleanup_signal(status: &ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt;
    matches!(status.signal(), Some(libc::SIGTERM) | Some(libc::SIGKILL))
}

#[cfg(not(unix))]
fn terminate_stdio_process_group(
    child: &mut std::process::Child,
    _pgid: i32,
) -> Result<StdioCleanup> {
    if child.try_wait().context("poll local MCP child")?.is_none() {
        child.kill().context("terminate local MCP child")?;
    }
    Ok(StdioCleanup {
        status: child.wait().context("reap local MCP child")?,
        descendants: 0,
        leader_was_live: true,
    })
}

#[cfg(not(unix))]
fn stdio_status_was_cleanup_signal(_status: &ExitStatus) -> bool {
    true
}

/// List the tools a local stdio MCP server exposes: (name, description, input schema).
pub async fn list_tools_stdio(
    server: &StdioServer,
    timeout: std::time::Duration,
) -> Result<Vec<(String, String, Value)>> {
    let result = stdio_exchange(
        server,
        json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" }),
        3,
        timeout,
    )
    .await?;
    parse_tool_list(result, true)
}

/// Call one tool on a local stdio MCP server and return its text content.
pub async fn call_tool_stdio(
    server: &StdioServer,
    tool: &str,
    arguments: Value,
    timeout: std::time::Duration,
) -> Result<String> {
    validate_tool_name(tool)?;
    validate_arguments(&arguments)?;
    let result = stdio_exchange(
        server,
        json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": tool, "arguments": arguments }
        }),
        2,
        timeout,
    )
    .await?;
    if !result.is_object() {
        bail!("MCP tool {tool} returned a non-object result");
    }
    let is_error = match result.get("isError") {
        Some(Value::Bool(value)) => *value,
        Some(_) => bail!("MCP tool {tool} returned a non-boolean isError"),
        None => false,
    };
    if is_error {
        bail!(
            "MCP tool {tool} reported failure: {}",
            extract_content_text(&result)?
        );
    }
    extract_content_text(&result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    struct TestHttpReply {
        status: &'static str,
        body: Vec<u8>,
        headers: Vec<(String, String)>,
        declared_length: Option<usize>,
    }

    impl TestHttpReply {
        fn json(body: impl Into<Vec<u8>>) -> Self {
            Self {
                status: "200 OK",
                body: body.into(),
                headers: Vec::new(),
                declared_length: None,
            }
        }

        fn status(status: &'static str, body: impl Into<Vec<u8>>) -> Self {
            Self {
                status,
                body: body.into(),
                headers: Vec::new(),
                declared_length: None,
            }
        }
    }

    fn initialize_reply() -> TestHttpReply {
        let mut reply = TestHttpReply::json(
            format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"protocolVersion\":\"{PROTOCOL_VERSION}\"}}}}"
            )
            .into_bytes(),
        );
        reply
            .headers
            .push(("Mcp-Session-Id".to_string(), "test-session".to_string()));
        reply
    }

    fn initialized_reply() -> TestHttpReply {
        TestHttpReply::status("202 Accepted", Vec::new())
    }

    fn read_test_request(stream: &mut TcpStream) {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut buffer).expect("read test HTTP request");
            assert!(read > 0, "client closed before HTTP headers completed");
            request.extend_from_slice(&buffer[..read]);
            assert!(request.len() <= 64 * 1024, "test HTTP headers overflowed");
            if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = std::str::from_utf8(&request[..header_end]).unwrap();
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        while request.len() < header_end + content_length {
            let read = stream.read(&mut buffer).expect("read test HTTP body");
            assert!(read > 0, "client closed before HTTP request body completed");
            request.extend_from_slice(&buffer[..read]);
        }
    }

    fn spawn_http_server(replies: Vec<TestHttpReply>) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            for reply in replies {
                let (mut stream, _) = listener.accept().expect("accept test MCP request");
                read_test_request(&mut stream);
                let declared_length = reply.declared_length.unwrap_or(reply.body.len());
                write!(
                    stream,
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {declared_length}\r\nConnection: close\r\n",
                    reply.status
                )
                .unwrap();
                for (name, value) in reply.headers {
                    write!(stream, "{name}: {value}\r\n").unwrap();
                }
                write!(stream, "\r\n").unwrap();
                stream.write_all(&reply.body).unwrap();
                stream.flush().unwrap();
            }
        });
        (format!("http://{address}/mcp"), handle)
    }

    fn shell_server(script: impl Into<String>) -> StdioServer {
        StdioServer {
            command: "sh".to_string(),
            args: vec!["-c".to_string(), script.into()],
            cwd: None,
            env: Default::default(),
        }
    }

    #[cfg(target_os = "linux")]
    fn process_is_live(pid: i32) -> bool {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        stat.rsplit_once(") ")
            .and_then(|(_, fields)| fields.as_bytes().first().copied())
            .is_some_and(|state| !matches!(state, b'Z' | b'X'))
    }

    #[test]
    fn parses_sse_framed_jsonrpc() {
        let body =
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n\n";
        let v = parse_jsonrpc_body(body).unwrap();
        assert_eq!(v["result"]["ok"], serde_json::json!(true));
    }

    #[test]
    fn parses_bare_json_jsonrpc() {
        let body = "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"x\":5}}";
        let v = parse_jsonrpc_body(body).unwrap();
        assert_eq!(v["result"]["x"], serde_json::json!(5));
    }

    #[test]
    fn parses_distinct_sse_messages_without_concatenating_them() {
        let body = concat!(
            "event: message\n",
            "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\n",
            "event: message\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"tools\":[]}}\n\n"
        );
        let messages = parse_jsonrpc_messages(body).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(
            response_for_id(body, 3, "tools/list").unwrap()["tools"],
            json!([])
        );
    }

    #[tokio::test]
    async fn http_tools_list_checks_status_after_bounded_body_read() {
        let (endpoint, server) = spawn_http_server(vec![
            initialize_reply(),
            initialized_reply(),
            TestHttpReply::status(
                "503 Service Unavailable",
                b"temporarily unavailable".to_vec(),
            ),
        ]);
        let error = list_tools(&endpoint, &[]).await.unwrap_err().to_string();
        server.join().unwrap();
        assert!(error.contains("503"), "{error}");
        assert!(error.contains("temporarily unavailable"), "{error}");
    }

    #[tokio::test]
    async fn http_tools_list_propagates_truncated_error_body() {
        let mut truncated = TestHttpReply::status("500 Internal Server Error", b"bad".to_vec());
        truncated.declared_length = Some(100);
        let (endpoint, server) =
            spawn_http_server(vec![initialize_reply(), initialized_reply(), truncated]);
        let error = list_tools(&endpoint, &[]).await.unwrap_err().to_string();
        server.join().unwrap();
        assert!(error.contains("body read failed"), "{error}");
    }

    #[tokio::test]
    async fn http_tools_list_rejects_oversized_declared_body_without_allocating_it() {
        let mut oversized = TestHttpReply::json(Vec::new());
        oversized.declared_length = Some(MAX_HTTP_BODY_BYTES + 1);
        let (endpoint, server) =
            spawn_http_server(vec![initialize_reply(), initialized_reply(), oversized]);
        let error = list_tools(&endpoint, &[]).await.unwrap_err().to_string();
        server.join().unwrap();
        assert!(error.contains("response exceeds"), "{error}");
    }

    #[tokio::test]
    async fn http_tools_list_surfaces_jsonrpc_error_and_malformed_success() {
        let (endpoint, server) = spawn_http_server(vec![
            initialize_reply(),
            initialized_reply(),
            TestHttpReply::json(
                br#"{"jsonrpc":"2.0","id":3,"error":{"code":-32603,"message":"broken"}}"#.to_vec(),
            ),
        ]);
        let error = list_tools(&endpoint, &[]).await.unwrap_err().to_string();
        server.join().unwrap();
        assert!(error.contains("broken"), "{error}");

        let (endpoint, server) = spawn_http_server(vec![
            initialize_reply(),
            initialized_reply(),
            TestHttpReply::json(
                br#"{"jsonrpc":"2.0","id":3,"result":{"tools":[{"name":"bad"}]}}"#.to_vec(),
            ),
        ]);
        let error = list_tools(&endpoint, &[]).await.unwrap_err().to_string();
        server.join().unwrap();
        assert!(error.contains("inputSchema"), "{error}");
    }

    #[test]
    fn flattens_content_array_to_text() {
        let result = serde_json::json!({
            "content": [{"type": "text", "text": "hello"}, {"type": "text", "text": "world"}]
        });
        assert_eq!(extract_content_text(&result).unwrap(), "hello\nworld");
    }

    #[test]
    fn non_text_content_falls_back_to_json() {
        let result = serde_json::json!({ "structured": { "a": 1 } });
        assert!(extract_content_text(&result)
            .unwrap()
            .contains("structured"));
    }

    #[test]
    fn base64_round_trips_with_and_without_padding() {
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("aGVsbG8").unwrap(), b"hello");
        assert_eq!(base64_decode("aGVs\nbG8=").unwrap(), b"hello");
        assert!(base64_decode("not base64!!").is_none());
    }

    /// A screenshot must reach the agent as a PATH, never as inline base64:
    /// the model cannot see a base64 string, and one real screenshot is ~400KB
    /// of base64 (≈100k tokens) of pure context poison.
    #[cfg(unix)]
    #[test]
    fn binary_content_is_spilled_to_a_file_not_inlined() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(dir.path());
        use base64::Engine;
        use std::os::unix::fs::PermissionsExt;

        let png = b"\x89PNG\r\n\x1a\n";
        let encoded = base64::engine::general_purpose::STANDARD.encode(png);
        let result = serde_json::json!({
            "content": [
                {"type": "text", "text": "reference: split hero"},
                {"type": "image", "data": encoded, "mimeType": "image/png"}
            ]
        });
        let out = extract_content_text(&result).unwrap();

        assert!(
            out.contains("reference: split hero"),
            "text survives: {out}"
        );
        assert!(
            !out.contains("iVBOR"),
            "raw base64 must never be inlined: {out}"
        );
        assert!(
            out.contains("image_analyze"),
            "tells the agent how to look: {out}"
        );
        let path = out
            .split_once("[image saved: ")
            .and_then(|(_, rest)| rest.split_once(" ("))
            .map(|(p, _)| std::path::PathBuf::from(p))
            .expect("a saved path");
        assert_eq!(std::fs::read(&path).unwrap(), png, "bytes land on disk");
        assert_eq!(path.extension().unwrap(), "png", "mime picks the extension");
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0,
            "attachment directory is private"
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o077,
            0,
            "attachment is private"
        );

        // Content-addressed: the same blob twice is one file, same path.
        assert_eq!(extract_content_text(&result).unwrap(), out);
    }

    #[test]
    fn saved_binary_guidance_matches_media_without_claiming_perception() {
        use base64::Engine;
        let home = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let fixtures: &[(&str, &str, &[u8], &str)] = &[
            ("audio", "audio/wav", b"RIFF\x04\0\0\0WAVE", "not listened to or transcribed"),
            ("resource", "application/pdf", b"%PDF-1.4\n", "read"),
            ("image", "image/svg+xml", b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>", "render"),
            ("resource", "application/octet-stream", b"\0\x01\x02", "not decoded"),
        ];
        for (kind, mime, bytes, guidance) in fixtures {
            let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
            let output = spill_blob(kind, &encoded, mime).unwrap();
            assert!(!output.contains("open it with image_analyze"), "incompatible image instruction for {mime}: {output}");
            assert!(output.contains(guidance), "{mime}: {output}");
            let path = output.split_once(" saved: ").unwrap().1.split_once(" (").unwrap().0;
            assert_eq!(std::fs::read(path).unwrap(), *bytes, "routing hints must not rewrite source bytes");
        }
        let png = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\n");
        assert!(spill_blob("image", &png, "image/png").unwrap().contains("image_analyze"));
    }

    #[cfg(unix)]
    #[test]
    fn blob_spill_refuses_a_symlink_destination_without_touching_its_target() {
        use base64::Engine;
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::symlink;

        let home = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let attachments = home.path().join("attachments");
        crate::config::private_io::prepare_phoenix_directory(&attachments).unwrap();
        let png = b"\x89PNG\r\n\x1a\n";
        let digest = Sha256::digest(png)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let destination = attachments.join(format!("mcp-{digest}.png"));
        let victim_dir = tempfile::tempdir().unwrap();
        let victim = victim_dir.path().join("victim");
        std::fs::write(&victim, b"preserve me").unwrap();
        symlink(&victim, &destination).unwrap();

        let encoded = base64::engine::general_purpose::STANDARD.encode(png);
        let error = spill_blob("image", &encoded, "image/png")
            .unwrap_err()
            .to_string();
        assert!(error.contains("save MCP image"), "{error}");
        assert_eq!(std::fs::read(victim).unwrap(), b"preserve me");
    }

    #[test]
    fn blob_decode_rejects_oversize_and_declared_type_mismatch() {
        use base64::Engine;
        let oversized = "A".repeat(MAX_BLOB_ENCODED_BYTES + 1);
        assert!(base64_decode(&oversized).is_none());
        let mismatch = base64::engine::general_purpose::STANDARD.encode(b"not a png");
        assert!(spill_blob("image", &mismatch, "image/png").is_err());
    }

    #[test]
    fn embedded_resource_text_is_read_not_json_dumped() {
        let result = serde_json::json!({
            "content": [{
                "type": "resource",
                "resource": {"uri": "file:///a.css", "mimeType": "text/css", "text": ".hero{}"}
            }]
        });
        assert_eq!(extract_content_text(&result).unwrap(), ".hero{}");
    }

    /// A minimal stdio MCP server written inline as a shell script: it echoes a
    /// canned initialize reply and a tools/list reply, proving the newline
    /// framing + id-matching in `stdio_exchange` without needing Node.
    #[tokio::test]
    async fn stdio_transport_lists_tools_from_a_fake_server() {
        let script = "\
read -r _init
echo '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-06-18\"}}'
read -r _note
read -r _list
echo '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"tools\":[{\"name\":\"probe\",\"description\":\"a probe\",\"inputSchema\":{\"type\":\"object\",\"properties\":{\"target\":{\"type\":\"string\"}},\"required\":[\"target\"]}}]}}'
";
        let server = StdioServer {
            command: "sh".to_string(),
            args: vec!["-c".to_string(), script.to_string()],
            cwd: None,
            env: Default::default(),
        };
        let tools = list_tools_stdio(&server, std::time::Duration::from_secs(10))
            .await
            .expect("list_tools_stdio");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].0, "probe");
        assert!(tools[0].1.contains("a probe"));
        assert_eq!(tools[0].2["properties"]["target"]["type"], "string");
    }

    #[tokio::test]
    async fn scoped_stdio_mcp_cannot_override_the_agent_desktop_environment() {
        let toolchain_present =
            ["Xvfb", "xauth", "xdotool", "xdpyinfo"]
                .into_iter()
                .all(|binary| {
                    std::env::var_os("PATH").is_some_and(|path| {
                        std::env::split_paths(&path)
                            .any(|directory| directory.join(binary).is_file())
                    })
                });
        if !toolchain_present {
            eprintln!("skipping scoped stdio MCP test: Xvfb toolchain unavailable");
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let desktop =
            crate::tools::isolated_desktop::DesktopScope::worker("mcp-scope", "coder", "worker-a")
                .unwrap();
        let cleanup = desktop.clone();
        let script = r#"read -r _init
echo '{"jsonrpc":"2.0","id":1,"result":{}}'
read -r _note
read -r _list
if [ "$DISPLAY" != "host-bogus" ] && [ -z "$DBUS_SESSION_BUS_ADDRESS" ] && [ -z "$WAYLAND_DISPLAY" ]; then desc=isolated; else desc=escaped; fi
echo "{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"tools\":[{\"name\":\"probe\",\"description\":\"$desc\",\"inputSchema\":{\"type\":\"object\"}}]}}"
"#;
        let mut server = shell_server(script);
        server
            .env
            .insert("DISPLAY".to_string(), "host-bogus".to_string());
        server.env.insert(
            "DBUS_SESSION_BUS_ADDRESS".to_string(),
            "unix:path=/host-bus".to_string(),
        );
        server
            .env
            .insert("WAYLAND_DISPLAY".to_string(), "host-wayland".to_string());
        let handle = tokio::runtime::Handle::current();
        let result = tokio::task::spawn_blocking(move || {
            crate::tools::isolated_desktop::with_scope(Some(desktop), || {
                handle.block_on(list_tools_stdio(&server, Duration::from_secs(8)))
            })
        })
        .await
        .unwrap();
        crate::tools::isolated_desktop::discard_scope(&cleanup);
        let tools = result.expect("scoped stdio MCP call");
        assert_eq!(tools[0].1, "isolated");
    }

    #[tokio::test]
    async fn stdio_transport_calls_a_tool_on_a_fake_server() {
        // The server replies to tools/call (id 2) with a text content block.
        let script = "\
read -r _init
echo '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}'
read -r _note
read -r _call
echo '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"recon done\"}]}}'
";
        let server = StdioServer {
            command: "sh".to_string(),
            args: vec!["-c".to_string(), script.to_string()],
            cwd: None,
            env: Default::default(),
        };
        let text = call_tool_stdio(
            &server,
            "security_recon",
            json!({"target": "localhost"}),
            std::time::Duration::from_secs(10),
        )
        .await
        .expect("call_tool_stdio");
        assert_eq!(text, "recon done");
    }

    #[tokio::test]
    async fn stdio_rejects_corrupt_stdout_and_nonzero_exit() {
        let corrupt = shell_server(
            "read -r _init\n\
             echo '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}'\n\
             read -r _note\nread -r _call\n\
             echo not-json\nsleep 60\n",
        );
        let error = call_tool_stdio(&corrupt, "probe", json!({}), Duration::from_secs(2))
            .await
            .unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains("non-JSON protocol data"), "{error}");

        let nonzero = shell_server(
            "read -r _init\n\
             echo '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}'\n\
             read -r _note\nread -r _call\n\
             echo '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"false success\"}]}}'\n\
             exit 7\n",
        );
        let error = call_tool_stdio(&nonzero, "probe", json!({}), Duration::from_secs(2))
            .await
            .unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains("exit status: 7"), "{error}");
    }

    #[tokio::test]
    async fn stdio_timeout_and_both_output_floods_are_bounded() {
        let timeout_server = shell_server(
            "read -r _init\n\
             echo '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}'\n\
             read -r _note\nread -r _call\n\
             sleep 60\n",
        );
        let started = Instant::now();
        let error = call_tool_stdio(
            &timeout_server,
            "probe",
            json!({}),
            Duration::from_millis(100),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("timed out"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(3));

        let mut stdout_flood = shell_server(
            "read -r _init\n\
             echo '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}'\n\
             read -r _note\nread -r _call\n\
             head -c \"$MCP_FLOOD_BYTES\" /dev/zero | tr '\\000' x\n\
             sleep 60\n",
        );
        stdout_flood.env.insert(
            "MCP_FLOOD_BYTES".to_string(),
            (MAX_STDIO_LINE_BYTES + 1).to_string(),
        );
        let error = call_tool_stdio(&stdout_flood, "probe", json!({}), Duration::from_secs(5))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("stdout line exceeded"), "{error}");

        let mut stderr_flood = shell_server(
            "read -r _init\n\
             echo '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}'\n\
             read -r _note\nread -r _call\n\
             head -c \"$MCP_FLOOD_BYTES\" /dev/zero >&2\n\
             sleep 60\n",
        );
        stderr_flood.env.insert(
            "MCP_FLOOD_BYTES".to_string(),
            (MAX_STDIO_STDERR_BYTES + 1).to_string(),
        );
        let error = call_tool_stdio(&stderr_flood, "probe", json!({}), Duration::from_secs(5))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("stderr exceeded"), "{error}");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn stdio_kills_background_descendants_and_reaps_early_success_server() {
        let pid_dir = tempfile::tempdir().unwrap();
        let background_pid_path = pid_dir.path().join("background.pid");
        let mut background = shell_server(
            "read -r _init\n\
             echo '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}'\n\
             read -r _note\nread -r _call\n\
             (trap '' HUP TERM; while :; do :; done) &\n\
             echo \"$!\" > \"$MCP_PID_FILE\"\n\
             echo '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"done\"}]}}'\n\
             exit 0\n",
        );
        background.env.insert(
            "MCP_PID_FILE".to_string(),
            background_pid_path.display().to_string(),
        );
        let error = call_tool_stdio(&background, "probe", json!({}), Duration::from_secs(3))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("background descendant"), "{error}");
        let background_pid = std::fs::read_to_string(&background_pid_path)
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        assert!(!process_is_live(background_pid));

        let leader_pid_path = pid_dir.path().join("leader.pid");
        let mut lingering = shell_server(
            "echo \"$$\" > \"$MCP_PID_FILE\"\n\
             read -r _init\n\
             echo '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}'\n\
             read -r _note\nread -r _call\n\
             echo '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"done\"}]}}'\n\
             while :; do :; done\n",
        );
        lingering.env.insert(
            "MCP_PID_FILE".to_string(),
            leader_pid_path.display().to_string(),
        );
        let result = call_tool_stdio(&lingering, "probe", json!({}), Duration::from_secs(3))
            .await
            .unwrap();
        assert_eq!(result, "done");
        let leader_pid = std::fs::read_to_string(&leader_pid_path)
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        assert!(!process_is_live(leader_pid));
    }

    /// End-to-end against the REAL built T3MP3ST server. Ignored by default
    /// (needs node + external/T3MP3ST built); run explicitly:
    ///   cargo test --lib real_t3mp3st -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn real_t3mp3st_lists_security_recon() {
        let cwd = concat!(env!("CARGO_MANIFEST_DIR"), "/../external/T3MP3ST");
        let server = StdioServer {
            command: "node".to_string(),
            args: vec!["dist/mcp-server.js".to_string()],
            cwd: Some(cwd.to_string()),
            env: Default::default(),
        };
        let tools = list_tools_stdio(&server, std::time::Duration::from_secs(30))
            .await
            .expect("T3MP3ST tools/list");
        assert!(
            tools.iter().any(|(name, _, _)| name == "security_recon"),
            "expected security_recon, got: {:?}",
            tools.iter().map(|t| &t.0).collect::<Vec<_>>()
        );
    }
}
