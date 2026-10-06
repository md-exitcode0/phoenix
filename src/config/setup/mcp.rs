//! `phoenix configure` → "MCP servers" — connect Phoenix to MCP servers
//! without hand-editing config.toml. Local (stdio command) and remote
//! (Streamable-HTTP URL) servers, live tools/list probing, park/remove.
//!
//! Every mutation rewrites the `[[profile.mcp_server]]` blocks through
//! `patch_mcp_server_blocks`; the meta-tools reload config per call, so
//! changes are live immediately — no gateway restart.

use super::*;
use crate::config::McpServerConfig;

pub(super) fn run_mcp_menu(theme: &ColorfulTheme, config_path: &std::path::Path) -> Result<()> {
    loop {
        let servers = load_servers()?;
        println!();
        print_server_list(&servers);
        let actions = [
            "Add a local server      a command Phoenix launches (stdio) — e.g. `npx -y @modelcontextprotocol/server-filesystem /path`",
            "Add a remote server     a URL (Streamable HTTP) — paste the endpoint + auth token if it needs one",
            "Test a server           connect now and list the tools it exposes",
            "Park / unpark           keep a server registered but toggle it off/on",
            "Remove a server         delete the registration (the server itself is untouched)",
            "← Back",
        ];
        let pick = Select::with_theme(theme)
            .with_prompt("MCP servers")
            .items(&actions)
            .default(0)
            .interact()
            .context("MCP menu cancelled")?;
        let outcome = match pick {
            0 => add_server(theme, config_path, false),
            1 => add_server(theme, config_path, true),
            2 => test_server(theme),
            3 => toggle_server(theme, config_path),
            4 => remove_server(theme, config_path),
            _ => return Ok(()),
        };
        if let Err(error) = outcome {
            if is_cancel(&error) {
                println!("  {}", style("cancelled — back to MCP servers").dim());
            } else {
                return Err(error);
            }
        }
    }
}

fn load_servers() -> Result<Vec<McpServerConfig>> {
    PhoenixConfig::load()
        .map(|config| config.profile.mcp_servers)
        .context("failed to load MCP configuration")
}

fn print_server_list(servers: &[McpServerConfig]) {
    if servers.is_empty() {
        println!(
            "  {}",
            style("No MCP servers registered yet. Add one below — agents reach them through the mcp_servers/mcp_call tools.").dim()
        );
        return;
    }
    for server in servers {
        let transport = if server.is_remote() {
            format!("remote · {}", server.url.as_deref().unwrap_or(""))
        } else {
            let mut cmd = server.command.clone();
            if !server.args.is_empty() {
                cmd.push(' ');
                cmd.push_str(&server.args.join(" "));
            }
            format!("local · {cmd}")
        };
        let mut notes = Vec::new();
        if let Some(route) = server.route.as_deref().filter(|r| !r.is_empty()) {
            notes.push(format!("for: {route}"));
        }
        if !server.enabled {
            notes.push("parked".to_string());
        }
        let suffix = if notes.is_empty() {
            String::new()
        } else {
            format!("  ({})", notes.join(" · "))
        };
        let bullet = if server.enabled {
            style("●").green()
        } else {
            style("○").dim()
        };
        println!(
            "  {bullet} {}  {}{suffix}",
            style(&server.name).bold(),
            style(transport).dim()
        );
        if let Some(desc) = server.description.as_deref().filter(|d| !d.is_empty()) {
            println!("      {}", style(desc).dim());
        }
    }
}

