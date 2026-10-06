//! User-registered MCP servers as external tool providers — the generic lane
//! for reaching any MCP server, local (a child process spoken to over stdio,
//! e.g. T3MP3ST's `security_recon`) or remote (a Streamable-HTTP URL). Two
//! meta-tools, mirroring the Composio shape: `mcp_servers` discovers what's
//! registered and what each exposes, `mcp_call` invokes one tool on a named
//! server.
//!
//! Servers are registered in config (`[[profile.mcp_server]]`, managed via
//! `phoenix configure` → "MCP servers"); nothing is reachable unless the
//! user listed it there.

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::mcp_client::{self, StdioServer};
use super::ToolOutput;
use crate::config::{McpServerConfig, PhoenixConfig};

/// Per-operation ceiling. Recon/scan tools genuinely take a while (nmap), so
/// this is generous; a wedged server still can't hang a turn forever.
const MCP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(240);

/// Is this server meant for `agent`?
///
/// A server with no `route` is shared by everyone. A ROUTED server belongs to
/// exactly one lane: `route = "hacker"` keeps T3MP3ST's recon tools out of the
/// design agent's hands, and `route = "frontend"` keeps a design-reference
/// server out of everyone else's. Until 2026-07-30 this field was printed in
/// the discovery listing and otherwise ignored, so "route it to Iris" bought
/// no isolation at all. `None` (an executor with no agent identity — the
/// legacy runner, tests) sees everything, exactly as before.
fn routed_to(server: &McpServerConfig, agent: Option<&str>) -> bool {
    match (server.route.as_deref().map(str::trim), agent) {
        (None | Some(""), _) => true,
        (Some(_), None) => true,
        (Some(route), Some(agent)) => route.eq_ignore_ascii_case(agent),
    }
}

fn enabled_servers(agent: Option<&str>) -> Result<Vec<McpServerConfig>> {
    let config = PhoenixConfig::load().context("failed to load MCP configuration")?;
    Ok(config
        .profile
        .mcp_servers
        .into_iter()
        .filter(|server| server.enabled && routed_to(server, agent))
        .collect())
}

fn find_server(name: &str, agent: Option<&str>) -> Result<McpServerConfig> {
    // Look the name up across ALL enabled servers first, so a route miss can
    // say why instead of claiming the server does not exist.
    let all = enabled_servers(None)?;
    let available = configured_names(agent)?;
    let found = all
        .iter()
        .find(|s| s.name.eq_ignore_ascii_case(name))
        .cloned()
        .with_context(|| {
            format!("no enabled MCP server named `{name}`. Available to you: {available}")
        })?;
    if !routed_to(&found, agent) {
        let route = found.route.as_deref().unwrap_or("");
        anyhow::bail!(
            "MCP server `{name}` is routed to `{route}` and is not available to this agent. \
             Available to you: {}",
            configured_names(agent)?
        );
    }
    Ok(found)
}

fn configured_names(agent: Option<&str>) -> Result<String> {
    let names: Vec<String> = enabled_servers(agent)?
        .into_iter()
        .map(|s| s.name)
        .collect();
    Ok(if names.is_empty() {
        "(none — the user can add one with `phoenix configure` → MCP servers)".to_string()
    } else {
        names.join(", ")
    })
}

fn to_stdio(server: &McpServerConfig) -> StdioServer {
    StdioServer {
        command: server.command.clone(),
        args: server.args.clone(),
        cwd: server.cwd.clone(),
        env: server.env.clone().into_iter().collect(),
    }
}

