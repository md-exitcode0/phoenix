use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentTurnResponse {
    ToolRequest {
        tool_calls: Vec<RequestedToolCall>,
        #[serde(default)]
        rationale: String,
    },
    Final(FinalResponse),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestedToolCall {
    #[serde(alias = "tool", alias = "name", alias = "function")]
    pub tool_name: String,
    #[serde(
        default,
        alias = "args",
        alias = "arguments",
        alias = "parameters",
        alias = "tool_input"
    )]
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FinalResponse {
    #[serde(default)]
    pub summary: String,
    pub final_markdown: String,
    #[serde(default)]
    pub changes_made: Vec<String>,
    #[serde(default)]
    pub verification: Vec<String>,
    pub execution_mode: String,
    #[serde(default)]
    pub tool_transcript: Vec<TurnToolTranscriptEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TurnToolTranscriptEntry {
    #[serde(alias = "tool_name")]
    pub name: String,
    #[serde(
        default,
        alias = "input",
        deserialize_with = "deserialize_summary_value"
    )]
    pub input_summary: String,
    #[serde(default, alias = "result_summary")]
    pub outcome: String,
}

pub fn parse_agent_turn_response(content: &str) -> anyhow::Result<AgentTurnResponse> {
    // Reasoning models (MiniMax-M3, DeepSeek-R1, Kimi-thinking, …) wrap their
    // chain-of-thought in <think>…</think> before the JSON envelope. Strip it so
    // the envelope parses cleanly. (Ported from OpenClaw's reasoning-tags pass.)
    let dethought = strip_reasoning_tags(content);
    let content = strip_json_fences(&dethought);

    if let Ok(parsed) = serde_json::from_str(&content) {
        return Ok(normalize_agent_turn_response(parsed));
    }

    if let Some(parsed) = extract_agent_turn_response(&content) {
        return Ok(normalize_agent_turn_response(parsed));
    }

    // Some weak providers print a *bare* tool object as the whole response:
    // `{ "tool_name": "str_replace", "input": { ... } }`.
    // That is not a valid Phoenix envelope, but it is clearly executable intent.
    // Recover it as a one-call tool_request instead of wrapping/leaking it as a
    // plain-text final.
    if let Some(call) = extract_bare_tool_call(&content) {
        return Ok(AgentTurnResponse::ToolRequest {
            tool_calls: vec![call],
            rationale: "Recovered bare printed tool object as an executable tool request."
                .to_string(),
        });
    }

    // Last resort: some models (esp. free/non-native-tool-calling ones) emit tool
    // calls as XML text instead of the JSON envelope — e.g. mimo emitting
    // `<tool_request><tool name="talk" args={...}/></tool_request>`. Recover those
    // so delegation still fires (and the XML doesn't leak into the final answer).
    let xml_calls = extract_xml_tool_calls(&content);
    if !xml_calls.is_empty() {
        return Ok(AgentTurnResponse::ToolRequest {
            tool_calls: xml_calls,
            rationale: String::new(),
        });
    }

    // Final resort: the envelope is a tool_request whose JSON is *malformed* —
    // typically a cheap model leaving an unescaped quote/newline inside a long
    // string (e.g. a `talk` body), which prematurely closes the string and
    // breaks strict parsing. Rather than coerce the broken envelope into a
    // leaked "final", field-salvage the tool call so delegation still fires.
    if let Some(calls) = salvage_tool_calls(&content) {
        if !calls.is_empty() {
            return Ok(AgentTurnResponse::ToolRequest {
                tool_calls: calls,
                rationale: String::new(),
            });
        }
    }

    anyhow::bail!("failed to parse agent turn response JSON")
}

