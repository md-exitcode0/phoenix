//! Config file rendering and writing: destination, TOML blocks, patching.

use super::*;

pub fn config_destination() -> Result<PathBuf> {
    // phoenix_home() honors PHOENIX_HOME — the writer must target the same
    // file the loader reads, or a sandboxed configure edits the real config.
    Ok(crate::config::phoenix_home().join("config.toml"))
}

/// Write a sensitive file with owner-only permissions established before any
/// bytes reach disk. `OpenOptionsExt::mode` protects a newly-created file;
/// `set_permissions` also repairs a stale, overly-permissive temp file before
/// it is reused.
fn write_private_file(path: &std::path::Path, contents: &[u8]) -> Result<()> {
    crate::config::private_io::write_private_file(path, contents)
}

fn read_config_private(path: &std::path::Path) -> Result<String> {
    let bytes = crate::config::private_io::read_private_file(path)?
        .with_context(|| format!("Failed to read {}", path.display()))?;
    String::from_utf8(bytes).with_context(|| format!("{} is not UTF-8", path.display()))
}

/// The ONE way config.toml reaches disk: atomic tmp+rename. The gateway
/// re-reads this file on every turn/tick, so a plain write mid-patch could
/// be observed torn (parse failure → lanes degrade for that read) and a
/// process killed mid-write would leave a corrupt config behind.
pub(super) fn write_config_atomic(path: &std::path::Path, contents: &str) -> Result<()> {
    crate::config::private_io::atomic_write_private(path, contents.as_bytes())
}

fn replace_config_atomic(path: &std::path::Path, expected: &str, contents: &str) -> Result<()> {
    crate::config::private_io::compare_and_swap_private(
        path,
        Some(expected.as_bytes()),
        contents.as_bytes(),
    )
}

#[cfg(all(test, unix))]
mod private_config_write_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn atomic_config_write_creates_private_temp_and_final_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let temp = path.with_extension("toml.tmp");

        write_private_file(&temp, b"temporary secret").unwrap();
        assert_eq!(
            std::fs::metadata(&temp).unwrap().permissions().mode() & 0o777,
            0o600
        );

        std::fs::remove_file(&temp).unwrap();

        // Unique staging names prevent concurrent writers from reusing one
        // another's temp file; replacement still repairs an old permissive
        // destination.
        std::fs::write(&path, "old secret").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_config_atomic(&path, "new secret").unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new secret");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(!temp.exists());
    }
}

/// Render the `[profile.browser]` TOML block. Shared by the onboarding writer
/// and the `configure` in-place patcher so both emit byte-identical output.
pub(super) fn render_browser_block(
    source: &str,
    attach_port: Option<u16>,
    binary: Option<&str>,
    extra_args: Option<&[String]>,
    login_source: Option<&str>,
    suspend_after_seconds: Option<u64>,
) -> String {
    let port_line = attach_port
        .map(|port| format!("attach_port = {port}\n"))
        .unwrap_or_default();
    let binary_line = binary
        .map(|binary| format!("binary = \"{binary}\"\n"))
        .unwrap_or_default();
    let args_line = extra_args
        .filter(|args| !args.is_empty())
        .map(|args| {
            let quoted: Vec<String> = args.iter().map(|a| format!("\"{a}\"")).collect();
            format!("extra_args = [{}]\n", quoted.join(", "))
        })
        .unwrap_or_default();
    let login_line = login_source
        .filter(|s| !s.is_empty() && *s != "none")
        .map(|s| format!("login_source = \"{s}\"\n"))
        .unwrap_or_default();
    let suspend_line = suspend_after_seconds
        .map(|seconds| format!("suspend_after_seconds = {seconds}\n"))
        .unwrap_or_default();
    format!(
        "\n[profile.browser]\nsource = \"{source}\"\n{port_line}{binary_line}{args_line}{login_line}{suspend_line}"
    )
}

/// Drop a whole `[section]` table (header through the line before the next
/// `[header]` or EOF) from a TOML document, leaving everything else verbatim.
pub(super) fn strip_toml_section(contents: &str, section: &str) -> String {
    let header = format!("[{section}]");
    let mut out = String::new();
    let mut skipping = false;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed == header {
            skipping = true;
            continue;
        }
        if skipping {
            let is_section_header = trimmed.starts_with('[') && trimmed.ends_with(']');
            if is_section_header {
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

/// Set or clear one role's `<role>_model` / `<role>_provider` lines inside
/// `[profile.llm]`, leaving everything else verbatim. `model: None` removes
/// both lines (the role rides the main model again).
pub(super) fn patch_llm_role_lines(
    path: &std::path::Path,
    role: &str,
    model: Option<&str>,
    provider_id: Option<&str>,
) -> Result<()> {
    let existing = read_config_private(path)?;
    let model_key = format!("{role}_model");
    let provider_key = format!("{role}_provider");
    let mut out: Vec<String> = Vec::new();
    let mut in_llm = false;
    let mut wrote_model = false;
    let mut wrote_provider = false;
    let inject = |out: &mut Vec<String>, wrote_model: &mut bool, wrote_provider: &mut bool| {
        if !*wrote_model {
            if let Some(m) = model {
                out.push(format!("{model_key} = \"{m}\""));
            }
            *wrote_model = true;
        }
        if !*wrote_provider {
            if let Some(p) = provider_id {
                out.push(format!("{provider_key} = \"{p}\""));
            }
            *wrote_provider = true;
        }
    };
    for line in existing.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            if in_llm {
                // Leaving [profile.llm] — inject anything not yet written.
                inject(&mut out, &mut wrote_model, &mut wrote_provider);
            }
            in_llm = trimmed.trim_end() == "[profile.llm]";
        }
        if in_llm
            && (trimmed.starts_with(&format!("{model_key} "))
                || trimmed.starts_with(&format!("{model_key}=")))
        {
            if let Some(m) = model {
                out.push(format!("{model_key} = \"{m}\""));
            }
            wrote_model = true;
            continue;
        }
        if in_llm
            && (trimmed.starts_with(&format!("{provider_key} "))
                || trimmed.starts_with(&format!("{provider_key}=")))
        {
            if let Some(p) = provider_id {
                out.push(format!("{provider_key} = \"{p}\""));
            }
            wrote_provider = true;
            continue;
        }
        out.push(line.to_string());
    }
    if in_llm {
        inject(&mut out, &mut wrote_model, &mut wrote_provider);
    }
    if !wrote_model {
        anyhow::bail!(
            "config at {} has no [profile.llm] table — run `phoenix onboard`",
            path.display()
        );
    }
    let mut joined = out.join("\n");
    if !joined.ends_with('\n') {
        joined.push('\n');
    }
    replace_config_atomic(path, &existing, &joined)
}

/// Replace ONLY the `model =` line inside `[profile.llm]` (provider + auth
/// untouched) — the reconfigure-primary path's orchestrator pick.
pub(super) fn patch_llm_main_model(path: &std::path::Path, model: &str) -> Result<()> {
    let existing = read_config_private(path)?;
    let mut out: Vec<String> = Vec::new();
    let mut in_llm = false;
    let mut wrote = false;
    for line in existing.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            in_llm = trimmed.trim_end() == "[profile.llm]";
        }
        if in_llm && (trimmed.starts_with("model ") || trimmed.starts_with("model=")) {
            out.push(format!("model = \"{model}\""));
            wrote = true;
            continue;
        }
        out.push(line.to_string());
    }
    if !wrote {
        anyhow::bail!(
            "config at {} has no [profile.llm] model line — run `phoenix onboard`",
            path.display()
        );
    }
    let mut joined = out.join("\n");
    if !joined.ends_with('\n') {
        joined.push('\n');
    }
    replace_config_atomic(path, &existing, &joined)
}