fn header_pairs(server: &McpServerConfig) -> Vec<(String, String)> {
    server
        .headers
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// tools/list against one configured server, whichever transport it uses.
pub async fn probe_tools(server: &McpServerConfig) -> Result<Vec<(String, String, Value)>> {
    if let Some(url) = server.url.as_deref().filter(|u| !u.trim().is_empty()) {
        mcp_client::list_tools_with_schema(url, &header_pairs(server)).await
    } else {
        mcp_client::list_tools_stdio(&to_stdio(server), MCP_TIMEOUT).await
    }
}

async fn dispatch_call(server: &McpServerConfig, tool: &str, arguments: Value) -> Result<String> {
    if let Some(url) = server.url.as_deref().filter(|u| !u.trim().is_empty()) {
        mcp_client::call_tool(url, &header_pairs(server), tool, arguments).await
    } else {
        mcp_client::call_tool_stdio(&to_stdio(server), tool, arguments, MCP_TIMEOUT).await
    }
}

/// `mcp_servers` — discovery. Lists every enabled MCP server (local stdio or
/// remote HTTP) and, by reaching each briefly, the tools it exposes (name ·
/// description · input schema). No arguments. This is how an agent learns
/// what it can call.
pub async fn servers(_input: Value, agent: Option<&str>) -> Result<ToolOutput> {
    let servers = enabled_servers(agent)?;
    if servers.is_empty() {
        return Ok(ToolOutput {
            summary: "no MCP servers configured".to_string(),
            content: "No MCP servers are available to you. The user can add one (local command or remote URL) with `phoenix configure` → MCP servers."
                .to_string(),
        });
    }

    let mut out = String::new();
    for server in &servers {
        out.push_str(&format!("● {}", server.name));
        if server.is_remote() {
            out.push_str("  [remote]");
        }
        if let Some(route) = &server.route {
            out.push_str(&format!("  (for: {route})"));
        }
        out.push('\n');
        if let Some(desc) = &server.description {
            out.push_str(&format!("  {desc}\n"));
        }
        match probe_tools(server).await {
            Ok(tools) if !tools.is_empty() => {
                for (name, desc, schema) in tools {
                    let first_line = desc.lines().next().unwrap_or("").trim();
                    out.push_str(&format!("  • {name}"));
                    if !first_line.is_empty() {
                        out.push_str(&format!(" — {first_line}"));
                    }
                    out.push('\n');
                    if let Some(params) = schema_hint(&schema) {
                        out.push_str(&format!("      args: {params}\n"));
                    }
                }
            }
            Ok(_) => out.push_str("  (server exposed no tools)\n"),
            Err(error) => out.push_str(&format!("  ⚠ could not reach server: {error}\n")),
        }
        out.push('\n');
    }
    out.push_str(
        "Call one with mcp_call({ \"server\": \"<name>\", \"tool\": \"<tool>\", \"arguments\": { … } }).",
    );
    Ok(ToolOutput {
        summary: format!("{} MCP server(s)", servers.len()),
        content: out.trim_end().to_string(),
    })
}

/// A compact "field: type" hint from a JSON-Schema object's properties, so the
/// discovery listing shows callable arguments without dumping the whole schema.
fn schema_hint(schema: &Value) -> Option<String> {
    let props = schema.get("properties")?.as_object()?;
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut parts = Vec::new();
    for (name, spec) in props {
        let ty = spec.get("type").and_then(Value::as_str).unwrap_or("any");
        let flag = if required.contains(&name.as_str()) {
            "*"
        } else {
            ""
        };
        parts.push(format!("{name}{flag}: {ty}"));
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

#[derive(serde::Deserialize)]
struct CallInput {
    server: String,
    tool: String,
    #[serde(default)]
    arguments: Value,
}

/// `mcp_call` — execute one tool on a named configured MCP server.
pub async fn call(input: Value, agent: Option<&str>) -> Result<ToolOutput> {
    let call: CallInput =
        serde_json::from_value(input).context("mcp_call expects { server, tool, arguments }")?;
    let server = find_server(&call.server, agent)?;
    let arguments = if call.arguments.is_null() {
        json!({})
    } else {
        call.arguments
    };
    let text = dispatch_call(&server, &call.tool, arguments)
        .await
        .with_context(|| format!("mcp_call {}::{} failed", call.server, call.tool))?;
    Ok(ToolOutput {
        summary: format!("mcp_call {}::{} ok", call.server, call.tool),
        content: text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(name: &str, route: Option<&str>) -> McpServerConfig {
        McpServerConfig {
            name: name.into(),
            command: "true".into(),
            args: Vec::new(),
            cwd: None,
            env: Default::default(),
            url: None,
            headers: Default::default(),
            route: route.map(str::to_string),
            enabled: true,
            description: None,
        }
    }

    /// `route` is isolation, not a label. A recon server routed to the hacker
    /// lane must be invisible to the design agent, and a design-reference
    /// server routed to frontend must be invisible to everyone else.
    #[test]
    fn routing_scopes_a_server_to_its_lane() {
        let recon = server("t3mp3st", Some("hacker"));
        let design = server("landingfolio", Some("frontend"));
        let shared = server("notes", None);

        assert!(routed_to(&recon, Some("hacker")));
        assert!(!routed_to(&recon, Some("frontend")));
        assert!(routed_to(&design, Some("frontend")));
        assert!(!routed_to(&design, Some("hacker")));

        // An unrouted server belongs to everyone…
        assert!(routed_to(&shared, Some("frontend")));
        assert!(routed_to(&shared, Some("hacker")));
        // …and an executor with no agent identity (legacy runner, tests) keeps
        // the pre-routing behaviour of seeing everything.
        assert!(routed_to(&recon, None));
        assert!(routed_to(&design, None));

        // Instance labels are collapsed by the caller, and matching is
        // case-insensitive so `route = "Frontend"` still works.
        assert!(routed_to(&server("x", Some("Frontend")), Some("frontend")));
        // A blank route is the same as none.
        assert!(routed_to(&server("x", Some("  ")), Some("anyone")));
    }

    #[test]
    fn corrupt_config_is_not_reported_as_zero_mcp_servers() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(dir.path());
        crate::config::private_io::atomic_write_private(
            &dir.path().join("config.toml"),
            b"[profile.llm\ninvalid",
        )
        .unwrap();

        assert!(enabled_servers(None).is_err());
    }

    /// Remote transport against a real public MCP server (DeepWiki, no auth).
    /// Ignored (network). Run: cargo test --lib remote_mcp -- --ignored --nocapture
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn remote_mcp_server_lists_tools_over_http() {
        let server = McpServerConfig {
            name: "deepwiki".into(),
            command: String::new(),
            args: Vec::new(),
            cwd: None,
            env: Default::default(),
            url: Some("https://mcp.deepwiki.com/mcp".into()),
            headers: Default::default(),
            route: None,
            enabled: true,
            description: None,
        };
        let tools = probe_tools(&server).await.expect("remote tools/list");
        for (name, desc, schema) in &tools {
            println!("• {name} — {}", desc.lines().next().unwrap_or(""));
            if let Some(hint) = schema_hint(schema) {
                println!("    args: {hint}");
            }
        }
        assert!(
            !tools.is_empty(),
            "expected the remote server to expose tools"
        );
        // And the call path, end to end.
        let text = dispatch_call(
            &server,
            "read_wiki_structure",
            json!({ "repoName": "elder-plinius/T3MP3ST" }),
        )
        .await
        .expect("remote tools/call");
        println!("call returned {} chars", text.len());
        assert!(!text.trim().is_empty(), "expected call output");
    }

    /// Full chain: read the user's real ~/.phoenix/config.toml, launch each
    /// registered server, list its tools. Ignored (needs T3MP3ST registered +
    /// built). Run: cargo test --lib mcp_discovery -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn mcp_discovery_reaches_registered_servers() {
        let out = servers(json!({}), None).await.expect("mcp_servers");
        println!("{}", out.content);
        assert!(
            out.content.contains("security_recon") || out.content.contains("t3mp3st"),
            "expected a registered server in discovery output"
        );
    }
}

/// One-line status for the runtime context injection (how many servers are wired).
pub fn context_line() -> Option<String> {
    // Session-level line: names every configured server. Per-agent filtering
    // happens at discovery/call time (`routed_to`).
    let servers = match enabled_servers(None) {
        Ok(servers) => servers,
        Err(error) => {
            return Some(format!(
                "MCP configuration is unreadable; server availability is unknown: {error:#}"
            ))
        }
    };
    if servers.is_empty() {
        return None;
    }
    let names: Vec<String> = servers.into_iter().map(|s| s.name).collect();
    Some(format!(
        "MCP servers available via mcp_servers/mcp_call: {}.",
        names.join(", ")
    ))
}

/// The MCP capability line for ONE agent's runtime context — only the servers
/// that agent may actually reach, and an explicit empty state when it may
/// reach none.
///
/// Naming a server the agent would then be REFUSED (route mismatch) is a
/// capability lie, and silence is worse: an agent that sees no MCP line cannot
/// tell "no servers configured" from "this lane was never wired", so it either
/// invents a capability or refuses one it has. Both were live failure modes.
pub fn context_line_for(agent: Option<&str>) -> String {
    let servers = match enabled_servers(agent) {
        Ok(servers) => servers,
        Err(error) => {
            return format!(
                "MCP · configuration unreadable; mcp_servers will report the same error and no server call is safe until the config is repaired: {error:#}"
            )
        }
    };
    if servers.is_empty() {
        let others = match enabled_servers(None) {
            Ok(servers) => servers.len(),
            Err(error) => {
                return format!(
                    "MCP · configuration unreadable; mcp_servers will report the same error and no server call is safe until the config is repaired: {error:#}"
                )
            }
        };
        if others > 0 {
            return format!(
                "MCP · no server is routed to your lane ({others} configured for other lanes). \
                 mcp_servers confirms; the user adds one with `phoenix configure` → MCP servers."
            );
        }
        return "MCP · no servers configured. The lane works (mcp_servers/mcp_call) but nothing \
                is registered — the user adds one with `phoenix configure` → MCP servers."
            .to_string();
    }
    let listed: Vec<String> = servers
        .iter()
        .map(|s| {
            let transport = if s.is_remote() { "remote" } else { "local" };
            match s
                .description
                .as_deref()
                .map(str::trim)
                .filter(|d| !d.is_empty())
            {
                Some(desc) => format!("{} [{transport}] — {desc}", s.name),
                None => format!("{} [{transport}]", s.name),
            }
        })
        .collect();
    format!(
        "MCP · {} server(s) you can call — run mcp_servers for their tools, then mcp_call:\n  {}",
        servers.len(),
        listed.join("\n  ")
    )
}
