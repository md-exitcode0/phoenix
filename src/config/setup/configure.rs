//! `phoenix configure` — post-onboarding settings changes.

use super::*;

pub async fn run_configure() -> Result<()> {
    let theme = crate::config::setup::phoenix_theme();
    let path = config_destination()?;
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!(
                "  {} No config at {} yet — run `phoenix onboard` first.",
                style("•").yellow(),
                style(path.display()).dim()
            );
            return Ok(());
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    }
    println!();
    println!("{}", crate::config::setup::wordmark("configure"));
    println!("  {}", style(path.display()).dim());
    println!("  {}", style("─".repeat(64)).black().bright());
    println!();

    loop {
        // Each entry answers "what is it set to NOW?" before asking anything
        // — the menu is a status screen you can act on, not a blind list.
        // Do not turn malformed/symlinked/oversized config into an "unset"
        // status screen from which another action could overwrite it.
        let cfg = Some(PhoenixConfig::load().context("active config is unreadable")?);
        let llm_now = cfg
            .as_ref()
            .map(|c| format!("{} · {}", c.profile.llm.provider, c.profile.llm.model))
            .unwrap_or_else(|| "unset".to_string());
        let browser_now = cfg
            .as_ref()
            .and_then(|c| c.profile.browser.as_ref())
            .and_then(|b| b.source.clone())
            .unwrap_or_else(|| "phoenix (managed profile)".to_string());
        let composio_path = crate::config::phoenix_home().join("composio-mcp.key");
        let composio_now = match crate::tools::composio::load_consumer_key_file(&composio_path) {
            Ok(Some(_)) => "key stored",
            Ok(None) => "not connected",
            Err(_) => "stored key unreadable",
        };
        let profile_count = match load_auth_profile_store() {
            Ok(store) => store.profiles.len().to_string(),
            Err(_) => "unreadable".to_string(),
        };
        let mcp_now = {
            let servers = cfg
                .as_ref()
                .map(|c| c.profile.mcp_servers.clone())
                .unwrap_or_default();
            let active = servers.iter().filter(|s| s.enabled).count();
            if servers.is_empty() {
                "none registered".to_string()
            } else if active == servers.len() {
                format!("{active} registered")
            } else {
                format!("{active} active · {} parked", servers.len() - active)
            }
        };
        let roles_now = cfg
            .as_ref()
            .map(|c| {
                let llm = &c.profile.llm;
                format!(
                    "spec {} · libr {}{}",
                    llm.specialist(),
                    llm.librarian(),
                    llm.vision_model
                        .as_deref()
                        .map(|v| format!(" · vision {v}"))
                        .unwrap_or_default()
                )
            })
            .unwrap_or_else(|| "unset".to_string());
        let web_now = cfg
            .as_ref()
            .map(|c| {
                let mut lanes: Vec<&str> = Vec::new();
                if c.profile.search.is_some() {
                    lanes.push("search");
                }
                if c.profile.crawl.is_some() {
                    lanes.push("crawl");
                }
                if c.profile.scrape.is_some() {
                    lanes.push("scrape");
                }
                if lanes.is_empty() {
                    "none".to_string()
                } else {
                    lanes.join("+")
                }
            })
            .unwrap_or_else(|| "none".to_string());
        let custom_agent_count = crate::sub_agents::registry::custom_roles_on_disk()
            .context("agent registry is unreadable")?
            .len();
        let sections = [
            format!(
                "Configure LLMs       primary {llm_now} · {profile_count} extra — profiles: add / remove / reconfigure"
            ),
            format!(
                "Agents               {} built-in + {} custom — roster, scaffolds, overrides",
                crate::sub_agents::registry::BUILTIN_ROLES.len(),
                custom_agent_count
            ),
            format!("Configure web search now {web_now} — search / crawl / scrape providers & accounts"),
            format!("Configure browser    now {browser_now} — engine + port-in login source"),
            format!("Composio apps        now {composio_now} — Gmail/GitHub/Slack/… key"),
            format!("MCP servers          now {mcp_now} — connect local/remote tool servers"),
            "Full onboarding      rerun everything from scratch".to_string(),
            "Exit".to_string(),
        ];
        let _ = &roles_now; // roles surface inside Configure LLMs now
        let pick = match Select::with_theme(&theme)
            .with_prompt("Change")
            .items(&sections)
            .default(0)
            .interact()
        {
            Ok(pick) => pick,
            Err(_) => break, // Esc on the menu = done
        };
        let outcome = match pick {
            0 => super::profiles::run_llm_menu(&theme, &path),
            1 => super::agents::run_agents_menu(&theme, &path),
            2 => super::profiles::run_web_menu(&theme, &path),
            3 => run_browser_setup(&theme).and_then(|setup| {
                patch_browser_block(&path, &setup)?;
                println!(
                    "  {} Browser settings updated. Restart the gateway to apply.",
                    style("✔").green()
                );
                Ok(())
            }),
            4 => configure_composio_key(&theme),
            5 => super::mcp::run_mcp_menu(&theme, &path),
            6 => {
                run_setup(None, None, None).await?;
                // Full wizard rewrote everything; nothing more to do here.
                return Ok(());
            }
            _ => break,
        };
        // Esc inside a section = back to this menu, never out of configure.
        if let Err(error) = outcome {
            if is_cancel(&error) {
                println!("  {}", style("cancelled — back to the menu").dim());
            } else {
                return Err(error);
            }
        }
    }
    println!(
        "  {} Done. Restart the gateway (`phoenix restart`) to pick up changes.",
        style("✔").green()
    );
    Ok(())
}