/// Set or clear the legacy global `reasoning_effort` line in `[profile.llm]`
/// (the per-lane truth lives in `[profile.llm.efforts]`).
pub(super) fn patch_llm_reasoning_line(path: &std::path::Path, effort: Option<&str>) -> Result<()> {
    let existing = read_config_private(path)?;
    let mut out: Vec<String> = Vec::new();
    let mut in_llm = false;
    let mut wrote = false;
    let write_line = |out: &mut Vec<String>, wrote: &mut bool| {
        if !*wrote {
            if let Some(effort) = effort {
                out.push(format!("reasoning_effort = \"{effort}\""));
            }
            *wrote = true;
        }
    };
    for line in existing.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            if in_llm {
                write_line(&mut out, &mut wrote);
            }
            in_llm = trimmed.trim_end() == "[profile.llm]";
        }
        if in_llm
            && (trimmed.starts_with("reasoning_effort ")
                || trimmed.starts_with("reasoning_effort="))
        {
            write_line(&mut out, &mut wrote);
            continue;
        }
        out.push(line.to_string());
    }
    if in_llm {
        write_line(&mut out, &mut wrote);
    }
    let mut joined = out.join("\n");
    if !joined.ends_with('\n') {
        joined.push('\n');
    }
    replace_config_atomic(path, &existing, &joined)
}

/// Replace the web-provider tables (`[profile.search/crawl/scrape]` + their
/// `.auth` subtables) with a freshly-run web setup's selections.
pub(super) fn patch_web_blocks(path: &std::path::Path, web: &WebSetupSelections) -> Result<()> {
    let existing = read_config_private(path)?;
    let mut stripped = existing.clone();
    for section in [
        "profile.search",
        "profile.search.auth",
        "profile.crawl",
        "profile.crawl.auth",
        "profile.scrape",
        "profile.scrape.auth",
    ] {
        stripped = strip_toml_section(&stripped, section);
    }
    let mut out = stripped.trim_end().to_string();
    out.push('\n');
    out.push_str(&format_web_profile_blocks(web));
    if !out.ends_with('\n') {
        out.push('\n');
    }
    replace_config_atomic(path, &existing, &out)
}

