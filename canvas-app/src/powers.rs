//! Skills, Composio apps, and MCP servers — the agent's reachable powers.
//! Desktop-only: reads/writes ~/.phoenix and talks to skills.sh. The gateway
//! reloads MCP from config on the next tool call; no restart required.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{json, Value};

fn urlencoding_query(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

const MIN_REGISTRY_INSTALLS: u64 = 500;
const MAX_SKILL_NAME: usize = 128;
const MAX_SKILL_FILE: u64 = 16 * 1024 * 1024;
const MAX_SKILLS: usize = 2_048;

fn skills_root() -> PathBuf {
    crate::phoenix_home().join("skills")
}

fn config_path() -> PathBuf {
    crate::phoenix_home().join("config.toml")
}

fn valid_skill_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !name.is_empty()
        && name.len() <= MAX_SKILL_NAME
        && name != "."
        && name != ".."
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn parse_frontmatter(skill_md: &str) -> (Option<String>, Option<String>) {
    let mut inside = false;
    let mut name = None;
    let mut description = None;
    for line in skill_md.lines().take(80) {
        let trimmed = line.trim();
        if trimmed == "---" {
            if inside {
                break;
            }
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("name:") {
            name = Some(
                value
                    .trim()
                    .trim_matches(['"', '\''])
                    .chars()
                    .take(256)
                    .collect(),
            );
        } else if let Some(value) = trimmed.strip_prefix("description:") {
            description = Some(
                value
                    .trim()
                    .trim_matches(['"', '\''])
                    .chars()
                    .take(280)
                    .collect(),
            );
        }
    }
    (name, description)
}

#[tauri::command]
pub fn skills_list() -> Result<Value, String> {
    let root = skills_root();
    let mut skills = Vec::new();
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({ "root": root, "skills": [] }));
        }
        Err(error) => return Err(format!("could not read skills: {error}")),
    };
    for entry in entries.flatten() {
        if skills.len() >= MAX_SKILLS {
            break;
        }
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let dir_name = entry.file_name().to_string_lossy().to_string();
        if dir_name.starts_with('.') || !valid_skill_name(&dir_name) {
            continue;
        }
        let manifest = path.join("SKILL.md");
        let raw = fs::read_to_string(&manifest).unwrap_or_default();
        let (fm_name, description) = parse_frontmatter(&raw);
        let display = fm_name
            .filter(|n| valid_skill_name(n))
            .unwrap_or_else(|| dir_name.clone());
        skills.push(json!({
            "id": dir_name,
            "name": display,
            "description": description.unwrap_or_default(),
            "path": path,
        }));
    }
    skills.sort_by(|a, b| {
        a["name"]
            .as_str()
            .unwrap_or("")
            .cmp(b["name"].as_str().unwrap_or(""))
    });
    Ok(json!({ "root": root, "skills": skills }))
}

fn resolve_skill_dir(root: &Path, name: &str) -> Result<PathBuf, String> {
    let direct = root.join(name);
    if direct.is_dir() {
        return Ok(direct);
    }
    let entries = fs::read_dir(root).map_err(|error| format!("could not read skills: {error}"))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let raw = fs::read_to_string(path.join("SKILL.md")).unwrap_or_default();
        let (fm_name, _) = parse_frontmatter(&raw);
        if fm_name.as_deref() == Some(name) {
            return Ok(path);
        }
    }
    Err(format!("skill `{name}` is not installed"))
}

#[tauri::command]
pub fn skill_remove(name: String) -> Result<String, String> {
    if !valid_skill_name(&name) {
        return Err("that is not a skill name".into());
    }
    let dest = resolve_skill_dir(&skills_root(), &name)?;
    let root = skills_root()
        .canonicalize()
        .map_err(|error| format!("skills folder missing: {error}"))?;
    let canon = dest
        .canonicalize()
        .map_err(|_| format!("skill `{name}` is not installed"))?;
    if !canon.starts_with(&root) || canon == root {
        return Err("refusing to remove a path outside the skills folder".into());
    }
    fs::remove_dir_all(&canon).map_err(|error| format!("could not remove `{name}`: {error}"))?;
    Ok(format!("removed `{name}`"))
}

