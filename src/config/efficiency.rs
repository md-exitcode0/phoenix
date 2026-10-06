//! Explicit subscription-efficient lane defaults.
//!
//! Phoenix remains the strong deliberative lane. Named specialists, volume
//! workers, memory, and compaction use Luna by default; individual agent model
//! and effort overrides remain authoritative. Applying the migration is an
//! explicit setup/settings action rather than a silent rewrite of user config.

use super::LLMProfile;

pub const CODEX_PHOENIX_MODEL: &str = "gpt-5.6-sol";
pub const CODEX_TEAM_MODEL: &str = "gpt-5.6-luna";
pub const CODEX_TEAM_EFFORT: &str = "max";

/// The one-click policy only edits Codex-backed company lanes. If a user has
/// deliberately routed a company lane through another provider, settings must
/// leave that choice alone instead of pairing a Codex model with the wrong
/// provider.
pub fn codex_subscription_defaults_applicable(profile: &LLMProfile) -> bool {
    profile.provider == "openai-codex"
        && [
            profile.specialist_provider.as_deref(),
            profile.librarian_provider.as_deref(),
            profile.memory_provider.as_deref(),
        ]
        .into_iter()
        .flatten()
        .all(|provider| provider == "openai-codex")
}

/// Whether the shared lanes already match the subscription-efficient policy.
/// Individual coworker overrides are intentionally outside this check: they
/// remain authoritative before and after applying the company defaults.
pub fn codex_subscription_defaults_active(profile: &LLMProfile) -> bool {
    codex_subscription_defaults_applicable(profile)
        && profile.orchestrator() == CODEX_PHOENIX_MODEL
        && profile.specialist() == CODEX_TEAM_MODEL
        && profile.librarian() == CODEX_TEAM_MODEL
        && profile.memory() == CODEX_TEAM_MODEL
        && profile.effort_for("orchestrator").as_deref() == Some("high")
        && profile.effort_for("specialist").as_deref() == Some(CODEX_TEAM_EFFORT)
        && profile.effort_for("librarian").as_deref() == Some(CODEX_TEAM_EFFORT)
}

pub fn recommended_lane_model<'a>(
    provider: &str,
    lane: &str,
    provider_default: &'a str,
) -> &'a str {
    if provider != "openai-codex" {
        return provider_default;
    }
    match lane {
        "orchestrator" => CODEX_PHOENIX_MODEL,
        "specialist" | "librarian" | "memory" => CODEX_TEAM_MODEL,
        // Every individual named coworker and ephemeral volume worker is a
        // specialist lane. Image and vision have their own catalogs.
        "image" | "vision" => provider_default,
        _ => CODEX_TEAM_MODEL,
    }
}