/// Replace (or remove) the `[profile.browser]` table in an existing config file
/// without touching any other section. `source: None` removes the table.
pub(super) fn patch_browser_block(path: &std::path::Path, setup: &BrowserSetup) -> Result<()> {
    let existing = read_config_private(path)?;
    let (source, attach_port, binary, extra_args, login_source, suspend_after_seconds) = setup;
    let stripped = strip_toml_section(&existing, "profile.browser");
    let mut out = stripped.trim_end().to_string();
    if let Some(source) = source {
        out.push_str(&render_browser_block(
            source,
            *attach_port,
            binary.as_deref(),
            extra_args.as_deref(),
            login_source.as_deref(),
            *suspend_after_seconds,
        ));
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    replace_config_atomic(path, &existing, &out)?;
    Ok(())
}

/// Escape a string for a double-quoted TOML value.
fn toml_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Render every `[[profile.mcp_server]]` block. Shared by nothing else yet —
/// the onboarding writer never emits MCP servers (they're added later via
/// `phoenix configure`), so the patcher below is the single writer.
pub(super) fn render_mcp_server_blocks(servers: &[crate::config::McpServerConfig]) -> String {
    fn inline_table(name: &str, map: &std::collections::BTreeMap<String, String>) -> String {
        if map.is_empty() {
            return String::new();
        }
        let pairs: Vec<String> = map
            .iter()
            .map(|(k, v)| format!("{} = {}", toml_quote(k), toml_quote(v)))
            .collect();
        format!("{name} = {{ {} }}\n", pairs.join(", "))
    }
    let mut out = String::new();
    for server in servers {
        out.push_str("\n[[profile.mcp_server]]\n");
        out.push_str(&format!("name = {}\n", toml_quote(&server.name)));
        if let Some(url) = server.url.as_deref().filter(|u| !u.trim().is_empty()) {
            out.push_str(&format!("url = {}\n", toml_quote(url)));
            out.push_str(&inline_table("headers", &server.headers));
        } else {
            out.push_str(&format!("command = {}\n", toml_quote(&server.command)));
            if !server.args.is_empty() {
                let quoted: Vec<String> = server.args.iter().map(|a| toml_quote(a)).collect();
                out.push_str(&format!("args = [{}]\n", quoted.join(", ")));
            }
            if let Some(cwd) = server.cwd.as_deref().filter(|c| !c.is_empty()) {
                out.push_str(&format!("cwd = {}\n", toml_quote(cwd)));
            }
            out.push_str(&inline_table("env", &server.env));
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

/// Drop every `[[section]]` array-of-tables block (header through the line
/// before the next `[…]`/`[[…]]` header or EOF), leaving the rest verbatim.
pub(super) fn strip_toml_array_of_tables(contents: &str, section: &str) -> String {
    let header = format!("[[{section}]]");
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
            // A dotted subtable of an array element ([profile.mcp_server.env])
            // belongs to the block being dropped, not to what follows.
            let is_own_subtable = trimmed.starts_with(&format!("[{section}."));
            if is_header && trimmed != header && !is_own_subtable {
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

/// Replace the full set of `[[profile.mcp_server]]` blocks in an existing
/// config with `servers` (empty = remove them all), leaving every other
/// section verbatim. Callers pass the complete desired registry — add/remove/
/// toggle flows all mutate a loaded Vec and hand it here.
pub fn patch_mcp_server_blocks(
    path: &std::path::Path,
    servers: &[crate::config::McpServerConfig],
) -> Result<()> {
    let existing = read_config_private(path)?;
    let stripped = strip_toml_array_of_tables(&existing, "profile.mcp_server");
    let mut out = stripped.trim_end().to_string();
    out.push_str(&render_mcp_server_blocks(servers));
    if !out.ends_with('\n') {
        out.push('\n');
    }
    replace_config_atomic(path, &existing, &out)?;
    Ok(())
}

/// Render one string→string subtable of `[profile.llm]` (`efforts`,
/// `agent_models`). Empty map = no block at all.
pub(super) fn render_llm_map_block(
    name: &str,
    map: &std::collections::BTreeMap<String, String>,
) -> String {
    if map.is_empty() {
        return String::new();
    }
    let mut out = format!("\n[profile.llm.{name}]\n");
    for (key, value) in map {
        out.push_str(&format!("{key} = {}\n", toml_quote(value)));
    }
    out
}

/// Render the provider-id → explicit compaction-mode policy table.
pub(super) fn render_compaction_modes_block(
    modes: &std::collections::BTreeMap<String, crate::config::CompactionMode>,
) -> String {
    if modes.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n[profile.llm.compaction_modes]\n");
    for (provider, mode) in modes {
        out.push_str(&format!("{provider} = {}\n", toml_quote(mode.as_str())));
    }
    out
}

/// Replace `[profile.llm.efforts]` / `[profile.llm.agent_models]` in an
/// existing config (empty map removes the table), leaving the rest verbatim.
pub(super) fn patch_llm_map_block(
    path: &std::path::Path,
    name: &str,
    map: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    let existing = read_config_private(path)?;
    let stripped = strip_toml_section(&existing, &format!("profile.llm.{name}"));
    let mut out = stripped.trim_end().to_string();
    out.push_str(&render_llm_map_block(name, map));
    if !out.ends_with('\n') {
        out.push('\n');
    }
    replace_config_atomic(path, &existing, &out)
}

/// Set one provider's explicit native-compaction policy while preserving all
/// other provider entries.  The parser is used against the caller's path (not
/// the process-wide default) so configure works in isolated/test homes too.
pub(super) fn patch_llm_compaction_mode(
    path: &std::path::Path,
    provider_id: &str,
    mode: crate::config::CompactionMode,
) -> Result<()> {
    let existing = read_config_private(path)?;
    let mut modes = crate::config::ConfigLoader::new()
        .with_path(path.to_path_buf())
        .load()
        .with_context(|| format!("failed to parse config at {}", path.display()))?
        .profile
        .llm
        .compaction_modes;
    modes.insert(provider_id.to_string(), mode);
    let mut stripped = strip_toml_section(&existing, "profile.llm.compaction_modes");
    stripped = strip_toml_section(&stripped, "profile.llm.compaction");
    let mut out = stripped.trim_end().to_string();
    out.push_str(&render_compaction_modes_block(&modes));
    if !out.ends_with('\n') {
        out.push('\n');
    }
    replace_config_atomic(path, &existing, &out)
}

/// Render the account-fallback chain tables. Only non-empty chains emit lines;
/// fully empty chains emit nothing (the sections disappear from the config).
pub(super) fn render_fallback_blocks(
    chains: &crate::config::FallbackChains,
    web: &crate::config::WebFallbackChains,
) -> String {
    fn arr_line(name: &str, values: &[String]) -> String {
        if values.is_empty() {
            return String::new();
        }
        let quoted: Vec<String> = values.iter().map(|v| format!("\"{v}\"")).collect();
        format!("{name} = [{}]\n", quoted.join(", "))
    }
    let mut out = String::new();
    if !chains.is_empty() {
        out.push_str("\n[profile.llm.fallback]\n");
        out.push_str(&arr_line("orchestrator", &chains.orchestrator));
        out.push_str(&arr_line("specialist", &chains.specialist));
        out.push_str(&arr_line("librarian", &chains.librarian));
        out.push_str(&arr_line("vision", &chains.vision));
        out.push_str(&arr_line("image", &chains.image));
        if !chains.agents.is_empty() {
            out.push_str("\n[profile.llm.fallback.agents]\n");
            for (agent, chain) in &chains.agents {
                out.push_str(&arr_line(agent, chain));
            }
        }
    }
    if !web.is_empty() {
        out.push_str("\n[profile.web_fallback]\n");
        out.push_str(&arr_line("search", &web.search));
        out.push_str(&arr_line("crawl", &web.crawl));
        out.push_str(&arr_line("scrape", &web.scrape));
    }
    out
}

/// Replace the fallback-chain tables in an existing config, leaving every
/// other section verbatim.
pub(super) fn patch_fallback_blocks(
    path: &std::path::Path,
    chains: &crate::config::FallbackChains,
    web: &crate::config::WebFallbackChains,
) -> Result<()> {
    let existing = read_config_private(path)?;
    let mut stripped = existing.clone();
    for section in [
        "profile.llm.fallback",
        "profile.llm.fallback.agents",
        "profile.web_fallback",
    ] {
        stripped = strip_toml_section(&stripped, section);
    }
    let mut out = stripped.trim_end().to_string();
    out.push_str(&render_fallback_blocks(chains, web));
    if !out.ends_with('\n') {
        out.push('\n');
    }
    replace_config_atomic(path, &existing, &out)?;
    Ok(())
}

/// Switch ONLY the main provider/model/auth in an existing config, preserving
/// every other line: roles, vision, reasoning, web, browser. Line-surgical:
/// `provider =` / `model =` are replaced inside `[profile.llm]`, the old
/// `[profile.llm.auth]` table is dropped, and the new one (when auth is
/// needed) is appended. Used by `phoenix configure`'s quick switch and the
/// in-session `/provider` command — one patcher, two callers.
pub fn patch_llm_provider_block(
    path: &std::path::Path,
    provider_id: &str,
    model: &str,
    auth: &SetupAuthSelection,
) -> Result<()> {
    let existing = read_config_private(path)?;
    let stripped = strip_toml_section(&existing, "profile.llm.auth");
    let mut out: Vec<String> = Vec::new();
    let mut in_llm = false;
    let mut wrote_provider = false;
    let mut wrote_model = false;
    for line in stripped.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            in_llm = trimmed == "[profile.llm]";
        }
        if in_llm && (trimmed.starts_with("provider ") || trimmed.starts_with("provider=")) {
            out.push(format!("provider = \"{provider_id}\""));
            wrote_provider = true;
            continue;
        }
        if in_llm && (trimmed.starts_with("model ") || trimmed.starts_with("model=")) {
            out.push(format!("model = \"{model}\""));
            wrote_model = true;
            continue;
        }
        out.push(line.to_string());
    }
    if !wrote_provider || !wrote_model {
        anyhow::bail!(
            "config at {} has no [profile.llm] provider/model lines to update — run `phoenix onboard`",
            path.display()
        );
    }
    let mut joined = out.join("\n");
    joined = joined.trim_end().to_string();
    if auth.method != "none" {
        joined.push_str("\n\n");
        joined.push_str(&format_auth_block(auth));
    }
    if !joined.ends_with('\n') {
        joined.push('\n');
    }
    replace_config_atomic(path, &existing, &joined)?;
    Ok(())
}

/// `phoenix configure` — change anything configurable after the one-time
/// `onboard`. A menu jumps straight to a section; each section reuses the same
/// pickers as onboarding and writes in place, preserving the rest of the config.

pub fn write_config_file(
    path: &PathBuf,
    selections: &SetupSelections,
    auth_selection: &SetupAuthSelection,
    web: &WebSetupSelections,
) -> Result<()> {
    let auth_hint = match auth_selection.source.as_str() {
        "env" => auth_selection
            .env_var
            .as_ref()
            .map(|env_var| format!("Configured to read auth from {env_var}."))
            .unwrap_or_else(|| "Configured to read auth from environment.".to_string()),
        "profile" => auth_selection
            .profile
            .as_ref()
            .map(|profile| {
                format!(
                    "Configured to read auth from ~/.phoenix/auth-profiles.json profile {profile}."
                )
            })
            .unwrap_or_else(|| {
                "Configured to read auth from ~/.phoenix/auth-profiles.json.".to_string()
            }),
        _ => "No provider auth is required for this configuration.".to_string(),
    };
    let auth_block = format_auth_block(auth_selection);
    let web_block = format_web_profile_blocks(web);
    let contents = format!(
        "# Phoenix configuration\n\
         # Generated by `phoenix onboard`\n\
         # {auth_hint}\n\
         \n\
         [global]\n\
         log_level = \"info\"\n\
         \n\
         [profile]\n\
         name = \"default\"\n\
         \n\
         [profile.llm]\n\
         provider = \"{provider}\"\n\
         timeout_seconds = 0\n\
         temperature = 0.0\n\
         model = \"{orch}\"\n\
         {specialist_line}\
         {librarian_line}\
         {vision_lines}\
         {reasoning_line}\
         {context_line}\
         {auth_block}\
         {llm_maps}\
         {web_block}\
         {browser_block}",
        auth_hint = auth_hint,
        provider = selections.provider_id,
        orch = selections.orchestrator_model,
        specialist_line = {
            let mut lines = String::new();
            if selections.specialist_model != selections.orchestrator_model {
                lines.push_str(&format!(
                    "specialist_model = \"{}\"\n",
                    selections.specialist_model
                ));
            }
            if let Some(provider) = &selections.specialist_provider {
                lines.push_str(&format!("specialist_provider = \"{provider}\"\n"));
            }
            lines
        },
        librarian_line = {
            let mut lines = String::new();
            if selections.librarian_model != selections.orchestrator_model {
                lines.push_str(&format!(
                    "librarian_model = \"{}\"\n",
                    selections.librarian_model
                ));
            }
            if let Some(provider) = &selections.librarian_provider {
                lines.push_str(&format!("librarian_provider = \"{provider}\"\n"));
            }
            lines
        },
        vision_lines = {
            let mut lines = selections
                .vision_model
                .as_ref()
                .map(|vision| {
                    format!(
                        "vision_model = \"{vision}\"\nvision_provider = \"{}\"\n",
                        selections
                            .vision_provider
                            .as_deref()
                            .unwrap_or(&selections.provider_id)
                    )
                })
                .unwrap_or_default();
            if let Some(image) = &selections.image_model {
                lines.push_str(&format!(
                    "image_model = \"{image}\"\nimage_provider = \"{}\"\n",
                    selections
                        .image_provider
                        .as_deref()
                        .unwrap_or(&selections.provider_id)
                ));
            }
            lines
        },
        reasoning_line = selections
            .reasoning_effort
            .as_ref()
            .map(|effort| format!("reasoning_effort = \"{effort}\"\n"))
            .unwrap_or_default(),
        context_line = selections
            .context_window
            .map(|window| format!("context_window = {window}\n"))
            .unwrap_or_default(),
        auth_block = auth_block,
        llm_maps = {
            let mut blocks = render_llm_map_block("efforts", &selections.efforts);
            blocks.push_str(&render_llm_map_block(
                "agent_models",
                &selections.agent_models,
            ));
            blocks.push_str(&render_compaction_modes_block(&selections.compaction_modes));
            blocks
        },
        web_block = web_block,
        browser_block = selections
            .browser_source
            .as_ref()
            .map(|source| {
                render_browser_block(
                    source,
                    selections.browser_attach_port,
                    selections.browser_binary.as_deref(),
                    selections.browser_extra_args.as_deref(),
                    selections.browser_login_source.as_deref(),
                    selections.browser_suspend_after_seconds,
                )
            })
            .unwrap_or_default(),
    );
    write_config_atomic(path, &contents)?;
    Ok(())
}

pub(super) fn format_web_profile_blocks(web: &WebSetupSelections) -> String {
    let mut out = String::new();
    if let Some(search) = &web.search {
        out.push_str("\n[profile.search]\n");
        out.push_str(&format!("provider = \"{}\"\n", search.provider_id));
        out.push_str("num_results = 10\n");
        out.push_str(&format_web_auth_toml_block("profile.search", &search.auth));
    }
    if let Some(crawl) = &web.crawl {
        out.push_str("\n[profile.crawl]\n");
        out.push_str(&format!("provider = \"{}\"\n", crawl.provider_id));
        out.push_str("rate_limit = 10\n");
        out.push_str(&format_web_auth_toml_block("profile.crawl", &crawl.auth));
    }
    if let Some(scrape) = &web.scrape {
        out.push_str("\n[profile.scrape]\n");
        out.push_str(&format!("provider = \"{}\"\n", scrape.provider_id));
        out.push_str(&format_web_auth_toml_block("profile.scrape", &scrape.auth));
    }
    out
}

pub(super) fn format_auth_block(auth: &SetupAuthSelection) -> String {
    let mut lines = String::from("[profile.llm.auth]\n");
    lines.push_str(&format!("method = \"{}\"\n", auth.method));
    lines.push_str(&format!("source = \"{}\"\n", auth.source));
    if let Some(profile) = &auth.profile {
        lines.push_str(&format!("profile = \"{}\"\n", profile));
    }
    if let Some(env_var) = &auth.env_var {
        lines.push_str(&format!("env_var = \"{}\"\n", env_var));
    }
    lines
}

pub(super) fn build_runtime_config(
    provider: &ProviderModels,
    orchestrator_model: &str,
    specialist_model: &str,
    librarian_model: &str,
    auth_selection: &SetupAuthSelection,
) -> PhoenixConfig {
    PhoenixConfig {
        profile: Profile {
            name: "default".to_string(),
            llm: LLMProfile {
                provider: provider.id.to_string(),
                model: orchestrator_model.to_string(),
                orchestrator_model: Some(orchestrator_model.to_string()),
                specialist_model: (specialist_model != orchestrator_model)
                    .then(|| specialist_model.to_string()),
                librarian_model: (librarian_model != orchestrator_model)
                    .then(|| librarian_model.to_string()),
                vision_model: None,
                vision_provider: None,
                native_vision: true,
                image_model: None,
                image_provider: None,
                specialist_provider: None,
                librarian_provider: None,
                memory_model: None,
                memory_provider: None,
                stt_model: None,
                stt_provider: None,
                tts_model: None,
                tts_provider: None,
                tts_voice: None,
                realtime_model: None,
                realtime_provider: None,
                realtime_voice: None,
                auth: Some(crate::config::LLMAuthConfig {
                    method: Some(auth_selection.method.clone()),
                    source: Some(auth_selection.source.clone()),
                    profile: auth_selection.profile.clone(),
                    env_var: auth_selection.env_var.clone(),
                }),
                temperature: Some(0.0),
                max_tokens: None,
                timeout_seconds: 0,
                reasoning_effort: None,
                efforts: Default::default(),
                service_tiers: Default::default(),
                agent_models: Default::default(),
                auth_by_lane: Default::default(),
                compaction_modes: Default::default(),
                context_window: None,
                context_windows: Default::default(),
                fallback: crate::config::FallbackChains::default(),
            },
            search: None,
            crawl: None,
            scrape: None,
            browser: None,
            web_fallback: crate::config::WebFallbackChains::default(),
            mcp_servers: Vec::new(),
        },
    }
}

pub(super) fn should_run_provider_check(auth_selection: &SetupAuthSelection) -> bool {
    auth_selection.method == "api" || auth_selection.method == "none"
}

fn handler_name_is_zen(handler: &str) -> bool {
    let normalized = handler
        .trim()
        .to_ascii_lowercase()
        .strip_suffix(".desktop")
        .unwrap_or_else(|| handler.trim())
        .to_ascii_lowercase();
    normalized == "zen" || normalized.starts_with("zen-") || normalized.starts_with("userapp-zen-")
}

#[cfg(target_os = "linux")]
const XDG_MIME_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
#[cfg(target_os = "linux")]
const XDG_MIME_OUTPUT_LIMIT: usize = 16 * 1024;

#[cfg(target_os = "linux")]
struct OwnedCommandGroup {
    child: Option<std::process::Child>,
    pgid: libc::pid_t,
}

#[cfg(target_os = "linux")]
impl OwnedCommandGroup {
    fn child_mut(&mut self) -> std::io::Result<&mut std::process::Child> {
        self.child.as_mut().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::Other,
                "bounded command child is no longer available",
            )
        })
    }

    /// Kill the complete child-owned process group and reap the direct child
    /// without letting an uninterruptible process defeat the caller's hard
    /// deadline. The rare child that cannot be reaped promptly is handed to a
    /// dedicated reaper thread instead of becoming a zombie.
    fn cleanup_bounded(&mut self) {
        if self.child.is_none() {
            return;
        }
        if self.pgid > 1 {
            let _ = unsafe { libc::kill(-self.pgid, libc::SIGKILL) };
        }
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
        }

        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
        loop {
            let reaped = self
                .child
                .as_mut()
                .and_then(|child| child.try_wait().ok())
                .flatten()
                .is_some();
            if reaped {
                self.child.take();
                return;
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        if let Some(mut child) = self.child.take() {
            let _ = std::thread::Builder::new()
                .name("phoenix-command-reaper".to_string())
                .spawn(move || {
                    let _ = child.wait();
                });
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for OwnedCommandGroup {
    fn drop(&mut self) {
        self.cleanup_bounded();
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct BoundedCommandOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
}

#[cfg(target_os = "linux")]
fn set_pipe_nonblocking<T: std::os::fd::AsRawFd>(pipe: &T) -> std::io::Result<()> {
    let fd = pipe.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn drain_bounded_pipe<R: std::io::Read>(
    pipe: &mut R,
    captured: &mut Vec<u8>,
    limit: usize,
) -> std::io::Result<bool> {
    let mut chunk = [0_u8; 4 * 1024];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) => return Ok(false),
            Ok(read) => {
                let remaining = limit.saturating_sub(captured.len());
                let keep = remaining.min(read);
                captured.extend_from_slice(&chunk[..keep]);
                if keep < read {
                    return Ok(true);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

#[cfg(target_os = "linux")]
fn run_bounded_command(
    program: &std::path::Path,
    args: &[&str],
    timeout: std::time::Duration,
    output_limit: usize,
) -> std::io::Result<BoundedCommandOutput> {
    use std::os::unix::process::CommandExt;

    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0)
        .spawn()?;
    let pgid = match libc::pid_t::try_from(child.id()) {
        Ok(pgid) => pgid,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "bounded command PID does not fit a process-group id",
            ));
        }
    };
    let mut owned = OwnedCommandGroup {
        child: Some(child),
        pgid,
    };
    let mut stdout = owned.child_mut()?.stdout.take().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::Other,
            "bounded command did not provide stdout",
        )
    })?;
    let mut stderr = owned.child_mut()?.stderr.take().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::Other,
            "bounded command did not provide stderr",
        )
    })?;
    set_pipe_nonblocking(&stdout)?;
    set_pipe_nonblocking(&stderr)?;

    let deadline = std::time::Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "timeout overflow"))?;
    let mut stdout_capture = Vec::new();
    let mut stderr_capture = Vec::new();
    loop {
        let stdout_flood = drain_bounded_pipe(&mut stdout, &mut stdout_capture, output_limit)?;
        let stderr_flood = drain_bounded_pipe(&mut stderr, &mut stderr_capture, output_limit)?;
        if stdout_flood || stderr_flood {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "bounded command exceeded its output limit",
            ));
        }

        if let Some(status) = owned.child_mut()?.try_wait()? {
            // Capture bytes written just before exit. Any descendant retaining
            // a pipe is killed by OwnedCommandGroup as this function returns.
            let stdout_flood = drain_bounded_pipe(&mut stdout, &mut stdout_capture, output_limit)?;
            let stderr_flood = drain_bounded_pipe(&mut stderr, &mut stderr_capture, output_limit)?;
            if stdout_flood || stderr_flood {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "bounded command exceeded its output limit",
                ));
            }
            return Ok(BoundedCommandOutput {
                status,
                stdout: stdout_capture,
            });
        }

        let now = std::time::Instant::now();
        if now >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "bounded command timed out",
            ));
        }
        std::thread::sleep((deadline - now).min(std::time::Duration::from_millis(5)));
    }
}