#[derive(Deserialize)]
struct RegistryHit {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    installs: u64,
    #[serde(default)]
    source: String,
}

fn skill_search_blocking(query: String) -> Result<Value, String> {
    let query = query.trim();
    if query.is_empty() || query.len() > 256 {
        return Err("search needs 1–256 characters".into());
    }
    if query.chars().any(char::is_control) {
        return Err("search cannot contain control characters".into());
    }
    let client = reqwest::blocking::Client::builder()
        .user_agent("PhoenixDesktop/0.1 (skill search)")
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|error| format!("could not open skills.sh: {error}"))?;
    let url = format!(
        "https://skills.sh/api/search?q={}",
        urlencoding_query(query)
    );
    let response = client
        .get(url)
        .send()
        .map_err(|error| format!("skills.sh did not answer: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("skills.sh returned HTTP {}", response.status()));
    }
    #[derive(Deserialize)]
    struct SearchResponse {
        #[serde(default)]
        skills: Vec<RegistryHit>,
    }
    let parsed: SearchResponse = response
        .json()
        .map_err(|error| format!("skills.sh returned unexpected JSON: {error}"))?;
    let hits: Vec<Value> = parsed
        .skills
        .into_iter()
        .filter(|hit| {
            !hit.id.is_empty()
                && hit.id.len() <= 2048
                && !hit.id.contains(['?', '#'])
                && (hit.id.starts_with("https://github.com/") || !hit.id.contains("://"))
        })
        .take(16)
        .map(|hit| {
            json!({
                "id": hit.id,
                "name": hit.name,
                "installs": hit.installs,
                "source": hit.source,
                "allowed": hit.installs >= MIN_REGISTRY_INSTALLS,
            })
        })
        .collect();
    Ok(json!({ "query": query, "hits": hits }))
}

#[tauri::command]
pub async fn skill_search(query: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || skill_search_blocking(query))
        .await
        .map_err(|error| error.to_string())?
}

