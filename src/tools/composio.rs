//! Composio — app-action reach (Gmail, Slack, GitHub, Notion, X, Reddit, …).
//!
//! ONE lane: the "Composio For You" managed MCP server, where the user's
//! dashboard-connected apps actually live. It exposes a search-then-execute
//! meta-tool flow relayed by four Phoenix tools: `composio_search` finds the
//! right tool across the user's apps, `composio_schemas` fetches exact input
//! schemas, `composio_run` executes, and `composio_connections` manages
//! (list/add/remove) per-app connections. Auth is the X-CONSUMER-API-KEY
//! (`ck_…`) at `~/.phoenix/composio-mcp.key` or `COMPOSIO_MCP_KEY`.
//!
//! The legacy Platform REST lane (composio_apps/tools/accounts/connect/
//! execute, `ak_` key) is archived at `_archive/composio-rest-2026-07-02/` —
//! it pointed at a different Composio project than the user's dashboard and
//! was superseded end-to-end by this lane (live-verified 2026-07-02).
//!
//! Side-effect discipline: executing app actions reaches into the user's real
//! accounts. The tool descriptions bind the agent to explicit user intent and
//! to naming irreversible actions (send/post/delete) before running them.

use anyhow::{bail, Context, Result};

use super::ToolOutput;

const MCP_ENDPOINT: &str = "https://connect.composio.dev/mcp";
const MAX_KEY_BYTES: usize = 16 * 1024;
const MAX_REGISTRY_ENTRIES: usize = 1_024;
const MAX_TOOLKIT_SLUG_BYTES: usize = 63;
const MAX_STATUS_BYTES: usize = 64;
const MAX_CATALOG_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Write sensitive Composio state with owner-only permissions in place before
/// any bytes are written. Re-applying the mode to the opened handle repairs a
/// stale permissive temp file as well as protecting a newly-created one.
#[cfg(test)]
fn write_private_file(path: &std::path::Path, contents: &[u8]) -> Result<()> {
    crate::config::private_io::write_private_file(path, contents)
}

fn write_private_file_atomic(path: &std::path::Path, contents: &[u8]) -> Result<()> {
    crate::config::private_io::atomic_write_private(path, contents)
}

fn validated_key(raw: &str, label: &str) -> Result<Option<String>> {
    let key = raw.trim();
    if key.is_empty() {
        return Ok(None);
    }
    if key.len() > MAX_KEY_BYTES {
        bail!(
            "{label} is too large ({} bytes; max {MAX_KEY_BYTES})",
            key.len()
        );
    }
    if !key.chars().all(|ch| ch.is_ascii_graphic()) {
        bail!("{label} must be one line of printable ASCII without whitespace");
    }
    Ok(Some(key.to_string()))
}

pub(crate) fn validate_consumer_key(key: &str) -> Result<()> {
    if validated_key(key, "Composio consumer key")?.is_none() {
        bail!("cannot store an empty Composio consumer key");
    }
    Ok(())
}

fn load_key_file(path: &std::path::Path, label: &str) -> Result<Option<String>> {
    let Some(bytes) = crate::config::private_io::read_private_file(path)
        .with_context(|| format!("failed to read {label} from {}", path.display()))?
    else {
        return Ok(None);
    };
    let raw = String::from_utf8(bytes)
        .with_context(|| format!("{label} at {} is not valid UTF-8", path.display()))?;
    validated_key(&raw, label)
}

/// Strict, bounded status/read path shared by onboarding and `configure`.
/// Missing/empty is `None`; malformed, symlinked, non-regular, or oversized
/// state is an error and must never be presented as "not configured".
pub(crate) fn load_consumer_key_file(path: &std::path::Path) -> Result<Option<String>> {
    load_key_file(path, "Composio consumer key")
}

/// Persist the For You consumer key without placing the value in config,
/// command arguments, logs, or error messages.
pub(crate) fn store_consumer_key(key: &str) -> Result<std::path::PathBuf> {
    store_consumer_key_in(&crate::config::phoenix_home(), key)
}

fn store_consumer_key_in(home: &std::path::Path, key: &str) -> Result<std::path::PathBuf> {
    validate_consumer_key(key)?;
    let key = key.trim();
    let path = home.join("composio-mcp.key");
    write_private_file_atomic(&path, format!("{key}\n").as_bytes())?;
    Ok(path)
}

/// Consumer key: `COMPOSIO_MCP_KEY` env first, then `~/.phoenix/composio-mcp.key`.
fn consumer_key() -> Result<String> {
    if let Ok(key) = std::env::var("COMPOSIO_MCP_KEY") {
        if let Some(key) = validated_key(&key, "COMPOSIO_MCP_KEY")? {
            return Ok(key);
        }
    }
    let path = crate::config::phoenix_home().join("composio-mcp.key");
    if let Some(key) = load_consumer_key_file(&path)? {
        return Ok(key);
    }
    bail!(
        "Composio For You (MCP) is not configured. In the Composio dashboard copy your \
X-CONSUMER-API-KEY (starts with `ck_`) and write it to {} (single line). One-time setup.",
        path.display()
    )
}

async fn mcp_call(tool: &str, input: serde_json::Value) -> Result<ToolOutput> {
    let key = consumer_key()?;
    let headers = vec![("x-consumer-api-key".to_string(), key)];
    let text = crate::tools::mcp_client::call_tool(MCP_ENDPOINT, &headers, tool, input).await?;
    Ok(ToolOutput {
        summary: format!("composio {tool} ok"),
        content: text,
    })
}

pub async fn mcp_search(input: serde_json::Value) -> Result<ToolOutput> {
    let mut out = mcp_call("COMPOSIO_SEARCH_TOOLS", input).await?;
    // Search currently returns an execution essay, pitfalls, workbench
    // snippets, connection account metadata, AND the exact schemas needed for
    // the primary tools. Passing all of that back through a reasoning model
    // made a one-message Gmail lookup spend another model round asking for the
    // entire Gmail catalog (14KB+ discovery, then a much larger schema dump).
    // Keep the executable contract and remove the redundant planner prose.
    // This also keeps connected-account profile details out of model context.
    if let Err(error) = record_connection_statuses(&out.content) {
        log_registry_error("search-status merge", &error);
    }
    out.content = compact_search_response(&out.content);
    Ok(out)
}

fn compact_search_response(text: &str) -> String {
    let Ok(mut root) = serde_json::from_str::<serde_json::Value>(text) else {
        return text.to_string();
    };
    let Some(results) = root
        .get_mut("data")
        .and_then(|data| data.get_mut("results"))
        .and_then(serde_json::Value::as_array_mut)
    else {
        return text.to_string();
    };

    for result in results {
        let Some(object) = result.as_object_mut() else {
            continue;
        };
        for redundant in [
            "execution_guidance",
            "recommended_plan_steps",
            "known_pitfalls",
            "reference_workbench_snippets",
            "related_tool_slugs",
        ] {
            object.remove(redundant);
        }
        object.insert(
            "phoenix_next_step".to_string(),
            serde_json::Value::String(
                "Exact executable schemas are included below. Call composio_run next. Prefer inline app results; when a response supplies a remote file, inspect it with COMPOSIO_REMOTE_WORKBENCH in a separate composio_run call using its exact schema. Remote paths are not local files."
                    .to_string(),
            ),
        );

        if let Some(schemas) = object
            .get_mut("tool_schemas")
            .and_then(serde_json::Value::as_object_mut)
        {
            // Router helpers are dispatched directly by mcp_run, not through
            // multi-execute. Preserve their executable schemas for overflow.
            for schema in schemas.values_mut() {
                if let Some(description) = schema
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                    .map(|text| cap(text, 240).to_string())
                {
                    schema["description"] = serde_json::Value::String(description);
                }
                if let Some(input_schema) = schema.get_mut("input_schema") {
                    strip_schema_prose(input_schema);
                }
            }
        }

        if let Some(statuses) = object
            .get_mut("toolkit_connection_statuses")
            .and_then(serde_json::Value::as_array_mut)
        {
            for status in statuses {
                let Some(status) = status.as_object_mut() else {
                    continue;
                };
                status.remove("description");
                status.remove("status_message");
                status.remove("accounts");
            }
        }
    }
    root.to_string()
}

fn strip_schema_prose(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            object.remove("description");
            object.remove("examples");
            object.remove("title");
            for child in object.values_mut() {
                strip_schema_prose(child);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                strip_schema_prose(item);
            }
        }
        _ => {}
    }
}