#[cfg(target_os = "linux")]
fn default_https_handler_is_zen() -> bool {
    run_bounded_command(
        std::path::Path::new("xdg-mime"),
        &["query", "default", "x-scheme-handler/https"],
        XDG_MIME_TIMEOUT,
        XDG_MIME_OUTPUT_LIMIT,
    )
    .ok()
    .filter(|output| output.status.success())
    .and_then(|output| String::from_utf8(output.stdout).ok())
    .map(|handler| handler_name_is_zen(&handler))
    .unwrap_or(false)
}

#[cfg(not(target_os = "linux"))]
fn default_https_handler_is_zen() -> bool {
    false
}

const MAX_ZEN_COMPATIBILITY_BYTES: u64 = 64 * 1024;

/// Read a small compatibility file supplied by an external application.
///
/// Unlike Phoenix-owned state, a browser profile file may legitimately be a
/// symlink (for example into a versioned/snap profile). Open the resolved
/// object non-blocking and validate the opened inode instead of rejecting the
/// symlink by name. That accepts regular-file links without ever waiting on a
/// FIFO/device or allocating from an unbounded file.
fn read_external_regular_file_bounded(path: &std::path::Path, max_bytes: u64) -> Result<Vec<u8>> {
    use std::io::Read;

    let read_limit = max_bytes
        .checked_add(1)
        .context("external-file read limit overflow")?;

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to open external file {}", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect external file {}", path.display()))?;
    if !metadata.is_file() {
        anyhow::bail!("external path is not a regular file: {}", path.display());
    }
    if metadata.len() > max_bytes {
        anyhow::bail!(
            "external file {} is too large ({} bytes; max {max_bytes})",
            path.display(),
            metadata.len()
        );
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take(read_limit)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read external file {}", path.display()))?;
    if bytes.len() as u64 > max_bytes {
        anyhow::bail!(
            "external file {} grew beyond the {max_bytes}-byte limit while reading",
            path.display()
        );
    }
    Ok(bytes)
}