fn skill_install_blocking(source: String, user_authorized: Option<bool>) -> Result<String, String> {
    let source = source.trim();
    if source.is_empty() {
        return Err("paste a skills.sh id or GitHub path".into());
    }
    let authorized = user_authorized.unwrap_or(false);
    let parsed = parse_github_source(source)?;
    if !authorized {
        if let Ok(search) = skill_search_blocking(parsed.skill_hint.clone()) {
            let allowed = search["hits"].as_array().is_some_and(|hits| {
                hits.iter().any(|hit| {
                    hit["id"].as_str() == Some(source) && hit["allowed"].as_bool() == Some(true)
                })
            });
            if !allowed {
                return Err(format!(
                    "`{source}` is below the {MIN_REGISTRY_INSTALLS}-install bar. Confirm if you still want it."
                ));
            }
        }
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let scratch = std::env::temp_dir().join(format!("phoenix-skill-{stamp}"));
    let _ = fs::remove_dir_all(&scratch);
    fs::create_dir_all(&scratch).map_err(|error| format!("could not stage the clone: {error}"))?;
    let repo_root = scratch.join("repo");
    let mut command = Command::new("git");
    command
        .arg("clone")
        .arg("--depth")
        .arg("1")
        .arg("--single-branch")
        .arg(&parsed.repo_url)
        .arg(&repo_root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(branch) = &parsed.branch {
        command.arg("--branch").arg(branch);
    }
    let output = command
        .output()
        .map_err(|error| format!("git is not available: {error}"))?;
    if !output.status.success() {
        let _ = fs::remove_dir_all(&scratch);
        return Err(format!(
            "git clone failed for {} — {}",
            parsed.repo_url,
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .next()
                .unwrap_or("unknown error")
        ));
    }
    let skill_dir = find_skill_dir(&repo_root, parsed.subpath.as_deref())
        .ok_or_else(|| format!("`{source}` has no SKILL.md"))?;
    let raw = fs::read_to_string(skill_dir.join("SKILL.md"))
        .map_err(|error| format!("could not read SKILL.md: {error}"))?;
    let (fm_name, _) = parse_frontmatter(&raw);
    let install_name = parsed
        .subpath
        .as_deref()
        .and_then(|p| p.rsplit('/').next())
        .map(str::to_string)
        .or(fm_name)
        .unwrap_or_else(|| "skill".into());
    if !valid_skill_name(&install_name) {
        let _ = fs::remove_dir_all(&scratch);
        return Err("the skill name is not safe to install".into());
    }
    let dest = skills_root().join(&install_name);
    if dest.exists() {
        let _ = fs::remove_dir_all(&scratch);
        return Err(format!("`{install_name}` is already installed"));
    }
    fs::create_dir_all(skills_root())
        .map_err(|error| format!("could not create skills: {error}"))?;
    copy_tree(&skill_dir, &dest).map_err(|error| {
        let _ = fs::remove_dir_all(&dest);
        format!("could not copy skill: {error}")
    })?;
    let _ = fs::remove_dir_all(&scratch);
    Ok(format!("installed `{install_name}`"))
}

#[tauri::command]
pub async fn skill_install(
    source: String,
    user_authorized: Option<bool>,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || skill_install_blocking(source, user_authorized))
        .await
        .map_err(|error| error.to_string())?
}

struct GithubSource {
    repo_url: String,
    branch: Option<String>,
    subpath: Option<String>,
    skill_hint: String,
}

fn parse_github_source(source: &str) -> Result<GithubSource, String> {
    if source.contains(['?', '#']) || source.contains("://") && !source.contains("github.com/") {
        return Err("only GitHub skill sources are accepted".into());
    }
    let rest = source
        .trim()
        .trim_start_matches("https://github.com/")
        .trim_start_matches("http://github.com/")
        .trim_start_matches("github.com/")
        .trim_end_matches('/');
    let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() < 2 {
        return Err("expected owner/repo or a skills.sh id".into());
    }
    if parts.iter().any(|p| *p == "." || *p == "..") {
        return Err("skill source contains an unsafe path".into());
    }
    let owner = parts[0];
    let repo = parts[1].trim_end_matches(".git");
    let (branch, subpath) = if parts.len() >= 4 && parts[2] == "tree" {
        (
            Some(parts[3].to_string()),
            (!parts[4..].is_empty()).then(|| parts[4..].join("/")),
        )
    } else if parts.len() > 2 {
        (None, Some(parts[2..].join("/")))
    } else {
        (None, None)
    };
    Ok(GithubSource {
        repo_url: format!("https://github.com/{owner}/{repo}.git"),
        branch,
        subpath: subpath.clone(),
        skill_hint: subpath
            .as_deref()
            .and_then(|p| p.rsplit('/').next())
            .unwrap_or(repo)
            .to_string(),
    })
}

fn find_skill_dir(root: &Path, wanted: Option<&str>) -> Option<PathBuf> {
    if root.join("SKILL.md").is_file() {
        return Some(root.to_path_buf());
    }
    if let Some(wanted) = wanted {
        let direct = root.join(wanted);
        if direct.join("SKILL.md").is_file() {
            return Some(direct);
        }
        let last = wanted.rsplit('/').next().unwrap_or(wanted);
        return walk_named(root, last, 0);
    }
    walk_named(root, "", 0)
}

fn walk_named(root: &Path, last: &str, depth: u8) -> Option<PathBuf> {
    if depth > 6 {
        return None;
    }
    let entries = fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if path.join("SKILL.md").is_file() && (last.is_empty() || name == last) {
            return Some(path);
        }
        if let Some(found) = walk_named(&path, last, depth + 1) {
            return Some(found);
        }
    }
    None
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(from).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let meta = entry.metadata().map_err(|e| e.to_string())?;
        let dest = to.join(entry.file_name());
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            copy_tree(&entry.path(), &dest)?;
        } else if meta.len() <= MAX_SKILL_FILE {
            fs::copy(entry.path(), dest).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct McpServer {
    name: String,
    command: String,
    args: Vec<String>,
    cwd: Option<String>,
    url: Option<String>,
    bearer: Option<String>,
    route: Option<String>,
    enabled: bool,
    description: Option<String>,
}

impl McpServer {
    fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "kind": if self.url.is_some() { "remote" } else { "local" },
            "command": self.command,
            "args": self.args,
            "cwd": self.cwd,
            "url": self.url,
            "has_token": self.bearer.as_deref().is_some_and(|t| !t.is_empty() && t != "lf_YOUR_TOKEN"),
            "route": self.route,
            "enabled": self.enabled,
            "description": self.description,
        })
    }
}

