//! Shared OpenAI-compatible tool call serialization and parsing.
//!
//! Used by providers that speak the OpenAI chat/completions wire format:
//! OpenAI, OpenRouter, DeepSeek, Ollama (openai-compat), and any future
//! OpenAI-compatible gateway.

use super::contracts::{
    ChatMessage, CompletionResponse, MessageRole, NativeToolCall, TokenUsage, ToolDefinition,
};

/// Strip the `Tool result. / Tool: / Success: / Input:` header that
/// `Message::format_tool_result` prepends, leaving just the tool's output.
///
/// Failures keep an explicit `Error:` prefix — dropping the whole header would
/// otherwise make a failed call indistinguishable from a successful one, since
/// the OpenAI tool message carries no status field of its own.
fn strip_tool_result_header(content: &str) -> String {
    if !content.starts_with("Tool result.\n") {
        return content.to_string();
    }
    let failed = content
        .lines()
        .take(4)
        .any(|l| l.trim_start().starts_with("Success:") && l.contains("no"));
    // The header is separated from the payload by the first blank line.
    let body = match content.find("\n\n") {
        Some(idx) => &content[idx + 2..],
        None => return content.to_string(),
    };
    if failed {
        format!("Error: {body}")
    } else {
        body.to_string()
    }
}

/// Serialize a `ChatMessage` to the OpenAI API message object.
///
/// Handles assistant messages with tool calls, tool result messages,
/// and plain user/system messages.
pub fn serialize_message(m: &ChatMessage) -> serde_json::Value {
    if !m.tool_calls.is_empty() {
        let calls: Vec<serde_json::Value> = m
            .tool_calls
            .iter()
            .map(|tc| {
                serde_json::json!({
                    "id": tc.id,
                    "type": "function",
                    "function": {
                        "name": tc.tool_name,
                        "arguments": serde_json::to_string(&tc.arguments)
                            .unwrap_or_else(|_| "{}".to_string())
                    }
                })
            })
            .collect();
        let content = if m.content.trim().is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::json!(m.content)
        };
        serde_json::json!({
            "role": "assistant",
            "content": content,
            "tool_calls": calls
        })
    } else if m.role == MessageRole::Tool {
        // Send the tool's OUTPUT, not Phoenix's transcript header. On the
        // native path `tool_call_id` already binds this result to its call and
        // the arguments are in the assistant's `tool_calls`, so the
        // "Tool result. / Tool: / Success: / Input:" block is pure redundancy —
        // and it is the format models were copying back as their answer.
        serde_json::json!({
            "role": "tool",
            "content": strip_tool_result_header(&m.content),
            "tool_call_id": m.tool_call_id.as_deref().unwrap_or("")
        })
    } else {
        // Inline images: OpenAI multipart content — a text part followed by
        // one image_url part per attached data URI.
        let content =
            if m.images.is_empty() {
                serde_json::json!(m.content)
            } else {
                let mut parts = vec![serde_json::json!({"type": "text", "text": m.content})];
                parts.extend(m.images.iter().map(
                    |uri| serde_json::json!({"type": "image_url", "image_url": {"url": uri}}),
                ));
                serde_json::json!(parts)
            };
        let mut obj = serde_json::json!({
            "role": m.role.to_string(),
            "content": content,
        });
        if let Some(name) = &m.name {
            obj["name"] = serde_json::json!(name);
        }
        obj
    }
}

/// Serialize tool definitions to the OpenAI `tools` array format.
pub fn serialize_tool_definitions(tools: &[ToolDefinition]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters
                }
            })
        })
        .collect()
}

/// Attach tools and `tool_choice: "auto"` to a request body if tools are present.
pub fn attach_tools(body: &mut serde_json::Value, tools: &[ToolDefinition]) {
    if !tools.is_empty() {
        body["tools"] = serde_json::json!(serialize_tool_definitions(tools));
        body["tool_choice"] = serde_json::json!("auto");
    }
}