pub async fn mcp_schemas(input: serde_json::Value) -> Result<ToolOutput> {
    // Toolkit enumeration: the For-You search planner only routes to toolkits
    // with ACTIVE auth connections, so no-auth toolkits (hackernews) and
    // fresh connections are invisible to it even though execution works.
    // Passing `toolkit` here lists the toolkit's REAL tool slugs from the
    // global catalog and fetches their schemas — no slug guessing.
    let explicit_slugs = input["tool_slugs"]
        .as_array()
        .is_some_and(|slugs| !slugs.is_empty());
    if !explicit_slugs {
        if let Some(toolkit) = input["toolkit"].as_str() {
            let slugs = rest_toolkit_tool_slugs(toolkit).await?;
            if slugs.is_empty() {
                bail!(
                    "Composio catalog lists no tools for toolkit '{toolkit}' — check the slug \
(lowercase app name, e.g. 'hackernews', 'gmail')."
                );
            }
            // An app can expose hundreds of tools. Fetching every exact schema
            // here turned a simple Gmail lookup into 536K model tokens. The
            // slugs are the compact chooser; after selecting one, the model
            // requests only that tool's exact schema.
            return Ok(ToolOutput {
                summary: format!(
                    "composio catalog listed {} {toolkit} tool slug(s)",
                    slugs.len()
                ),
                content: serde_json::json!({
                    "toolkit": toolkit,
                    "tool_slugs": slugs,
                    "phoenix_next_step": "Choose the exact tool slug, then call composio_schemas with tool_slugs:[selected] before composio_run. Do not request the whole toolkit again."
                })
                .to_string(),
            });
        }
    }
    schemas_call(input).await
}

/// GET_TOOL_SCHEMAS with partial-success tolerance: the server flags the
/// whole call as an error when ANY slug is unknown, even while returning
/// every resolved schema plus corrective suggestions (live 2026-07-03: the
/// global catalog and the For-You lane disagree on 2 of 6 hackernews slugs).
/// Resolved schemas + the not_found/suggestions block ARE the useful answer.
async fn schemas_call(input: serde_json::Value) -> Result<ToolOutput> {
    let key = consumer_key()?;
    let headers = vec![("x-consumer-api-key".to_string(), key)];
    let (mut text, mut is_error) = crate::tools::mcp_client::call_tool_lenient(
        MCP_ENDPOINT,
        &headers,
        "COMPOSIO_GET_TOOL_SCHEMAS",
        input.clone(),
    )
    .await?;
    let mut resolved_any = response_has_resolved_schema(&text);
    // The catalog and execution lane occasionally disagree on a renamed slug,
    // but the schema response includes the authoritative replacement. This is
    // read-only discovery, so apply one exact server suggestion automatically
    // instead of making the agent guess or retry the stale slug repeatedly.
    if is_error && !resolved_any {
        if let Some(corrected) = schema_suggestion_retry_input(&input, &text) {
            (text, is_error) = crate::tools::mcp_client::call_tool_lenient(
                MCP_ENDPOINT,
                &headers,
                "COMPOSIO_GET_TOOL_SCHEMAS",
                corrected,
            )
            .await?;
            resolved_any = response_has_resolved_schema(&text);
        }
    }
    if is_error && !resolved_any {
        bail!("MCP tool COMPOSIO_GET_TOOL_SCHEMAS reported failure: {text}");
    }
    Ok(ToolOutput {
        summary: if is_error {
            "composio COMPOSIO_GET_TOOL_SCHEMAS partial (some slugs unknown — see suggestions)"
                .to_string()
        } else {
            "composio COMPOSIO_GET_TOOL_SCHEMAS ok".to_string()
        },
        content: compact_schema_response(&text),
    })
}

fn response_has_resolved_schema(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|value| {
            value["data"]["tool_schemas"]
                .as_object()
                .map(|schemas| !schemas.is_empty())
        })
        .unwrap_or(false)
}

fn schema_suggestion_retry_input(
    original: &serde_json::Value,
    response: &str,
) -> Option<serde_json::Value> {
    let root: serde_json::Value = serde_json::from_str(response).ok()?;
    let not_found = root["data"]["not_found"].as_array()?;
    if not_found.is_empty() {
        return None;
    }
    let suggestions = root["data"]["suggestions"].as_object()?;
    let corrected = not_found
        .iter()
        .map(|slug| {
            suggestions
                .get(slug.as_str()?)?
                .as_array()?
                .first()?
                .as_str()
                .map(str::to_string)
        })
        .collect::<Option<Vec<_>>>()?;
    if corrected.is_empty() {
        return None;
    }
    let mut input = original.as_object()?.clone();
    input.insert("tool_slugs".to_string(), serde_json::json!(corrected));
    Some(serde_json::Value::Object(input))
}

fn compact_schema_response(text: &str) -> String {
    let Ok(mut root) = serde_json::from_str::<serde_json::Value>(text) else {
        return text.to_string();
    };
    if let Some(schemas) = root
        .get_mut("data")
        .and_then(|data| data.get_mut("tool_schemas"))
        .and_then(serde_json::Value::as_object_mut)
    {
        for schema in schemas.values_mut() {
            if let Some(description) = schema
                .get("description")
                .and_then(serde_json::Value::as_str)
                .map(|text| cap(text, 240).to_string())
            {
                schema["description"] = serde_json::Value::String(description);
            }
            if let Some(input_schema) = schema.get_mut("input_schema") {
                strip_schema_prose(input_schema);
            }
        }
    }
    root.to_string()
}

pub async fn mcp_run(input: serde_json::Value) -> Result<ToolOutput> {
    let input = normalize_run_input(input)?;
    if let Some(arguments) = remote_workbench_arguments(&input)? {
        return mcp_call("COMPOSIO_REMOTE_WORKBENCH", arguments).await;
    }
    let key = consumer_key()?;
    let headers = vec![("x-consumer-api-key".to_string(), key)];
    let (text, is_error) = crate::tools::mcp_client::call_tool_lenient(
        MCP_ENDPOINT,
        &headers,
        "COMPOSIO_MULTI_EXECUTE_TOOL",
        input,
    )
    .await?;
    let successful = successful_multi_execute_results(&text);
    if is_error && successful == 0 {
        bail!("MCP tool COMPOSIO_MULTI_EXECUTE_TOOL reported failure: {text}");
    }
    let content = match overflow_recovery_hint(&text) {
        Some(hint) => format!("{text}\n\n{hint}"),
        None => text,
    };
    Ok(ToolOutput {
        summary: if is_error {
            format!("composio multi-execute partial — {successful} action(s) succeeded")
        } else {
            "composio COMPOSIO_MULTI_EXECUTE_TOOL ok".to_string()
        },
        content,
    })
}

/// Large app responses come back as a truncated `data_preview` while the full
/// JSON lands in Composio's remote workbench. Without the exact next call the
/// agent went looking elsewhere (opening the app in an unsigned browser,
/// searching for "parser" toolkits) instead of reading the saved response.
fn overflow_recovery_hint(text: &str) -> Option<String> {
    let root: serde_json::Value = serde_json::from_str(text).ok()?;
    let path = root["data"]["remote_file_info"]["file_path"].as_str()?;
    let session = root["data"]["session"]["id"].as_str().unwrap_or("");
    let call = serde_json::json!({
        "session_id": session,
        "tools": [{"tool_slug": "COMPOSIO_REMOTE_WORKBENCH", "arguments": {
            "code_to_execute": format!("import json\npayload=json.load(open({}))\nresult=payload['results'][0]['response']['data']\n# print only the fields you need", serde_json::to_string(path).ok()?),
            "thought": "Read the needed part of the full saved response."
        }}]
    });
    Some(format!(
        "[Phoenix] This result is only a preview; the full response is saved in Composio's remote workbench at {path}. Read the part you need with composio_run {call} and print only the fields you need. Stay on this connected-app route: the app's website in your browser is a separate, usually signed-out session."
    ))
}

