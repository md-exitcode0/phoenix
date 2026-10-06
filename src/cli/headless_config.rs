//! Headless, machine-readable config surface for GUI clients (the canvas
//! dashboard). Every mutation goes through the same stores that
//! `phoenix configure` uses — GUI clients never re-implement config logic,
//! they shell out to these commands.

use anyhow::{Context, Result};
use serde_json::json;
use std::io::BufRead;

use crate::config::auth_profile::{
    load_auth_profile_store, profile_provider_id, update_auth_profile_store, AuthProfileCredential,
};
use crate::config::web_providers_data::{
    WebProviderEntry, CRAWL_PROVIDERS, SCRAPE_PROVIDERS, SEARCH_PROVIDERS,
};
use crate::providers::providers_data;

/// Open a sensitive file with owner-only permissions before writing bytes.
/// Applying the mode through both `OpenOptions` and the opened handle covers
/// new files and repairs pre-existing temp/backup files respectively.
fn write_private_file(path: &std::path::Path, contents: &[u8]) -> Result<()> {
    crate::config::private_io::atomic_write_private(path, contents)
}

fn replace_private_file_atomic(
    path: &std::path::Path,
    expected: &[u8],
    contents: &[u8],
) -> Result<()> {
    crate::config::private_io::compare_and_swap_private(path, Some(expected), contents)
}

/// The full picture the dashboard needs in ONE call: the provider catalog
/// (auth methods, models, recommended), the auth profiles already configured
/// per provider (summaries only — never secrets), and the web
/// search/crawl/scrape provider catalog.
pub fn providers_json() -> Result<String> {
    let store = load_auth_profile_store().context("failed to load auth profiles")?;
    let mut providers = Vec::new();
    for p in providers_data::all_providers() {
        let mut profiles: Vec<serde_json::Value> = store
            .profiles
            .iter()
            .filter(|(_, cred)| profile_provider_id(cred) == p.id)
            .map(|(id, cred)| {
                let mut row = profile_summary(id, cred);
                if let Some(label) = store.labels.get(id) { row["display"] = json!(label); }
                row
            })
            .collect();
        profiles.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        providers.push(json!({
            "id": p.id,
            "name": p.name,
            "base_url": p.base_url,
            "recommended": providers_data::recommended_model(p.id),
            "auth_methods": p.auth_methods.iter().map(|m| json!({
                "type": m.method_type,
                "label": m.label,
                "env_var": m.env_var,
                "llm_supported": m.llm_supported,
            })).collect::<Vec<_>>(),
            "env_vars": p.env_vars,
            "models": p.models.iter().map(|m| json!({
                "id": m.id,
                "name": m.name,
                "context_window": m.context_window,
                "reasoning": m.reasoning,
                "efforts": providers_data::effort_levels(p.id, m.id),
            })).collect::<Vec<_>>(),
            "image_models": providers_data::image_models(p.id).iter().map(|(id, name)| json!({
                "id": id,
                "name": name,
            })).collect::<Vec<_>>(),
            "profiles": profiles,
        }));
    }
    providers.sort_by(|a, b| {
        // Configured providers first, then alphabetical — the dashboard
        // renders this order directly.
        let ca = !a["profiles"].as_array().map(Vec::is_empty).unwrap_or(true);
        let cb = !b["profiles"].as_array().map(Vec::is_empty).unwrap_or(true);
        cb.cmp(&ca).then(a["id"].as_str().cmp(&b["id"].as_str()))
    });
    let web = |entries: &[WebProviderEntry]| {
        entries
            .iter()
            .map(|e| {
                json!({
                    "id": e.id,
                    "name": e.name,
                    "blurb": e.blurb,
                    "env_var": e.env_var,
                    "signup_url": e.signup_url,
                    "requires_key": e.requires_key,
                })
            })
            .collect::<Vec<_>>()
    };
    let doc = json!({
        "providers": providers,
        "web": {
            "search": web(SEARCH_PROVIDERS),
            "crawl": web(CRAWL_PROVIDERS),
            "scrape": web(SCRAPE_PROVIDERS),
        },
        "oauth_login_providers": ["xai", "grok-cli", "openai-codex", "google-gemini-cli", "chutes", "github-copilot", "minimax-portal"],
    });
    Ok(serde_json::to_string_pretty(&doc)?)
}

/// Flat list of stored auth profiles — summaries only, never secrets.
pub fn auth_list_json() -> Result<String> {
    let store = load_auth_profile_store().context("failed to load auth profiles")?;
    let mut rows: Vec<serde_json::Value> = store
        .profiles
        .iter()
        .map(|(id, cred)| {
            let mut row = profile_summary(id, cred);
            if let Some(label) = store.labels.get(id) { row["display"] = json!(label); }
            row["provider"] = json!(profile_provider_id(cred));
            row
        })
        .collect();
    rows.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    Ok(serde_json::to_string_pretty(&json!({ "profiles": rows }))?)
}

