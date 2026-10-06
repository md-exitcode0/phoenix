//! `phoenix configure` → Configure LLMs: the profile list and its verbs.
//!
//! A profile = one provider account + per-lane picks (model + effort). The
//! PRIMARY profile lives in config.toml (`[profile.llm]`); every other profile
//! is a stored login in
//! `~/.phoenix/auth-profiles.json` slotted into per-lane fallback chains at
//! an explicit, reorderable position. Four Codex accounts on Phoenix = it
//! keeps answering until all four run out; the runtime rotates, never asks.

use dialoguer::MultiSelect;

use super::profile_wizard::{self, LanePick};
use super::*;
use crate::config::auth_profile::AuthProfileStore;
use crate::config::{FallbackChains, WebFallbackChains};

/// The provider a capability is currently configured to use, as a plain id
/// ("tavily"), for mismatch warnings when assigning fallback keys.
fn configured_web_provider(capability: &str) -> Option<String> {
    let config = PhoenixConfig::load().ok()?;
    let id = match capability {
        "search" => match config.profile.search?.provider {
            crate::config::SearchProvider::Tavily => "tavily",
            crate::config::SearchProvider::Exa => "exa",
            crate::config::SearchProvider::Brave => "brave",
            crate::config::SearchProvider::Serper => "serper",
            crate::config::SearchProvider::SerpAPI => "serpapi",
            crate::config::SearchProvider::Firecrawl => "firecrawl",
            _ => return None,
        },
        "crawl" => match config.profile.crawl?.provider {
            crate::config::CrawlProvider::Firecrawl => "firecrawl",
            crate::config::CrawlProvider::Tavily => "tavily",
            _ => return None,
        },
        "scrape" => match config.profile.scrape?.provider {
            crate::config::ScrapeProvider::Firecrawl => "firecrawl",
            crate::config::ScrapeProvider::Tavily => "tavily",
            _ => return None,
        },
        _ => return None,
    };
    Some(id.to_string())
}

/// Warn when a key is being chained onto a capability that runs a DIFFERENT
/// provider — the key would only ever produce auth errors there.
fn warn_on_capability_mismatch(profile_provider: &str, capability: &str) {
    let core = profile_provider
        .split(':')
        .next_back()
        .unwrap_or(profile_provider);
    if let Some(configured) = configured_web_provider(capability) {
        if configured != core {
            println!(
                "  {} {capability} is configured for `{configured}`, but this key is for `{core}` — it will fail there and just get skipped.",
                style("⚠").yellow()
            );
        }
    }
}

/// True when a stored provider label is a web key, whatever convention wrote
/// it: the profiles menu stores plain ids ("tavily"), onboarding stores
/// capability-prefixed ones ("search:tavily"). Both are web, never LLM.
fn is_web_provider_label(label: &str) -> bool {
    let core = label.split(':').next_back().unwrap_or(label);
    label.starts_with("search:")
        || label.starts_with("crawl:")
        || label.starts_with("scrape:")
        || WEB_PROVIDERS.iter().any(|(id, _)| *id == core)
}

pub(super) const WEB_PROVIDERS: [(&str, &str); 6] = [
    ("tavily", "Tavily — search / extract / crawl"),
    ("firecrawl", "Firecrawl — search / scrape / crawl"),
    ("exa", "Exa — semantic search"),
    ("brave", "Brave Search"),
    ("serper", "Serper — Google results"),
    ("serpapi", "SerpAPI"),
];

/// `phoenix configure` → Configure LLMs.
pub(super) fn run_llm_menu(theme: &ColorfulTheme, config_path: &std::path::Path) -> Result<()> {
    loop {
        let store = load_auth_profile_store()?;
        let (chains, web_chains) = current_chains()?;
        println!();
        print_profile_list(&store, &chains, &web_chains);
        let actions = [
            "Add profile             log in once, pick its agents, models, efforts",
            "Remove profile          delete the login and unhook it everywhere",
            "Reconfigure profile     models/efforts · fallback order · unhook lanes",
            "Switch primary provider change the main login everything answers on first",
            "Advanced role overrides one role on a different provider · memory graph",
            "Compaction policy       choose native-preferred or Phoenix-only per provider",
            "← Back",
        ];
        let pick = match Select::with_theme(theme)
            .with_prompt("Configure LLMs")
            .items(&actions)
            .default(0)
            .interact()
        {
            Ok(pick) => pick,
            Err(_) => return Ok(()),
        };
        let outcome = match pick {
            0 => add_llm_profile(theme, config_path),
            1 => remove_profile(theme, config_path, ProfileKind::Llm),
            2 => reconfigure_profile(theme, config_path),
            3 => super::configure::configure_provider_quick_switch(theme, config_path),
            4 => super::configure::run_roles_menu(theme, config_path),
            5 => super::configure::configure_compaction_mode(theme, config_path),
            _ => return Ok(()),
        };
        // Esc anywhere inside a flow = back to this menu, never out of
        // configure entirely; real errors still surface.
        if let Err(error) = outcome {
            if is_cancel(&error) {
                println!("  {}", style("cancelled — back to Configure LLMs").dim());
            } else {
                return Err(error);
            }
        }
    }
}