/// Shared add flow. `remote` picks the URL path; otherwise a launch command.
/// Ends with a live tools/list probe — a failing server can still be kept
/// (some need creds or binaries that aren't there yet).
fn add_server(theme: &ColorfulTheme, config_path: &std::path::Path, remote: bool) -> Result<()> {
    let name: String = Input::with_theme(theme)
        .with_prompt("  Name (how agents refer to it)")
        .validate_with(|input: &String| {
            if input.trim().is_empty() {
                Err("a name is required")
            } else {
                Ok(())
            }
        })
        .interact_text()
        .context("name entry cancelled")?;
    let name = name.trim().to_string();

    let mut server = McpServerConfig {
        name: name.clone(),
        command: String::new(),
        args: Vec::new(),
        cwd: None,
        env: Default::default(),
        url: None,
        headers: Default::default(),
        route: None,
        enabled: true,
        description: None,
    };

    if remote {
        let url: String = Input::with_theme(theme)
            .with_prompt("  Server URL (e.g. https://example.com/mcp)")
            .validate_with(|input: &String| {
                let t = input.trim();
                if t.starts_with("http://") || t.starts_with("https://") {
                    Ok(())
                } else {
                    Err("expected an http(s):// URL")
                }
            })
            .interact_text()
            .context("URL entry cancelled")?;
        server.url = Some(url.trim().to_string());
        let token: String = Input::with_theme(theme)
            .with_prompt("  Bearer token / API key (blank = none)")
            .allow_empty(true)
            .interact_text()
            .context("token entry cancelled")?;
        if !token.trim().is_empty() {
            server
                .headers
                .insert("Authorization".into(), format!("Bearer {}", token.trim()));
        }
        loop {
            let header: String = Input::with_theme(theme)
                .with_prompt("  Extra header as `Name: value` (blank = done)")
                .allow_empty(true)
                .interact_text()
                .context("header entry cancelled")?;
            let header = header.trim();
            if header.is_empty() {
                break;
            }
            match header.split_once(':') {
                Some((k, v)) if !k.trim().is_empty() => {
                    server.headers.insert(k.trim().into(), v.trim().into());
                }
                _ => println!("  {}", style("expected `Name: value` — skipped").yellow()),
            }
        }
    } else {
        let command_line: String = Input::with_theme(theme)
            .with_prompt("  Launch command (with args, quotes ok)")
            .validate_with(|input: &String| {
                if input.trim().is_empty() {
                    Err("a command is required")
                } else {
                    Ok(())
                }
            })
            .interact_text()
            .context("command entry cancelled")?;
        let mut parts = split_command_line(&command_line);
        server.command = parts.remove(0);
        server.args = parts;
        let cwd: String = Input::with_theme(theme)
            .with_prompt("  Working directory (blank = anywhere)")
            .allow_empty(true)
            .interact_text()
            .context("cwd entry cancelled")?;
        if !cwd.trim().is_empty() {
            server.cwd = Some(cwd.trim().to_string());
        }
        loop {
            let pair: String = Input::with_theme(theme)
                .with_prompt("  Env var as `KEY=value` (blank = done)")
                .allow_empty(true)
                .interact_text()
                .context("env entry cancelled")?;
            let pair = pair.trim();
            if pair.is_empty() {
                break;
            }
            match pair.split_once('=') {
                Some((k, v)) if !k.trim().is_empty() => {
                    server.env.insert(k.trim().into(), v.trim().into());
                }
                _ => println!("  {}", style("expected `KEY=value` — skipped").yellow()),
            }
        }
    }

    let route: String = Input::with_theme(theme)
        .with_prompt(
            "  Route to one specialist? (e.g. hacker, coder — blank = every MCP-capable agent)",
        )
        .allow_empty(true)
        .interact_text()
        .context("route entry cancelled")?;
    if !route.trim().is_empty() {
        server.route = Some(route.trim().to_lowercase());
    }
    let description: String = Input::with_theme(theme)
        .with_prompt("  One-line description (shown to agents in discovery; blank = none)")
        .allow_empty(true)
        .interact_text()
        .context("description entry cancelled")?;
    if !description.trim().is_empty() {
        server.description = Some(description.trim().to_string());
    }

    // Live probe BEFORE saving — the single most useful moment to catch a
    // typo'd command/URL. A failure is a warning, not a wall.
    println!("  {}", style("connecting…").dim());
    let probe = probe_blocking(&server);
    match &probe {
        Ok(tools) if !tools.is_empty() => {
            println!(
                "  {} Connected — {} tool(s):",
                style("✔").green(),
                tools.len()
            );
            for (tool_name, desc, _) in tools.iter().take(12) {
                let first = desc.lines().next().unwrap_or("").trim();
                if first.is_empty() {
                    println!("    • {tool_name}");
                } else {
                    println!("    • {tool_name} — {}", style(first).dim());
                }
            }
            if tools.len() > 12 {
                println!("    … and {} more", tools.len() - 12);
            }
        }
        Ok(_) => println!(
            "  {} Connected, but the server exposed no tools.",
            style("⚠").yellow()
        ),
        Err(error) => println!("  {} Could not connect: {error:#}", style("⚠").yellow()),
    }
    if probe.as_ref().map(|t| t.is_empty()).unwrap_or(true) {
        let keep = Confirm::with_theme(theme)
            .with_prompt("  Save it anyway? (some servers need creds/binaries that come later)")
            .default(true)
            .interact()
            .context("save confirmation cancelled")?;
        if !keep {
            println!("  {} Not saved.", style("•").dim());
            return Ok(());
        }
    }

    let mut servers = load_servers()?;
    let replaced = servers.iter().any(|s| s.name.eq_ignore_ascii_case(&name));
    servers.retain(|s| !s.name.eq_ignore_ascii_case(&name));
    servers.push(server);
    patch_mcp_server_blocks(config_path, &servers)?;
    println!(
        "  {} {} `{}` — live now; agents see it through mcp_servers/mcp_call.",
        style("✔").green(),
        if replaced { "Replaced" } else { "Added" },
        name
    );
    Ok(())
}