fn remote_workbench_arguments(input: &serde_json::Value) -> Result<Option<serde_json::Value>> {
    let tools = input["tools"].as_array().context("tools must be an array")?;
    let Some(call) = tools.iter().find(|call| {
        call["tool_slug"].as_str() == Some("COMPOSIO_REMOTE_WORKBENCH")
    }) else {
        return Ok(None);
    };
    anyhow::ensure!(tools.len() == 1,
        "COMPOSIO_REMOTE_WORKBENCH must be called separately from app actions; no actions were executed");
    let mut arguments = call["arguments"].as_object()
        .context("COMPOSIO_REMOTE_WORKBENCH arguments must be an object using its exact schema")?
        .clone();
    if !arguments.contains_key("session_id") {
        if let Some(session) = input.get("session_id") {
            arguments.insert("session_id".into(), session.clone());
        }
    }
    Ok(Some(serde_json::Value::Object(arguments)))
}

fn normalize_run_input(input: serde_json::Value) -> Result<serde_json::Value> {
    let mut object = input
        .as_object()
        .cloned()
        .context("composio_run input must be an object")?;
    if let Some(serde_json::Value::String(encoded)) = object.get("tools") {
        let decoded: serde_json::Value =
            serde_json::from_str(encoded).context("composio_run tools string is not valid JSON")?;
        object.insert("tools".to_string(), decoded);
    }
    if object
        .get("tools")
        .and_then(serde_json::Value::as_object)
        .is_some()
    {
        if let Some(one) = object.remove("tools") {
            object.insert("tools".to_string(), serde_json::Value::Array(vec![one]));
        }
    }
    let shared_account = object.remove("account");
    let tools = object
        .get_mut("tools")
        .and_then(serde_json::Value::as_array_mut)
        .context("composio_run tools must be an array of tool calls")?;
    if tools.is_empty() {
        bail!("composio_run tools must contain at least one tool call");
    }
    for tool in tools.iter_mut() {
        let call = tool
            .as_object_mut()
            .context("each composio_run tool must be an object")?;
        if !call.contains_key("account") {
            if let Some(account) = shared_account.clone() {
                call.insert("account".to_string(), account);
            }
        }
        // Rescheduling, renaming, or enriching an existing Notion row must not
        // silently reopen completed work. In the Avery regression the model
        // fetched Checkbox=true, then included Checkbox=false beside a date
        // change and moved finished Test Drive 1.3 back into the plan. Require
        // a local, explicit acknowledgement for that state transition; it is
        // stripped before the request reaches Composio.
        let user_confirmed_reopen = call
            .remove("user_confirmed_reopen_completed")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        if notion_update_marks_incomplete(call) && !user_confirmed_reopen {
            bail!(
                "refusing to mark an existing Notion item incomplete without explicit user confirmation. Omit the completion property when rescheduling/renaming, or set user_confirmed_reopen_completed:true only when the user explicitly asked to reopen or mark this item incomplete"
            );
        }
        normalize_tool_account_selector(call)?;
        normalize_tool_argument_aliases(call)?;
    }
    // Prefer inline results. The server may still overflow large responses
    // into its remote workspace; mcp_run supports direct workbench retrieval.
    object.insert(
        "sync_response_to_workbench".to_string(),
        serde_json::Value::Bool(false),
    );
    Ok(serde_json::Value::Object(object))
}

fn notion_update_marks_incomplete(call: &serde_json::Map<String, serde_json::Value>) -> bool {
    let slug = call
        .get("tool_slug")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_ascii_uppercase();
    if !slug.starts_with("NOTION_") || !slug.contains("UPDATE") {
        return false;
    }
    let Some(properties) = call
        .get("arguments")
        .and_then(|value| value.get("properties"))
    else {
        return false;
    };
    match properties {
        serde_json::Value::Object(properties) => properties
            .iter()
            .any(|(name, value)| completion_property_name(name) && value_explicitly_false(value)),
        serde_json::Value::Array(properties) => properties.iter().any(|property| {
            let Some(property) = property.as_object() else {
                return false;
            };
            let name = property
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            completion_property_name(name)
                && property.get("value").is_some_and(value_explicitly_false)
        }),
        _ => false,
    }
}

fn completion_property_name(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "checkbox" | "complete" | "completed" | "done"
    )
}

fn value_explicitly_false(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Bool(false) => true,
        serde_json::Value::String(value) => value.trim().eq_ignore_ascii_case("false"),
        serde_json::Value::Number(value) => value.as_i64() == Some(0),
        serde_json::Value::Object(value) => value
            .get("checkbox")
            .or_else(|| value.get("value"))
            .is_some_and(value_explicitly_false),
        _ => false,
    }
}

/// Repair only aliases that are unambiguous for one exact tool. Composio's
/// current Google Docs export schema calls the document identifier `file_id`,
/// while adjacent Docs tools call the same value `document_id` or `id`. The
/// live Avery failure was a valid document rejected solely for that naming
/// mismatch; normalizing it here avoids another model/network round trip.
fn normalize_tool_argument_aliases(
    call: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<()> {
    let exports_google_doc_pdf = call.get("tool_slug").and_then(serde_json::Value::as_str)
        == Some("GOOGLEDOCS_EXPORT_DOCUMENT_AS_PDF");
    let Some(arguments) = call
        .get_mut("arguments")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Ok(());
    };
    if exports_google_doc_pdf && !arguments.contains_key("file_id") {
        if let Some(value) = arguments
            .remove("document_id")
            .or_else(|| arguments.remove("id"))
        {
            arguments.insert("file_id".to_string(), value);
        }
    }
    Ok(())
}

/// Composio's multi-execute envelope selects a connected account beside
/// `tool_slug`, not inside the downstream app's `arguments`. Models naturally
/// confuse that with app fields such as Gmail's `user_id`; repairing the
/// unambiguous aliases here turns a validation loop into the intended single
/// request without weakening the remote tool schema.
fn normalize_tool_account_selector(
    call: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<()> {
    if call.contains_key("account") {
        return Ok(());
    }
    let tool_slug = call
        .get("tool_slug")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let toolkit = tool_slug
        .split('_')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let Some(arguments) = call
        .get_mut("arguments")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Ok(());
    };

    let selector = ["account", "account_id", "connected_account_id"]
        .into_iter()
        .find_map(|key| arguments.remove(key));
    let selector = selector.or_else(|| {
        let user_id = arguments.get("user_id")?.as_str()?;
        let is_connected_account_id = !toolkit.is_empty()
            && user_id
                .to_ascii_lowercase()
                .starts_with(&format!("{toolkit}_"));
        is_connected_account_id.then(|| arguments.remove("user_id").expect("key exists"))
    });
    if let Some(selector) = selector {
        if !selector.is_string() {
            bail!("composio_run connected account selector must be a string");
        }
        call.insert("account".to_string(), selector);
    }
    Ok(())
}

fn successful_multi_execute_results(text: &str) -> usize {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|root| root["data"]["results"].as_array().cloned())
        .map(|results| {
            results
                .iter()
                .filter(|item| {
                    item["response"]["successful"].as_bool() == Some(true)
                        || item["successful"].as_bool() == Some(true)
                })
                .count()
        })
        .unwrap_or(0)
}

pub async fn mcp_connections(input: serde_json::Value) -> Result<ToolOutput> {
    let out = mcp_call("COMPOSIO_MANAGE_CONNECTIONS", input).await?;
    // Every connections response teaches the local registry which apps exist
    // and their status — the capability snapshot self-updates as the user
    // connects things, no separate discovery pass needed.
    if let Err(error) = record_connection_statuses(&out.content) {
        // The remote action may already have happened. Do not report the
        // whole tool call as failed (which could invite a duplicate retry)
        // merely because the local capability cache could not be updated.
        log_registry_error("connection-status merge", &error);
    }
    // Re-render the injected line NOW so a connection made this round is in
    // the very next round's runtime context — no TTL wait, no network.
    match read_registry() {
        Ok(registry) => store_context_line(render_context_line(&registry)),
        Err(error) => log_registry_error("read after connection action", &error),
    }
    Ok(out)
}

// ── runtime-context snapshot ──────────────────────────────────────────
//
// The mandatory pre-task gate is INJECTED, not hoped for: the runtime plants
// the Composio line in every composio-holding agent's RUNTIME CONTEXT block
// (same pattern as the memory survey and the installed-skills lines). The
// model never has to choose to look.

static CONTEXT_CACHE: std::sync::Mutex<Option<(std::time::Instant, String)>> =
    std::sync::Mutex::new(None);

/// The cached connected-apps line for prompt injection. Non-blocking — returns
/// whatever the last `refresh_context_cache` produced, or None before the
/// first refresh / when Composio is unconfigured.
pub fn context_line() -> Option<String> {
    let cache = CONTEXT_CACHE.lock().ok()?;
    cache.as_ref().map(|(_, line)| line.clone())
}

