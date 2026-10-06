use std::path::PathBuf;

use anyhow::{Context, Result};
use base64::Engine;
use console::style;
use dialoguer::{theme::ColorfulTheme, Confirm, FuzzySelect, Input, Select};
use directories::BaseDirs;
use reqwest::blocking::Client;
use serde::Deserialize;
use url::Url;

use crate::auth::device_code::{login_device_code, DeviceCodeConfig};
use crate::config::auth_profile::{
    load_auth_profile_store, update_auth_profile_store, AuthProfileCredential,
};
use crate::config::{
    format_web_auth_toml_block, store_web_api_key_profile, web_providers_data, CompactionMode,
    LLMProfile, PhoenixConfig, Profile, WebAuthConfig,
};
use crate::providers::providers_data::{self, AuthMethod, ProviderModels};

mod agents;
mod auth;
mod config_io;
mod configure;
mod mcp;
mod profile_wizard;
mod profiles;
mod surfaces;
mod theme;
mod web;

pub use config_io::{config_destination, patch_mcp_server_blocks, write_config_file};
pub use configure::run_configure;
pub use theme::{phoenix_theme, wordmark};

/// Headless browser-OAuth login for subscription providers — `phoenix login
/// <provider>`. Prints an `AUTH_URL=` marker line, opens the browser, waits
/// on the loopback callback, and stores the auth profile. Used by GUI
/// clients (the canvas dashboard) that can't run the interactive TUI.
/// `profile_id: None` = next free slot (`<provider>:default`, then `:2`, `:3`…),
/// so signing in again ADDS an account rather than replacing the first one.
pub fn headless_oauth_login(provider_id: &str, profile_id: Option<&str>) -> anyhow::Result<()> {
    headless_provider_login(provider_id, "oauth", profile_id)
}

/// Native/headless provider login shared by the desktop modal and CLI. Device
/// flows print one `DEVICE_AUTH=<json>` line before polling so a GUI can show
/// the verification code while the credential itself stays in the Rust store.
pub fn headless_provider_login(
    provider_id: &str,
    method_type: &str,
    profile_id: Option<&str>,
) -> anyhow::Result<()> {
    crate::config::ensure_phoenix_home().context("failed to prepare Phoenix home")?;
    let provider = crate::providers::providers_data::get_provider(provider_id)
        .with_context(|| format!("unknown provider '{provider_id}'"))?;
    let method = provider
        .auth_methods
        .iter()
        .find(|method| method.method_type == method_type && method.llm_supported)
        .with_context(|| {
            format!("provider '{provider_id}' has no model login method '{method_type}'")
        })?;
    if !matches!(method_type, "oauth" | "device_code" | "device_code_cn") {
        anyhow::bail!(
            "'{method_type}' is not a native sign-in flow — API-key providers use `phoenix auth set-key <profile-id> <provider>`"
        );
    }
    if let Some(id) = profile_id.map(str::trim).filter(|id| !id.is_empty()) {
        if id.len() > 128
            || !id.starts_with(&format!("{provider_id}:"))
            || id.bytes().any(|byte| {
                !byte.is_ascii_alphanumeric() && !matches!(byte, b':' | b'.' | b'-' | b'_')
            })
        {
            anyhow::bail!("invalid account id '{id}' for provider '{provider_id}'");
        }
    }
    if method_type.starts_with("device_code") {
        return auth::run_device_code_flow_as(
            &provider,
            method,
            profile_id,
            &auth::device_code_notice,
        )
        .map(|_| ());
    }
    match provider_id {
        "xai" | "grok-cli" => auth::run_grok_oauth_flow_as(provider_id, profile_id).map(|_| ()),
        "openai-codex" => auth::run_openai_codex_oauth_flow_as(profile_id).map(|_| ()),
        "google-gemini-cli" => {
            auth::run_google_gemini_cli_oauth_flow_as(profile_id).map(|_| ())
        }
        "chutes" => auth::run_chutes_oauth_flow_as(profile_id).map(|_| ()),
        other => anyhow::bail!(
            "no native login flow for '{other}' with method '{method_type}' — API-key providers use `phoenix auth set-key <profile-id> <provider>`"
        ),
    }
}