fn profile_summary(id: &str, cred: &AuthProfileCredential) -> serde_json::Value {
    match cred {
        AuthProfileCredential::ApiKey { display_name, .. } => json!({
            "id": id, "method": "api", "display": display_name,
        }),
        AuthProfileCredential::Token { expires, .. } => json!({
            "id": id, "method": "token", "expires": expires,
        }),
        AuthProfileCredential::OAuth {
            expires,
            email,
            refresh,
            ..
        } => json!({
            "id": id, "method": "oauth", "expires": expires,
            "email": email, "refreshable": refresh.is_some(),
        }),
    }
}

/// Store an API key as an auth profile. The key arrives on STDIN (first
/// line) so it never appears in `ps` output or shell history. `provider`
/// must exist in the LLM catalog or the web provider catalog.
pub fn auth_set_key(profile_id: &str, provider: &str) -> Result<()> {
    crate::config::ensure_phoenix_home().context("failed to prepare Phoenix home")?;
    let known_llm = providers_data::get_provider(provider).is_some();
    // Web keys store capability-scoped provider ids ("search:tavily") — the
    // shape the resolver already expects in auth-profiles.json.
    let bare_web = provider
        .split_once(':')
        .filter(|(cap, _)| matches!(*cap, "search" | "crawl" | "scrape"))
        .map(|(_, id)| id)
        .unwrap_or(provider);
    let known_web = SEARCH_PROVIDERS
        .iter()
        .chain(CRAWL_PROVIDERS)
        .chain(SCRAPE_PROVIDERS)
        .any(|e| e.id == bare_web);
    if !known_llm && !known_web {
        anyhow::bail!("unknown provider '{provider}' — not in the LLM or web catalog");
    }
    let mut key = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut key)
        .context("failed to read API key from stdin")?;
    let key = key.trim().to_string();
    if key.is_empty() {
        anyhow::bail!("empty API key on stdin");
    }
    let (path, ()) = update_auth_profile_store(|store| {
        store.profiles.insert(
            profile_id.to_string(),
            AuthProfileCredential::ApiKey {
                provider: provider.to_string(),
                key,
                display_name: Some(format!("{provider} API key")),
            },
        );
        Ok(())
    })?;
    println!("stored {profile_id} in {}", path.display());
    Ok(())
}

/// Does this failure mean "the account is fine, the PLAN has no API access"?
///
/// The distinction decides which remedy the dashboard offers — re-authenticate
/// (useless here) versus paste a console API key or move the lane — so getting
/// it wrong sends the user round a loop that cannot work.
///
/// The subtlety: by the time an error reaches this layer the provider has
/// already HUMANISED it ([`crate::providers::tier_gate::user_message_for`]),
/// and the humanised sentence carries neither `permission-denied` nor the raw
/// status code. Matching only those two — which is what this did — reported
/// `tier_gated: false` next to a message that spent two sentences explaining a
/// tier gate. The humanised wording is checked FIRST for that reason; the raw
/// shapes stay as a fallback for anything that gets here unmapped.
fn reads_as_tier_gate(msg: &str, provider_id: &str) -> bool {
    msg.contains("tier gate")
        || msg.contains("no API access")
        || msg.contains("permission-denied")
        || (msg.contains("403") && provider_id == "xai")
}

/// Account setup is interactive UI. A provider is never allowed to hold that
/// UI hostage for the normal request timeout used by full agent turns.
const AUTH_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);