fn store_context_line(line: String) {
    if let Ok(mut cache) = CONTEXT_CACHE.lock() {
        *cache = Some((std::time::Instant::now(), line));
    }
}

pub fn local_status_line() -> String {
    let mcp = credential_state(
        "COMPOSIO_MCP_KEY",
        &crate::config::phoenix_home().join("composio-mcp.key"),
    );
    let cached = context_line()
        .map(|line| cap(&line, 140).to_string())
        .unwrap_or_else(|| "no cached connected-app snapshot yet".to_string());
    format!("Composio · For-You MCP {mcp} · {cached}")
}

fn credential_state(env_var: &str, path: &std::path::Path) -> &'static str {
    if let Ok(value) = std::env::var(env_var) {
        match validated_key(&value, env_var) {
            Ok(Some(_)) => return "configured via env",
            Err(_) => return "invalid env credential",
            Ok(None) => {}
        }
    }
    match load_key_file(path, "Composio credential") {
        Ok(Some(_)) => "configured via file",
        Ok(None) => "not configured",
        Err(_) => "stored credential unreadable",
    }
}

// ── connection registry (the agent's real capability list) ─────────────
//
// The For-You MCP server embeds the AUTHORITATIVE per-consumer connected-apps
// list inside COMPOSIO_SEARCH_TOOLS' own description ("User has manually
// connected the apps: …", live-verified 2026-07-03) — one tools/list call
// returns every connected app, including ones Phoenix never heard of. That is
// the primary discovery source. Two quirks it can't cover, so the local
// registry stays: (1) no-auth toolkits (hackernews) never count as
// "connected" server-side even while their tools execute fine — the registry
// remembers their activation and never downgrades them; (2) on network
// failure the last-known registry still renders. The registry also learns
// from every composio_connections response, and the rendered line is
// re-cached IMMEDIATELY on those responses so new connections are usable in
// the very next round.

/// Probed by the fallback path when the server list can't be parsed. `list`
/// is side-effect-free; unknown slugs just report as not active.
const SEED_TOOLKITS: &[&str] = &[
    "github",
    "hackernews",
    "gmail",
    "discord",
    "reddit",
    "notion",
    "slack",
];

/// Extract the connected-apps list the server plants in its search tool's
/// description: "User has manually connected the apps: a, b, c. Prefer …".
pub(crate) fn parse_connected_apps(description: &str) -> Vec<String> {
    let Some(idx) = description.find("connected the apps:") else {
        return Vec::new();
    };
    let rest = &description[idx + "connected the apps:".len()..];
    // A period only terminates the sentence when it is followed by whitespace
    // (or the end of the description). Invalid/untrusted list entries can
    // themselves contain periods (`../escape`, for example); stopping at the
    // first one would discard every legitimate toolkit that follows it.
    let list_end = rest
        .char_indices()
        .find_map(|(index, ch)| {
            if ch != '.' {
                return None;
            }
            let after = &rest[index + ch.len_utf8()..];
            match after.chars().next() {
                None => Some(index),
                Some(next) if next.is_whitespace() => Some(index),
                Some(_) => None,
            }
        })
        .unwrap_or(rest.len());
    let list = &rest[..list_end];
    list.split(',')
        .map(|app| app.trim().to_lowercase())
        .filter(|app| valid_toolkit_slug(app))
        .take(MAX_REGISTRY_ENTRIES)
        .collect()
}

/// One tools/list round-trip → the server's own connected-apps list.
async fn fetch_server_connected_apps() -> Result<Vec<String>> {
    let key = consumer_key()?;
    let headers = vec![("x-consumer-api-key".to_string(), key)];
    let tools = crate::tools::mcp_client::list_tools(MCP_ENDPOINT, &headers).await?;
    for (name, description) in tools {
        if name == "COMPOSIO_SEARCH_TOOLS" {
            return Ok(parse_connected_apps(&description));
        }
    }
    Ok(Vec::new())
}

// ── REST catalog (global, project-independent) ─────────────────────────
//
// The catalog of what tools a toolkit HAS is global — any valid platform key
// can list it, unlike connections which are project-scoped. This powers
// `composio_schemas {toolkit: …}` enumeration for toolkits the search
// planner refuses to surface.

fn rest_key() -> Result<String> {
    if let Ok(key) = std::env::var("COMPOSIO_API_KEY") {
        if let Some(key) = validated_key(&key, "COMPOSIO_API_KEY")? {
            return Ok(key);
        }
    }
    let path = crate::config::phoenix_home().join("composio.key");
    if let Some(key) = load_key_file(&path, "Composio platform key")? {
        return Ok(key);
    }
    bail!(
        "Listing a toolkit's tools needs the Composio platform key (`ak_…`) at {} or \
COMPOSIO_API_KEY — the For-You lane itself can't enumerate a toolkit. Alternative: pass exact \
`tool_slugs` (APPNAME_ACTION, e.g. HACKERNEWS_SEARCH_POSTS).",
        path.display()
    )
}

/// List every tool slug a toolkit ships, via the global v3 catalog.
async fn rest_toolkit_tool_slugs(toolkit: &str) -> Result<Vec<String>> {
    let key = rest_key()?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("reqwest client build failed");
    let mut slugs = Vec::new();
    let mut cursor: Option<String> = None;
    // Bounded pagination — the largest toolkits are a few pages.
    for _ in 0..5 {
        let mut req = client
            .get("https://backend.composio.dev/api/v3/tools")
            .query(&[("toolkit_slug", toolkit)])
            .header("x-api-key", &key);
        if let Some(c) = &cursor {
            req = req.query(&[("cursor", c.as_str())]);
        }
        let mut resp = req.send().await?;
        let status = resp.status();
        if resp
            .content_length()
            .is_some_and(|length| length > MAX_CATALOG_RESPONSE_BYTES as u64)
        {
            bail!("Composio catalog response exceeds the {MAX_CATALOG_RESPONSE_BYTES}-byte limit");
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .context("read Composio catalog response")?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_CATALOG_RESPONSE_BYTES {
                bail!(
                    "Composio catalog response exceeds the {MAX_CATALOG_RESPONSE_BYTES}-byte limit"
                );
            }
            bytes.extend_from_slice(&chunk);
        }
        let body: serde_json::Value =
            serde_json::from_slice(&bytes).context("Composio catalog returned invalid JSON")?;
        if !status.is_success() {
            bail!(
                "Composio catalog listing for '{toolkit}' failed ({status}): {}",
                cap(&body.to_string(), 200)
            );
        }
        if let Some(items) = body["items"].as_array() {
            slugs.extend(
                items
                    .iter()
                    .filter_map(|t| t["slug"].as_str().map(str::to_string)),
            );
        }
        cursor = body["next_cursor"].as_str().map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    Ok(slugs)
}

/// Snapshot lifetime: within it, turn starts are network-free.
const SNAPSHOT_TTL: std::time::Duration = std::time::Duration::from_secs(300);

fn registry_path() -> std::path::PathBuf {
    crate::config::phoenix_home().join("composio-connections.json")
}

/// toolkit slug -> last-known status ("active" / "initiated" / "failed").
type Registry = std::collections::BTreeMap<String, String>;

fn valid_toolkit_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= MAX_TOOLKIT_SLUG_BYTES
        && slug
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '_' | '-'))
}

fn validate_registry(registry: &Registry) -> Result<()> {
    if registry.len() > MAX_REGISTRY_ENTRIES {
        bail!(
            "Composio connection registry has {} entries (max {MAX_REGISTRY_ENTRIES})",
            registry.len()
        );
    }
    for (toolkit, status) in registry {
        if !valid_toolkit_slug(toolkit) {
            bail!("invalid toolkit slug in Composio registry: {toolkit:?}");
        }
        if status.is_empty()
            || status.len() > MAX_STATUS_BYTES
            || !status
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '_' | '-'))
        {
            bail!("invalid status for Composio toolkit {toolkit:?}");
        }
    }
    Ok(())
}

