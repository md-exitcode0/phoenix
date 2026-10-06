//! The profile wizard — ONE flow for "an account + what it powers".
//!
//! A profile is a provider account plus per-lane assignments (model +
//! reasoning effort). Onboarding runs this wizard to build the PRIMARY
//! profile; `configure` → Configure LLMs → Add profile runs the same wizard
//! for fallback accounts. Same questions, same order, everywhere (plan 018):
//!
//!   provider → "which agents use this profile?" (multi-select, expandable
//!   specialists) → per agent: model → reasoning effort.

use dialoguer::MultiSelect;

use super::*;

/// Chain/effort keys for the lanes a profile can power. `specialist` is the
/// umbrella lane; individual agents use their role name ("coder").
pub(super) const LANE_PHOENIX: &str = "orchestrator";
pub(super) const LANE_SPECIALIST: &str = "specialist";
pub(super) const LANE_INDEXER: &str = "librarian";
pub(super) const LANE_VISION: &str = "vision";
pub(super) const LANE_IMAGE: &str = "image";

/// The built-in specialist roles a profile can be pinned to individually.
pub(super) const SPECIALIST_ROLES: [(&str, &str); 14] = [
    ("scribe", "communication, inbox, calendar, and follow-ups"),
    (
        "planner",
        "projects, operations, dependencies, and work systems",
    ),
    (
        "finance",
        "money, records, subscriptions, and administration",
    ),
    ("coder", "engineering, software, and technical automation"),
    (
        "frontend",
        "product design, interaction, and frontend quality",
    ),
    (
        "researcher",
        "current primary-source research and intelligence",
    ),
    (
        "presentation",
        "documents, reports, decks, and visual narratives",
    ),
    (
        "critic",
        "quality, review, decisions, and realistic verification",
    ),
    ("sales", "commercial relationships, CRM, and pipeline"),
    (
        "marketing",
        "positioning, campaigns, audience, and channels",
    ),
    (
        "personal_logistics",
        "travel, appointments, reservations, and errands",
    ),
    ("database", "hidden data and database expertise mode"),
    ("hacker", "hidden security and privacy expertise mode"),
    ("tester", "hidden focused test-engineering expertise mode"),
];

/// Built-ins + registry customs: every individually pinnable specialist,
/// as (role, blurb) rows.
pub(super) fn all_specialist_roles() -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = SPECIALIST_ROLES
        .iter()
        .map(|(role, blurb)| (role.to_string(), blurb.to_string()))
        .collect();
    rows.extend(crate::sub_agents::registry::custom_roster());
    rows
}

/// One row of the agent multi-select.
struct LaneRow {
    key: &'static str,
    label: String,
    /// Pre-checked in the onboarding run (the recommended core set).
    core: bool,
}

/// The expander pseudo-row: toggling it re-renders the list with the
/// individual specialists spliced in (spec: "the whole list just expands").
const EXPAND_KEY: &str = "__expand__";

fn base_rows() -> Vec<LaneRow> {
    let row = |key, label: String, core| LaneRow { key, label, core };
    vec![
        row(
            LANE_PHOENIX,
            "Phoenix           — the orchestrator: talks to you, plans, routes all work".into(),
            true,
        ),
        row(
            LANE_SPECIALIST,
            "All specialists   — the whole hands-on team: code, browser, research, desktop…".into(),
            true,
        ),
        row(
            EXPAND_KEY,
            "Choose specialists… — expand the list and pick individual teammates".into(),
            false,
        ),
        row(
            LANE_IMAGE,
            "Image model       — image generation for frontend/design work (needs an OpenAI-compatible images API)".into(),
            false,
        ),
        row(
            LANE_VISION,
            "Vision            — captions browser/desktop screenshots so agents see the screen".into(),
            false,
        ),
        row(
            LANE_INDEXER,
            "Indexer           — memory digests + context compaction (cheap/fast is right)".into(),
            true,
        ),
    ]
}

fn specialist_rows() -> Vec<LaneRow> {
    all_specialist_roles()
        .into_iter()
        .map(|(role, blurb)| LaneRow {
            // Custom roles are interned for the process lifetime, so leaking
            // the key matches the &'static str the row structure carries.
            key: Box::leak(role.clone().into_boxed_str()),
            label: format!(
                "  ↳ {:<15} — {blurb}",
                crate::runtime::delegation::agent_display_name(&role)
            ),
            core: false,
        })
        .collect()
}

/// One lane's final pick: which model this profile runs there, at what effort.
#[derive(Debug, Clone)]
pub(super) struct LanePick {
    pub lane: String,
    pub model: String,
    pub effort: Option<String>,
}