fn pick_server(theme: &ColorfulTheme, prompt: &str) -> Result<Option<McpServerConfig>> {
    let servers = load_servers()?;
    if servers.is_empty() {
        println!("  {}", style("No MCP servers registered.").dim());
        return Ok(None);
    }
    let labels: Vec<String> = servers
        .iter()
        .map(|s| {
            format!(
                "{}  {}{}",
                s.name,
                if s.is_remote() { "remote" } else { "local" },
                if s.enabled { "" } else { " · parked" }
            )
        })
        .collect();
    let pick = Select::with_theme(theme)
        .with_prompt(prompt)
        .items(&labels)
        .default(0)
        .interact()
        .context("server selection cancelled")?;
    Ok(servers.into_iter().nth(pick))
}

fn test_server(theme: &ColorfulTheme) -> Result<()> {
    let Some(server) = pick_server(theme, "Test which server")? else {
        return Ok(());
    };
    println!("  {}", style("connecting…").dim());
    match probe_blocking(&server) {
        Ok(tools) if !tools.is_empty() => {
            println!(
                "  {} `{}` is reachable — {} tool(s):",
                style("✔").green(),
                server.name,
                tools.len()
            );
            for (name, desc, _) in tools {
                let first = desc.lines().next().unwrap_or("").trim();
                if first.is_empty() {
                    println!("    • {name}");
                } else {
                    println!("    • {name} — {}", style(first).dim());
                }
            }
        }
        Ok(_) => println!(
            "  {} `{}` connected but exposed no tools.",
            style("⚠").yellow(),
            server.name
        ),
        Err(error) => println!(
            "  {} `{}` unreachable: {error:#}",
            style("✘").red(),
            server.name
        ),
    }
    Ok(())
}

fn toggle_server(theme: &ColorfulTheme, config_path: &std::path::Path) -> Result<()> {
    let Some(picked) = pick_server(theme, "Park / unpark which server")? else {
        return Ok(());
    };
    let mut servers = load_servers()?;
    for server in &mut servers {
        if server.name.eq_ignore_ascii_case(&picked.name) {
            server.enabled = !server.enabled;
            let state = if server.enabled { "active" } else { "parked" };
            println!("  {} `{}` is now {state}.", style("✔").green(), server.name);
        }
    }
    patch_mcp_server_blocks(config_path, &servers)
}

fn remove_server(theme: &ColorfulTheme, config_path: &std::path::Path) -> Result<()> {
    let Some(picked) = pick_server(theme, "Remove which server")? else {
        return Ok(());
    };
    let confirmed = Confirm::with_theme(theme)
        .with_prompt(format!("  Remove `{}`?", picked.name))
        .default(false)
        .interact()
        .context("remove confirmation cancelled")?;
    if !confirmed {
        println!("  {} Kept.", style("•").dim());
        return Ok(());
    }
    let mut servers = load_servers()?;
    servers.retain(|s| !s.name.eq_ignore_ascii_case(&picked.name));
    patch_mcp_server_blocks(config_path, &servers)?;
    println!("  {} Removed `{}`.", style("✔").green(), picked.name);
    Ok(())
}

/// Run the async tools/list probe from this sync menu (configure runs inside
/// the tokio runtime, so block_in_place + block_on is the sanctioned bridge —
/// same shape as the auth flows).
fn probe_blocking(server: &McpServerConfig) -> Result<Vec<(String, String, serde_json::Value)>> {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(crate::tools::local_mcp::probe_tools(server))
    })
}

/// Quote-aware whitespace split for the launch-command prompt, so
/// `node server.js --root "/my path"` registers with the path intact.
fn split_command_line(line: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in line.trim().chars() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => current.push(ch),
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch.is_whitespace() => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            None => current.push(ch),
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    if parts.is_empty() {
        parts.push(String::new());
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_split_respects_quotes() {
        assert_eq!(
            split_command_line("node server.js --root \"/my path\" -v"),
            vec!["node", "server.js", "--root", "/my path", "-v"]
        );
        assert_eq!(split_command_line("t3mp3st"), vec!["t3mp3st"]);
    }
}