/// The id a fresh login/key for this provider would take.
pub fn next_profile_id(provider_id: &str) -> anyhow::Result<String> {
    auth::next_free_profile_id(provider_id)
}

use auth::*;
use config_io::*;
use surfaces::*;
use web::*;

/// Dialoguer aborts (Esc / Ctrl-C inside a prompt) — navigation, not failure:
/// menu loops treat these as "go back one level", never as a crash.
pub(super) fn is_cancel(error: &anyhow::Error) -> bool {
    let text = format!("{error:#}").to_ascii_lowercase();
    text.contains("cancelled") || text.contains("interrupted")
}

/// One numbered stage header — onboarding reads as a walk through Phoenix's
/// anatomy, not a flat wall of prompts.
fn stage(n: u8, total: u8, title: &str, blurb: &str) {
    println!();
    println!(
        "  {} {}",
        style(format!("[{n}/{total}]")).dim(),
        style(title).bold()
    );
    if !blurb.is_empty() {
        for line in blurb.lines() {
            println!("  {}", style(line).dim());
        }
    }
    println!();
}
fn pick_role_provider_and_model(
    theme: &dialoguer::theme::ColorfulTheme,
    main_provider: &ProviderModels,
    role: &str,
    fallback_model: &str,
) -> Result<(Option<String>, String)> {
    let same = Confirm::with_theme(theme)
        .with_prompt(format!(
            "Use {} for the {role} too? (No = pick a different provider — e.g. a cheap/free executor)",
            main_provider.name
        ))
        .default(true)
        .interact()
        .context("Role provider confirmation cancelled")?;
    if same {
        let model = pick_model_with_custom(theme, main_provider, role, fallback_model)?;
        return Ok((None, model));
    }
    let picked = pick_provider_flat(theme)?;
    let (picked, auth_method) = pick_provider_detail_and_auth(theme, &picked, None)?;
    // Auth lands in the machine auth-profile store keyed by provider — the
    // runtime resolves it from there for role providers (config auth section
    // stays bound to the MAIN provider).
    tokio::task::block_in_place(|| run_auth_flow(theme, &picked, &auth_method))?;
    let default_model = providers_data::recommended_model(picked.id);
    let default_model = if default_model.is_empty() {
        fallback_model
    } else {
        default_model
    };
    let model = pick_model_with_custom(theme, &picked, role, default_model)?;
    Ok((Some(picked.id.to_string()), model))
}