fn zen_binary_for_profile(profile: &std::path::Path) -> Option<PathBuf> {
    let compatibility = read_external_regular_file_bounded(
        &profile.join("compatibility.ini"),
        MAX_ZEN_COMPATIBILITY_BYTES,
    )
    .ok()?;
    let compatibility = String::from_utf8(compatibility).ok()?;
    let platform_dir = compatibility.lines().find_map(|line| {
        line.trim()
            .strip_prefix("LastPlatformDir=")
            .map(std::path::PathBuf::from)
    })?;
    let binary = platform_dir.join(if cfg!(windows) { "zen.exe" } else { "zen" });
    binary.is_file().then_some(binary)
}

fn zen_oauth_args(profile: &std::path::Path, url: &str) -> Vec<std::ffi::OsString> {
    vec![
        "-profile".into(),
        profile.as_os_str().to_owned(),
        "-new-tab".into(),
        url.into(),
    ]
}

fn oauth_browser_command(program: &std::path::Path, args: &[std::ffi::OsString]) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    command.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // The browser is intentionally persistent. It must not inherit the
    // bounded login helper's process group, which the desktop cleans up.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
}

fn spawn_and_reap(program: &std::path::Path, args: &[std::ffi::OsString]) -> std::io::Result<()> {
    let mut child = oauth_browser_command(program, args).spawn()?;
    // Browser launchers normally remote the URL into the existing process and
    // exit. Reap that short-lived helper without blocking the OAuth flow.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// When Zen is the desktop default, target the live profile explicitly.
///
/// Calling only `xdg-open URL` leaves profile selection to profiles.ini. A
/// damaged/mid-update registry can then show an empty profile manager or create
/// a fresh profile even while the user's real session is open. The profile's
/// compatibility.ini also tells us the exact Zen installation that owns it, so
/// this avoids cross-install profile IDs.
fn open_in_active_zen(url: &str) -> Result<bool> {
    if !default_https_handler_is_zen() {
        return Ok(false);
    }
    let profile = match crate::tools::browser_cookies::locate_active_profile("zen") {
        Ok(profile) => profile,
        Err(_) => return Ok(false),
    };
    let binary = match zen_binary_for_profile(&profile) {
        Some(binary) => binary,
        None => return Ok(false),
    };
    spawn_and_reap(&binary, &zen_oauth_args(&profile, url)).with_context(|| {
        format!(
            "Failed to launch Zen OAuth tab with profile {}",
            profile.display()
        )
    })?;
    Ok(true)
}

pub(super) fn open_auth_url(url: &str) -> Result<()> {
    if let Ok(path) = std::env::var("PHOENIX_OAUTH_OPEN_LOG") {
        crate::config::private_io::atomic_write_private(
            std::path::Path::new(&path),
            format!("{url}\n").as_bytes(),
        )
        .with_context(|| format!("Failed to write PHOENIX_OAUTH_OPEN_LOG {}", path))?;
    }
    if let Ok(browser) = std::env::var("BROWSER") {
        if !browser.trim().is_empty() {
            if open::with_detached(url, browser.trim()).is_ok() {
                return Ok(());
            }
            println!("  BROWSER command failed; trying the desktop browser.");
        }
    }
    match open_in_active_zen(url) {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        Err(error) => println!("  Safe Zen launch failed; trying the desktop browser. ({error})"),
    }
    if let Err(error) = open::that_detached(url) {
        println!("  Browser launch failed; open the URL above manually. ({error})");
    }
    Ok(())
}

#[cfg(test)]
mod oauth_browser_tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn oauth_browser_has_its_own_process_group() {
        let mut command = oauth_browser_command(std::path::Path::new("/bin/sh"), &[
            "-c".into(), "exec ps -o pgid= -p $$".into(),
        ]);
        command.stdout(std::process::Stdio::piped());
        let child = command.spawn().unwrap();
        let pid = child.id();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        let pgid: u32 = String::from_utf8(output.stdout).unwrap().trim().parse().unwrap();
        assert_eq!(pgid, pid, "browser helper must leave the login process group");
    }

    #[test]
    fn recognizes_only_zen_desktop_handlers() {
        assert!(handler_name_is_zen("zen.desktop\n"));
        assert!(handler_name_is_zen("ZEN"));
        assert!(handler_name_is_zen("userapp-Zen-9W6HO3.desktop"));
        assert!(!handler_name_is_zen("firefox.desktop"));
        assert!(!handler_name_is_zen("org.gnome.Zenity.desktop"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn bounded_command_captures_small_output_and_rejects_timeout_and_flood() {
        let success = run_bounded_command(
            std::path::Path::new("/bin/sh"),
            &["-c", "printf 'zen.desktop\\n'; printf warning >&2"],
            std::time::Duration::from_secs(1),
            1024,
        )
        .unwrap();
        assert!(success.status.success());
        assert_eq!(success.stdout, b"zen.desktop\n");

        let started = std::time::Instant::now();
        let timeout = run_bounded_command(
            std::path::Path::new("/bin/sh"),
            &["-c", "sleep 30 & wait"],
            std::time::Duration::from_millis(40),
            1024,
        )
        .unwrap_err();
        assert_eq!(timeout.kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));

        let started = std::time::Instant::now();
        let flood = run_bounded_command(
            std::path::Path::new("/bin/sh"),
            &[
                "-c",
                "while :; do printf '0123456789abcdef0123456789abcdef'; done",
            ],
            std::time::Duration::from_secs(1),
            1024,
        )
        .unwrap_err();
        assert_eq!(flood.kind(), std::io::ErrorKind::InvalidData);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn resolves_the_binary_that_owns_the_profile() {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("install");
        let profile = dir.path().join("profile");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::create_dir_all(&profile).unwrap();
        let binary = install.join(if cfg!(windows) { "zen.exe" } else { "zen" });
        std::fs::write(&binary, b"").unwrap();
        std::fs::write(
            profile.join("compatibility.ini"),
            format!("[Compatibility]\nLastPlatformDir={}\n", install.display()),
        )
        .unwrap();
        assert_eq!(zen_binary_for_profile(&profile), Some(binary));
    }

    #[cfg(unix)]
    #[test]
    fn zen_compatibility_allows_regular_symlink_but_rejects_fifo() {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("install");
        let profile = dir.path().join("profile");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::create_dir_all(&profile).unwrap();
        let binary = install.join("zen");
        std::fs::write(&binary, b"").unwrap();
        let real = dir.path().join("real-compatibility.ini");
        std::fs::write(
            &real,
            format!("[Compatibility]\nLastPlatformDir={}\n", install.display()),
        )
        .unwrap();
        let compatibility = profile.join("compatibility.ini");
        symlink(&real, &compatibility).unwrap();
        assert_eq!(zen_binary_for_profile(&profile), Some(binary));

        std::fs::remove_file(&compatibility).unwrap();
        let raw = std::ffi::CString::new(compatibility.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(raw.as_ptr(), 0o600) }, 0);
        assert_eq!(zen_binary_for_profile(&profile), None);
    }

    #[test]
    fn zen_compatibility_rejects_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        let file = std::fs::File::create(profile.join("compatibility.ini")).unwrap();
        file.set_len(MAX_ZEN_COMPATIBILITY_BYTES + 1).unwrap();
        assert_eq!(zen_binary_for_profile(&profile), None);
    }

    #[test]
    fn zen_launch_is_pinned_to_the_existing_profile() {
        let profile = std::path::Path::new("/tmp/existing zen profile");
        let args = zen_oauth_args(profile, "https://example.com/oauth");
        assert_eq!(
            args,
            vec![
                std::ffi::OsString::from("-profile"),
                profile.as_os_str().to_owned(),
                std::ffi::OsString::from("-new-tab"),
                std::ffi::OsString::from("https://example.com/oauth"),
            ]
        );
    }
}

pub(super) fn wait_or_prompt_for_oauth_code(
    redirect_uri: &str,
    expected_state: &str,
    timeout_secs: u64,
) -> Result<(String, String)> {
    match crate::auth::oauth_server::wait_for_oauth_code_on(
        redirect_uri,
        std::time::Duration::from_secs(timeout_secs),
    ) {
        Ok(pair) => Ok(pair),
        Err(_) => {
            let raw = Input::<String>::new()
                .with_prompt("  Paste the authorization code (or full redirect URL)")
                .allow_empty(false)
                .interact_text()
                .context("OAuth input cancelled")?;
            if raw.len() > 72 * 1024 {
                anyhow::bail!("OAuth code/redirect input is oversized");
            }
            if raw.starts_with("http://") || raw.starts_with("https://") {
                let parsed = Url::parse(&raw).context("Invalid redirect URL")?;
                let code = parsed
                    .query_pairs()
                    .find_map(|(k, v)| (k == "code").then(|| v.to_string()))
                    .context("Missing code in redirect URL")?;
                let state = parsed
                    .query_pairs()
                    .find_map(|(k, v)| (k == "state").then(|| v.to_string()))
                    .unwrap_or_default();
                Ok((code, state))
            } else {
                Ok((raw, expected_state.to_string()))
            }
        }
    }
}

pub(super) fn read_env_first(keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        std::env::var(key)
            .ok()
            .filter(|value| !value.trim().is_empty())
    })
}