fn parse_registry_bytes(bytes: Option<&[u8]>, path: &std::path::Path) -> Result<Registry> {
    let Some(bytes) = bytes else {
        return Ok(Registry::new());
    };
    let decoded: Registry = serde_json::from_slice(bytes)
        .with_context(|| format!("invalid Composio connection registry at {}", path.display()))?;
    if decoded.len() > MAX_REGISTRY_ENTRIES {
        bail!(
            "Composio connection registry has {} entries (max {MAX_REGISTRY_ENTRIES})",
            decoded.len()
        );
    }
    // Older server responses were not consistent about status casing. Accept
    // and normalize that harmless legacy shape, while still rejecting names
    // that could be path/control data and ambiguous case-colliding entries.
    let mut registry = Registry::new();
    for (toolkit, status) in decoded {
        let toolkit = toolkit.trim().to_ascii_lowercase();
        let status = status.trim().to_ascii_lowercase();
        if registry.insert(toolkit.clone(), status).is_some() {
            bail!("duplicate case-insensitive toolkit in Composio registry: {toolkit:?}");
        }
    }
    validate_registry(&registry)?;
    Ok(registry)
}

fn read_registry_from(path: &std::path::Path) -> Result<Registry> {
    let bytes = crate::config::private_io::read_private_file(path)
        .with_context(|| format!("failed to read Composio registry at {}", path.display()))?;
    parse_registry_bytes(bytes.as_deref(), path)
}

fn read_registry() -> Result<Registry> {
    read_registry_from(&registry_path())
}

fn registry_json(registry: &Registry) -> Result<Vec<u8>> {
    validate_registry(registry)?;
    let mut raw = serde_json::to_vec_pretty(registry)?;
    raw.push(b'\n');
    Ok(raw)
}

#[cfg(test)]
fn write_registry_to(path: &std::path::Path, registry: &Registry) -> Result<()> {
    let raw = registry_json(registry)?;
    write_private_file_atomic(path, &raw)
}

fn update_registry_at(
    path: &std::path::Path,
    update: impl FnOnce(&mut Registry) -> Result<()>,
) -> Result<Registry> {
    crate::config::private_io::read_modify_write_private(path, |current| {
        let mut registry = parse_registry_bytes(current, path)?;
        update(&mut registry)?;
        let raw = registry_json(&registry)?;
        Ok((registry, raw))
    })
}

fn merge_active_apps(apps: impl IntoIterator<Item = String>) -> Result<Registry> {
    let path = registry_path();
    merge_active_apps_at(&path, apps)
}

fn merge_active_apps_at(
    path: &std::path::Path,
    apps: impl IntoIterator<Item = String>,
) -> Result<Registry> {
    update_registry_at(path, |registry| {
        for app in apps {
            if valid_toolkit_slug(&app) {
                registry.insert(app, "active".to_string());
            }
        }
        Ok(())
    })
}

fn log_registry_error(context: &str, error: &anyhow::Error) {
    crate::runtime::gwlog(&format!(
        "Composio registry {context} failed: {error:#}; preserving existing state"
    ));
}

fn normalized_status(entry: &serde_json::Value, account_active: bool, no_auth: bool) -> String {
    if account_active
        || no_auth
        || entry["status"]
            .as_str()
            .is_some_and(|status| status.eq_ignore_ascii_case("active"))
    {
        return "active".to_string();
    }
    let status = entry["status"]
        .as_str()
        .unwrap_or("unknown")
        .trim()
        .to_ascii_lowercase();
    if status.is_empty()
        || status.len() > MAX_STATUS_BYTES
        || !status
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '_' | '-'))
    {
        "unknown".to_string()
    } else {
        status
    }
}

/// Parse a COMPOSIO_MANAGE_CONNECTIONS response and merge the per-toolkit
/// statuses into the registry. A toolkit counts as active when its status
/// says so OR any of its accounts is active.
pub(crate) fn record_connection_statuses(response_text: &str) -> Result<()> {
    record_connection_statuses_at(&registry_path(), response_text)
}

fn record_connection_statuses_at(path: &std::path::Path, response_text: &str) -> Result<()> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(response_text) else {
        return Ok(());
    };
    let Some(results) = value["data"]["results"].as_object() else {
        return Ok(());
    };
    let mut updates = Vec::new();
    for (toolkit, entry) in results {
        let toolkit = toolkit.trim().to_ascii_lowercase();
        if !valid_toolkit_slug(&toolkit) {
            continue;
        }
        let account_active = entry["accounts"].as_array().is_some_and(|accounts| {
            accounts.iter().any(|account| {
                account["status"]
                    .as_str()
                    .is_some_and(|status| status.eq_ignore_ascii_case("active"))
            })
        });
        // No-auth toolkits never carry accounts; activation responses say so
        // in `instruction` ("hackernews does not require authentication").
        let no_auth = entry["instruction"]
            .as_str()
            .is_some_and(|text| text.contains("does not require authentication"));
        let status = normalized_status(entry, account_active, no_auth);
        updates.push((toolkit, status));
        if updates.len() > MAX_REGISTRY_ENTRIES {
            bail!("Composio response contains too many toolkit statuses");
        }
    }
    if updates.is_empty() {
        return Ok(());
    }
    update_registry_at(path, move |registry| {
        for (toolkit, status) in updates {
            // A `list` probe cannot distinguish "no-auth toolkit" from "never
            // connected" — BOTH report "initiated" with zero accounts, while
            // execution works fine (live 2026-07-04: hackernews served the front
            // page while listing as initiated). Never downgrade a known-active
            // toolkit on that ambiguous signal; only an explicit failure does.
            let known_active = registry
                .get(&toolkit)
                .is_some_and(|stored| stored == "active");
            if known_active && status != "active" && status != "failed" {
                continue;
            }
            registry.insert(toolkit, status);
        }
        Ok(())
    })?;
    Ok(())
}

/// Render the injected capability line from the registry. Deterministic order
/// (BTreeMap) → byte-stable across rounds within a snapshot window.
fn render_context_line(registry: &Registry) -> String {
    let active: Vec<&str> = registry
        .iter()
        .filter(|(_, status)| status.as_str() == "active")
        .map(|(name, _)| name.as_str())
        .collect();
    let inactive: Vec<&str> = registry
        .iter()
        .filter(|(_, status)| status.as_str() != "active")
        .map(|(name, _)| name.as_str())
        .take(8)
        .collect();
    let active_part = if active.is_empty() {
        "none yet".to_string()
    } else {
        active.join(", ")
    };
    let inactive_part = if inactive.is_empty() {
        String::new()
    } else {
        format!(
            " Not connected: {} (`composio_connections` activates an app — no-auth apps activate instantly, others return a sign-in link for the user).",
            inactive.join(", ")
        )
    };
    format!(
        "Composio (For You) — connected apps: {active_part}.{inactive_part} Built-in web/news \
search is always available. CAPABILITY CHECK FIRST: this list is authoritative and outranks \
whatever `composio_search` returns. When a task names a connected app or needs app/news/web \
data: consult this list, then `composio_search` for the tool, then `composio_run` — BEFORE any \
generic web tool. KNOWN LIMIT: search only ever surfaces apps with active auth connections, so \
it MISSES no-auth apps (hackernews) and brand-new connections even though they work. If an app \
on this list isn't in search results, enumerate it directly: `composio_schemas` with \
{{\"toolkit\": \"<app>\"}} returns that app's REAL tools + schemas, then `composio_run`. Never \
conclude a listed app is unavailable because search ignored it."
    )
}

/// Refresh the injected snapshot. Primary source: the server's OWN
/// connected-apps list (one tools/list call — authoritative, catches apps
/// connected moments ago from the dashboard, no seed list to maintain).
/// Fallback when it can't be parsed: the old bounded MANAGE_CONNECTIONS
/// `list` probe over registry∪seeds. TTL-guarded (network at most once per
/// window); on total failure the last-known registry still renders so the
/// agent keeps its capabilities. Unconfigured → inject nothing, not noise.
pub async fn refresh_context_cache() {
    if consumer_key().is_err() {
        return;
    }
    if let Ok(cache) = CONTEXT_CACHE.lock() {
        if let Some((at, _)) = cache.as_ref() {
            if at.elapsed() < SNAPSHOT_TTL {
                return;
            }
        }
    }
    match fetch_server_connected_apps().await {
        Ok(apps) if !apps.is_empty() => {
            // Server-side "connected" omits no-auth toolkits, so absence is
            // NOT deactivation — known-active entries stay (never-downgrade).
            if let Err(error) = merge_active_apps(apps) {
                log_registry_error("server-list merge", &error);
            }
        }
        _ => {
            // Fallback: bounded status probe. `action` is EXPLICIT — the
            // server default is "add", which mutates; the probe must not.
            let mut toolkits: std::collections::BTreeSet<String> = match read_registry() {
                Ok(registry) => registry.into_keys().collect(),
                Err(error) => {
                    log_registry_error("fallback preflight read", &error);
                    return;
                }
            };
            toolkits.extend(SEED_TOOLKITS.iter().map(|s| s.to_string()));
            let probe: Vec<serde_json::Value> = toolkits
                .iter()
                .map(|name| serde_json::json!({"name": name, "action": "list"}))
                .collect();
            // mcp_connections records the fresh statuses into the registry.
            let _ = mcp_connections(serde_json::json!({ "toolkits": probe })).await;
        }
    }
    match read_registry() {
        Ok(registry) => store_context_line(render_context_line(&registry)),
        Err(error) => log_registry_error("final context read", &error),
    }
}