pub struct SetupSelections {
    pub provider_id: String,
    pub orchestrator_model: String,
    pub specialist_model: String,
    pub librarian_model: String,
    /// Per-lane reasoning efforts (`[profile.llm.efforts]`).
    pub efforts: std::collections::BTreeMap<String, String>,
    /// Individual-specialist primary models (`[profile.llm.agent_models]`).
    pub agent_models: std::collections::BTreeMap<String, String>,
    /// Provider for specialists when different from the orchestrator's.
    pub specialist_provider: Option<String>,
    /// Provider for the librarian when different from the orchestrator's.
    pub librarian_provider: Option<String>,
    /// Vision sidecar (captions browser/desktop screenshots). None = disabled.
    pub vision_model: Option<String>,
    /// Provider for the vision sidecar when different from the main one.
    pub vision_provider: Option<String>,
    /// Image GENERATION model (the image_gen tool). None = disabled.
    pub image_model: Option<String>,
    /// Provider for image generation when different from the main one.
    pub image_provider: Option<String>,
    /// Reasoning effort for providers that support it (e.g. openai-codex).
    pub reasoning_effort: Option<String>,
    /// Context-window override (e.g. Codex top-plan 1M; catalog default otherwise).
    pub context_window: Option<u64>,
    /// Explicit provider-native compaction policy.  Missing providers stay
    /// Phoenix-only at runtime.
    pub compaction_modes: std::collections::BTreeMap<String, CompactionMode>,
    /// Browser connect-once: "chrome" attaches to the user's own Chrome.
    pub browser_source: Option<String>,
    pub browser_attach_port: Option<u16>,
    /// Custom browser binary (CloakBrowser etc.) carried from existing config.
    pub browser_binary: Option<String>,
    pub browser_extra_args: Option<Vec<String>>,
    /// Port logins from this source browser (Zen/Firefox-family) at session start.
    pub browser_login_source: Option<String>,
    /// Cleanly suspend managed Chromium after this many idle seconds. Zero
    /// disables suspension. Preserve it through every config rewrite.
    pub browser_suspend_after_seconds: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct SetupAuthSelection {
    pub method: String,
    pub source: String,
    pub profile: Option<String>,
    pub env_var: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct WebCapabilitySetup {
    pub provider_id: String,
    pub auth: WebAuthConfig,
}

#[derive(Debug, Clone, Default)]
pub struct WebSetupSelections {
    pub search: Option<WebCapabilitySetup>,
    pub crawl: Option<WebCapabilitySetup>,
    pub scrape: Option<WebCapabilitySetup>,
}

fn pick_model_with_custom(
    theme: &ColorfulTheme,
    provider: &ProviderModels,
    label: &str,
    default: &str,
) -> Result<String> {
    const CUSTOM_ENTRY: &str = "✎ Enter custom model ID…";
    let catalog: Vec<String> = provider.models.iter().map(|m| m.id.to_string()).collect();
    if catalog.is_empty() {
        let custom: String = Input::with_theme(theme)
            .with_prompt(format!("  {label} model ID"))
            .default(default.to_string())
            .interact_text()
            .context(format!("{label} model entry cancelled"))?;
        let custom = custom.trim();
        if custom.is_empty() {
            anyhow::bail!("{label} model ID cannot be empty");
        }
        return Ok(custom.to_string());
    }

    let mut choices: Vec<&str> = catalog.iter().map(String::as_str).collect();
    choices.push(CUSTOM_ENTRY);
    let default_idx = catalog
        .iter()
        .position(|id| id == default)
        .unwrap_or(0)
        .min(choices.len().saturating_sub(1));

    let selection = FuzzySelect::with_theme(theme)
        .with_prompt(format!("  {label} model (type to filter)"))
        .items(&choices)
        .default(default_idx)
        .interact()
        .context(format!("{label} model selection cancelled"))?;

    if choices[selection] == CUSTOM_ENTRY {
        let custom: String = Input::with_theme(theme)
            .with_prompt(format!("  Custom {label} model ID"))
            .interact_text()
            .context(format!("{label} custom model entry cancelled"))?;
        let custom = custom.trim();
        if custom.is_empty() {
            anyhow::bail!("{label} model ID cannot be empty");
        }
        Ok(custom.to_string())
    } else {
        Ok(catalog[selection].clone())
    }
}

/// Ask once per configured provider whether its native context protocol may be
/// used.  Native mode is only a preference: the runtime keeps Phoenix's
/// portable transcript and falls back to it whenever the provider route,
/// account, model, or endpoint cannot honor native compaction.
pub(super) fn pick_compaction_mode(
    theme: &ColorfulTheme,
    provider_id: &str,
    current: Option<CompactionMode>,
) -> Result<CompactionMode> {
    let choices = [
        "Provider-native preferred (portable Phoenix fallback retained)",
        "Phoenix portable only",
    ];
    let default = match current.unwrap_or(CompactionMode::NativePreferred) {
        CompactionMode::NativePreferred => 0,
        CompactionMode::PhoenixOnly => 1,
    };
    let picked = Select::with_theme(theme)
        .with_prompt(format!(
            "  {provider_id} context compaction policy (native support is provider-specific)"
        ))
        .items(&choices)
        .default(default)
        .interact()
        .context("Compaction policy selection cancelled")?;
    Ok(if picked == 0 {
        CompactionMode::NativePreferred
    } else {
        CompactionMode::PhoenixOnly
    })
}

pub async fn run_setup(
    provider_override: Option<String>,
    model_override: Option<String>,
    auth_override: Option<String>,
) -> Result<PathBuf> {
    crate::config::ensure_phoenix_home().context("failed to prepare Phoenix home")?;
    let theme = crate::config::setup::phoenix_theme();
    banner();

    stage(
        1,
        5,
        "Your first profile",
        "A profile is one provider account plus the agents it powers.\nPick the provider, log in once, choose which agents use it, then a\nmodel (and reasoning effort) for each. You will also choose this\nprovider's native-vs-Phoenix compaction policy explicitly. Extra\naccounts and providers stack later as fallbacks: configure → Configure\nLLMs → Add profile.",
    );
    let provider = match provider_override {
        Some(id) => providers_data::get_provider(&id)
            .with_context(|| format!("Unknown provider '{}'", id))?,
        None => pick_provider_flat(&theme)?,
    };

    // Re-bind `provider`: the picker may have switched it via "← Back to provider
    // list", so the rest of setup (auth flow, model defaults, config write) must
    // use what was actually chosen, not the first pick.
    let (provider, auth_method) =
        pick_provider_detail_and_auth(&theme, &provider, auth_override.as_deref())?;
    let auth_selection =
        tokio::task::block_in_place(|| run_auth_flow(&theme, &provider, &auth_method))?;

    // The wizard: which agents ride this profile, then model + effort each.
    let picks = if let Some(model) = model_override {
        // Scripted setup (--model): the override IS the phoenix pick; the
        // team and indexer ride it, matching the old flag's meaning.
        vec![profile_wizard::LanePick {
            lane: profile_wizard::LANE_PHOENIX.to_string(),
            model,
            effort: None,
        }]
    } else {
        profile_wizard::run_lane_wizard(&theme, &provider, true, true)?
    };
    let pick_for =
        |lane: &str| -> Option<&profile_wizard::LanePick> { picks.iter().find(|p| p.lane == lane) };
    let phoenix_pick = pick_for(profile_wizard::LANE_PHOENIX)
        .expect("wizard guarantees a Phoenix pick")
        .clone();
    let orchestrator_model = phoenix_pick.model.clone();
    // Codex context window rides the plan, not the model — still one question.
    let context_window = if provider.id == "openai-codex" {
        let plans = [
            "Plus / Team — 256k context (default)",
            "Pro / Enterprise — 1M context",
        ];
        let picked = Select::with_theme(&theme)
            .with_prompt("  ChatGPT plan (sets the real context window)")
            .items(&plans)
            .default(0)
            .interact()
            .context("Plan selection cancelled")?;
        (picked == 1).then_some(1_000_000)
    } else {
        None
    };
    let mut compaction_modes = std::collections::BTreeMap::new();
    let compaction_mode = pick_compaction_mode(&theme, provider.id, None)?;
    compaction_modes.insert(provider.id.to_string(), compaction_mode);

    // Lane picks → config shape. Unpicked lanes ride the orchestrator
    // (specialist/indexer) or stay off/derived (vision, image, watchers).
    let specialist_model = pick_for(profile_wizard::LANE_SPECIALIST)
        .map(|p| p.model.clone())
        .unwrap_or_else(|| orchestrator_model.clone());
    let librarian_model = pick_for(profile_wizard::LANE_INDEXER)
        .map(|p| p.model.clone())
        .unwrap_or_else(|| orchestrator_model.clone());
    let (specialist_provider, librarian_provider): (Option<String>, Option<String>) = (None, None);
    let vision_model = pick_for(profile_wizard::LANE_VISION).map(|p| p.model.clone());
    let vision_provider = vision_model.is_some().then(|| provider.id.to_string());
    let image_model = pick_for(profile_wizard::LANE_IMAGE).map(|p| p.model.clone());
    let image_provider = image_model.is_some().then(|| provider.id.to_string());
    // Efforts: every lane's explicit pick lands in [profile.llm.efforts];
    // the phoenix effort doubles as the legacy global default.
    let mut efforts: std::collections::BTreeMap<String, String> = Default::default();
    let mut agent_models: std::collections::BTreeMap<String, String> = Default::default();
    for pick in &picks {
        if let Some(effort) = &pick.effort {
            efforts.insert(pick.lane.clone(), effort.clone());
        }
        if profile_wizard::is_agent_lane(&pick.lane) {
            agent_models.insert(pick.lane.clone(), pick.model.clone());
        }
    }
    let reasoning_effort = phoenix_pick.effort.clone();

    stage(
        2,
        5,
        "Your apps",
        "Composio connects Gmail, GitHub, Slack, Notion and friends as\nauthenticated API lanes — the fast alternative to browser clicking.",
    );
    // Composio For You (optional): the user's connected apps — Gmail, GitHub,
    // Slack, Notion, X, Reddit, Tavily — reached over the managed MCP server
    // with the CONSUMER key (`ck_`). This is where apps connected on the
    // dashboard actually live. The Platform REST key (`ak_`,
    // ~/.phoenix/composio.key) is a separate, legacy path Phoenix keeps only as
    // a fallback and no longer onboards here.
    let composio_key_path = crate::config::phoenix_home().join("composio-mcp.key");
    let env_composio = std::env::var("COMPOSIO_MCP_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty());
    if let Some(key) = &env_composio {
        crate::tools::composio::validate_consumer_key(key)
            .context("COMPOSIO_MCP_KEY is invalid")?;
    }
    let file_composio = crate::tools::composio::load_consumer_key_file(&composio_key_path)
        .with_context(|| {
            format!(
                "stored Composio key at {} is unreadable",
                composio_key_path.display()
            )
        })?;
    // (status text for the summary, key to persist if newly entered)
    let (composio_status, composio_key): (&str, Option<String>) = if let Some(env_key) =
        env_composio.filter(|_| file_composio.is_none())
    {
        println!();
        println!(
            "  {} Composio For You: COMPOSIO_MCP_KEY found in your environment — persisting to {} so the gateway daemon sees it too",
            style("✔").green(),
            composio_key_path.display()
        );
        // The env var only reaches the gateway when it is started from a
        // shell that sourced the user's profile; a detached daemon would see
        // no key. The file is the context-proof location.
        (
            "connected — Composio For You (env, persisted)",
            Some(env_key),
        )
    } else if file_composio.is_some() {
        println!();
        println!(
            "  {} Composio For You: already configured ({})",
            style("✔").green(),
            composio_key_path.display()
        );
        ("connected — Composio For You", None)
    } else {
        println!();
        let entered: String = Input::with_theme(&theme)
            .with_prompt("  Composio For You — paste your X-CONSUMER-API-KEY (starts with `ck_`) from the dashboard MCP panel; connects your apps: Gmail/GitHub/Slack/Tavily/Reddit… (Enter to skip)")
            .allow_empty(true)
            .interact_text()
            .context("Composio key entry cancelled")?;
        let trimmed = entered.trim().to_string();
        if trimmed.is_empty() {
            ("skipped — add later: ~/.phoenix/composio-mcp.key", None)
        } else {
            crate::tools::composio::validate_consumer_key(&trimmed)?;
            if trimmed.starts_with("ak_") {
                println!(
                    "  {} That looks like a Platform key (ak_), not a For You consumer key (ck_) — your connected apps live under For You. Storing it, but if apps don't appear grab the ck_ key from the dashboard MCP panel.",
                    style("!").yellow()
                );
            }
            ("connected — Composio For You (key stored)", Some(trimmed))
        }
    };

    // The review card: dim labels, bright values, one quiet frame — the
    // moment to scan everything before it becomes config.
    let row = |label: &str, value: &str| {
        println!(
            "  {} {}",
            style(format!("{label:<13}")).dim(),
            style(value).bold()
        );
    };
    let role_line = |model: &str, provider_id: &Option<String>| -> String {
        match provider_id {
            Some(id) => format!("{model}  (via {id})"),
            None => model.to_string(),
        }
    };
    println!();
    println!("  {}", style("─".repeat(64)).black().bright());
    row("provider", provider.name);
    row("orchestrator", &orchestrator_model);
    row(
        "specialists",
        &role_line(&specialist_model, &specialist_provider),
    );
    row(
        "librarian",
        &role_line(&librarian_model, &librarian_provider),
    );
    if let Some(vision) = &vision_model {
        row("vision", &role_line(vision, &vision_provider));
    }
    if let Some(image) = &image_model {
        row("image", &role_line(image, &image_provider));
    }
    for (agent, model) in &agent_models {
        row(agent, model);
    }
    row("composio", composio_status);
    if let Some(effort) = &reasoning_effort {
        row("reasoning", effort);
    }
    row("compaction", compaction_mode.as_str());
    println!("  {}", style("─".repeat(64)).black().bright());
    println!();

    let config_preview = build_runtime_config(
        &provider,
        &orchestrator_model,
        &specialist_model,
        &librarian_model,
        &auth_selection,
    );
    if should_run_provider_check(&auth_selection) {
        match crate::config::check::run_check(&config_preview).await {
            Ok(_) => {}
            Err(error) => {
                let detail = error.to_string();
                if crate::config::check::is_openrouter_free_tier_rate_limit(&detail) {
                    println!();
                    println!(
                        "  {} {}",
                        style("⚠").yellow(),
                        style(
                            "OpenRouter rate-limited this free model on shared upstream capacity."
                        )
                        .yellow()
                    );
                    println!(
                        "    Your OPENROUTER_API_KEY is in use (OpenRouter returned is_byok=false, not a missing key)."
                    );
                    println!(
                        "    Add upstream keys at https://openrouter.ai/settings/integrations, retry later, or pick another model."
                    );
                    println!();
                    if !Confirm::with_theme(&theme)
                        .with_prompt("Continue setup without a successful verification ping?")
                        .default(true)
                        .interact()
                        .context("Confirmation cancelled")?
                    {
                        anyhow::bail!("Setup cancelled after provider rate limit");
                    }
                } else {
                    return Err(error).context("Provider verification failed during onboarding");
                }
            }
        }
    } else {
        println!(
            "  {} Skipping direct provider ping for {} auth; OAuth setup was completed separately.",
            style("•").yellow(),
            auth_selection.method
        );
    }

    stage(
        3,
        5,
        "Web senses",
        "Search, crawl, and scrape providers — how the team reads the live web.\nExtra keys stack later: configure → Configure web search → Add account.",
    );
    let web_selections = run_web_provider_setup(&theme)?;

    stage(
        4,
        5,
        "Browser",
        "Phoenix drives a real Chrome. Attach your own (your logins) or let it\nmanage a fresh profile.",
    );
    let (
        browser_source,
        browser_attach_port,
        browser_binary,
        browser_extra_args,
        browser_login_source,
        browser_suspend_after_seconds,
    ) = run_browser_setup(&theme)?;

    stage(
        5,
        5,
        "Write it",
        "The review card above becomes ~/.phoenix/config.toml.",
    );
    if !Confirm::with_theme(&theme)
        .with_prompt("Write this configuration?")
        .default(true)
        .interact()
        .context("Confirmation cancelled")?
    {
        anyhow::bail!("Setup cancelled by user");
    }

    let destination = config_destination()?;
    write_config_file(
        &destination,
        &SetupSelections {
            provider_id: provider.id.to_string(),
            orchestrator_model,
            specialist_model,
            librarian_model,
            efforts,
            agent_models,
            specialist_provider,
            librarian_provider,
            vision_model,
            vision_provider,
            image_model,
            image_provider,
            reasoning_effort,
            context_window,
            compaction_modes,
            browser_source,
            browser_attach_port,
            browser_binary,
            browser_extra_args,
            browser_login_source,
            browser_suspend_after_seconds,
        },
        &auth_selection,
        &web_selections,
    )?;

    println!(
        "  {} Wrote config to {}",
        style("✔").green(),
        style(destination.display()).dim()
    );

    if let Some(key) = &composio_key {
        let key_path = crate::tools::composio::store_consumer_key(key)?;
        println!(
            "  {} Composio key stored at {}",
            style("✔").green(),
            style(key_path.display()).dim()
        );
    }

    // Profiles stack: offer the second account right here — the multi-account
    // user shouldn't have to discover `configure` first.
    while Confirm::with_theme(&theme)
        .with_prompt(
            "Add another profile now? (a second account or provider — extra quota, cheaper lanes, rotated automatically)",
        )
        .default(false)
        .interact()
        .unwrap_or(false)
    {
        if let Err(error) = profiles::add_llm_profile(&theme, &destination) {
            if is_cancel(&error) {
                println!("  {}", style("cancelled — you can add profiles any time via `phoenix configure`").dim());
                break;
            }
            return Err(error);
        }
    }

    run_desktop_bridge_setup(&theme)?;
    print_finish_summary();
    Ok(destination)
}

/// Browser connect-once: agents drive a real browser. Attaching to the user's
/// own Chrome (their profile, their logins) is the browser-use-grade setup;
/// the managed profile is the zero-touch default.
/// Binary, extra args, and idle suspension are carried through from existing
/// config even where the terminal wizard does not expose an editor for them.
type BrowserSetup = (
    Option<String>,
    Option<u16>,
    Option<String>,
    Option<Vec<String>>,
    Option<String>,
    Option<u64>,
);

/// Ask which browser to port logins from. Phoenix always drives Chrome; this
/// copies a Firefox-family browser's logged-in sessions into it, fresh each run.

fn print_finish_summary() {
    let cmd = |name: &str, blurb: &str| {
        println!(
            "    {}  {}",
            style(format!("{name:<15}")).color256(208).bold(),
            style(blurb).dim()
        );
    };
    println!();
    println!("{}", crate::config::setup::wordmark("ready"));
    println!();
    cmd(
        "phoenix",
        "boots the gateway — keep it running in its own terminal",
    );
    cmd("phoenix start", "opens the agent TUI");
    cmd("phoenix check", "verifies provider connectivity any time");
    println!();
}

fn banner() {
    println!();
    println!("{}", crate::config::setup::wordmark("onboarding"));
    println!(
        "  {}",
        style("models · web · browser · apps — arrows move, Enter picks, type to filter").dim()
    );
    println!("  {}", style("─".repeat(64)).black().bright());
    println!();
}

#[cfg(test)]
mod configure_tests {
    use super::*;

    #[test]
    fn strip_removes_only_the_named_section() {
        let doc = "\
[profile.llm]
provider = \"openai\"
model = \"gpt-5\"

[profile.browser]
source = \"chrome\"
attach_port = 9222

[profile.search]
provider = \"tavily\"
";
        let out = strip_toml_section(doc, "profile.browser");
        assert!(!out.contains("[profile.browser]"));
        assert!(!out.contains("attach_port"));
        // Surrounding sections survive verbatim.
        assert!(out.contains("[profile.llm]"));
        assert!(out.contains("model = \"gpt-5\""));
        assert!(out.contains("[profile.search]"));
        assert!(out.contains("provider = \"tavily\""));
    }

    #[test]
    fn strip_handles_section_at_eof() {
        let doc = "[profile.llm]\nmodel = \"x\"\n\n[profile.browser]\nsource = \"phoenix\"\n";
        let out = strip_toml_section(doc, "profile.browser");
        assert!(out.contains("[profile.llm]"));
        assert!(!out.contains("[profile.browser]"));
    }

    #[test]
    fn render_block_emits_login_source_and_omits_none() {
        let with = render_browser_block("chrome", Some(9222), None, None, Some("zen"), Some(600));
        assert!(with.contains("login_source = \"zen\""));
        assert!(with.contains("source = \"chrome\""));
        assert!(with.contains("attach_port = 9222"));
        assert!(with.contains("suspend_after_seconds = 600"));

        let without = render_browser_block("phoenix", None, None, None, Some("none"), None);
        assert!(!without.contains("login_source"));
        let unset = render_browser_block("phoenix", None, None, None, None, None);
        assert!(!unset.contains("login_source"));
    }

    #[test]
    fn patch_replaces_browser_block_in_place() {
        let dir = std::env::temp_dir().join(format!("phoenix-cfg-{}", random_hex(6)));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[profile.llm]\nmodel = \"gpt-5\"\n\n[profile.browser]\nsource = \"phoenix\"\n",
        )
        .unwrap();

        let setup: BrowserSetup = (
            Some("chrome".to_string()),
            Some(9222),
            None,
            None,
            Some("zen".to_string()),
            Some(900),
        );
        patch_browser_block(&path, &setup).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("[profile.llm]"));
        assert!(out.contains("model = \"gpt-5\""));
        assert!(out.contains("source = \"chrome\""));
        assert!(out.contains("login_source = \"zen\""));
        assert!(out.contains("suspend_after_seconds = 900"));
        // Old value gone, not duplicated.
        assert_eq!(out.matches("[profile.browser]").count(), 1);
        assert!(!out.contains("source = \"phoenix\""));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn fallback_blocks_round_trip_through_patch_and_loader() {
        let dir = std::env::temp_dir().join(format!("phoenix-cfg-{}", random_hex(6)));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[profile.llm]\nprovider = \"openai-codex\"\nmodel = \"gpt-5.4\"\n\n[profile.browser]\nsource = \"chrome\"\n",
        )
        .unwrap();

        let mut chains = crate::config::FallbackChains {
            orchestrator: vec!["openai-codex:2".into(), "openai-codex:3".into()],
            specialist: vec!["openai-codex:2".into()],
            vision: vec!["openai-codex:2".into()],
            ..Default::default()
        };
        chains
            .agents
            .insert("coder".into(), vec!["openai-codex:3".into()]);
        let web = crate::config::WebFallbackChains {
            search: vec!["tavily:2".into()],
            crawl: vec![],
            scrape: vec!["tavily:2".into(), "tavily:3".into()],
        };
        patch_fallback_blocks(&path, &chains, &web).unwrap();

        // Loader reads back exactly what the patcher wrote.
        let config = crate::config::ConfigLoader::new()
            .with_path(path.clone())
            .load()
            .unwrap();
        assert_eq!(config.profile.llm.fallback, chains);
        assert_eq!(config.profile.web_fallback, web);
        // Other sections untouched.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("[profile.browser]"));

        // Removing a profile strips it everywhere; empty chains disappear.
        let mut chains = config.profile.llm.fallback;
        chains.remove_profile("openai-codex:3");
        assert!(
            !chains.agents.contains_key("coder"),
            "empty agent chain dropped"
        );
        patch_fallback_blocks(&path, &chains, &crate::config::WebFallbackChains::default())
            .unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("openai-codex:3"));
        assert!(!raw.contains("[profile.web_fallback]"));
        assert!(!raw.contains("[profile.llm.fallback.agents]"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn patch_with_no_source_removes_the_section() {
        let dir = std::env::temp_dir().join(format!("phoenix-cfg-{}", random_hex(6)));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[profile.llm]\nmodel = \"gpt-5\"\n\n[profile.browser]\nsource = \"phoenix\"\n",
        )
        .unwrap();
        let setup: BrowserSetup = (None, None, None, None, None, None);
        patch_browser_block(&path, &setup).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("[profile.llm]"));
        assert!(!out.contains("[profile.browser]"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn headless_native_login_rejects_mismatched_methods_and_profile_ids_before_network() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(dir.path());

        let mismatch =
            headless_provider_login("openai-codex", "oauth", Some("github-copilot:default"))
                .expect_err("cross-provider profile id must be rejected");
        assert!(format!("{mismatch:#}").contains("invalid account id"));

        let nonexistent = headless_provider_login("github-copilot", "oauth", None)
            .expect_err("undeclared login method must be rejected");
        assert!(format!("{nonexistent:#}").contains("no model login method"));

        let api = headless_provider_login("openai", "api", Some("openai:default"))
            .expect_err("API keys must stay on the stdin-based command");
        assert!(format!("{api:#}").contains("not a native sign-in flow"));
    }
}