/// The chains as currently written in config.toml (empty when unparseable).
pub(super) fn current_chains() -> Result<(FallbackChains, WebFallbackChains)> {
    PhoenixConfig::load()
        .map(|config| (config.profile.llm.fallback, config.profile.web_fallback))
        .context("failed to load fallback-chain configuration")
}

/// The profile list: the primary first, then every stored account with its
/// provider, lane assignments, and chain positions.
fn print_profile_list(
    store: &AuthProfileStore,
    chains: &FallbackChains,
    web_chains: &WebFallbackChains,
) {
    if let Ok(config) = PhoenixConfig::load() {
        let llm = &config.profile.llm;
        println!(
            "  {} {}  {}",
            style(format!("{:<28}", "primary")).bold(),
            style(format!("{:<16}", llm.provider)).dim(),
            format!(
                "answers first · {}{} · compaction {}",
                llm.orchestrator(),
                llm.reasoning_effort
                    .as_deref()
                    .map(|e| format!(" @ {e}"))
                    .unwrap_or_default(),
                llm.compaction_mode_for(&llm.provider).as_str(),
            )
        );
    }
    let primary_llm = PhoenixConfig::load()
        .ok()
        .and_then(|c| c.profile.llm.auth.and_then(|a| a.profile));
    let mut ids: Vec<&String> = store.profiles.keys().collect();
    ids.sort();
    for id in ids {
        let provider = crate::config::auth_profile::profile_provider_id(&store.profiles[id]);
        let mut uses = assignment_summary(id, chains, web_chains);
        if primary_llm.as_deref() == Some(id.as_str()) {
            uses.insert(0, "primary login".to_string());
        }
        if let Some(lanes) = store.assignments.get(id) {
            let mut lane_bits: Vec<String> = lanes
                .iter()
                .map(|(lane, a)| {
                    format!(
                        "{lane} {}{}",
                        a.model,
                        a.effort
                            .as_deref()
                            .map(|e| format!(" @ {e}"))
                            .unwrap_or_default()
                    )
                })
                .collect();
            lane_bits.sort();
            uses.extend(lane_bits);
        } else if let Some(model) = store.models.get(id) {
            uses.push(format!("model {model}"));
        }
        let uses = if uses.is_empty() {
            style("unassigned").dim().to_string()
        } else {
            uses.join(" · ")
        };
        println!(
            "  {} {}  {}",
            style(format!("{id:<28}")).bold(),
            style(format!("{provider:<16}")).dim(),
            uses
        );
    }
    if store.profiles.is_empty() {
        println!(
            "  {}",
            style("no extra profiles yet — the primary answers alone").dim()
        );
    }
}

/// Where a profile id appears across the chains ("coder #2").
fn assignment_summary(
    id: &str,
    chains: &FallbackChains,
    web_chains: &WebFallbackChains,
) -> Vec<String> {
    let mut uses = Vec::new();
    let mut chain_use = |name: &str, chain: &[String]| {
        if let Some(pos) = chain.iter().position(|p| p == id) {
            uses.push(format!("{name} #{}", pos + 2)); // primary is #1
        }
    };
    for (label, chain) in chains.role_lanes() {
        chain_use(label, chain);
    }
    for (agent, chain) in &chains.agents {
        chain_use(agent, chain);
    }
    chain_use("search", &web_chains.search);
    chain_use("crawl", &web_chains.crawl);
    chain_use("scrape", &web_chains.scrape);
    uses
}

/// Smallest free `provider:N` id in the store.
fn next_free_id(store: &AuthProfileStore, provider_id: &str) -> String {
    for n in 2..100 {
        let candidate = format!("{provider_id}:{n}");
        if !store.profiles.contains_key(&candidate) {
            return candidate;
        }
    }
    format!("{provider_id}:{}", random_hex(4))
}

