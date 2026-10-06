//! Resolve API keys for search / crawl / scrape providers.

use anyhow::{Context, Result};

use super::auth_profile::{
    extract_profile_secret, load_auth_profile_store, update_auth_profile_store,
    AuthProfileCredential,
};
use super::types::WebAuthConfig;

#[derive(Debug, Clone)]
pub struct ResolvedWebKey {
    pub source: String,
    pub profile_id: Option<String>,
    pub env_var: Option<String>,
    pub api_key: String,
}

pub fn resolve_web_api_key(
    auth: Option<&WebAuthConfig>,
    inline_key: Option<&str>,
    default_env_vars: &[&str],
) -> Result<Option<ResolvedWebKey>> {
    if let Some(key) = inline_key.filter(|k| !k.trim().is_empty()) {
        return Ok(Some(ResolvedWebKey {
            source: "config".to_string(),
            profile_id: None,
            env_var: None,
            api_key: key.trim().to_string(),
        }));
    }

    if let Some(auth) = auth {
        return resolve_declared_web_auth(auth, default_env_vars);
    }

    for env_var in default_env_vars {
        if let Ok(value) = std::env::var(env_var) {
            let value = value.trim();
            if !value.is_empty() {
                return Ok(Some(ResolvedWebKey {
                    source: "env".to_string(),
                    profile_id: None,
                    env_var: Some(env_var.to_string()),
                    api_key: value.to_string(),
                }));
            }
        }
    }

    Ok(None)
}

fn resolve_declared_web_auth(
    auth: &WebAuthConfig,
    default_env_vars: &[&str],
) -> Result<Option<ResolvedWebKey>> {
    let source = auth.source.clone().unwrap_or_else(|| "env".to_string());

    match source.as_str() {
        "env" => {
            let mut candidates = Vec::new();
            if let Some(env_var) = auth.env_var.as_deref() {
                candidates.push(env_var.to_string());
            }
            candidates.extend(default_env_vars.iter().map(|v| (*v).to_string()));
            for env_var in candidates {
                if let Ok(value) = std::env::var(&env_var) {
                    let value = value.trim();
                    if !value.is_empty() {
                        return Ok(Some(ResolvedWebKey {
                            source: "env".to_string(),
                            profile_id: None,
                            env_var: Some(env_var),
                            api_key: value.to_string(),
                        }));
                    }
                }
            }
            Ok(None)
        }
        "profile" => {
            let profile_id = auth
                .profile
                .clone()
                .context("Web auth source=profile requires `profile` id")?;
            let store = load_auth_profile_store()?;
            let credential = store.profiles.get(&profile_id).with_context(|| {
                format!(
                    "Web auth profile '{profile_id}' was not found in ~/.phoenix/auth-profiles.json"
                )
            })?;
            Ok(Some(ResolvedWebKey {
                source: "profile".to_string(),
                profile_id: Some(profile_id),
                env_var: None,
                api_key: extract_profile_secret(credential)?,
            }))
        }
        "none" => Ok(None),
        other => {
            anyhow::bail!("Unsupported web auth source '{other}'. Expected env, profile, or none.")
        }
    }
}

pub fn store_web_api_key_profile(
    profile_id: &str,
    provider_label: &str,
    api_key: &str,
) -> Result<()> {
    update_auth_profile_store(|store| {
        store.profiles.insert(
            profile_id.to_string(),
            AuthProfileCredential::ApiKey {
                provider: provider_label.to_string(),
                key: api_key.trim().to_string(),
                display_name: Some(profile_id.to_string()),
            },
        );
        Ok(())
    })?;
    Ok(())
}

pub fn format_web_auth_toml_block(prefix: &str, auth: &WebAuthConfig) -> String {
    let mut lines = format!("\n[{prefix}.auth]\n");
    if let Some(source) = &auth.source {
        lines.push_str(&format!("source = \"{source}\"\n"));
    }
    if let Some(profile) = &auth.profile {
        lines.push_str(&format!("profile = \"{profile}\"\n"));
    }
    if let Some(env_var) = &auth.env_var {
        lines.push_str(&format!("env_var = \"{env_var}\"\n"));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_key_wins_over_env() {
        let key = resolve_web_api_key(None, Some("inline-key"), &["TAVILY_API_KEY"]).unwrap();
        assert_eq!(key.unwrap().api_key, "inline-key");
    }
}