/// LIVE-probe a stored auth profile: build the real provider exactly the way
/// a turn would (factory + stored credential, including OAuth auto-refresh)
/// and make a 1-token completion. Prints a JSON verdict — the dashboard
/// shows this truth right after connect instead of a blind "saved ✓".
pub async fn auth_probe_json(
    profile_id: &str,
    model_override: Option<&str>,
    effort_override: Option<&str>,
) -> Result<String> {
    use crate::providers::{ChatMessage, CompletionRequest, MessageRole, ProviderFactory};
    let store = load_auth_profile_store()?;
    let cred = store
        .profiles
        .get(profile_id)
        .with_context(|| format!("no auth profile '{profile_id}'"))?;
    let provider_id = profile_provider_id(cred).to_string();
    if providers_data::get_provider(&provider_id).is_none() {
        // Web-capability profiles ("search:tavily") aren't LLM-probeable here.
        return Ok(serde_json::to_string_pretty(&json!({
            "ok": true, "skipped": true,
            "message": format!("'{provider_id}' is not an LLM provider — nothing to probe"),
        }))?);
    }
    // Probe the model this profile ACTUALLY runs: the legacy single-model pin,
    // then the per-lane assignments (orchestrator → shared specialist), and
    // only then the catalog's recommended model. Probing the catalog default
    // (gpt-oss:20b on ollama-cloud) tested a model the user never picked and
    // made a healthy account look broken in the dashboard.
    let model = model_override
        .filter(|model| !model.trim().is_empty())
        .map(str::to_string)
        .or_else(|| store.models.get(profile_id).cloned())
        .or_else(|| {
            store
                .assignment_for(profile_id, "orchestrator")
                .map(|assignment| assignment.model)
        })
        .unwrap_or_else(|| providers_data::recommended_model(&provider_id).to_string());
    let effort = effort_override
        .filter(|effort| !effort.trim().is_empty())
        .map(str::to_string);
    let method = match cred {
        AuthProfileCredential::ApiKey { .. } => "api",
        _ => "oauth",
    };
    let profile = crate::config::types::LLMProfile {
        provider: provider_id.clone(),
        model: model.clone(),
        auth: Some(crate::config::types::LLMAuthConfig {
            method: Some(method.to_string()),
            source: Some("profile".to_string()),
            profile: Some(profile_id.to_string()),
            env_var: None,
        }),
        ..Default::default()
    };
    let verdict = tokio::time::timeout(AUTH_PROBE_TIMEOUT, async {
        let provider = ProviderFactory::new().build_llm_provider(&profile)?;
        let mut req = CompletionRequest::new(
            model.clone(),
            vec![ChatMessage {
                role: MessageRole::User,
                content: "Reply with exactly: OK".to_string(),
                name: None,
                tool_call_id: None,
                tool_calls: Vec::new(),
                images: Vec::new(),
                provider_replay: None,
            }],
        );
        req.max_tokens = Some(8);
        if let Some(effort) = &effort {
            req.extra_body.insert(
                "reasoning".to_string(),
                serde_json::json!({ "effort": effort }),
            );
        }
        provider.complete(req).await
    })
    .await;
    let doc = match verdict {
        Ok(Ok(resp)) => json!({
            "ok": true, "provider": provider_id,
            "model": if resp.model.trim().is_empty() { model.clone() } else { resp.model },
            "requested_model": model, "effort": effort,
            "preview": resp.content.chars().take(40).collect::<String>(),
        }),
        Ok(Err(error)) => {
            let msg = format!("{error:#}");
            let tier_gated = reads_as_tier_gate(&msg, &provider_id);
            json!({
                "ok": false, "provider": provider_id, "model": model.clone(),
                "requested_model": model, "effort": effort,
                "error": msg.chars().take(400).collect::<String>(),
                "tier_gated": tier_gated,
                "hint": if tier_gated {
                    "the account authenticates but the plan tier has no API access — \
                     upgrade the subscription tier or use an API key from the provider console"
                } else { "" },
            })
        }
        Err(_) => json!({
            "ok": false, "provider": provider_id, "model": model.clone(),
            "requested_model": model, "effort": effort,
            "error": format!(
                "live credential check timed out after {} seconds; the profile is still stored",
                AUTH_PROBE_TIMEOUT.as_secs()
            ),
            "timed_out": true,
            "tier_gated": false,
        }),
    };
    Ok(serde_json::to_string_pretty(&doc)?)
}