/// Add a profile: the SAME wizard as onboarding — provider + fresh login,
/// then agents → model + effort each — stored as a fallback account.
pub(super) fn add_llm_profile(theme: &ColorfulTheme, config_path: &std::path::Path) -> Result<()> {
    let picked = pick_provider_flat(theme)?;
    let (picked, method) = pick_provider_detail_and_auth(theme, &picked, None)?;
    let compaction_mode = super::pick_compaction_mode(theme, picked.id, None)?;
    if method.method_type == "none" {
        println!(
            "  {} {} needs no login — there is no account to add, so there is nothing to fall back to. Fallback profiles only make sense for providers with keys/logins.",
            style("•").yellow(),
            picked.name
        );
        return Ok(());
    }
    // FRESH login always: the whole point is a DIFFERENT account, so stored
    // login / env-key reuse is skipped. A few legacy flows still use a fixed
    // landing id; snapshot it so we can restore it without replacing any
    // unrelated concurrent store updates.
    println!(
        "  {}",
        style("log in with the ADDITIONAL account when the provider asks").dim()
    );
    let before = load_auth_profile_store()?;
    let auth = tokio::task::block_in_place(|| run_auth_flow_fresh(theme, &picked, &method))?;

    let store = load_auth_profile_store()?;
    let credential = match &auth.profile {
        Some(flow_id) => store
            .profiles
            .get(flow_id)
            .cloned()
            .with_context(|| format!("auth flow did not store profile '{flow_id}'"))?,
        None => {
            // Env-based auth: capture the value NOW into a stored key so the
            // fallback chain never depends on the gateway's environment.
            let env_var = auth
                .env_var
                .clone()
                .context("auth flow returned neither a profile nor an env var")?;
            let value = std::env::var(&env_var)
                .with_context(|| format!("{env_var} is not set in this shell"))?;
            AuthProfileCredential::ApiKey {
                provider: picked.id.to_string(),
                key: value.trim().to_string(),
                display_name: Some(format!("{} (from {env_var})", picked.name)),
            }
        }
    };

    // Same-account detection: identical secret under a different name is a
    // fake fallback (zero extra quota) — say so before storing anything.
    if let Ok(new_secret) = crate::config::auth_profile::extract_profile_secret(&credential) {
        let twin = before.profiles.iter().find(|(_, existing)| {
            crate::config::auth_profile::profile_provider_id(existing) == picked.id
                && crate::config::auth_profile::extract_profile_secret(existing)
                    .map(|secret| secret == new_secret)
                    .unwrap_or(false)
        });
        if let Some((twin_id, _)) = twin {
            println!(
                "  {} This credential is IDENTICAL to `{twin_id}` — it is the same account, so it adds no quota as a fallback.",
                style("⚠").yellow()
            );
            if !Confirm::with_theme(theme)
                .with_prompt("  Store it anyway?")
                .default(false)
                .interact()
                .context("Duplicate confirmation cancelled")?
            {
                println!("  {} Nothing stored.", style("•").dim());
                return Ok(());
            }
        }
    }

    let default_id = next_free_id(&store, picked.id);
    let profile_id: String = Input::with_theme(theme)
        .with_prompt("  Profile name")
        .default(default_id)
        .interact_text()
        .context("Profile name entry cancelled")?;
    let profile_id = profile_id.trim().to_string();
    if profile_id.is_empty() {
        anyhow::bail!("profile name cannot be empty");
    }

    let target_was_present = store.profiles.contains_key(&profile_id);
    let captured_credential = serde_json::to_vec(&credential)?;
    let (path, ()) = update_auth_profile_store(|store| {
        if !target_was_present && store.profiles.contains_key(&profile_id) {
            anyhow::bail!(
                "profile '{profile_id}' was added by another process; choose a different name"
            );
        }
        // Only restore a fixed flow slot if it still contains the credential
        // this flow wrote. A second writer may have legitimately updated the
        // same slot while the profile-name prompt was open.
        let restore_flow_slot = auth.profile.as_ref().is_some_and(|flow_id| {
            flow_id != &profile_id
                && store
                    .profiles
                    .get(flow_id)
                    .and_then(|current| serde_json::to_vec(current).ok())
                    .is_some_and(|current| current == captured_credential)
        });
        store.profiles.insert(profile_id.clone(), credential);
        if restore_flow_slot {
            let flow_id = auth.profile.as_ref().expect("checked above");
            match before.profiles.get(flow_id) {
                Some(previous) => {
                    store.profiles.insert(flow_id.clone(), previous.clone());
                }
                None => {
                    store.profiles.remove(flow_id);
                }
            }
        }
        Ok(())
    })?;
    println!(
        "  {} Profile `{profile_id}` stored in {}",
        style("✔").green(),
        style(path.display()).dim()
    );

    // The wizard tail: which agents, then model + effort for each.
    let picks = profile_wizard::run_lane_wizard(theme, &picked, false, false)?;
    apply_fallback_lane_picks(theme, config_path, &picked, &profile_id, &picks)?;
    patch_llm_compaction_mode(config_path, picked.id, compaction_mode)?;
    Ok(())
}

/// Write a fallback profile's picks: store assignments, chain membership,
/// and primary promotion for lanes that have no primary yet.
fn apply_fallback_lane_picks(
    _theme: &ColorfulTheme,
    config_path: &std::path::Path,
    provider: &ProviderModels,
    profile_id: &str,
    picks: &[LanePick],
) -> Result<()> {
    update_auth_profile_store(|store| {
        // Replace this profile's assignments wholesale — reconfigure semantics.
        store.assignments.remove(profile_id);
        profile_wizard::store_lane_assignments(store, profile_id, picks);
        Ok(())
    })?;

    let (mut chains, web_chains) = current_chains()?;
    chains.remove_profile(profile_id);
    for pick in picks {
        profile_wizard::append_to_chain(&mut chains, &pick.lane, profile_id);
    }
    patch_fallback_blocks(config_path, &chains, &web_chains)?;

    // Lanes that are OFF at the primary level come alive through this
    // profile: vision/image need a primary model line to exist at all, and a
    // watcher pick should actually run, not sit behind an indexer default.
    let config = PhoenixConfig::load().ok();
    if let Some(config) = config {
        let llm = &config.profile.llm;
        for pick in picks {
            match pick.lane.as_str() {
                l if l == profile_wizard::LANE_VISION && llm.vision_model.is_none() => {
                    patch_llm_role_lines(
                        config_path,
                        "vision",
                        Some(&pick.model),
                        Some(provider.id),
                    )?;
                    println!(
                        "  {} Vision had no primary — this profile's pick ({}) is now the vision lane.",
                        style("•").dim(),
                        pick.model
                    );
                }
                l if l == profile_wizard::LANE_IMAGE && llm.image_model.is_none() => {
                    patch_llm_role_lines(
                        config_path,
                        "image",
                        Some(&pick.model),
                        Some(provider.id),
                    )?;
                    println!(
                        "  {} Image generation had no primary — this profile's pick ({}) now powers image_gen.",
                        style("•").dim(),
                        pick.model
                    );
                }
                _ => {}
            }
        }
    }

    let lanes: Vec<String> = picks
        .iter()
        .map(|p| profile_wizard::lane_label(&p.lane))
        .collect();
    println!(
        "  {} `{profile_id}` now backs: {}. Restart the gateway to apply.",
        style("✔").green(),
        lanes.join(", ")
    );
    Ok(())
}