fn cap(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((idx, _)) => &text[..idx],
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[cfg(unix)]
    #[test]
    fn composio_key_and_registry_writes_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();

        let key_path = dir.path().join("composio-mcp.key");
        let key_temp = key_path.with_extension("key.tmp");
        write_private_file(&key_temp, b"stale key").unwrap();
        assert_eq!(
            std::fs::metadata(&key_temp).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_file(&key_temp).unwrap();
        std::fs::write(&key_path, "old key\n").unwrap();
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let stored = store_consumer_key_in(dir.path(), "ck_test_private").unwrap();
        assert_eq!(stored, key_path);
        assert_eq!(
            std::fs::metadata(&stored).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::read_to_string(&stored).unwrap(),
            "ck_test_private\n"
        );
        assert!(!key_temp.exists());

        let mut registry = Registry::new();
        registry.insert("gmail".to_string(), "active".to_string());
        let registry_path = dir.path().join("composio-connections.json");
        write_registry_to(&registry_path, &registry).unwrap();
        assert_eq!(
            std::fs::metadata(&registry_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn key_reader_rejects_invalid_utf8_multiline_and_oversized_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("composio-mcp.key");

        std::fs::write(&path, [0xff, 0xfe]).unwrap();
        assert!(load_consumer_key_file(&path).is_err());

        std::fs::write(&path, b"ck_first\nck_second\n").unwrap();
        assert!(load_consumer_key_file(&path).is_err());

        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_KEY_BYTES as u64 + 1).unwrap();
        assert!(load_consumer_key_file(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn key_reader_rejects_symlinks_and_fifos() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.key");
        let linked = dir.path().join("linked.key");
        std::fs::write(&target, "ck_secret\n").unwrap();
        symlink(&target, &linked).unwrap();
        assert!(load_consumer_key_file(&linked).is_err());

        let fifo = dir.path().join("fifo.key");
        let c_path = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        assert!(load_consumer_key_file(&fifo).is_err());
    }

    #[test]
    fn corrupt_registry_is_preserved_instead_of_replaced_with_empty_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("composio-connections.json");
        let corrupt = b"{ definitely not json";
        std::fs::write(&path, corrupt).unwrap();

        let result = record_connection_statuses_at(
            &path,
            r#"{"data":{"results":{"gmail":{"status":"active"}}}}"#,
        );
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), corrupt);
    }

    #[test]
    fn concurrent_registry_merges_preserve_every_toolkit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("composio-connections.json");

        std::thread::scope(|scope| {
            for index in 0..24 {
                let path = path.clone();
                scope.spawn(move || {
                    let response = format!(
                        r#"{{"data":{{"results":{{"app_{index}":{{"status":"active"}}}}}}}}"#
                    );
                    record_connection_statuses_at(&path, &response).unwrap();
                });
            }
        });

        let registry = read_registry_from(&path).unwrap();
        assert_eq!(registry.len(), 24);
        for index in 0..24 {
            assert_eq!(
                registry.get(&format!("app_{index}")).map(String::as_str),
                Some("active")
            );
        }
    }

    #[test]
    fn malformed_registry_keys_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("composio-connections.json");
        std::fs::write(&path, r#"{"../escape":"active"}"#).unwrap();
        assert!(read_registry_from(&path).is_err());
    }

    #[test]
    fn oversized_registry_is_rejected_before_json_parsing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("composio-connections.json");
        let file = std::fs::File::create(&path).unwrap();
        // The shared private-state ceiling is 64 MiB. A sparse file exercises
        // the pre-allocation metadata guard without consuming that disk space.
        file.set_len(64 * 1024 * 1024 + 1).unwrap();
        assert!(read_registry_from(&path).is_err());
    }

    #[test]
    fn connection_registry_learns_statuses_and_renders_capability_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("composio-connections.json");

        // Live response shape (2026-07-03): github active via account,
        // hackernews stuck at initiated with no accounts.
        record_connection_statuses_at(
            &path,
            r#"{"data":{"results":{
                "github":{"toolkit":"github","status":"active","accounts":[{"id":"a","status":"active"}]},
                "hackernews":{"toolkit":"hackernews","status":"initiated","accounts":[]}
            }}}"#,
        )
        .unwrap();
        let line = render_context_line(&read_registry_from(&path).unwrap());
        assert!(line.contains("connected apps: github."), "{line}");
        assert!(line.contains("Not connected: hackernews"), "{line}");
        assert!(line.contains("CAPABILITY CHECK FIRST"), "{line}");

        // A later response flipping hackernews active merges into the registry
        // (no-auth activation: no accounts, but the instruction says no auth).
        record_connection_statuses_at(
            &path,
            r#"{"data":{"results":{
                "hackernews":{"toolkit":"hackernews","status":"initiated","instruction":"hackernews does not require authentication"}
            }}}"#,
        )
        .unwrap();
        let line = render_context_line(&read_registry_from(&path).unwrap());
        assert!(
            line.contains("connected apps: github, hackernews."),
            "{line}"
        );
        assert!(!line.contains("Not connected"), "{line}");

        // A later ambiguous `list` probe ("initiated", zero accounts — what
        // no-auth toolkits ALWAYS report) must NOT downgrade a known-active
        // toolkit back to not-connected.
        record_connection_statuses_at(
            &path,
            r#"{"data":{"results":{
                "hackernews":{"toolkit":"hackernews","status":"initiated","accounts":[]}
            }}}"#,
        )
        .unwrap();
        let line = render_context_line(&read_registry_from(&path).unwrap());
        assert!(
            line.contains("connected apps: github, hackernews."),
            "ambiguous probe must not downgrade active: {line}"
        );
    }

    #[test]
    fn parse_connected_apps_extracts_server_list() {
        // Verbatim shape from the live COMPOSIO_SEARCH_TOOLS description
        // (2026-07-03) — the server's authoritative per-consumer list.
        let desc = "Usage guidelines:\n  - blah\n  \n- User has manually connected the apps: \
discord, firecrawl, github, gmail, googledrive, notion, reddit, tavily. Prefer these apps when \
intent is unclear.\n\nSplitting guidelines";
        assert_eq!(
            parse_connected_apps(desc),
            vec![
                "discord",
                "firecrawl",
                "github",
                "gmail",
                "googledrive",
                "notion",
                "reddit",
                "tavily"
            ]
        );
        assert!(parse_connected_apps("no such line here").is_empty());
        assert_eq!(
            parse_connected_apps(
                "User has manually connected the apps: github, ../escape, Has Space, gmail."
            ),
            vec!["github", "gmail"]
        );
        assert_eq!(
            parse_connected_apps(
                "User has manually connected the apps: github, bad.slug, gmail. Next sentence."
            ),
            vec!["github", "gmail"]
        );
    }

    #[test]
    fn search_response_keeps_executable_schema_without_planner_bulk_or_account_data() {
        let raw = serde_json::json!({
            "data": {
                "results": [{
                    "use_case": "find a Gmail verification code",
                    "execution_guidance": "follow this long plan",
                    "recommended_plan_steps": ["search", "sort", "hydrate"],
                    "known_pitfalls": ["large payload"],
                    "reference_workbench_snippets": [{"code": "print(secret)"}],
                    "primary_tool_slugs": ["GMAIL_FETCH_EMAILS"],
                    "tool_schemas": {
                        "GMAIL_FETCH_EMAILS": {
                            "tool_slug": "GMAIL_FETCH_EMAILS",
                            "description": "Fetch messages",
                            "input_schema": {
                                "type": "object",
                                "required": ["query"],
                                "properties": {
                                    "query": {
                                        "type": "string",
                                        "description": "Gmail query syntax",
                                        "examples": ["from:example.com"]
                                    },
                                    "max_results": {
                                        "type": "integer",
                                        "default": 1,
                                        "description": "result count"
                                    }
                                }
                            }
                        }
                    },
                    "toolkit_connection_statuses": [{
                        "toolkit": "gmail",
                        "status": "ACTIVE",
                        "accounts": [{"user_info": {"emailAddress": "private@example.com"}}]
                    }]
                }]
            }
        })
        .to_string();

        let compact = compact_search_response(&raw);
        assert!(compact.contains("GMAIL_FETCH_EMAILS"));
        assert!(compact.contains("\"required\":[\"query\"]"));
        assert!(compact.contains("\"default\":1"));
        assert!(compact.contains("Call composio_run next"));
        assert!(!compact.contains("recommended_plan_steps"));
        assert!(!compact.contains("Gmail query syntax"));
        assert!(!compact.contains("private@example.com"));
        assert!(compact.len() < raw.len(), "{compact}");
    }

    #[test]
    fn exact_schema_response_drops_argument_essays_but_keeps_the_contract() {
        let raw = serde_json::json!({
            "data": {
                "tool_schemas": {
                    "GMAIL_FETCH_EMAILS": {
                        "description": "Fetch matching email messages.",
                        "input_schema": {
                            "type": "object",
                            "required": ["query"],
                            "properties": {
                                "query": {
                                    "type": "string",
                                    "description": "A very long explanation of Gmail syntax.",
                                    "examples": ["from:tokenrouter.com"]
                                },
                                "max_results": {
                                    "type": "integer",
                                    "minimum": 1,
                                    "maximum": 500,
                                    "default": 1
                                }
                            }
                        }
                    }
                }
            }
        })
        .to_string();

        let compact = compact_schema_response(&raw);
        assert!(compact.contains("GMAIL_FETCH_EMAILS"));
        assert!(compact.contains("\"required\":[\"query\"]"));
        assert!(compact.contains("\"maximum\":500"));
        assert!(!compact.contains("very long explanation"));
        assert!(!compact.contains("from:tokenrouter.com"));
    }

    #[test]
    fn stale_schema_slug_uses_the_servers_exact_suggestion_once() {
        let original = serde_json::json!({
            "tool_slugs": ["TAVILY_TAVILY_SEARCH"],
            "session_id": "safe-session"
        });
        let response = serde_json::json!({
            "data": {
                "success": false,
                "tool_schemas": {},
                "not_found": ["TAVILY_TAVILY_SEARCH"],
                "suggestions": {"TAVILY_TAVILY_SEARCH": ["TAVILY_SEARCH"]}
            }
        })
        .to_string();
        let corrected = schema_suggestion_retry_input(&original, &response).unwrap();
        assert_eq!(
            corrected["tool_slugs"],
            serde_json::json!(["TAVILY_SEARCH"])
        );
        assert_eq!(corrected["session_id"], "safe-session");
    }

    #[test]
    fn composio_run_normalizes_stringified_and_single_tool_calls() {
        let encoded = normalize_run_input(serde_json::json!({
            "tools": "[{\"tool_slug\":\"GMAIL_FETCH_EMAILS\",\"arguments\":{}}]",
            "sync_response_to_workbench": false
        }))
        .unwrap();
        assert_eq!(encoded["tools"].as_array().unwrap().len(), 1);
        assert_eq!(encoded["sync_response_to_workbench"], false);

        let single = normalize_run_input(serde_json::json!({
            "tools": {"tool_slug":"GITHUB_GET_REPOSITORY", "arguments":{}},
            "sync_response_to_workbench": false
        }))
        .unwrap();
        assert_eq!(single["tools"].as_array().unwrap().len(), 1);
        assert_eq!(single["sync_response_to_workbench"], false);
        assert!(normalize_run_input(serde_json::json!({"tools": []})).is_err());
    }

    #[test]
    fn notion_reschedule_cannot_silently_reopen_completed_work() {
        let unsafe_update = serde_json::json!({
            "tools": [{
                "tool_slug": "NOTION_UPDATE_PAGE",
                "arguments": {
                    "page_id": "math-1-3",
                    "properties": {
                        "Checkbox": {"checkbox": false},
                        "Date": {"date": {"start": "2026-09-04"}}
                    }
                }
            }],
            "sync_response_to_workbench": false
        });
        let error = normalize_run_input(unsafe_update.clone()).unwrap_err();
        assert!(error.to_string().contains("refusing to mark"));

        let mut confirmed = unsafe_update;
        confirmed["tools"][0]["user_confirmed_reopen_completed"] = serde_json::Value::Bool(true);
        let normalized = normalize_run_input(confirmed).unwrap();
        assert_eq!(
            normalized["tools"][0]["arguments"]["properties"]["Checkbox"]["checkbox"],
            false
        );
        assert!(normalized["tools"][0]
            .get("user_confirmed_reopen_completed")
            .is_none());
    }

    #[test]
    fn notion_reschedule_without_completion_property_remains_allowed() {
        let normalized = normalize_run_input(serde_json::json!({
            "tools": [{
                "tool_slug": "NOTION_UPDATE_ROW_DATABASE",
                "arguments": {
                    "row_id": "math-1-3",
                    "properties": [{"name":"Date","type":"date","value":"2026-09-04"}]
                }
            }],
            "sync_response_to_workbench": false
        }))
        .unwrap();
        assert_eq!(normalized["tools"][0]["arguments"]["row_id"], "math-1-3");
    }

    #[test]
    fn composio_run_lifts_connected_account_selectors_out_of_app_arguments() {
        let normalized = normalize_run_input(serde_json::json!({
            "tools": [
                {"tool_slug":"GMAIL_FETCH_EMAILS", "arguments":{
                    "account_id":"gmail_teacher", "query":"VVS"
                }},
                {"tool_slug":"GMAIL_FETCH_EMAILS", "arguments":{
                    "user_id":"gmail_school", "query":"Moodle"
                }}
            ]
        }))
        .unwrap();
        let tools = normalized["tools"].as_array().unwrap();
        assert_eq!(tools[0]["account"], "gmail_teacher");
        assert_eq!(tools[0]["arguments"]["query"], "VVS");
        assert!(tools[0]["arguments"].get("account_id").is_none());
        assert_eq!(tools[1]["account"], "gmail_school");
        assert!(tools[1]["arguments"].get("user_id").is_none());

        let legitimate_user = normalize_run_input(serde_json::json!({
            "tools": [{"tool_slug":"GMAIL_FETCH_EMAILS", "arguments":{
                "user_id":"teacher@example.com", "query":"VVS"
            }}]
        }))
        .unwrap();
        assert!(legitimate_user["tools"][0].get("account").is_none());
        assert_eq!(
            legitimate_user["tools"][0]["arguments"]["user_id"],
            "teacher@example.com"
        );

        let shared = normalize_run_input(serde_json::json!({
            "account":"gmail_teacher",
            "tools": [{"tool_slug":"GMAIL_FETCH_EMAILS", "arguments":{"query":"VVS"}}]
        }))
        .unwrap();
        assert_eq!(shared["tools"][0]["account"], "gmail_teacher");
        assert!(shared.get("account").is_none());
    }

    #[test]
    fn composio_run_prefers_inline_but_routes_remote_overflow_directly() {
        let normalized = normalize_run_input(serde_json::json!({
            "tools": [{"tool_slug":"GMAIL_FETCH_EMAILS","arguments":{"max_results":20}}],
            "sync_response_to_workbench": true
        }))
        .unwrap();
        assert_eq!(normalized["sync_response_to_workbench"], false);
        let workbench = normalize_run_input(serde_json::json!({
            "session_id":"document-session",
            "tools": [{"tool_slug":"COMPOSIO_REMOTE_WORKBENCH","arguments":{"code_to_execute":"print(1)"}}]
        })).unwrap();
        assert_eq!(remote_workbench_arguments(&workbench).unwrap().unwrap(),
            serde_json::json!({"code_to_execute":"print(1)","session_id":"document-session"}));
        assert!(remote_workbench_arguments(&normalized).unwrap().is_none());
        let mixed = normalize_run_input(serde_json::json!({"tools":[
            {"tool_slug":"GMAIL_SEND_EMAIL","arguments":{}},
            {"tool_slug":"COMPOSIO_REMOTE_WORKBENCH","arguments":{}}
        ]})).unwrap();
        assert!(remote_workbench_arguments(&mixed).is_err());
    }

    #[tokio::test]
    #[ignore = "live: requires Composio credentials; computes a constant without app actions"]
    async fn live_composio_remote_workbench_dispatch() {
        let output = mcp_run(serde_json::json!({"tools":[{
            "tool_slug":"COMPOSIO_REMOTE_WORKBENCH",
            "arguments":{
                "code_to_execute":"print('phoenix_remote_route_verified_' + str(2 + 2))",
                "thought":"Verify Phoenix direct helper routing with a constant; do not access files or apps."
            }
        }]})).await.expect("direct remote helper executes");
        assert!(output.content.contains("phoenix_remote_route_verified_4"),
            "remote helper did not return the computation result");
    }

    #[tokio::test]
    #[ignore = "live read-only document recovery; requires PHOENIX_TEST_GOOGLE_DOCUMENT_ID and account"]
    async fn live_composio_large_document_remote_recovery() {
        let id = std::env::var("PHOENIX_TEST_GOOGLE_DOCUMENT_ID").expect("document id required");
        let account = std::env::var("PHOENIX_TEST_GOOGLE_DOCS_ACCOUNT").expect("account required");
        let response = mcp_run(serde_json::json!({"tools":[{
            "tool_slug":"GOOGLEDOCS_GET_DOCUMENT_BY_ID","account":account,
            "arguments":{"id":id,"includeTabsContent":false}
        }]})).await.expect("read document");
        let root: serde_json::Value = serde_json::from_str(&response.content).unwrap();
        let path = root["data"]["remote_file_info"]["file_path"].as_str()
            .expect("this acceptance requires a genuinely overflowed response");
        let code = format!(
            "import json\npayload=json.load(open({}))\ndoc=payload['results'][0]['response']['data']\nassert doc['documentId']=={}\nbody=doc['body']['content']\ntables=[item['table'] for item in body if 'table' in item]\nassert len(tables)>0\nprint('phoenix_full_document_recovered tables='+str(len(tables)))",
            serde_json::to_string(path).unwrap(), serde_json::to_string(&id).unwrap()
        );
        let recovered = mcp_run(serde_json::json!({
            "session_id":root["data"]["session"]["id"],
            "tools":[{"tool_slug":"COMPOSIO_REMOTE_WORKBENCH","arguments":{
                "code_to_execute":code,
                "thought":"Read the supplied full document response and verify its identity/table count; do not modify files or apps."
            }}]
        })).await.expect("recover full remote document");
        assert!(recovered.content.contains("phoenix_full_document_recovered tables="),
            "remote recovery did not confirm document identity and tables");
    }

    #[test]
    fn overflowed_response_names_the_exact_recovery_call() {
        let text = serde_json::json!({"data":{"results":[{"response":{"successful":true,"data_preview":{"body":{"content":[{"endIndex":1},"...43 more items"]}}}}],
            "remote_file_info":{"file_path":"/mnt/files/mex/warn.json"},"session":{"id":"mex"}}}).to_string();
        let hint = overflow_recovery_hint(&text).expect("hint for overflowed response");
        assert!(hint.contains("/mnt/files/mex/warn.json") && hint.contains("COMPOSIO_REMOTE_WORKBENCH") && hint.contains("\"session_id\":\"mex\""));
        assert!(overflow_recovery_hint(r#"{"data":{"results":[]}}"#).is_none());
    }

    #[test]
    fn composio_run_repairs_google_docs_pdf_identifier_aliases() {
        let normalized = normalize_run_input(serde_json::json!({
            "tools": [{
                "tool_slug":"GOOGLEDOCS_EXPORT_DOCUMENT_AS_PDF",
                "arguments":{"document_id":"doc-123","filename":"lesson.pdf"}
            }]
        }))
        .unwrap();
        assert_eq!(normalized["tools"][0]["arguments"]["file_id"], "doc-123");
        assert!(normalized["tools"][0]["arguments"]
            .get("document_id")
            .is_none());
        assert_eq!(
            normalized["tools"][0]["arguments"]["filename"],
            "lesson.pdf"
        );
    }

    #[test]
    fn mixed_multi_execute_response_preserves_successful_actions() {
        let response = serde_json::json!({
            "data": {"results": [
                {"response": {"successful": true, "data": {"id": "kept"}}},
                {"response": {"successful": false, "error": "rate limited"}}
            ]}
        })
        .to_string();
        assert_eq!(successful_multi_execute_results(&response), 1);
        assert_eq!(successful_multi_execute_results("not json"), 0);
    }

    #[test]
    fn server_list_marks_apps_active_without_downgrading_no_auth_toolkits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("composio-connections.json");

        // hackernews is active only in the local registry (no-auth quirk:
        // the server's connected list will never include it).
        let mut registry = read_registry_from(&path).unwrap();
        registry.insert("hackernews".into(), "active".into());
        write_registry_to(&path, &registry).unwrap();

        // Simulate the refresh merge from a parsed server list.
        merge_active_apps_at(
            &path,
            parse_connected_apps(
                "- User has manually connected the apps: github, tavily. Prefer these.",
            ),
        )
        .unwrap();

        let line = render_context_line(&read_registry_from(&path).unwrap());
        assert!(
            line.contains("connected apps: github, hackernews, tavily."),
            "{line}"
        );
    }

    /// LIVE: the server's own connected-apps list is fetchable and parseable
    /// from the search tool's description — the primary discovery source.
    #[tokio::test]
    #[ignore = "live: needs ~/.phoenix/composio-mcp.key and network"]
    async fn live_composio_server_connected_apps_list_parses() {
        let apps = fetch_server_connected_apps()
            .await
            .expect("tools/list failed");
        assert!(
            apps.contains(&"github".to_string()),
            "server connected-apps list missing github: {apps:?}"
        );
    }

    /// LIVE: toolkit enumeration — the fallback for apps the search planner
    /// refuses to surface. Needs BOTH keys (ck_ for MCP, ak_ for catalog).
    #[tokio::test]
    #[ignore = "live: needs ~/.phoenix/composio-mcp.key + ~/.phoenix/composio.key and network"]
    async fn live_composio_toolkit_enumeration_returns_hackernews_schemas() {
        let out = mcp_schemas(serde_json::json!({"toolkit": "hackernews"}))
            .await
            .expect("toolkit enumeration failed");
        assert!(
            out.content.contains("HACKERNEWS_"),
            "no HACKERNEWS_* tools in: {}",
            &out.content[..out.content.len().min(400)]
        );
    }

    #[test]
    fn local_status_line_reports_configured_credentials_without_values() {
        let _guard = ENV_LOCK.lock().unwrap();
        let prev_mcp = std::env::var_os("COMPOSIO_MCP_KEY");
        std::env::set_var("COMPOSIO_MCP_KEY", "ck_secret_test");

        let line = local_status_line();
        assert!(line.contains("For-You MCP configured via env"), "{line}");
        assert!(!line.contains("ck_secret_test"), "{line}");

        restore_env("COMPOSIO_MCP_KEY", prev_mcp);
    }

    /// LIVE: the whole Composio For-You lane through Phoenix's own code path —
    /// consumer key discovery → MCP handshake → COMPOSIO_MANAGE_CONNECTIONS.
    /// Proves the user's connected apps are actually reachable.
    /// Run: `cargo test --lib -- --ignored live_composio`.
    #[tokio::test]
    #[ignore = "live: needs ~/.phoenix/composio-mcp.key and network"]
    async fn live_composio_mcp_lists_connections() {
        let out = mcp_connections(
            serde_json::json!({"toolkits": [{"name": "github", "action": "list"}]}),
        )
        .await
        .expect("COMPOSIO_MANAGE_CONNECTIONS failed — the For-You MCP lane is broken");
        assert!(!out.content.trim().is_empty(), "empty connections response");
        println!(
            "connections: {}",
            &out.content[..out.content.len().min(600)]
        );
    }

    /// LIVE: search-tools meta-tool round-trip (the discovery step every real
    /// Composio task starts with).
    #[tokio::test]
    #[ignore = "live: needs ~/.phoenix/composio-mcp.key and network"]
    async fn live_composio_mcp_search_finds_tools() {
        let out = mcp_search(serde_json::json!({"queries": ["send an email"]}))
            .await
            .expect("COMPOSIO_SEARCH_TOOLS failed");
        assert!(!out.content.trim().is_empty(), "empty search response");
        println!("search: {}", &out.content[..out.content.len().min(600)]);
    }

    fn restore_env(key: &str, prev: Option<std::ffi::OsString>) {
        match prev {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
}