/// Parse native tool calls from an OpenAI chat/completions response.
///
/// Reads `choices[0].message.tool_calls[]` and deserializes each into a
/// `NativeToolCall`. Arguments are JSON strings that get parsed into values.
pub fn parse_tool_calls(json: &serde_json::Value) -> Vec<NativeToolCall> {
    json["choices"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|c| c["message"]["tool_calls"].as_array())
        .map(|arr| {
            arr.iter()
                .enumerate()
                .filter_map(|(idx, tc)| {
                    let name = tc["function"]["name"].as_str()?.to_string();
                    // Some OpenAI-compatible servers omit the call id — synthesize
                    // one so call/result pairing still works across rounds.
                    let id = tc["id"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .unwrap_or_else(|| format!("call_{name}_{idx}"));
                    // Arguments arrive as a JSON string on OpenAI, but some
                    // compatible servers send the object directly.
                    let arguments = match &tc["function"]["arguments"] {
                        serde_json::Value::String(s) => {
                            serde_json::from_str(s).unwrap_or(serde_json::json!({}))
                        }
                        serde_json::Value::Object(o) => serde_json::Value::Object(o.clone()),
                        _ => serde_json::json!({}),
                    };
                    Some(NativeToolCall {
                        id,
                        tool_name: name,
                        arguments,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Extract extended thinking / reasoning text when the provider returns it.
pub fn parse_reasoning(json: &serde_json::Value) -> Option<String> {
    let message = json.get("choices")?.as_array()?.first()?["message"].as_object()?;

    if let Some(text) = message
        .get("reasoning")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        return Some(text.to_string());
    }
    if let Some(text) = message
        .get("reasoning_content")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        return Some(text.to_string());
    }
    if let Some(details) = message.get("reasoning_details").and_then(|v| v.as_array()) {
        let mut parts = Vec::new();
        for item in details {
            if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                if !text.trim().is_empty() {
                    parts.push(text.trim());
                }
            } else if let Some(text) = item.get("content").and_then(|v| v.as_str()) {
                if !text.trim().is_empty() {
                    parts.push(text.trim());
                }
            }
        }
        if !parts.is_empty() {
            return Some(parts.join("\n"));
        }
    }
    None
}

/// Parse a full OpenAI chat/completions response into a `CompletionResponse`.
///
/// Extracts content, tool calls, model, usage, and finish reason.
pub fn parse_completion_response(
    json: &serde_json::Value,
    fallback_model: &str,
) -> CompletionResponse {
    let choice = &json["choices"][0];
    let content = choice["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let tool_calls = parse_tool_calls(json);
    let model = json["model"].as_str().unwrap_or(fallback_model).to_string();
    let input_tokens = json["usage"]["prompt_tokens"].as_u64().unwrap_or(0) as u32;
    let output_tokens = json["usage"]["completion_tokens"].as_u64().unwrap_or(0) as u32;
    let stop_reason = choice["finish_reason"].as_str().map(String::from);
    let reasoning = parse_reasoning(json);

    let mut usage = TokenUsage::new(input_tokens, output_tokens);
    // OpenAI implicit prompt caching reports the discounted subset here
    // (donor: opencode openai-chat protocol). Guard against nonsense
    // breakdowns (cached > prompt) some gateways emit — no cache figure
    // beats a lying one.
    usage.cache_read_tokens = json["usage"]["prompt_tokens_details"]["cached_tokens"]
        .as_u64()
        .map(|n| n as u32)
        .filter(|n| *n > 0 && *n <= input_tokens);

    CompletionResponse {
        content,
        model,
        usage,
        reasoning,
        stop_reason,
        tool_calls,
        provider_replay: None,
    }
}

/// Send the session as OpenAI's `prompt_cache_key`: requests sharing a key
/// route to the same cache shard, which is what turns automatic prefix
/// caching from "sometimes" into "every intra-session round" under load.
/// Only lanes documented to accept the param should call this — a strict
/// OpenAI-compat server may reject unknown fields.
pub fn attach_prompt_cache_key(body: &mut serde_json::Value, session_id: Option<&str>) {
    if let Some(key) = session_id.filter(|k| !k.is_empty()) {
        body["prompt_cache_key"] = serde_json::json!(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tool_calls_without_ids_and_with_object_arguments() {
        // Some OpenAI-compatible servers omit call ids and/or send arguments
        // as an object instead of a JSON string — both must still parse.
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [
                        {"function": {"name": "read", "arguments": "{\"path\": \"a.rs\"}"}},
                        {"function": {"name": "grep", "arguments": {"pattern": "fn main"}}}
                    ]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let calls = parse_tool_calls(&json);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "call_read_0");
        assert_eq!(calls[0].arguments["path"], "a.rs");
        assert_eq!(calls[1].id, "call_grep_1");
        assert_eq!(calls[1].arguments["pattern"], "fn main");
    }

    #[test]
    fn parses_cached_tokens_with_sanity_guard() {
        // OpenAI implicit caching reports the discounted subset; the E1/E2
        // telemetry had NO cache column because this was never parsed.
        let json = serde_json::json!({
            "choices": [{"message": {"content": "hi"}, "finish_reason": "stop"}],
            "model": "gpt-5.5",
            "usage": {
                "prompt_tokens": 40_000,
                "completion_tokens": 200,
                "prompt_tokens_details": {"cached_tokens": 38_500}
            }
        });
        let parsed = parse_completion_response(&json, "gpt-5.5");
        assert_eq!(parsed.usage.input_tokens, 40_000);
        assert_eq!(parsed.usage.cache_read_tokens, Some(38_500));

        // Nonsense breakdown (cached > prompt) → no cache figure, not a lie.
        let bogus = serde_json::json!({
            "choices": [{"message": {"content": "hi"}, "finish_reason": "stop"}],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 5,
                "prompt_tokens_details": {"cached_tokens": 900}
            }
        });
        assert_eq!(
            parse_completion_response(&bogus, "m")
                .usage
                .cache_read_tokens,
            None
        );

        // Absent details → None (lanes that don't report).
        let plain = serde_json::json!({
            "choices": [{"message": {"content": "hi"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 100, "completion_tokens": 5}
        });
        assert_eq!(
            parse_completion_response(&plain, "m")
                .usage
                .cache_read_tokens,
            None
        );
    }

    #[test]
    fn prompt_cache_key_attaches_only_when_session_present() {
        let mut body = serde_json::json!({"model": "gpt-5.5"});
        attach_prompt_cache_key(&mut body, Some("main-abc123"));
        assert_eq!(body["prompt_cache_key"], "main-abc123");

        let mut empty = serde_json::json!({"model": "gpt-5.5"});
        attach_prompt_cache_key(&mut empty, None);
        assert!(empty.get("prompt_cache_key").is_none());
        attach_prompt_cache_key(&mut empty, Some(""));
        assert!(empty.get("prompt_cache_key").is_none());
    }

    #[test]
    fn user_message_with_images_serializes_multipart_content() {
        let msg = ChatMessage::user_with_images(
            "CURRENT SCREEN",
            vec!["data:image/jpeg;base64,QUJD".to_string()],
        );
        let json = serialize_message(&msg);
        assert_eq!(json["role"], "user");
        let parts = json["content"].as_array().expect("multipart content");
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[0]["text"], "CURRENT SCREEN");
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(parts[1]["image_url"]["url"], "data:image/jpeg;base64,QUJD");
    }

    #[test]
    fn plain_messages_keep_string_content() {
        let json = serialize_message(&ChatMessage::user("hello"));
        assert_eq!(json["content"], "hello");
    }

    #[test]
    fn provider_issued_ids_are_kept() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "tool_calls": [
                        {"id": "abc-1", "function": {"name": "read", "arguments": "{}"}}
                    ]
                }
            }]
        });
        let calls = parse_tool_calls(&json);
        assert_eq!(calls[0].id, "abc-1");
    }
}

#[cfg(test)]
mod native_tool_request_tests {
    use super::*;
    use crate::providers::contracts::{ChatMessage, MessageRole};

    fn tool_msg(content: &str) -> ChatMessage {
        ChatMessage {
            role: MessageRole::Tool,
            content: content.to_string(),
            name: None,
            tool_call_id: Some("call-1".to_string()),
            tool_calls: vec![],
            images: vec![],
            provider_replay: None,
        }
    }

    #[test]
    fn tool_result_sends_output_without_the_header() {
        let m = tool_msg("Tool result.\nTool: read\nSuccess: yes\nInput: {\"path\":\"/a\"}\n\nfile contents here");
        let v = serialize_message(&m);
        assert_eq!(v["content"], serde_json::json!("file contents here"));
        assert_eq!(v["tool_call_id"], "call-1");
    }

    #[test]
    fn a_failed_tool_result_stays_marked_as_an_error() {
        // The OpenAI tool message has no status field, so dropping the whole
        // header would make a failure look like a success.
        let m = tool_msg("Tool result.\nTool: bash\nSuccess: no\nInput: {}\n\ncommand not found");
        let v = serialize_message(&m);
        assert_eq!(v["content"], serde_json::json!("Error: command not found"));
    }

    #[test]
    fn a_tool_message_without_the_header_is_left_alone() {
        let m = tool_msg("raw payload, no header");
        let v = serialize_message(&m);
        assert_eq!(v["content"], serde_json::json!("raw payload, no header"));
    }
}