/// Add a web account: provider + key, then which capabilities it backs up.
pub(super) fn add_web_profile(theme: &ColorfulTheme, config_path: &std::path::Path) -> Result<()> {
    let labels: Vec<&str> = WEB_PROVIDERS.iter().map(|(_, label)| *label).collect();
    let pick = Select::with_theme(theme)
        .with_prompt("  Web provider")
        .items(&labels)
        .default(0)
        .interact()
        .context("Web provider selection cancelled")?;
    let provider_id = WEB_PROVIDERS[pick].0;

    let key: String = Input::with_theme(theme)
        .with_prompt(format!("  {provider_id} API key"))
        .allow_empty(false)
        .interact_text()
        .context("API key entry cancelled")?;

    let store = load_auth_profile_store()?;
    let default_id = next_free_id(&store, provider_id);
    let profile_id: String = Input::with_theme(theme)
        .with_prompt("  Profile name")
        .default(default_id)
        .interact_text()
        .context("Profile name entry cancelled")?;
    let profile_id = profile_id.trim().to_string();
    if profile_id.is_empty() {
        anyhow::bail!("profile name cannot be empty");
    }
    let target_was_present = store.profiles.contains_key(&profile_id);
    update_auth_profile_store(|store| {
        if !target_was_present && store.profiles.contains_key(&profile_id) {
            anyhow::bail!(
                "profile '{profile_id}' was added by another process; choose a different name"
            );
        }
        store.profiles.insert(
            profile_id.clone(),
            AuthProfileCredential::ApiKey {
                provider: provider_id.to_string(),
                key: key.trim().to_string(),
                display_name: Some(profile_id.clone()),
            },
        );
        Ok(())
    })?;

    let capabilities = ["Search", "Crawl", "Scrape"];
    let picks = MultiSelect::with_theme(theme)
        .with_prompt("  Fallback for which capabilities? (Space toggles, Enter confirms)")
        .items(&capabilities)
        .interact()
        .context("Capability assignment cancelled")?;
    let (chains, mut web_chains) = current_chains()?;
    let mut assigned = Vec::new();
    for pick in picks {
        let (chain, label) = match pick {
            0 => (&mut web_chains.search, "search"),
            1 => (&mut web_chains.crawl, "crawl"),
            _ => (&mut web_chains.scrape, "scrape"),
        };
        warn_on_capability_mismatch(provider_id, label);
        if !chain.iter().any(|p| p == &profile_id) {
            chain.push(profile_id.clone());
        }
        assigned.push(label);
    }
    if assigned.is_empty() {
        println!(
            "  {} Key stored, not assigned — reconfigure it later from this menu.",
            style("•").dim()
        );
        return Ok(());
    }
    patch_fallback_blocks(config_path, &chains, &web_chains)?;
    println!(
        "  {} `{profile_id}` is now a fallback key for: {}. Restart the gateway to apply.",
        style("✔").green(),
        assigned.join(", ")
    );
    Ok(())
}

/// Reconfigure a profile: models/efforts, fallback order, or unhooking.
fn reconfigure_profile(theme: &ColorfulTheme, config_path: &std::path::Path) -> Result<()> {
    let store = load_auth_profile_store()?;
    let Some(target) = pick_profile_or_primary(theme, &store, "Reconfigure which profile?")? else {
        return Ok(());
    };
    match target {
        ProfileTarget::Primary => reconfigure_primary(theme, config_path),
        ProfileTarget::Stored(profile_id) => {
            let provider_id =
                crate::config::auth_profile::profile_provider_id(&store.profiles[&profile_id])
                    .to_string();
            if is_web_provider_label(&provider_id) {
                return reconfigure_web_profile(theme, config_path, &profile_id);
            }
            let actions = [
                "Reconfigure models      re-pick its agents, models, efforts",
                "Change fallback order   move it up/down an agent's chain",
                "Remove from fallbacks   unhook it from lanes (login stays stored)",
                "← Back",
            ];
            let pick = Select::with_theme(theme)
                .with_prompt(format!("  `{profile_id}`"))
                .items(&actions)
                .default(0)
                .interact()
                .context("Reconfigure selection cancelled")?;
            match pick {
                0 => {
                    let provider = providers_data::get_provider(&provider_id)
                        .with_context(|| format!("provider '{provider_id}' left the catalog"))?;
                    let lanes = profile_wizard::pick_profile_lanes(theme, false, false)?;
                    // Pre-fill each lane's picker with this profile's current
                    // model there, so reconfigure edits instead of restarting.
                    let defaults: std::collections::BTreeMap<String, String> = lanes
                        .iter()
                        .filter_map(|lane| {
                            store
                                .assignment_for(&profile_id, lane)
                                .map(|a| (lane.clone(), a.model))
                        })
                        .collect();
                    let picks =
                        profile_wizard::pick_lane_models(theme, &provider, &lanes, &defaults)?;
                    apply_fallback_lane_picks(theme, config_path, &provider, &profile_id, &picks)
                }
                1 => reorder_profile(theme, config_path, &profile_id),
                2 => unhook_profile(theme, config_path, &profile_id),
                _ => Ok(()),
            }
        }
    }
}