fn extract_bare_tool_call(content: &str) -> Option<RequestedToolCall> {
    let trimmed = content.trim();
    // Native-tool providers sometimes print the call in OpenAI's shape
    // (`{"name":"final_answer","arguments":{...}}`) instead of placing it
    // in `response.tool_calls`. RequestedToolCall already accepts these field
    // aliases; the old pre-check accidentally made those aliases unreachable.
    if !trimmed.contains("\"tool_name\"")
        && !trimmed.contains("\"name\"")
        && !trimmed.contains("\"function\"")
    {
        return None;
    }

    if let Ok(mut call) = serde_json::from_str::<RequestedToolCall>(trimmed) {
        normalize_printed_call_input(&mut call);
        if !call.tool_name.trim().is_empty() {
            return Some(call);
        }
    }

    for (index, ch) in trimmed.char_indices() {
        if ch != '{' {
            continue;
        }
        let Some(candidate) = extract_balanced_json_object(trimmed, index) else {
            continue;
        };
        let Ok(mut call) = serde_json::from_str::<RequestedToolCall>(candidate) else {
            continue;
        };
        normalize_printed_call_input(&mut call);
        if !call.tool_name.trim().is_empty() {
            return Some(call);
        }
    }

    None
}

fn normalize_printed_call_input(call: &mut RequestedToolCall) {
    // A printed OpenAI function call often JSON-encodes `arguments` as a
    // string. Tool execution needs the decoded object, exactly as if the
    // provider had returned a real native call.
    if let serde_json::Value::String(raw) = &call.input {
        if let Ok(decoded) = serde_json::from_str::<serde_json::Value>(raw) {
            call.input = decoded;
        }
    }
}

/// Lenient recovery of tool calls from a malformed `tool_request` envelope.
///
/// Strict JSON parsing fails when a model emits an unescaped `"` or raw newline
/// inside a long string value (common with cheap models on big `talk` bodies).
/// This does NOT try to repair the JSON — it pulls the structurally-important
/// fields (`tool_name`, and for `talk` the `to`/`subject`/`mode`/`body`) by
/// scanning, so a fumbled-but-present delegation routes instead of leaking the
/// raw envelope to the user. Returns `None` when no tool name can be recovered.
fn salvage_tool_calls(content: &str) -> Option<Vec<RequestedToolCall>> {
    // Only attempt this when the content is clearly a tool_request envelope.
    if !content.contains("\"tool_request\"") || !content.contains("\"tool_name\"") {
        return None;
    }

    let mut calls = Vec::new();
    // Walk each `"tool_name"` occurrence and rebuild a best-effort call.
    let mut search_from = 0usize;
    while let Some(rel) = content[search_from..].find("\"tool_name\"") {
        let abs = search_from + rel;
        search_from = abs + "\"tool_name\"".len();

        let Some(tool_name) = lenient_string_field(&content[abs..], "tool_name") else {
            continue;
        };

        // Recover known input fields by best-effort scan within a bounded window
        // after this tool_name (up to the next tool_name or end of content).
        let window_end = content[search_from..]
            .find("\"tool_name\"")
            .map(|r| search_from + r)
            .unwrap_or(content.len());
        let window = &content[abs..window_end];

        let mut input = serde_json::Map::new();
        for key in [
            "to", "subject", "body", "path", "pattern", "query", "command",
        ] {
            if let Some(value) = lenient_string_field(window, key) {
                input.insert(key.to_string(), serde_json::Value::String(value));
            }
        }
        if let Some(mode) = lenient_int_field(window, "mode") {
            input.insert(key_mode(), serde_json::Value::from(mode));
        }

        calls.push(RequestedToolCall {
            tool_name,
            input: serde_json::Value::Object(input),
        });
    }

    if calls.is_empty() {
        None
    } else {
        Some(calls)
    }
}

fn key_mode() -> String {
    "mode".to_string()
}