fn load_mcp_servers(raw: &str) -> Vec<McpServer> {
    let Ok(value) = raw.parse::<toml::Value>() else {
        return Vec::new();
    };
    let Some(servers) = value
        .get("profile")
        .and_then(|p| p.get("mcp_server"))
        .and_then(|v| v.as_array())
    else {
        return Vec::new();
    };
    servers
        .iter()
        .filter_map(|entry| {
            let tbl = entry.as_table()?;
            let command = tbl
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let url = tbl
                .get("url")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .map(str::to_string);
            if command.is_empty() && url.is_none() {
                return None;
            }
            let args = tbl
                .get("args")
                .and_then(|v| v.as_array())
                .map(|rows| {
                    rows.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            Some(McpServer {
                name: tbl
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(url.as_deref().unwrap_or(&command))
                    .to_string(),
                command,
                args,
                cwd: tbl.get("cwd").and_then(|v| v.as_str()).map(str::to_string),
                url,
                bearer: tbl
                    .get("bearer")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                route: tbl
                    .get("route")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                enabled: tbl.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
                description: tbl
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            })
        })
        .collect()
}

fn toml_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn render_mcp_blocks(servers: &[McpServer]) -> String {
    let mut out = String::new();
    for server in servers {
        out.push_str("\n[[profile.mcp_server]]\n");
        out.push_str(&format!("name = {}\n", toml_quote(&server.name)));
        if let Some(url) = server.url.as_deref().filter(|u| !u.is_empty()) {
            out.push_str(&format!("url = {}\n", toml_quote(url)));
            if let Some(bearer) = server.bearer.as_deref().filter(|b| !b.is_empty()) {
                out.push_str(&format!("bearer = {}\n", toml_quote(bearer)));
            }
        } else {
            out.push_str(&format!("command = {}\n", toml_quote(&server.command)));
            if !server.args.is_empty() {
                let quoted: Vec<String> = server.args.iter().map(|a| toml_quote(a)).collect();
                out.push_str(&format!("args = [{}]\n", quoted.join(", ")));
            }
            if let Some(cwd) = server.cwd.as_deref().filter(|c| !c.is_empty()) {
                out.push_str(&format!("cwd = {}\n", toml_quote(cwd)));
            }
        }
        if let Some(route) = server.route.as_deref().filter(|r| !r.is_empty()) {
            out.push_str(&format!("route = {}\n", toml_quote(route)));
        }
        if !server.enabled {
            out.push_str("enabled = false\n");
        }
        if let Some(desc) = server.description.as_deref().filter(|d| !d.is_empty()) {
            out.push_str(&format!("description = {}\n", toml_quote(desc)));
        }
    }
    out
}

fn strip_mcp_blocks(contents: &str) -> String {
    let header = "[[profile.mcp_server]]";
    let mut out = String::new();
    let mut skipping = false;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed == header {
            skipping = true;
            continue;
        }
        if skipping {
            let is_header = trimmed.starts_with('[') && trimmed.ends_with(']');
            if is_header && trimmed != header && !trimmed.starts_with("[profile.mcp_server.") {
                skipping = false;
            } else {
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn write_mcp_servers(servers: &[McpServer]) -> Result<(), String> {
    let path = config_path();
    let existing = fs::read_to_string(&path).map_err(|error| format!("config.toml: {error}"))?;
    let mut next = strip_mcp_blocks(&existing).trim_end().to_string();
    next.push_str(&render_mcp_blocks(servers));
    if !next.ends_with('\n') {
        next.push('\n');
    }
    crate::backup_and_write_config(&path, &existing, next)
}

#[tauri::command]
pub fn mcp_list() -> Result<Value, String> {
    let path = config_path();
    let raw = fs::read_to_string(&path).unwrap_or_default();
    let servers = load_mcp_servers(&raw);
    Ok(json!({
        "path": path,
        "servers": servers.iter().map(McpServer::to_json).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
pub struct McpUpsert {
    pub name: String,
    pub kind: String,
    pub command: Option<String>,
    pub url: Option<String>,
    pub token: Option<String>,
    pub route: Option<String>,
    pub description: Option<String>,
    pub cwd: Option<String>,
}

#[tauri::command]
pub fn mcp_upsert(spec: McpUpsert) -> Result<String, String> {
    let name = spec.name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err("give the server a short name".into());
    }
    let path = config_path();
    let raw = fs::read_to_string(&path).map_err(|error| format!("config.toml: {error}"))?;
    let mut servers = load_mcp_servers(&raw);
    servers.retain(|server| server.name != name);
    let remote = spec.kind == "remote";
    let server = if remote {
        let url = spec.url.as_deref().unwrap_or("").trim();
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err("remote MCP needs an http(s) URL".into());
        }
        McpServer {
            name: name.to_string(),
            command: String::new(),
            args: Vec::new(),
            cwd: None,
            url: Some(url.to_string()),
            bearer: spec
                .token
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string),
            route: spec.route.filter(|r| !r.trim().is_empty()),
            enabled: true,
            description: spec.description.filter(|d| !d.trim().is_empty()),
        }
    } else {
        let line = spec.command.as_deref().unwrap_or("").trim();
        if line.is_empty() {
            return Err("local MCP needs a launch command".into());
        }
        let mut parts = split_command_line(line);
        let command = parts.remove(0);
        McpServer {
            name: name.to_string(),
            command,
            args: parts,
            cwd: spec.cwd.filter(|c| !c.trim().is_empty()),
            url: None,
            bearer: None,
            route: spec.route.filter(|r| !r.trim().is_empty()),
            enabled: true,
            description: spec.description.filter(|d| !d.trim().is_empty()),
        }
    };
    servers.push(server);
    write_mcp_servers(&servers)?;
    Ok(format!(
        "`{name}` is live for every coworker that can call MCP"
    ))
}

#[tauri::command]
pub fn mcp_toggle(name: String) -> Result<String, String> {
    let path = config_path();
    let raw = fs::read_to_string(&path).map_err(|error| format!("config.toml: {error}"))?;
    let mut servers = load_mcp_servers(&raw);
    let Some(server) = servers.iter_mut().find(|server| server.name == name) else {
        return Err(format!("no MCP server named `{name}`"));
    };
    server.enabled = !server.enabled;
    let label = if server.enabled { "live" } else { "parked" };
    write_mcp_servers(&servers)?;
    Ok(format!("`{name}` is {label}"))
}

#[tauri::command]
pub fn mcp_remove(name: String) -> Result<String, String> {
    let path = config_path();
    let raw = fs::read_to_string(&path).map_err(|error| format!("config.toml: {error}"))?;
    let mut servers = load_mcp_servers(&raw);
    let before = servers.len();
    servers.retain(|server| server.name != name);
    if servers.len() == before {
        return Err(format!("no MCP server named `{name}`"));
    }
    write_mcp_servers(&servers)?;
    Ok(format!("removed `{name}`"))
}

fn split_command_line(line: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for ch in line.chars() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                current.push(ch);
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            c if c.is_whitespace() => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

#[tauri::command]
pub fn composio_apps() -> Result<Value, String> {
    let path = crate::phoenix_home().join("composio-connections.json");
    let raw =
        crate::read_private_text(&path, 64 * 1024, "Composio connections")?.unwrap_or_default();
    let apps: BTreeMap<String, String> = if raw.trim().is_empty() {
        BTreeMap::new()
    } else {
        serde_json::from_str(&raw).unwrap_or_default()
    };
    Ok(json!({
        "path": path,
        "apps": apps.into_iter().map(|(name, status)| json!({ "name": name, "status": status })).collect::<Vec<_>>(),
    }))
}

#[tauri::command]
pub fn composio_key_clear() -> Result<String, String> {
    let path = crate::phoenix_home().join("composio-mcp.key");
    let home = crate::phoenix_home();
    if path.exists() {
        let canon = path.canonicalize().map_err(|error| error.to_string())?;
        if !canon.starts_with(&home) {
            return Err("refusing to touch a key outside Phoenix home".into());
        }
        fs::remove_file(&canon).map_err(|error| format!("could not forget the key: {error}"))?;
    }
    Ok("Composio key forgotten".into())
}