pub(super) fn fetch_google_email(access: &str) -> Result<Option<String>> {
    #[derive(Deserialize)]
    struct UserInfo {
        email: Option<String>,
    }
    let resp = super::auth::auth_http_client()?
        .get("https://www.googleapis.com/oauth2/v1/userinfo?alt=json")
        .bearer_auth(access)
        .send()
        .context("Google userinfo request failed")?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let info: UserInfo = super::auth::parse_auth_json(resp, "Google userinfo response")?;
    Ok(info.email.filter(|email| email.len() <= 4 * 1024))
}

pub(super) fn random_hex(bytes: usize) -> String {
    let raw: Vec<u8> = (0..bytes).map(|_| rand::random::<u8>()).collect();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
}

#[cfg(test)]
mod llm_map_block_tests {
    use super::*;

    /// The efforts/agent_models patcher and the loader are two halves of one
    /// contract: whatever the wizard writes must load back identically.
    #[test]
    fn efforts_and_agent_models_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[profile]\nname = \"t\"\n\n[profile.llm]\nprovider = \"ollama\"\nmodel = \"m\"\n\n[profile.browser]\nsource = \"chrome\"\n",
        )
        .unwrap();
        let mut efforts = std::collections::BTreeMap::new();
        efforts.insert("orchestrator".to_string(), "xhigh".to_string());
        efforts.insert("coder".to_string(), "high".to_string());
        let mut agent_models = std::collections::BTreeMap::new();
        agent_models.insert("coder".to_string(), "big-coder".to_string());
        patch_llm_map_block(&path, "efforts", &efforts).unwrap();
        patch_llm_map_block(&path, "agent_models", &agent_models).unwrap();
        // Patch twice — replace, not duplicate.
        patch_llm_map_block(&path, "efforts", &efforts).unwrap();

        let config = crate::config::ConfigLoader::new()
            .with_path(path.clone())
            .load()
            .unwrap();
        assert_eq!(config.profile.llm.efforts, efforts);
        assert_eq!(config.profile.llm.agent_models, agent_models);
        assert_eq!(
            config.profile.llm.effort_for("orchestrator").as_deref(),
            Some("xhigh")
        );
        assert_eq!(
            config.profile.llm.agent_effort("coder").as_deref(),
            Some("high")
        );
        assert_eq!(config.profile.llm.agent_model("coder"), "big-coder");
        // Unset agents ride the specialist → main model.
        assert_eq!(config.profile.llm.agent_model("tester"), "m");
        // Other sections untouched.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("[profile.browser]"));

        // Empty map removes the table.
        patch_llm_map_block(&path, "efforts", &Default::default()).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("[profile.llm.efforts]"));
        assert!(raw.contains("[profile.llm.agent_models]"));
    }
}