/// A web key's reconfigure: capabilities + order only (no models to pick).
fn reconfigure_web_profile(
    theme: &ColorfulTheme,
    config_path: &std::path::Path,
    profile_id: &str,
) -> Result<()> {
    let actions = [
        "Change capabilities     which of search/crawl/scrape it backs",
        "Change fallback order   move it up/down a capability's chain",
        "Remove from fallbacks   unhook it (key stays stored)",
        "← Back",
    ];
    let pick = Select::with_theme(theme)
        .with_prompt(format!("  `{profile_id}` (web key)"))
        .items(&actions)
        .default(0)
        .interact()
        .context("Reconfigure selection cancelled")?;
    match pick {
        0 => {
            let store = load_auth_profile_store()?;
            let provider =
                crate::config::auth_profile::profile_provider_id(&store.profiles[profile_id])
                    .to_string();
            let capabilities = ["Search", "Crawl", "Scrape"];
            let (chains, mut web_chains) = current_chains()?;
            let current = [
                web_chains.search.iter().any(|p| p == profile_id),
                web_chains.crawl.iter().any(|p| p == profile_id),
                web_chains.scrape.iter().any(|p| p == profile_id),
            ];
            let picks = MultiSelect::with_theme(theme)
                .with_prompt("  Which capabilities? (current state pre-checked)")
                .items(&capabilities)
                .defaults(&current)
                .interact()
                .context("Capability assignment cancelled")?;
            for (index, chain) in [
                &mut web_chains.search,
                &mut web_chains.crawl,
                &mut web_chains.scrape,
            ]
            .into_iter()
            .enumerate()
            {
                let wanted = picks.contains(&index);
                let label = ["search", "crawl", "scrape"][index];
                if wanted && !chain.iter().any(|p| p == profile_id) {
                    warn_on_capability_mismatch(&provider, label);
                    chain.push(profile_id.to_string());
                }
                if !wanted {
                    chain.retain(|p| p != profile_id);
                }
            }
            patch_fallback_blocks(config_path, &chains, &web_chains)?;
            println!(
                "  {} Capabilities updated. Restart the gateway to apply.",
                style("✔").green()
            );
            Ok(())
        }
        1 => reorder_profile(theme, config_path, profile_id),
        2 => unhook_profile(theme, config_path, profile_id),
        _ => Ok(()),
    }
}

/// Reconfigure the PRIMARY: rerun the wizard tail against the main provider
/// and patch config.toml lane by lane.
fn reconfigure_primary(theme: &ColorfulTheme, config_path: &std::path::Path) -> Result<()> {
    let config = PhoenixConfig::load().context("no readable config — run `phoenix onboard`")?;
    let llm = config.profile.llm.clone();
    let provider = providers_data::get_provider(&llm.provider)
        .with_context(|| format!("unknown provider '{}' in config", llm.provider))?;
    println!(
        "  {}",
        style("The primary always answers first; this re-picks its agents, models, and efforts. (Switch provider from the Configure LLMs menu.)").dim()
    );
    let lanes = profile_wizard::pick_profile_lanes(theme, true, true)?;
    // Pre-fill with what runs today so Enter-through changes nothing.
    let mut defaults: std::collections::BTreeMap<String, String> = Default::default();
    defaults.insert(profile_wizard::LANE_PHOENIX.into(), llm.orchestrator());
    defaults.insert(profile_wizard::LANE_SPECIALIST.into(), llm.specialist());
    defaults.insert(profile_wizard::LANE_INDEXER.into(), llm.librarian());
    if let Some(v) = &llm.vision_model {
        defaults.insert(profile_wizard::LANE_VISION.into(), v.clone());
    }
    if let Some(v) = &llm.image_model {
        defaults.insert(profile_wizard::LANE_IMAGE.into(), v.clone());
    }
    for (agent, model) in &llm.agent_models {
        defaults.insert(agent.clone(), model.clone());
    }
    let picks = profile_wizard::pick_lane_models(theme, &provider, &lanes, &defaults)?;

    let pick_for = |lane: &str| picks.iter().find(|p| p.lane == lane);
    // Main model line rides the provider patcher (keeps auth + provider).
    if let Some(phoenix) = pick_for(profile_wizard::LANE_PHOENIX) {
        patch_llm_main_model(config_path, &phoenix.model)?;
    }
    // Role lines: picked = set explicitly; unpicked = left alone (they keep
    // riding whatever they ride today).
    if let Some(pick) = pick_for(profile_wizard::LANE_SPECIALIST) {
        patch_llm_role_lines(config_path, "specialist", Some(&pick.model), None)?;
    }
    if let Some(pick) = pick_for(profile_wizard::LANE_INDEXER) {
        patch_llm_role_lines(config_path, "librarian", Some(&pick.model), None)?;
    }
    if let Some(pick) = pick_for(profile_wizard::LANE_VISION) {
        patch_llm_role_lines(
            config_path,
            "vision",
            Some(&pick.model),
            Some(&llm.provider),
        )?;
    }
    if let Some(pick) = pick_for(profile_wizard::LANE_IMAGE) {
        patch_llm_role_lines(config_path, "image", Some(&pick.model), Some(&llm.provider))?;
    }
    // Efforts + individual-agent models: merge picks over the existing maps.
    let mut efforts = llm.efforts.clone();
    let mut agent_models = llm.agent_models.clone();
    for pick in &picks {
        match &pick.effort {
            Some(effort) => {
                efforts.insert(pick.lane.clone(), effort.clone());
            }
            None => {
                efforts.remove(&pick.lane);
            }
        }
        if profile_wizard::is_agent_lane(&pick.lane) {
            agent_models.insert(pick.lane.clone(), pick.model.clone());
        }
    }
    patch_llm_map_block(config_path, "efforts", &efforts)?;
    patch_llm_map_block(config_path, "agent_models", &agent_models)?;
    // The phoenix effort doubles as the legacy global default.
    if let Some(phoenix) = pick_for(profile_wizard::LANE_PHOENIX) {
        patch_llm_reasoning_line(config_path, phoenix.effort.as_deref())?;
    }
    println!(
        "  {} Primary reconfigured. Restart the gateway (`phoenix restart`) to apply.",
        style("✔").green()
    );
    Ok(())
}