pub fn recommended_lane_effort<'a>(
    provider: &str,
    lane: &str,
    available: &'a [&'a str],
) -> Option<&'a str> {
    let preferred = if provider == "openai-codex" && lane != "orchestrator" {
        CODEX_TEAM_EFFORT
    } else {
        "high"
    };
    available
        .iter()
        .copied()
        .find(|level| *level == preferred)
        .or_else(|| available.iter().copied().find(|level| *level == "medium"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexEfficiencyMigration {
    pub changed_fields: Vec<&'static str>,
    pub preserved_agent_model_overrides: usize,
    pub preserved_agent_effort_overrides: usize,
}

/// Apply the product's efficient Codex subscription policy to an in-memory
/// profile. The caller owns persistence and confirmation. Per-agent overrides
/// are deliberately untouched.
pub fn apply_codex_subscription_defaults(
    profile: &mut LLMProfile,
) -> Option<CodexEfficiencyMigration> {
    if !codex_subscription_defaults_applicable(profile) {
        return None;
    }
    let mut changed = Vec::new();
    set_string(
        &mut profile.model,
        CODEX_PHOENIX_MODEL,
        "model",
        &mut changed,
    );
    set_option(
        &mut profile.orchestrator_model,
        CODEX_PHOENIX_MODEL,
        "orchestrator_model",
        &mut changed,
    );
    set_option(
        &mut profile.specialist_model,
        CODEX_TEAM_MODEL,
        "specialist_model",
        &mut changed,
    );
    set_option(
        &mut profile.librarian_model,
        CODEX_TEAM_MODEL,
        "librarian_model",
        &mut changed,
    );
    set_option(
        &mut profile.memory_model,
        CODEX_TEAM_MODEL,
        "memory_model",
        &mut changed,
    );
    set_effort(&mut profile.efforts, "orchestrator", "high", &mut changed);
    set_effort(
        &mut profile.efforts,
        "specialist",
        CODEX_TEAM_EFFORT,
        &mut changed,
    );
    set_effort(
        &mut profile.efforts,
        "librarian",
        CODEX_TEAM_EFFORT,
        &mut changed,
    );
    Some(CodexEfficiencyMigration {
        changed_fields: changed,
        preserved_agent_model_overrides: profile.agent_models.len(),
        preserved_agent_effort_overrides: profile
            .efforts
            .keys()
            .filter(|lane| !matches!(lane.as_str(), "orchestrator" | "specialist" | "librarian"))
            .count(),
    })
}

fn set_string(
    target: &mut String,
    value: &str,
    field: &'static str,
    changed: &mut Vec<&'static str>,
) {
    if target != value {
        *target = value.to_string();
        changed.push(field);
    }
}

fn set_option(
    target: &mut Option<String>,
    value: &str,
    field: &'static str,
    changed: &mut Vec<&'static str>,
) {
    if target.as_deref() != Some(value) {
        *target = Some(value.to_string());
        changed.push(field);
    }
}

fn set_effort(
    efforts: &mut std::collections::BTreeMap<String, String>,
    lane: &'static str,
    value: &str,
    changed: &mut Vec<&'static str>,
) {
    if efforts.get(lane).map(String::as_str) != Some(value) {
        efforts.insert(lane.to_string(), value.to_string());
        changed.push(lane);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_defaults_keep_sol_for_phoenix_and_luna_max_for_the_team() {
        assert_eq!(
            recommended_lane_model("openai-codex", "orchestrator", "gpt-5.6-terra"),
            CODEX_PHOENIX_MODEL
        );
        for lane in [
            "specialist",
            "browser",
            "coder",
            "librarian",
            "volume_worker",
        ] {
            assert_eq!(
                recommended_lane_model("openai-codex", lane, "gpt-5.6-terra"),
                CODEX_TEAM_MODEL
            );
            assert_eq!(
                recommended_lane_effort(
                    "openai-codex",
                    lane,
                    &["minimal", "low", "medium", "high", "xhigh", "max"]
                ),
                Some(CODEX_TEAM_EFFORT)
            );
        }
    }

    #[test]
    fn explicit_migration_preserves_individual_overrides() {
        let mut profile = LLMProfile::default();
        profile.provider = "openai-codex".into();
        profile.model = "gpt-5.6-sol".into();
        profile.specialist_model = Some("gpt-5.6-sol".into());
        profile
            .agent_models
            .insert("coder".into(), "custom-coder".into());
        profile.efforts.insert("coder".into(), "xhigh".into());

        let migration = apply_codex_subscription_defaults(&mut profile).unwrap();
        assert_eq!(profile.orchestrator(), CODEX_PHOENIX_MODEL);
        assert_eq!(profile.specialist(), CODEX_TEAM_MODEL);
        assert_eq!(profile.librarian(), CODEX_TEAM_MODEL);
        assert_eq!(profile.memory(), CODEX_TEAM_MODEL);
        assert_eq!(profile.agent_model("coder"), "custom-coder");
        assert_eq!(profile.agent_effort("coder").as_deref(), Some("xhigh"));
        assert_eq!(migration.preserved_agent_model_overrides, 1);
        assert_eq!(migration.preserved_agent_effort_overrides, 1);
    }

    #[test]
    fn another_provider_is_never_rewritten() {
        let mut profile = LLMProfile::default();
        profile.provider = "anthropic".into();
        let before = profile.model.clone();
        assert_eq!(apply_codex_subscription_defaults(&mut profile), None);
        assert_eq!(profile.model, before);
    }

    #[test]
    fn explicit_cross_provider_company_lane_is_never_rewritten() {
        let mut profile = LLMProfile::default();
        profile.provider = "openai-codex".into();
        profile.specialist_provider = Some("anthropic".into());
        profile.specialist_model = Some("claude-opus-4-6".into());
        let before = profile.specialist();
        assert_eq!(apply_codex_subscription_defaults(&mut profile), None);
        assert_eq!(profile.specialist(), before);
    }
}