#[cfg(test)]
mod mcp_block_tests {

    use super::*;
    use crate::config::McpServerConfig;

    fn server(name: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            command: String::new(),
            args: Vec::new(),
            cwd: None,
            env: Default::default(),
            url: None,
            headers: Default::default(),
            route: None,
            enabled: true,
            description: None,
        }
    }

    /// patch → load round-trip: local + remote entries survive a rewrite and
    /// other sections stay verbatim.
    #[test]
    fn patch_mcp_blocks_round_trips_through_the_loader() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[profile]\nname = \"t\"\n\n[profile.llm]\nprovider = \"ollama\"\nmodel = \"m\"\n",
        )
        .unwrap();

        let mut local = server("t3mp3st");
        local.command = "node".into();
        local.args = vec!["dist/index.js".into(), "--flag".into()];
        local.cwd = Some("/opt/t3mp3st".into());
        local.env.insert("API_KEY".into(), "with \"quotes\"".into());
        local.route = Some("hacker".into());
        local.enabled = false;
        let mut remote = server("ctx");
        remote.url = Some("https://example.com/mcp".into());
        remote
            .headers
            .insert("Authorization".into(), "Bearer tok".into());
        remote.description = Some("remote test server".into());

        patch_mcp_server_blocks(&path, &[local.clone(), remote.clone()]).unwrap();
        // Patch twice — the second write must replace, not duplicate.
        patch_mcp_server_blocks(&path, &[local.clone(), remote.clone()]).unwrap();

        let cfg = crate::config::ConfigLoader::new()
            .with_path(path.clone())
            .load()
            .unwrap();
        assert_eq!(cfg.profile.llm.provider, "ollama");
        assert_eq!(cfg.profile.mcp_servers, vec![local, remote]);

        // Removal: an empty registry leaves no mcp_server blocks behind.
        patch_mcp_server_blocks(&path, &[]).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(!contents.contains("mcp_server"));
        assert!(contents.contains("[profile.llm]"));
    }
}