/// Move a profile to a different position inside one lane's chain.
fn reorder_profile(
    theme: &ColorfulTheme,
    config_path: &std::path::Path,
    profile_id: &str,
) -> Result<()> {
    let (mut chains, mut web_chains) = current_chains()?;
    // Every lane this profile appears in, as (label, len, position).
    let mut lanes: Vec<(String, usize, usize)> = Vec::new();
    {
        let mut scan = |label: &str, chain: &[String]| {
            if let Some(pos) = chain.iter().position(|p| p == profile_id) {
                lanes.push((label.to_string(), chain.len(), pos));
            }
        };
        for (label, chain) in chains.role_lanes() {
            scan(label, chain);
        }
        for (agent, chain) in &chains.agents {
            scan(agent, chain);
        }
        scan("search", &web_chains.search);
        scan("crawl", &web_chains.crawl);
        scan("scrape", &web_chains.scrape);
    }
    if lanes.is_empty() {
        println!(
            "  {} `{profile_id}` is not in any fallback chain — nothing to reorder.",
            style("•").dim()
        );
        return Ok(());
    }
    let labels: Vec<String> = lanes
        .iter()
        .map(|(label, len, pos)| {
            format!(
                "{label} — currently #{} of {} fallbacks",
                pos + 2, // primary is #1
                len
            )
        })
        .collect();
    let pick = Select::with_theme(theme)
        .with_prompt("  Change order for which agent?")
        .items(&labels)
        .default(0)
        .interact()
        .context("Lane selection cancelled")?;
    let (lane, len, current) = lanes[pick].clone();
    let positions: Vec<String> = (0..len)
        .map(|i| {
            if i == current {
                format!("#{} (current)", i + 2)
            } else {
                format!("#{}", i + 2)
            }
        })
        .collect();
    let target = Select::with_theme(theme)
        .with_prompt(format!(
            "  New position in `{lane}` (the primary always answers at #1)"
        ))
        .items(&positions)
        .default(current)
        .interact()
        .context("Position selection cancelled")?;
    let chain: &mut Vec<String> = match lane.as_str() {
        "search" => &mut web_chains.search,
        "crawl" => &mut web_chains.crawl,
        "scrape" => &mut web_chains.scrape,
        lane => match chains.role_lane_mut(lane) {
            Some(chain) => chain,
            None => chains.agents.get_mut(lane).context("agent lane vanished")?,
        },
    };
    let entry = chain.remove(current);
    chain.insert(target.min(chain.len()), entry);
    patch_fallback_blocks(config_path, &chains, &web_chains)?;
    println!(
        "  {} `{profile_id}` now answers at #{} for {lane}. Restart the gateway to apply.",
        style("✔").green(),
        target + 2
    );
    Ok(())
}

/// Unhook a profile from chosen lanes; the login stays stored.
fn unhook_profile(
    theme: &ColorfulTheme,
    config_path: &std::path::Path,
    profile_id: &str,
) -> Result<()> {
    let (mut chains, mut web_chains) = current_chains()?;
    let mut present: Vec<String> = Vec::new();
    {
        let mut scan = |label: &str, chain: &[String]| {
            if chain.iter().any(|p| p == profile_id) {
                present.push(label.to_string());
            }
        };
        for (label, chain) in chains.role_lanes() {
            scan(label, chain);
        }
        for (agent, chain) in &chains.agents {
            scan(agent, chain);
        }
        scan("search", &web_chains.search);
        scan("crawl", &web_chains.crawl);
        scan("scrape", &web_chains.scrape);
    }
    if present.is_empty() {
        println!(
            "  {} `{profile_id}` is not in any fallback chain.",
            style("•").dim()
        );
        return Ok(());
    }
    let picks = MultiSelect::with_theme(theme)
        .with_prompt("  Remove from which lanes? (Space toggles, Enter confirms)")
        .items(&present)
        .interact()
        .context("Lane selection cancelled")?;
    if picks.is_empty() {
        println!("  {} Nothing changed.", style("•").dim());
        return Ok(());
    }
    for index in &picks {
        let lane = present[*index].as_str();
        match lane {
            "search" => web_chains.search.retain(|p| p != profile_id),
            "crawl" => web_chains.crawl.retain(|p| p != profile_id),
            "scrape" => web_chains.scrape.retain(|p| p != profile_id),
            lane => {
                if let Some(chain) = chains.role_lane_mut(lane) {
                    chain.retain(|p| p != profile_id);
                } else if let Some(chain) = chains.agents.get_mut(lane) {
                    chain.retain(|p| p != profile_id);
                }
            }
        }
    }
    chains.agents.retain(|_, chain| !chain.is_empty());
    patch_fallback_blocks(config_path, &chains, &web_chains)?;
    println!(
        "  {} `{profile_id}` unhooked from {} lane(s). The login stays stored. Restart the gateway to apply.",
        style("✔").green(),
        picks.len()
    );
    Ok(())
}