/// Remove every exact reference to an auth profile while preserving the rest
/// of config.toml. This covers primary auth pins, per-lane pins, role/agent
/// fallback arrays, and web-provider auth/fallbacks.
fn strip_auth_profile_references(raw: &str, profile_id: &str) -> Result<(String, usize)> {
    let mut doc: toml_edit::DocumentMut = raw.parse().context("config.toml unreadable")?;
    let mut edits = 0usize;

    fn strip_arrays(item: Option<&mut toml_edit::Item>, profile_id: &str) -> usize {
        let Some(table) = item.and_then(|item| item.as_table_like_mut()) else {
            return 0;
        };
        let keys: Vec<String> = table.iter().map(|(key, _)| key.to_string()).collect();
        let mut edits = 0usize;
        for key in keys {
            let Some(array) = table.get_mut(&key).and_then(|item| item.as_array_mut()) else {
                continue;
            };
            let before = array.len();
            array.retain(|value| value.as_str() != Some(profile_id));
            edits += before - array.len();
        }
        edits
    }

    fn strip_values(item: Option<&mut toml_edit::Item>, profile_id: &str) -> usize {
        let Some(table) = item.and_then(|item| item.as_table_like_mut()) else {
            return 0;
        };
        let keys: Vec<String> = table
            .iter()
            .filter(|(_, value)| value.as_str() == Some(profile_id))
            .map(|(key, _)| key.to_string())
            .collect();
        let edits = keys.len();
        for key in keys {
            table.remove(&key);
        }
        edits
    }

    fn strip_auth_pin(item: Option<&mut toml_edit::Item>, profile_id: &str) -> usize {
        let Some(auth) = item.and_then(|item| item.as_table_like_mut()) else {
            return 0;
        };
        if auth.get("profile").and_then(|value| value.as_str()) == Some(profile_id) {
            auth.remove("profile");
            1
        } else {
            0
        }
    }

    if let Some(llm) = doc
        .get_mut("profile")
        .and_then(|profile| profile.get_mut("llm"))
    {
        edits += strip_auth_pin(llm.get_mut("auth"), profile_id);
        edits += strip_values(llm.get_mut("auth_by_lane"), profile_id);
        if let Some(fallback) = llm.get_mut("fallback") {
            edits += strip_arrays(fallback.get_mut("agents"), profile_id);
            edits += strip_arrays(Some(fallback), profile_id);
        }
    }
    if let Some(profile) = doc.get_mut("profile") {
        edits += strip_arrays(profile.get_mut("web_fallback"), profile_id);
        for capability in ["search", "crawl", "scrape"] {
            edits += strip_auth_pin(
                profile
                    .get_mut(capability)
                    .and_then(|item| item.get_mut("auth")),
                profile_id,
            );
        }
    }
    Ok((doc.to_string(), edits))
}