/// Edit the native-compaction policy for the active provider. Provider
/// changes and added fallback accounts prompt separately, but this explicit
/// menu keeps the choice discoverable and reversible after onboarding.
pub(super) fn configure_compaction_mode(
    theme: &ColorfulTheme,
    path: &std::path::Path,
) -> Result<()> {
    let config = PhoenixConfig::load().context("active config is unreadable")?;
    let provider_id = config.profile.llm.provider.clone();
    let current = config.profile.llm.compaction_mode_for(&provider_id);
    let mode = super::pick_compaction_mode(theme, &provider_id, Some(current))?;
    patch_llm_compaction_mode(path, &provider_id, mode)?;
    println!(
        "  {} {} compaction policy set to {}. Restart the gateway to apply.",
        style("✔").green(),
        provider_id,
        mode.as_str()
    );
    Ok(())
}

/// `phoenix configure` → Configure LLMs → Advanced role overrides: change one
/// role's model AND provider without touching anything else.
pub(super) fn run_roles_menu(theme: &ColorfulTheme, path: &std::path::Path) -> Result<()> {
    loop {
        let cfg = PhoenixConfig::load().context("active config is unreadable")?;
        let llm = &cfg.profile.llm;
        let main = providers_data::get_provider(&llm.provider)
            .with_context(|| format!("unknown provider '{}' in config", llm.provider))?;
        let via = |provider: &Option<String>| -> String {
            provider
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .map(|id| format!(" via {id}"))
                .unwrap_or_default()
        };
        let items = [
            format!(
                "Specialists      now {}{} — the hands-on workers",
                llm.specialist(),
                via(&llm.specialist_provider)
            ),
            format!(
                "Librarian        now {}{} — memory digests + compaction (cheap)",
                llm.librarian(),
                via(&llm.librarian_provider)
            ),
            format!(
                "Memory graph     now {}{} — builds the knowledge graph from saved memories",
                llm.memory(),
                if llm.memory_provider.is_some() {
                    format!(" via {}", llm.memory_provider.as_deref().unwrap_or(""))
                } else if llm.memory_model.is_none() {
                    " (rides the librarian)".to_string()
                } else {
                    String::new()
                }
            ),
            format!(
                "Vision           now {} — screenshot captions",
                llm.vision_model.as_deref().unwrap_or("off")
            ),
            "Back".to_string(),
        ];
        let pick = match Select::with_theme(theme)
            .with_prompt("Model roles")
            .items(&items)
            .default(0)
            .interact()
        {
            Ok(pick) => pick,
            Err(_) => return Ok(()),
        };
        match pick {
            0 => {
                let (provider_id, model) = super::pick_role_provider_and_model(
                    theme,
                    &main,
                    "specialist",
                    &llm.orchestrator(),
                )?;
                patch_llm_role_lines(path, "specialist", Some(&model), provider_id.as_deref())?;
                if let Some(provider_id) = provider_id.as_deref() {
                    let mode = super::pick_compaction_mode(theme, provider_id, None)?;
                    patch_llm_compaction_mode(path, provider_id, mode)?;
                }
            }
            1 => {
                let (provider_id, model) = super::pick_role_provider_and_model(
                    theme,
                    &main,
                    "librarian",
                    &llm.orchestrator(),
                )?;
                patch_llm_role_lines(path, "librarian", Some(&model), provider_id.as_deref())?;
                if let Some(provider_id) = provider_id.as_deref() {
                    let mode = super::pick_compaction_mode(theme, provider_id, None)?;
                    patch_llm_compaction_mode(path, provider_id, mode)?;
                }
            }
            2 => {
                let ride = Confirm::with_theme(theme)
                    .with_prompt(format!(
                        "Run memory graph-building on the librarian model ({})? (No = pick a dedicated provider+model for it)",
                        llm.librarian()
                    ))
                    .default(false)
                    .interact()
                    .context("memory lane confirmation cancelled")?;
                if ride {
                    patch_llm_role_lines(path, "memory", None, None)?;
                } else {
                    let (provider_id, model) = super::pick_role_provider_and_model(
                        theme,
                        &main,
                        "memory graph",
                        &llm.librarian(),
                    )?;
                    patch_llm_role_lines(
                        path,
                        "memory",
                        Some(&model),
                        Some(provider_id.as_deref().unwrap_or(&llm.provider)),
                    )?;
                    if let Some(provider_id) = provider_id.as_deref() {
                        let mode = super::pick_compaction_mode(theme, provider_id, None)?;
                        patch_llm_compaction_mode(path, provider_id, mode)?;
                    }
                }
            }
            3 => {
                if llm.vision_model.is_some()
                    && Confirm::with_theme(theme)
                        .with_prompt("Vision is on — turn it OFF? (No = pick a different model)")
                        .default(false)
                        .interact()
                        .context("vision confirmation cancelled")?
                {
                    patch_llm_role_lines(path, "vision", None, None)?;
                } else {
                    let (provider_id, model) = super::pick_role_provider_and_model(
                        theme,
                        &main,
                        "vision",
                        &llm.orchestrator(),
                    )?;
                    patch_llm_role_lines(path, "vision", Some(&model), provider_id.as_deref())?;
                    if let Some(provider_id) = provider_id.as_deref() {
                        let mode = super::pick_compaction_mode(theme, provider_id, None)?;
                        patch_llm_compaction_mode(path, provider_id, mode)?;
                    }
                }
            }
            _ => return Ok(()),
        }
        println!(
            "  {} Role updated. Restart the gateway (`phoenix restart`) to apply.",
            style("✔").green()
        );
    }
}