/// `phoenix configure` → Configure web search: providers + stacked accounts.
pub(super) fn run_web_menu(theme: &ColorfulTheme, config_path: &std::path::Path) -> Result<()> {
    loop {
        let (chains, web_chains) = current_chains()?;
        let store = load_auth_profile_store()?;
        // Status: web keys and where they sit.
        println!();
        let mut shown = false;
        let mut ids: Vec<&String> = store.profiles.keys().collect();
        ids.sort();
        for id in ids {
            let provider = crate::config::auth_profile::profile_provider_id(&store.profiles[id]);
            if !is_web_provider_label(provider) {
                continue;
            }
            let uses = assignment_summary(id, &chains, &web_chains);
            println!(
                "  {} {}  {}",
                style(format!("{id:<28}")).bold(),
                style(format!("{provider:<16}")).dim(),
                if uses.is_empty() {
                    style("unassigned").dim().to_string()
                } else {
                    uses.join(" · ")
                }
            );
            shown = true;
        }
        if !shown {
            println!(
                "  {}",
                style("no extra web accounts yet — the configured keys answer alone").dim()
            );
        }
        let actions = [
            "Switch providers        pick the search / crawl / scrape providers",
            "Add account             stack another key as a fallback (more free quota)",
            "Reconfigure account     capabilities · fallback order · unhook",
            "Remove account          delete a stored web key",
            "← Back",
        ];
        let pick = match Select::with_theme(theme)
            .with_prompt("Configure web search")
            .items(&actions)
            .default(0)
            .interact()
        {
            Ok(pick) => pick,
            Err(_) => return Ok(()),
        };
        let outcome = match pick {
            0 => run_web_provider_setup(theme).and_then(|web| {
                patch_web_blocks(config_path, &web)?;
                println!(
                    "  {} Web providers updated. Restart the gateway to apply.",
                    style("✔").green()
                );
                Ok(())
            }),
            1 => add_web_profile(theme, config_path),
            2 => {
                let store = load_auth_profile_store()?;
                match pick_profile(
                    theme,
                    &store,
                    "Reconfigure which web account?",
                    ProfileKind::Web,
                )? {
                    Some(profile_id) => reconfigure_web_profile(theme, config_path, &profile_id),
                    None => Ok(()),
                }
            }
            3 => remove_profile(theme, config_path, ProfileKind::Web),
            _ => return Ok(()),
        };
        if let Err(error) = outcome {
            if is_cancel(&error) {
                println!("  {}", style("cancelled — back to web search").dim());
            } else {
                return Err(error);
            }
        }
    }
}

/// Delete a profile: out of the store, out of every chain.
fn remove_profile(
    theme: &ColorfulTheme,
    config_path: &std::path::Path,
    kind: ProfileKind,
) -> Result<()> {
    let store = load_auth_profile_store()?;
    let Some(profile_id) = pick_profile(theme, &store, "Remove which profile?", kind)? else {
        return Ok(());
    };
    let original_credential = serde_json::to_vec(
        store
            .profiles
            .get(&profile_id)
            .with_context(|| format!("profile '{profile_id}' disappeared"))?,
    )?;
    let primary_llm = PhoenixConfig::load()
        .ok()
        .and_then(|c| c.profile.llm.auth.and_then(|a| a.profile));
    if primary_llm.as_deref() == Some(profile_id.as_str()) {
        println!(
            "  {} `{profile_id}` is the PRIMARY login in config.toml — removing it breaks the main provider until you re-auth.",
            style("⚠").yellow()
        );
    }
    if !Confirm::with_theme(theme)
        .with_prompt(format!("  Delete `{profile_id}` and unhook it everywhere?"))
        .default(false)
        .interact()
        .context("Removal confirmation cancelled")?
    {
        println!("  {} Nothing removed.", style("•").dim());
        return Ok(());
    }
    update_auth_profile_store(|store| {
        let unchanged = store
            .profiles
            .get(&profile_id)
            .and_then(|credential| serde_json::to_vec(credential).ok())
            .is_some_and(|credential| credential == original_credential);
        if !unchanged {
            anyhow::bail!("profile '{profile_id}' changed in another process; retry");
        }
        store.profiles.remove(&profile_id);
        store.models.remove(&profile_id);
        store.assignments.remove(&profile_id);
        store.state.cooldown_until.remove(&profile_id);
        store
            .state
            .last_good
            .retain(|_, value| value != &profile_id);
        Ok(())
    })?;

    let (mut chains, mut web_chains) = current_chains()?;
    chains.remove_profile(&profile_id);
    for chain in [
        &mut web_chains.search,
        &mut web_chains.crawl,
        &mut web_chains.scrape,
    ] {
        chain.retain(|p| p != &profile_id);
    }
    patch_fallback_blocks(config_path, &chains, &web_chains)?;
    println!(
        "  {} `{profile_id}` removed from the store and every fallback chain. Restart the gateway to apply.",
        style("✔").green()
    );
    Ok(())
}