/// Remove a stored auth profile by id and unhook it everywhere. Config
/// references are pruned first, so a GUI delete never bounces the user into a
/// manual config-editing scavenger hunt. If credential-store persistence then
/// fails, the login remains stored but safely unused and deletion can be
/// retried.
pub fn auth_remove(profile_id: &str) -> Result<()> {
    crate::config::ensure_phoenix_home().context("failed to prepare Phoenix home")?;
    let original_store = load_auth_profile_store()?;
    let original_credential = original_store
        .profiles
        .get(profile_id)
        .with_context(|| format!("no auth profile '{profile_id}'"))?;
    let original_credential = serde_json::to_vec(original_credential)?;

    let mut stripped = 0usize;
    if let Ok(home) = crate::config::ensure_phoenix_home() {
        let config_path = home.join("config.toml");
        if let Some(raw) = crate::config::private_io::read_private_file(&config_path)? {
            let raw = String::from_utf8(raw)
                .with_context(|| format!("{} is not UTF-8", config_path.display()))?;
            let (cleaned, edits) = strip_auth_profile_references(&raw, profile_id)?;
            if edits > 0 {
                let backup = config_path.with_extension("toml.bak");
                // The backup can carry provider headers and environment
                // values, so create it privately instead of copying through
                // a potentially permissive umask.
                write_private_file(&backup, raw.as_bytes())?;
                replace_private_file_atomic(&config_path, raw.as_bytes(), cleaned.as_bytes())?;
                stripped = edits;
            }
        }
    }

    let (path, ()) = update_auth_profile_store(|store| {
        let unchanged = store
            .profiles
            .get(profile_id)
            .and_then(|credential| serde_json::to_vec(credential).ok())
            .is_some_and(|credential| credential == original_credential);
        if !unchanged {
            anyhow::bail!("auth profile '{profile_id}' changed in another process; retry");
        }
        store.profiles.remove(profile_id);
        store.labels.remove(profile_id);
        store.models.remove(profile_id);
        store.assignments.remove(profile_id);
        store.state.cooldown_until.remove(profile_id);
        store.state.last_good.retain(|_, id| id != profile_id);
        Ok(())
    })?;
    println!(
        "removed {profile_id} from {} (+ cleared {stripped} config reference{})",
        path.display(),
        if stripped == 1 { "" } else { "s" }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn sensitive_cli_writes_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let backup = dir.path().join("config.toml.bak");
        write_private_file(&backup, b"secret backup").unwrap();
        assert_eq!(
            std::fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let config = dir.path().join("config.toml");
        std::fs::write(&config, "old secret").unwrap();
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o644)).unwrap();
        let temp = config.with_extension("toml.tmp");
        std::fs::write(&temp, "stale temp secret").unwrap();
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o666)).unwrap();
        std::fs::remove_file(&temp).unwrap();

        replace_private_file_atomic(&config, b"old secret", b"new secret").unwrap();

        assert_eq!(std::fs::read_to_string(&config).unwrap(), "new secret");
        assert_eq!(
            std::fs::metadata(&config).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(!temp.exists());
    }

    #[test]
    fn providers_json_is_valid_and_never_leaks_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(dir.path());
        let raw = providers_json().expect("providers json");
        let doc: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
        let providers = doc["providers"].as_array().expect("providers array");
        assert!(providers.iter().any(|p| p["id"] == "xai"
            && p["recommended"] == "grok-4.5"
            && p["auth_methods"][0]["type"] == "oauth"));
        assert!(providers.iter().any(|p| p["id"] == "openai"
            && p["image_models"].as_array().is_some_and(|models| {
                models.iter().any(|model| model["id"] == "gpt-image-2")
            })));
        assert!(doc["web"]["search"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["id"] == "tavily"));
        let native_login = doc["oauth_login_providers"]
            .as_array()
            .expect("native login provider array");
        assert!(native_login.iter().any(|entry| entry == "github-copilot"));
        assert!(native_login.iter().any(|entry| entry == "minimax-portal"));
        // Secrets never serialize: no key/access/refresh fields anywhere.
        assert!(!raw.contains("\"key\""));
        assert!(!raw.contains("\"access\""));
        assert!(!raw.contains("\"refresh\":"));
    }

    #[test]
    fn headless_catalog_never_masks_a_corrupt_auth_store_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(dir.path());
        crate::config::private_io::atomic_write_private(
            &dir.path().join("auth-profiles.json"),
            b"{not json",
        )
        .unwrap();

        assert!(providers_json().is_err());
        assert!(auth_list_json().is_err());
    }

    /// The probe's own words have to agree with its flag. Observed on a live
    /// probe of `xai:default` (2026-07-25): a message that says "this is x.ai's
    /// tier gate, not a bad login" next to `"tier_gated": false`, because the
    /// classifier was matching the RAW shapes the humaniser had already
    /// replaced. The dashboard reads the flag, so it offered "re-authenticate"
    /// for an account whose plan simply has no API access.
    #[test]
    fn a_humanised_tier_gate_still_reads_as_one() {
        let humanised = crate::providers::tier_gate::user_message_for(
            "https://api.x.ai/v1",
            403,
            r#"{"code":"permission-denied"}"#,
        );
        assert!(
            reads_as_tier_gate(&humanised, "xai"),
            "the message the user actually sees must set the flag: {humanised}"
        );
        // The raw shapes still classify, for anything that arrives unmapped.
        assert!(reads_as_tier_gate(
            "xAI API error (403): permission-denied",
            "xai"
        ));
        assert!(reads_as_tier_gate("xAI API error (403): forbidden", "xai"));
        // And an ordinary dead credential is NOT a tier gate — offering the
        // "upgrade your plan" hint for a revoked key is its own wrong turn.
        assert!(!reads_as_tier_gate(
            r#"Ollama Cloud API error (401 Unauthorized): {"error":"Unauthorized"}"#,
            "ollama-cloud"
        ));
        assert!(!reads_as_tier_gate("connection timed out", "nvidia"));
    }

    #[test]
    fn interactive_auth_probe_has_a_short_hard_deadline() {
        assert!(AUTH_PROBE_TIMEOUT <= std::time::Duration::from_secs(30));
        assert!(AUTH_PROBE_TIMEOUT >= std::time::Duration::from_secs(10));
    }

    #[test]
    fn deleting_an_account_prunes_every_config_reference() {
        let raw = r#"[profile.llm]
provider = "openai-codex"
auth_by_lane = { specialist = "tokenrouter:default", coder = "nvidia:2" }

[profile.llm.auth]
method = "stored"
profile = "tokenrouter:default"

[profile.llm.fallback]
specialist = ["tokenrouter:default", "nvidia:default"]

[profile.llm.fallback.agents]
coder = ["tokenrouter:default", "openrouter:default"]

[profile.search.auth]
profile = "tokenrouter:default"

[profile.web_fallback]
search = ["tokenrouter:default", "tavily:2"]
"#;
        let (cleaned, edits) = strip_auth_profile_references(raw, "tokenrouter:default").unwrap();
        assert_eq!(edits, 6);
        assert!(!cleaned.contains("tokenrouter:default"));
        assert!(cleaned.contains("nvidia:default"));
        assert!(cleaned.contains("nvidia:2"));
        assert!(cleaned.contains("openrouter:default"));
        assert!(cleaned.contains("tavily:2"));
        cleaned
            .parse::<toml::Value>()
            .expect("cleaned config is valid TOML");
    }
}
