//! Web research capability setup (search/fetch providers + keys).

use super::*;

pub(super) fn run_web_provider_setup(theme: &ColorfulTheme) -> Result<WebSetupSelections> {
    println!();
    println!(
        "  {} {}",
        style("◆").color256(208),
        style("Web providers (search / crawl / scrape)").bold()
    );
    println!(
        "  {}",
        style("  Patterns borrowed from Hermes/Harness-style tool catalogs.").dim()
    );
    println!();

    let configure_web = Confirm::with_theme(theme)
        .with_prompt("Configure web search, crawl, and scrape providers now?")
        .default(true)
        .interact()
        .context("Web provider confirmation cancelled")?;

    if !configure_web {
        return Ok(WebSetupSelections::default());
    }

    // Step loop so "← Back" (or Esc) walks to the previous capability
    // instead of cancelling the whole wizard.
    let mut search = None;
    let mut crawl = None;
    let mut scrape = None;
    let mut step: usize = 0;
    while step < 3 {
        let picked = match step {
            0 => pick_web_capability(theme, "search", web_providers_data::SEARCH_PROVIDERS, 1)?,
            1 => pick_web_capability(theme, "crawl", web_providers_data::CRAWL_PROVIDERS, 0)?,
            _ => pick_web_capability(theme, "scrape", web_providers_data::SCRAPE_PROVIDERS, 0)?,
        };
        match picked {
            WebPick::Back => {
                if step == 0 {
                    // Back from the first step = back out of web setup entirely.
                    return Ok(WebSetupSelections::default());
                }
                step -= 1;
            }
            WebPick::Chosen(value) => {
                match step {
                    0 => search = value,
                    1 => crawl = value,
                    _ => scrape = value,
                }
                step += 1;
            }
        }
    }

    Ok(WebSetupSelections {
        search,
        crawl,
        scrape,
    })
}

/// A capability pick, or a request to go back one step.
pub(super) enum WebPick {
    Chosen(Option<WebCapabilitySetup>),
    Back,
}

pub(super) fn pick_web_capability(
    theme: &ColorfulTheme,
    capability: &str,
    providers: &[web_providers_data::WebProviderEntry],
    default_index: usize,
) -> Result<WebPick> {
    let mut labels: Vec<String> = providers
        .iter()
        .map(|p| format!("{} — {}", p.name, p.blurb))
        .collect();
    labels.push("← Back".to_string());
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    // interact_opt: Esc/q returns None — treated as Back, never a hard cancel.
    let Some(idx) = Select::with_theme(theme)
        .with_prompt(format!(
            "  {capability} provider (↑↓ + Enter · Esc or `← Back` to go back)"
        ))
        .items(&label_refs)
        .default(default_index.min(label_refs.len().saturating_sub(1)))
        .interact_opt()
        .context(format!("{capability} provider selection failed"))?
    else {
        return Ok(WebPick::Back);
    };
    if idx == providers.len() {
        return Ok(WebPick::Back);
    }

    let entry = providers[idx];
    if entry.id == "skip" {
        return Ok(WebPick::Chosen(None));
    }
    if entry.id == "duckduckgo" {
        return Ok(WebPick::Chosen(Some(WebCapabilitySetup {
            provider_id: entry.id.to_string(),
            auth: WebAuthConfig {
                source: Some("none".to_string()),
                profile: None,
                env_var: None,
            },
        })));
    }

    if !entry.requires_key {
        return Ok(WebPick::Chosen(Some(WebCapabilitySetup {
            provider_id: entry.id.to_string(),
            auth: WebAuthConfig {
                source: Some("none".to_string()),
                profile: None,
                env_var: None,
            },
        })));
    }

    println!();
    println!(
        "  {} Get a key at {}",
        style("•").dim(),
        style(entry.signup_url).color256(208)
    );
    if !entry.env_var.is_empty() {
        println!(
            "  {} Or set env var {}",
            style("•").dim(),
            style(entry.env_var).yellow()
        );
    }

    let profile_id = web_providers_data::web_profile_id(capability, entry.id);
    let api_key = prompt_web_api_key(theme, &entry, capability)?;

    store_web_api_key_profile(&profile_id, &format!("{capability}:{}", entry.id), &api_key)?;

    Ok(WebPick::Chosen(Some(WebCapabilitySetup {
        provider_id: entry.id.to_string(),
        auth: WebAuthConfig {
            source: Some("profile".to_string()),
            profile: Some(profile_id),
            env_var: if entry.env_var.is_empty() {
                None
            } else {
                Some(entry.env_var.to_string())
            },
        },
    })))
}

/// Web API key entry — mirrors LLM `run_api_key_flow` (visible `Input`, env-var shortcut).
/// `Password::interact()` often blocks paste in integrated terminals (Cursor/VS Code).
pub(super) fn prompt_web_api_key(
    theme: &ColorfulTheme,
    entry: &web_providers_data::WebProviderEntry,
    capability: &str,
) -> Result<String> {
    if !entry.env_var.is_empty() {
        if let Ok(existing) = std::env::var(entry.env_var) {
            let trimmed = existing.trim();
            if !trimmed.is_empty() {
                let prefix: String = trimmed.chars().take(4).collect();
                println!("  Detected env var {} = {}***", entry.env_var, prefix);
                let use_existing = Confirm::with_theme(theme)
                    .with_prompt(format!("Use existing {} from environment?", entry.env_var))
                    .default(true)
                    .interact()
                    .context("Web API key env confirmation cancelled")?;
                if use_existing {
                    return Ok(trimmed.to_string());
                }
            }
        }
    }

    println!(
        "  {}",
        style("Type or paste your API key on the next line (visible input — same as LLM setup).")
            .dim()
    );
    let key = Input::<String>::with_theme(theme)
        .with_prompt(format!("  {} API key", entry.name))
        .allow_empty(false)
        .interact_text()
        .context(format!("{capability} API key input cancelled"))?;
    let key = key.trim().to_string();
    if key.is_empty() {
        anyhow::bail!(
            "{capability} provider {} requires a non-empty API key",
            entry.name
        );
    }
    Ok(key)
}