/// Extract a string field's value from possibly-malformed JSON by key, tolerating
/// unescaped quotes by greedily taking text up to the next `","key2"` boundary or
/// a closing brace. Best-effort: returns the recovered text trimmed.
fn lenient_string_field(haystack: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let start = haystack.find(&needle)?;
    let after = &haystack[start + needle.len()..];
    let colon = after.find(':')?;
    let rest = after[colon + 1..].trim_start();
    let mut chars = rest.char_indices();
    if chars.next()?.1 != '"' {
        return None;
    }
    // Take until a quote that is immediately followed by `,` `}` or `"` (the next
    // key) — this skips over stray unescaped quotes inside the value.
    let value_region = &rest[1..];
    let bytes = value_region.as_bytes();
    let mut i = 0usize;
    let mut out = String::new();
    let mut escaped = false;
    while i < value_region.len() {
        let ch = bytes[i] as char;
        if escaped {
            match ch {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                other => out.push(other),
            }
            escaped = false;
            i += 1;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            i += 1;
            continue;
        }
        if ch == '"' {
            // Look ahead: is this a genuine string terminator?
            let lookahead = value_region[i + 1..].trim_start();
            if lookahead.starts_with(',')
                || lookahead.starts_with('}')
                || lookahead.starts_with("\"")
            {
                break;
            }
            // Stray unescaped quote inside the value — keep it.
            out.push('"');
            i += 1;
            continue;
        }
        out.push(ch);
        i += 1;
    }
    let trimmed = out.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Extract an integer field (e.g. `mode`) from possibly-malformed JSON by key.
fn lenient_int_field(haystack: &str, key: &str) -> Option<i64> {
    let needle = format!("\"{key}\"");
    let start = haystack.find(&needle)?;
    let after = &haystack[start + needle.len()..];
    let colon = after.find(':')?;
    let rest = after[colon + 1..].trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Recover tool calls from XML-style syntax some models emit instead of the JSON
/// envelope. Handles two common shapes:
///   - `<tool name="talk" args={ ...json... } />`  (attribute style)
///   - `<tool_call>{"name":"talk","arguments":{...}}</tool_call>`  (Nous/Hermes style)
fn extract_xml_tool_calls(content: &str) -> Vec<RequestedToolCall> {
    let mut calls = Vec::new();

    // Form 1: <tool_call>{json}</tool_call>
    let mut rest = content;
    while let Some(open) = rest.find("<tool_call>") {
        let after = &rest[open + "<tool_call>".len()..];
        let Some(close) = after.find("</tool_call>") else {
            break;
        };
        let inner = &after[..close];
        if let Some(start) = inner.find('{') {
            if let Some(object) = extract_balanced_json_object(inner, start) {
                if let Ok(call) = serde_json::from_str::<RequestedToolCall>(object) {
                    calls.push(call);
                }
            }
        }
        rest = &after[close + "</tool_call>".len()..];
    }

    // Form 2: <tool name="X" args={json} />
    let mut rest = content;
    while let Some(open) = rest.find("<tool ") {
        let tag = &rest[open..];
        let name = tag.find("name=\"").and_then(|index| {
            let after = &tag[index + "name=\"".len()..];
            after.find('"').map(|end| after[..end].to_string())
        });
        let input = tag
            .find("args=")
            .map(|index| index + "args=".len())
            .or_else(|| tag.find("input=").map(|index| index + "input=".len()))
            .and_then(|value_start| {
                let after = &tag[value_start..];
                let brace = after.find('{')?;
                extract_balanced_json_object(after, brace)
            })
            .and_then(|object| serde_json::from_str::<serde_json::Value>(object).ok())
            .unwrap_or(serde_json::Value::Null);

        if let Some(name) = name.filter(|name| !name.trim().is_empty()) {
            calls.push(RequestedToolCall {
                tool_name: name,
                input,
            });
        }

        let advance = tag
            .find('>')
            .map(|index| open + index + 1)
            .unwrap_or(rest.len());
        rest = &rest[advance..];
    }

    calls
}

fn extract_agent_turn_response(content: &str) -> Option<AgentTurnResponse> {
    let mut parsed_candidates = Vec::new();

    for (index, ch) in content.char_indices() {
        if ch != '{' {
            continue;
        }

        let Some(candidate) = extract_balanced_json_object(content, index) else {
            continue;
        };

        let Ok(parsed) = serde_json::from_str::<AgentTurnResponse>(candidate) else {
            continue;
        };

        parsed_candidates.push(parsed);
    }

    parsed_candidates
        .iter()
        .rev()
        .find(|candidate| matches!(candidate, AgentTurnResponse::Final(_)))
        .cloned()
        .or_else(|| parsed_candidates.into_iter().last())
}

fn normalize_agent_turn_response(response: AgentTurnResponse) -> AgentTurnResponse {
    match response {
        AgentTurnResponse::Final(mut final_response) => {
            if final_response.summary.trim().is_empty() {
                final_response.summary = first_nonempty_line(&final_response.final_markdown)
                    .unwrap_or("Final response")
                    .to_string();
            }
            AgentTurnResponse::Final(final_response)
        }
        other => other,
    }
}

/// Strip ```json / ``` fences so models that wrap JSON in code blocks still parse.
/// Reasoning-tag names that thinking models emit (with optional `antml:` prefix).
const REASONING_TAG_NAMES: &[&str] = &["thinking", "think", "thought", "antthinking"];

/// If `content[at..]` begins with a reasoning open/close tag, return
/// `(is_close, index_just_past_'>')`. `at` must be the index of a `<`.
fn match_reasoning_tag(content: &str, at: usize) -> Option<(bool, usize)> {
    let after_lt = at + 1;
    let gt_rel = content[after_lt..].find('>')?;
    let gt = after_lt + gt_rel;
    let inner = content[after_lt..gt].trim();

    let (is_close, name_part) = match inner.strip_prefix('/') {
        Some(rest) => (true, rest.trim_start()),
        None => (false, inner),
    };
    let name_part = name_part.strip_prefix("antml:").unwrap_or(name_part);
    let lower = name_part.to_ascii_lowercase();

    let matched = REASONING_TAG_NAMES.iter().any(|name| {
        lower
            .strip_prefix(name)
            // boundary: end of tag, or a non-alphanumeric char (whitespace/attr/`/`)
            .is_some_and(|rest| {
                rest.chars()
                    .next()
                    .is_none_or(|c| !c.is_ascii_alphanumeric())
            })
    });

    matched.then_some((is_close, gt + 1))
}

/// Split model output into `(visible, reasoning)`. `<think>…</think>` blocks
/// (and `<thinking>`/`<thought>`/`<antthinking>`/`antml:` variants) become
/// reasoning; everything else is visible. Handles nested blocks, an unclosed
/// trailing block (truncated mid-thought → its body is reasoning), and an orphan
/// close whose opener was lost at stream start (preceding text is reasoning).
/// Ported from OpenClaw's `stripReasoningTagsFromText`.
pub fn split_reasoning_tags(content: &str) -> (String, Option<String>) {
    let lower = content.to_ascii_lowercase();
    if !(lower.contains("<think")
        || lower.contains("</think")
        || lower.contains("<thought")
        || lower.contains("</thought")
        || lower.contains("antthinking"))
    {
        return (content.to_string(), None);
    }

    let mut visible = String::new();
    let mut reasoning = String::new();
    let mut depth: i32 = 0;
    let mut seg_start = 0usize;
    let mut i = 0usize;

    while i < content.len() {
        let ch = content[i..].chars().next().unwrap();
        if ch == '<' {
            if let Some((is_close, after)) = match_reasoning_tag(content, i) {
                let segment = &content[seg_start..i];
                if depth > 0 {
                    reasoning.push_str(segment);
                } else {
                    visible.push_str(segment);
                }
                if is_close {
                    if depth > 0 {
                        depth -= 1;
                    } else if !visible.trim().is_empty() {
                        // Orphan close at depth 0: the visible text so far was
                        // actually reasoning whose opening tag was lost.
                        reasoning.push_str(&visible);
                        visible.clear();
                    }
                } else {
                    depth += 1;
                }
                seg_start = after;
                i = after;
                continue;
            }
        }
        i += ch.len_utf8();
    }

    let tail = &content[seg_start..];
    if depth > 0 {
        reasoning.push_str(tail);
    } else {
        visible.push_str(tail);
    }

    let visible = visible.trim().to_string();
    let reasoning = reasoning.trim();
    let reasoning = (!reasoning.is_empty()).then(|| reasoning.to_string());
    (visible, reasoning)
}

/// Convenience: drop reasoning tags, keep only the visible text.
pub fn strip_reasoning_tags(content: &str) -> String {
    split_reasoning_tags(content).0
}

pub(crate) fn strip_json_fences(content: &str) -> String {
    let trimmed = content.trim();

    // ```json ... ```
    if let Some(inner) = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|rest| rest.strip_suffix("```"))
    {
        return inner.trim().to_string();
    }

    // ```json\n...\n``` (multiline)
    if trimmed.len() >= 6 && trimmed.starts_with("```") && trimmed.ends_with("```") {
        let inner = &trimmed[3..trimmed.len() - 3];
        let inner = inner
            .strip_prefix("json\n")
            .or_else(|| inner.strip_prefix("json"))
            .or_else(|| inner.strip_prefix("\n"))
            .unwrap_or(inner);
        return inner.trim().to_string();
    }

    content.to_string()
}

fn first_nonempty_line(value: &str) -> Option<&str> {
    value.lines().map(str::trim).find(|line| !line.is_empty())
}

/// Best-effort extraction of polished user text when a model puts AgentTurn JSON in a display field.
pub fn user_facing_markdown(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    if let Ok(AgentTurnResponse::Final(final_response)) = parse_agent_turn_response(trimmed) {
        let markdown = final_response.final_markdown.trim();
        if !markdown.is_empty() {
            return markdown.to_string();
        }
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(markdown) = value
            .get("final_markdown")
            .and_then(|field| field.as_str())
            .map(str::trim)
            .filter(|field| !field.is_empty())
        {
            return markdown.to_string();
        }
        if let Some(summary) = value
            .get("summary")
            .and_then(|field| field.as_str())
            .map(str::trim)
            .filter(|field| !field.is_empty() && !looks_like_agent_turn_json(field))
        {
            return summary.to_string();
        }
    }

    trimmed.to_string()
}

fn looks_like_agent_turn_json(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with('{')
        && trimmed.contains("\"type\"")
        && (trimmed.contains("\"final\"") || trimmed.contains("\"tool_request\""))
}

pub(crate) fn extract_balanced_json_object(content: &str, start: usize) -> Option<&str> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaping = false;

    for (offset, ch) in content[start..].char_indices() {
        if in_string {
            if escaping {
                escaping = false;
                continue;
            }

            match ch {
                '\\' => escaping = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
                if depth == 0 {
                    let end = start + offset + ch.len_utf8();
                    return content.get(start..end);
                }
            }
            _ => {}
        }
    }

    None
}