/// Ask "which agents use this profile?" — multi-select with descriptions and
/// an in-place expandable specialist list. `recommended_defaults` pre-checks
/// the core set (onboarding); add-profile starts unchecked.
pub(super) fn pick_profile_lanes(
    theme: &ColorfulTheme,
    recommended_defaults: bool,
    require_phoenix: bool,
) -> Result<Vec<String>> {
    let mut rows = base_rows();
    let mut checked: Vec<bool> = rows
        .iter()
        .map(|r| recommended_defaults && r.core)
        .collect();
    loop {
        let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        let defaults: Vec<bool> = checked.clone();
        let picks = MultiSelect::with_theme(theme)
            .with_prompt(
                "Which agents should use this profile? (Space toggles, Enter confirms — select all that apply)",
            )
            .items(&labels)
            .defaults(&defaults)
            .interact()
            .context("Agent selection cancelled")?;
        let picked: Vec<&str> = picks.iter().map(|i| rows[*i].key).collect();
        if picked.contains(&EXPAND_KEY) {
            // Expand: same list, the expander row replaced by the individual
            // specialists, previous picks kept checked.
            let expanded_at = rows
                .iter()
                .position(|r| r.key == EXPAND_KEY)
                .expect("expander row");
            let mut next_rows: Vec<LaneRow> = Vec::new();
            let mut next_checked: Vec<bool> = Vec::new();
            for (index, row) in rows.into_iter().enumerate() {
                if row.key == EXPAND_KEY {
                    for s in specialist_rows() {
                        next_rows.push(s);
                        next_checked.push(false);
                    }
                } else {
                    next_checked.push(picks.contains(&index));
                    next_rows.push(row);
                }
            }
            let _ = expanded_at;
            rows = next_rows;
            checked = next_checked;
            continue;
        }
        let mut lanes: Vec<String> = picked
            .iter()
            .filter(|k| **k != EXPAND_KEY)
            .map(|k| k.to_string())
            .collect();
        // Individual picks are redundant under the umbrella — an agent lane
        // only means something as its OWN chain/model, so keep both only when
        // the user actually diverges (all specialists on X, coder on Y).
        if lanes.contains(&LANE_SPECIALIST.to_string()) {
            let individual: Vec<&String> = lanes
                .iter()
                .filter(|l| SPECIALIST_ROLES.iter().any(|(r, _)| *r == l.as_str()))
                .collect();
            if !individual.is_empty() {
                println!(
                    "  {} {} selected individually AND under \"All specialists\" — the individual pick wins for those agents (their own model/chain).",
                    style("•").dim(),
                    individual
                        .iter()
                        .map(|l| l.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        if lanes.is_empty() {
            println!(
                "  {} Nothing selected — pick at least one agent (Space toggles, Enter confirms).",
                style("!").yellow()
            );
            continue;
        }
        if require_phoenix && !lanes.iter().any(|l| l == LANE_PHOENIX) {
            println!(
                "  {} The first profile must cover Phoenix — the orchestrator always needs a model. Select it (Space) and confirm again.",
                style("!").yellow()
            );
            continue;
        }
        // Ask in a stable, sensible order: phoenix first, then the rest as
        // listed (image right after phoenix, per the spec's walk-through).
        let order = [
            LANE_PHOENIX,
            LANE_IMAGE,
            LANE_VISION,
            LANE_SPECIALIST,
            "planner",
            "coder",
            "researcher",
            "browser",
            "frontend",
            "presentation",
            "computer_use",
            "database",
            "hacker",
            "critic",
            "tester",
            "scribe",
            LANE_INDEXER,
        ];
        lanes.sort_by_key(|lane| order.iter().position(|o| o == lane).unwrap_or(usize::MAX));
        return Ok(lanes);
    }
}

/// True when a lane key names an individual specialist (built-in or custom)
/// rather than a shared role lane.
pub(super) fn is_agent_lane(lane: &str) -> bool {
    SPECIALIST_ROLES.iter().any(|(role, _)| *role == lane)
        || crate::sub_agents::registry::custom_roster()
            .iter()
            .any(|(role, _)| role == lane)
}

/// Human label for a lane key ("coder" → "Leo (coder)").
pub(super) fn lane_label(lane: &str) -> String {
    match lane {
        LANE_PHOENIX => "Phoenix (orchestrator)".to_string(),
        LANE_SPECIALIST => "all specialists".to_string(),
        LANE_INDEXER => "Indexer (librarian)".to_string(),
        LANE_VISION => "Vision".to_string(),
        LANE_IMAGE => "image model".to_string(),
        agent => crate::runtime::delegation::agent_display_name(agent),
    }
}

/// The reasoning-effort picker for one lane. None = provider default.
pub(super) fn pick_lane_effort(
    theme: &ColorfulTheme,
    provider: &ProviderModels,
    lane: &str,
    model: &str,
) -> Result<Option<String>> {
    if !providers_data::model_supports_effort(provider.id, model) {
        return Ok(None);
    }
    let levels = providers_data::effort_levels(provider.id, model);
    let mut items: Vec<String> = vec!["provider default".to_string()];
    items.extend(levels.iter().map(|l| l.to_string()));
    // Phoenix stays deliberate. Codex specialists default to Luna Max: it
    // preserves craft while keeping the single Sol subscription lane for the
    // orchestrator. Other providers retain the historical high/medium policy.
    let preferred = crate::config::recommended_lane_effort(provider.id, lane, levels);
    let default_index = preferred
        .and_then(|preferred| levels.iter().position(|level| *level == preferred))
        .map(|i| i + 1)
        .unwrap_or(0);
    let picked = Select::with_theme(theme)
        .with_prompt(format!(
            "  {} reasoning effort for {model} (higher = smarter + slower)",
            lane_label(lane)
        ))
        .items(&items)
        .default(default_index)
        .interact()
        .context("Effort selection cancelled")?;
    Ok((picked > 0).then(|| levels[picked - 1].to_string()))
}

/// Model + effort for every selected lane, in order. `defaults` supplies a
/// per-lane starting model (reconfigure pre-fills the current picks).
pub(super) fn pick_lane_models(
    theme: &ColorfulTheme,
    provider: &ProviderModels,
    lanes: &[String],
    defaults: &std::collections::BTreeMap<String, String>,
) -> Result<Vec<LanePick>> {
    let recommended = {
        let r = providers_data::recommended_model(provider.id);
        if r.is_empty() {
            provider.models.first().map(|m| m.id).unwrap_or("")
        } else {
            r
        }
    };
    let mut picks: Vec<LanePick> = Vec::new();
    for lane in lanes {
        if lane == LANE_IMAGE {
            // Image GENERATION is its own catalog (/images/generations —
            // gpt-image-1 etc.), never the chat-model list. No reasoning dial.
            let default = defaults.get(lane).map(String::as_str);
            let model = pick_image_model(theme, provider, default)?;
            picks.push(LanePick {
                lane: lane.clone(),
                model,
                effort: None,
            });
            continue;
        }
        let lane_recommended =
            crate::config::recommended_lane_model(provider.id, lane, recommended);
        let default = defaults
            .get(lane)
            .map(String::as_str)
            .unwrap_or(lane_recommended);
        let model = pick_model_with_custom(theme, provider, &lane_label(lane), default)?;
        let effort = pick_lane_effort(theme, provider, lane, &model)?;
        picks.push(LanePick {
            lane: lane.clone(),
            model,
            effort,
        });
    }
    Ok(picks)
}

/// The image-model picker: the provider's known image-generation models plus
/// custom entry. Providers with no known images API go straight to custom
/// entry (any OpenAI-compatible `/images/generations` id works).
fn pick_image_model(
    theme: &ColorfulTheme,
    provider: &ProviderModels,
    default: Option<&str>,
) -> Result<String> {
    const CUSTOM_ENTRY: &str = "✎ Enter custom image model ID…";
    if provider.id == "openai-codex" {
        // The Codex OAuth token only reaches chatgpt.com's Codex backend —
        // /images/generations is an API-platform endpoint it can't call.
        println!(
            "  {} A ChatGPT/Codex subscription login can NOT generate images — the images API needs an OpenAI API-platform key. Add an `openai` profile (API key) and put the image lane there.",
            style("⚠").yellow()
        );
    }
    let catalog = providers_data::image_models(provider.id);
    if catalog.is_empty() {
        println!(
            "  {} {} has no cataloged image models — enter the image-generation model ID it serves (needs an OpenAI-compatible images API).",
            style("•").dim(),
            provider.name
        );
        let custom: String = Input::with_theme(theme)
            .with_prompt("  image model ID")
            .default(default.unwrap_or("gpt-image-2").to_string())
            .interact_text()
            .context("image model entry cancelled")?;
        let custom = custom.trim();
        if custom.is_empty() {
            anyhow::bail!("image model ID cannot be empty");
        }
        return Ok(custom.to_string());
    }
    let mut labels: Vec<String> = catalog
        .iter()
        .map(|(id, name)| format!("{name}  ({id})"))
        .collect();
    labels.push(CUSTOM_ENTRY.to_string());
    let default_index = default
        .and_then(|d| catalog.iter().position(|(id, _)| *id == d))
        .unwrap_or(0);
    let picked = Select::with_theme(theme)
        .with_prompt("  image model")
        .items(&labels)
        .default(default_index)
        .interact()
        .context("image model selection cancelled")?;
    if picked == catalog.len() {
        let custom: String = Input::with_theme(theme)
            .with_prompt("  Custom image model ID")
            .interact_text()
            .context("custom image model entry cancelled")?;
        let custom = custom.trim();
        if custom.is_empty() {
            anyhow::bail!("image model ID cannot be empty");
        }
        return Ok(custom.to_string());
    }
    Ok(catalog[picked].0.to_string())
}

/// The full wizard tail (agents → models → efforts) for an already
/// authenticated provider.
pub(super) fn run_lane_wizard(
    theme: &ColorfulTheme,
    provider: &ProviderModels,
    recommended_defaults: bool,
    require_phoenix: bool,
) -> Result<Vec<LanePick>> {
    let lanes = pick_profile_lanes(theme, recommended_defaults, require_phoenix)?;
    pick_lane_models(theme, provider, &lanes, &Default::default())
}

/// Append `profile_id` to the chain a lane pick names (fallback profiles).
pub(super) fn append_to_chain(
    chains: &mut crate::config::FallbackChains,
    lane: &str,
    profile_id: &str,
) {
    let chain = match chains.role_lane_mut(lane) {
        Some(chain) => chain,
        None => chains.agents.entry(lane.to_string()).or_default(),
    };
    if !chain.iter().any(|p| p == profile_id) {
        chain.push(profile_id.to_string());
    }
}

/// Write a fallback profile's lane picks into the auth-profile store
/// (`assignments`) — the rotation layer reads model+effort from there.
pub(super) fn store_lane_assignments(
    store: &mut crate::config::auth_profile::AuthProfileStore,
    profile_id: &str,
    picks: &[LanePick],
) {
    let lanes = store.assignments.entry(profile_id.to_string()).or_default();
    for pick in picks {
        lanes.insert(
            pick.lane.clone(),
            crate::config::auth_profile::RoleAssignment {
                model: pick.model.clone(),
                effort: pick.effort.clone(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_order_is_phoenix_then_image_then_team() {
        // pick_profile_lanes sorts by the spec's walk-through order; verify
        // the mapping helpers agree on keys.
        assert_eq!(lane_label(LANE_PHOENIX), "Phoenix (orchestrator)");
        assert_eq!(lane_label("coder"), "Leo (coder)");
    }

    #[test]
    fn append_to_chain_routes_lanes_and_agents() {
        let mut chains = crate::config::FallbackChains::default();
        append_to_chain(&mut chains, LANE_PHOENIX, "p:2");
        append_to_chain(&mut chains, LANE_VISION, "p:2");
        append_to_chain(&mut chains, LANE_IMAGE, "p:2");
        append_to_chain(&mut chains, "coder", "p:2");
        append_to_chain(&mut chains, "coder", "p:2"); // idempotent
        assert_eq!(chains.orchestrator, vec!["p:2"]);
        assert_eq!(chains.vision, vec!["p:2"]);
        assert_eq!(chains.image, vec!["p:2"]);
        assert_eq!(chains.agents["coder"], vec!["p:2"]);
    }

    #[test]
    fn assignments_prefer_exact_lane_then_specialist_then_legacy() {
        use crate::config::auth_profile::{AuthProfileStore, RoleAssignment};
        let mut store = AuthProfileStore::default();
        store_lane_assignments(
            &mut store,
            "p:2",
            &[
                LanePick {
                    lane: "specialist".into(),
                    model: "shared".into(),
                    effort: Some("low".into()),
                },
                LanePick {
                    lane: "coder".into(),
                    model: "special".into(),
                    effort: Some("xhigh".into()),
                },
            ],
        );
        store.models.insert("p:3".into(), "legacy".into());
        assert_eq!(
            store.assignment_for("p:2", "coder"),
            Some(RoleAssignment {
                model: "special".into(),
                effort: Some("xhigh".into())
            })
        );
        // Agent without its own pick rides the profile's specialist pick.
        assert_eq!(
            store.assignment_for("p:2", "tester").map(|a| a.model),
            Some("shared".into())
        );
        // Legacy single-model pin still answers.
        assert_eq!(
            store.assignment_for("p:3", "orchestrator").map(|a| a.model),
            Some("legacy".into())
        );
        assert_eq!(store.assignment_for("p:4", "coder"), None);
    }
}