/// What a picker should offer: everything, or only LLM-provider profiles
/// (model pinning is meaningless for a Tavily key).
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProfileKind {
    Any,
    Llm,
    Web,
}

/// A reconfigure target: the config.toml primary, or a stored account.
enum ProfileTarget {
    Primary,
    Stored(String),
}

/// Picker that offers the primary profile first, then every stored account.
fn pick_profile_or_primary(
    theme: &ColorfulTheme,
    store: &AuthProfileStore,
    prompt: &str,
) -> Result<Option<ProfileTarget>> {
    let primary_label = PhoenixConfig::load()
        .map(|c| {
            format!(
                "primary  ({} · {})",
                c.profile.llm.provider,
                c.profile.llm.orchestrator()
            )
        })
        .unwrap_or_else(|_| "primary".to_string());
    let mut ids: Vec<&String> = store.profiles.keys().collect();
    ids.sort();
    let mut labels: Vec<String> = vec![primary_label];
    labels.extend(ids.iter().map(|id| {
        format!(
            "{id}  ({})",
            crate::config::auth_profile::profile_provider_id(&store.profiles[*id])
        )
    }));
    labels.push("← Cancel".to_string());
    let pick = Select::with_theme(theme)
        .with_prompt(format!("  {prompt}"))
        .items(&labels)
        .default(0)
        .interact()
        .context("Profile selection cancelled")?;
    if pick == 0 {
        return Ok(Some(ProfileTarget::Primary));
    }
    if pick > ids.len() {
        return Ok(None);
    }
    Ok(Some(ProfileTarget::Stored(ids[pick - 1].clone())))
}

/// Shared profile picker (sorted ids + provider labels); None = cancelled.
fn pick_profile(
    theme: &ColorfulTheme,
    store: &AuthProfileStore,
    prompt: &str,
    filter: ProfileKind,
) -> Result<Option<String>> {
    let mut ids: Vec<&String> = store
        .profiles
        .iter()
        .filter(|(_, credential)| {
            let provider = crate::config::auth_profile::profile_provider_id(credential);
            let is_web = is_web_provider_label(provider);
            let is_llm = providers_data::get_provider(provider).is_some() && !is_web;
            match filter {
                ProfileKind::Any => true,
                ProfileKind::Llm => is_llm,
                ProfileKind::Web => is_web,
            }
        })
        .map(|(id, _)| id)
        .collect();
    if ids.is_empty() {
        println!(
            "  {}",
            style(match filter {
                ProfileKind::Llm => "no stored LLM profiles (web keys have no models)",
                ProfileKind::Web => "no stored web accounts yet",
                ProfileKind::Any => "no stored profiles",
            })
            .dim()
        );
        return Ok(None);
    }
    ids.sort();
    let mut labels: Vec<String> = ids
        .iter()
        .map(|id| {
            format!(
                "{id}  ({})",
                crate::config::auth_profile::profile_provider_id(&store.profiles[*id])
            )
        })
        .collect();
    labels.push("← Cancel".to_string());
    let pick = Select::with_theme(theme)
        .with_prompt(format!("  {prompt}"))
        .items(&labels)
        .default(0)
        .interact()
        .context("Profile selection cancelled")?;
    if pick >= ids.len() {
        return Ok(None);
    }
    Ok(Some(ids[pick].clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_labels_are_recognized_in_both_store_conventions() {
        // profiles-menu convention: plain provider id
        assert!(is_web_provider_label("tavily"));
        assert!(is_web_provider_label("firecrawl"));
        // onboarding convention: capability-prefixed
        assert!(is_web_provider_label("search:tavily"));
        assert!(is_web_provider_label("scrape:firecrawl"));
        assert!(is_web_provider_label("crawl:tavily"));
        // LLM providers are never web
        assert!(!is_web_provider_label("openai-codex"));
        assert!(!is_web_provider_label("openrouter"));
        assert!(!is_web_provider_label("ollama"));
    }

    #[test]
    fn next_free_id_skips_taken_slots() {
        let mut store = AuthProfileStore::default();
        store.profiles.insert(
            "openai-codex:2".to_string(),
            AuthProfileCredential::ApiKey {
                provider: "openai-codex".to_string(),
                key: "k".to_string(),
                display_name: None,
            },
        );
        assert_eq!(next_free_id(&store, "openai-codex"), "openai-codex:3");
        assert_eq!(next_free_id(&store, "tavily"), "tavily:2");
    }

    #[test]
    fn cancel_detection_matches_dialoguer_aborts() {
        assert!(is_cancel(&anyhow::anyhow!("Profile name entry cancelled")));
        assert!(is_cancel(&anyhow::anyhow!(
            "IO error: operation interrupted"
        )));
        assert!(!is_cancel(&anyhow::anyhow!("Failed to write config")));
    }
}