fn deserialize_summary_value<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(value) => value,
        other => other.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_reasoning_extracts_think_block_before_envelope() {
        // The exact MiniMax-M3 shape: <think>…</think> then the JSON envelope.
        let raw = "<think>\nThe user wants X. I'll do Y.\n</think>\n\n{\"type\":\"final\",\"final_markdown\":\"hi\",\"execution_mode\":\"x\"}";
        let (visible, reasoning) = split_reasoning_tags(raw);
        assert!(visible.starts_with('{') && visible.ends_with('}'));
        assert_eq!(reasoning.as_deref(), Some("The user wants X. I'll do Y."));
        // And the envelope parses cleanly through the full path.
        assert!(matches!(
            parse_agent_turn_response(raw).unwrap(),
            AgentTurnResponse::Final(_)
        ));
    }

    #[test]
    fn split_reasoning_handles_passthrough_nested_orphan_unclosed_and_variants() {
        // No tags → unchanged, no reasoning.
        assert_eq!(
            split_reasoning_tags("plain answer"),
            ("plain answer".to_string(), None)
        );
        // Does not match lookalike words.
        assert_eq!(
            split_reasoning_tags("a thinkable idea"),
            ("a thinkable idea".to_string(), None)
        );
        // Nested blocks.
        let (v, r) = split_reasoning_tags("<think>a<think>b</think>c</think>VISIBLE");
        assert_eq!(v, "VISIBLE");
        assert_eq!(r.as_deref(), Some("abc"));
        // Orphan close (opener lost): preceding text is reasoning.
        let (v, r) = split_reasoning_tags("leftover reasoning</think>\n\nANSWER");
        assert_eq!(v, "ANSWER");
        assert_eq!(r.as_deref(), Some("leftover reasoning"));
        // Unclosed (truncated mid-thought): body is reasoning, visible empty.
        let (v, r) = split_reasoning_tags("<thinking>still going");
        assert_eq!(v, "");
        assert_eq!(r.as_deref(), Some("still going"));
        // antml: prefix variant.
        let (v, _r) = split_reasoning_tags("<think>x</think>OUT");
        assert_eq!(v, "OUT");
    }

    #[test]
    fn salvages_talk_from_malformed_envelope_with_unescaped_quote() {
        // The exact failure shape from the live trace: a long talk body with an
        // unescaped quote prematurely closes the string and breaks strict JSON.
        // The runtime must still recover the talk and route it, not leak it.
        let broken = r#"{"type":"tool_request","tool_calls":[{"tool_name":"talk","input":{"to":"coder","mode":1,"subject":"Health check","body":"Run cargo test or an honest "nothing to fix" with evidence."}}],"rationale":"gate..."}"#;
        // Strict parse must fail (sanity: this really is malformed).
        assert!(serde_json::from_str::<AgentTurnResponse>(broken).is_err());

        match parse_agent_turn_response(broken).expect("should salvage") {
            AgentTurnResponse::ToolRequest { tool_calls, .. } => {
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].tool_name, "talk");
                assert_eq!(tool_calls[0].input["to"], "coder");
                assert_eq!(tool_calls[0].input["mode"], 1);
                assert!(tool_calls[0].input["body"]
                    .as_str()
                    .unwrap()
                    .contains("Run cargo test"));
            }
            AgentTurnResponse::Final(_) => panic!("malformed talk envelope leaked as a final"),
        }
    }

    #[test]
    fn salvages_bare_printed_tool_object() {
        let raw = r#"{"tool_name":"str_replace","input":{"path":"src/lib.rs","old_str":"old","new_str":"new"},"rationale":"I should edit now."}"#;
        match parse_agent_turn_response(raw).expect("should recover bare tool object") {
            AgentTurnResponse::ToolRequest {
                tool_calls,
                rationale,
            } => {
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].tool_name, "str_replace");
                assert_eq!(tool_calls[0].input["path"], "src/lib.rs");
                assert!(rationale.contains("Recovered bare printed tool object"));
            }
            AgentTurnResponse::Final(_) => panic!("bare tool object leaked as final"),
        }
    }

    #[test]
    fn salvages_printed_native_final_answer_call() {
        let raw = r#"{"name":"final_answer","arguments":"{\"summary\":\"Done\",\"final_markdown\":\"The browser verification passed.\",\"execution_mode\":\"browser\"}"}"#;
        match parse_agent_turn_response(raw).expect("should recover printed native call") {
            AgentTurnResponse::ToolRequest { tool_calls, .. } => {
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].tool_name, "final_answer");
                assert_eq!(
                    tool_calls[0].input["final_markdown"],
                    "The browser verification passed."
                );
            }
            AgentTurnResponse::Final(_) => panic!("printed final_answer call was not recovered"),
        }
    }

    #[test]
    fn does_not_salvage_genuine_prose() {
        // Real prose must NOT be mistaken for a malformed envelope.
        assert!(parse_agent_turn_response("The build passes and 166 tests are green.").is_err());
    }

    #[test]
    fn parses_tool_request_response() {
        let parsed = parse_agent_turn_response(
            r#"{
                "type": "tool_request",
                "tool_calls": [
                    { "tool_name": "glob", "input": { "pattern": "src/**/*.rs" } }
                ],
                "rationale": "Inspect the repo first."
            }"#,
        )
        .unwrap();

        match parsed {
            AgentTurnResponse::ToolRequest {
                tool_calls,
                rationale,
            } => {
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].tool_name, "glob");
                assert_eq!(rationale, "Inspect the repo first.");
            }
            AgentTurnResponse::Final(_) => panic!("expected tool request"),
        }
    }

    #[test]
    fn recovers_xml_tool_request_attribute_style() {
        // mimo-style leak: tool call emitted as XML instead of the JSON envelope.
        let raw = r#"<tool_request>
<tool name="talk" args={"to": "coder", "mode": 1, "subject": "Find value", "body": "Objective: find AGENT_MAX_TOOL_ROUNDS."} />
</tool_request>"#;
        match parse_agent_turn_response(raw).unwrap() {
            AgentTurnResponse::ToolRequest { tool_calls, .. } => {
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].tool_name, "talk");
                assert_eq!(tool_calls[0].input["to"], "coder");
                assert_eq!(tool_calls[0].input["mode"], 1);
            }
            AgentTurnResponse::Final(_) => panic!("expected recovered tool request, not a final"),
        }
    }

    #[test]
    fn recovers_tool_call_nous_style() {
        let raw = r#"Sure, let me check. <tool_call>{"name": "grep", "arguments": {"pattern": "foo"}}</tool_call>"#;
        match parse_agent_turn_response(raw).unwrap() {
            AgentTurnResponse::ToolRequest { tool_calls, .. } => {
                assert_eq!(tool_calls[0].tool_name, "grep");
                assert_eq!(tool_calls[0].input["pattern"], "foo");
            }
            AgentTurnResponse::Final(_) => panic!("expected recovered tool request"),
        }
    }

    #[test]
    fn parses_tool_request_with_tool_and_args_aliases() {
        // Wrong keys (`tool`/`args`) some models use instead of `tool_name`/`input`.
        let parsed = parse_agent_turn_response(
            r#"{ "type": "tool_request", "tool_calls": [ { "tool": "glob", "args": { "pattern": "*.rs" } } ], "rationale": "x" }"#,
        )
        .unwrap();
        match parsed {
            AgentTurnResponse::ToolRequest { tool_calls, .. } => {
                assert_eq!(tool_calls[0].tool_name, "glob");
                assert_eq!(tool_calls[0].input["pattern"], "*.rs");
            }
            AgentTurnResponse::Final(_) => panic!("expected tool request"),
        }
    }

    #[test]
    fn user_facing_markdown_extracts_from_json_blob() {
        let blob = serde_json::json!({
            "type": "final",
            "summary": "x",
            "final_markdown": "## Hello\nDone.",
            "changes_made": [],
            "verification": [],
            "execution_mode": "test",
            "tool_transcript": []
        })
        .to_string();
        assert_eq!(user_facing_markdown(&blob), "## Hello\nDone.");
    }

    #[test]
    fn user_facing_markdown_passes_through_plain_markdown() {
        assert_eq!(user_facing_markdown("## Plain answer"), "## Plain answer");
    }

    #[test]
    fn parses_final_response() {
        let parsed = parse_agent_turn_response(
            r###"{
                "type": "final",
                "summary": "Done.",
                "final_markdown": "## Done",
                "changes_made": [],
                "verification": ["cargo test"],
                "execution_mode": "provider_tools_readonly",
                "tool_transcript": []
            }"###,
        )
        .unwrap();

        match parsed {
            AgentTurnResponse::Final(final_response) => {
                assert_eq!(final_response.execution_mode, "provider_tools_readonly");
            }
            AgentTurnResponse::ToolRequest { .. } => panic!("expected final response"),
        }
    }

    #[test]
    fn parses_provider_final_without_summary() {
        let parsed = parse_agent_turn_response(
            r###"{
                "type": "final",
                "decision": "Direct answer",
                "reasoning": "The user requested an exact phrase.",
                "final_markdown": "provider smoke ok",
                "changes_made": [],
                "verification": ["Exact phrase returned verbatim"],
                "execution_mode": "provider_direct",
                "tool_transcript": []
            }"###,
        )
        .unwrap();

        match parsed {
            AgentTurnResponse::Final(final_response) => {
                assert_eq!(final_response.summary, "provider smoke ok");
                assert_eq!(final_response.final_markdown, "provider smoke ok");
            }
            AgentTurnResponse::ToolRequest { .. } => panic!("expected final response"),
        }
    }

    #[test]
    fn parses_json_with_trailing_commentary() {
        let parsed = parse_agent_turn_response(
            r#"{
                "type": "tool_request",
                "tool_calls": [
                    { "tool_name": "read", "input": { "path": "src/main.rs" } }
                ],
                "rationale": "Inspect the entry point first."
            }
            I will summarize after the tool result."#,
        )
        .unwrap();

        match parsed {
            AgentTurnResponse::ToolRequest { tool_calls, .. } => {
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].tool_name, "read");
            }
            AgentTurnResponse::Final(_) => panic!("expected tool request"),
        }
    }

    #[test]
    fn prefers_last_final_when_multiple_json_objects_exist() {
        let parsed = parse_agent_turn_response(
            r###"{
                "type": "tool_request",
                "tool_calls": [
                    { "tool_name": "read", "input": { "path": "src/lib.rs" } }
                ],
                "rationale": "Initial thought."
            }
            --- revised answer ---
            {
                "type": "final",
                "summary": "Enough context gathered.",
                "final_markdown": "## Result\nEnough context gathered.",
                "changes_made": [],
                "verification": [],
                "execution_mode": "provider_tools_readonly",
                "tool_transcript": []
            }"###,
        )
        .unwrap();

        match parsed {
            AgentTurnResponse::Final(final_response) => {
                assert_eq!(final_response.summary, "Enough context gathered.");
            }
            AgentTurnResponse::ToolRequest { .. } => panic!("expected final response"),
        }
    }

    #[test]
    fn parses_provider_friendly_tool_transcript_aliases() {
        let parsed = parse_agent_turn_response(
            r###"{
                "type": "final",
                "summary": "Done.",
                "final_markdown": "## Done",
                "changes_made": [],
                "verification": [],
                "execution_mode": "provider_tools_readonly",
                "tool_transcript": [
                    {
                        "tool_name": "talk",
                        "status": "success",
                        "input": { "to": "coder", "subject": "Inspect" },
                        "result_summary": "Coder replied."
                    }
                ]
            }"###,
        )
        .unwrap();

        match parsed {
            AgentTurnResponse::Final(final_response) => {
                assert_eq!(final_response.tool_transcript[0].name, "talk");
                assert!(final_response.tool_transcript[0]
                    .input_summary
                    .contains("\"to\":\"coder\""));
                assert_eq!(final_response.tool_transcript[0].outcome, "Coder replied.");
            }
            AgentTurnResponse::ToolRequest { .. } => panic!("expected final response"),
        }
    }
}