/// Quick provider/model switch: the SAME pickers and auth flows as onboarding
/// (full catalog, fuzzy filter, stored-auth reuse, API-key paste, device code,
/// OAuth) but patching only `[profile.llm]` provider/model + the auth table —
/// roles, vision, web, and browser settings all stay. Switching between a
/// local Ollama and Ollama Cloud (paste the key once, reuse it after) is the
/// two-answer path this exists for.
pub(super) fn configure_provider_quick_switch(
    theme: &ColorfulTheme,
    path: &std::path::Path,
) -> Result<()> {
    let current_config = PhoenixConfig::load().context("active config is unreadable")?;
    let current = current_config.profile.llm.provider.clone();
    if !current.is_empty() {
        println!(
            "  {}",
            style(format!("  Current provider: {current}")).dim()
        );
    }
    let picked = pick_provider_flat(theme)?;
    let (picked, method) = pick_provider_detail_and_auth(theme, &picked, None)?;
    let auth = tokio::task::block_in_place(|| run_auth_flow(theme, &picked, &method))?;
    let default_model = {
        let recommended = providers_data::recommended_model(picked.id);
        if recommended.is_empty() {
            picked.models.first().map(|m| m.id).unwrap_or("")
        } else {
            recommended
        }
    };
    let model = pick_model_with_custom(theme, &picked, "main", default_model)?;
    let compaction_mode = super::pick_compaction_mode(
        theme,
        picked.id,
        Some(current_config.profile.llm.compaction_mode_for(picked.id)),
    )?;
    patch_llm_provider_block(path, picked.id, &model, &auth)?;
    patch_llm_compaction_mode(path, picked.id, compaction_mode)?;
    println!(
        "  {} Provider switched to {} · {} — roles/vision/web/browser settings kept. Restart the gateway to apply.",
        style("✔").green(),
        style(picked.name).bold(),
        style(&model).bold()
    );
    Ok(())
}

/// Set or clear the Composio For You consumer key (`ck_…`) used by the managed
/// MCP server. Stored at `~/.phoenix/composio-mcp.key`, never in memory.
pub(super) fn configure_composio_key(theme: &ColorfulTheme) -> Result<()> {
    let key_path = crate::config::phoenix_home().join("composio-mcp.key");
    let present = crate::tools::composio::load_consumer_key_file(&key_path)
        .with_context(|| {
            format!(
                "stored Composio key at {} is unreadable",
                key_path.display()
            )
        })?
        .is_some();
    println!(
        "  {}",
        style(if present {
            "  A Composio key is already stored. Enter a new one to replace it, or leave blank to remove."
        } else {
            "  Paste your Composio For You key (starts with `ck_`). Leave blank to skip."
        })
        .dim()
    );
    let entered: String = Input::with_theme(theme)
        .with_prompt("  Composio key")
        .allow_empty(true)
        .interact_text()
        .context("Composio key entry cancelled")?;
    let entered = entered.trim();
    if entered.is_empty() {
        if present {
            if crate::config::private_io::remove_private_file(&key_path)
                .with_context(|| format!("Failed to remove {}", key_path.display()))?
            {
                println!("  {} Composio key removed.", style("✔").green());
            } else {
                println!("  {} Key was already removed.", style("•").dim());
            }
        } else {
            println!("  {} No change.", style("•").dim());
        }
        return Ok(());
    }
    if !entered.starts_with("ck_") {
        println!(
            "  {} That doesn't look like a For You key (expected `ck_…`). Saving anyway.",
            style("⚠").yellow()
        );
    }
    let key_path = crate::tools::composio::store_consumer_key(entered)?;
    println!(
        "  {} Composio key stored at {}",
        style("✔").green(),
        style(key_path.display()).dim()
    );
    Ok(())
}
