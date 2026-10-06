use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::config::auth_profile::{profile_auth_identity, resolve_llm_auth, ResolvedAuth};
use crate::config::LLMProfile;
use crate::providers::anthropic::AnthropicProvider;
use crate::providers::contracts::{
    AuthType, CompletionRequest, CompletionResponse, LLMProvider, ModelInfo,
    NativeCompactionCapability, NativeCompactionRequest, NativeCompactionResult,
    NativeCompactionRoute, ProviderAuthIdentity, ProviderResponseDialect, ProviderResponseRoute,
    RoutedCompletionResponse, RoutedStreamingResponse, StreamingResponse,
};
use crate::providers::deepseek::DeepSeekProvider;
use crate::providers::google::GoogleProvider;
use crate::providers::grok_cli::GrokCliProvider;
use crate::providers::ollama::OllamaProvider;
use crate::providers::openai::OpenAIProvider;
use crate::providers::openai_codex::OpenAICodexProvider;
use crate::providers::opencode::OpenCodeProvider;
use crate::providers::openrouter::OpenRouterProvider;
use crate::providers::providers_data::{self, ProviderModels};

#[derive(Debug, Clone)]
pub struct ResolvedProvider {
    pub provider_id: String,
    pub model_id: String,
    pub base_url: String,
    pub auth: ResolvedAuth,
}

/// The two fields `instantiate`'s provider match actually consumes.
struct ResolvedProviderParts {
    provider_id: String,
    base_url: String,
}

/// Binds a raw wire adapter to the non-secret account generation resolved by
/// the factory. Raw adapters deliberately know only their credential; this
/// wrapper is the final route gate for opaque replay and keeps auth identity
/// out of every provider constructor.
struct IdentityBoundProvider {
    inner: Arc<dyn LLMProvider>,
    auth_identity: ProviderAuthIdentity,
}

impl IdentityBoundProvider {
    fn new(inner: Arc<dyn LLMProvider>, auth_identity: ProviderAuthIdentity) -> Self {
        Self {
            inner,
            auth_identity,
        }
    }

    fn request_for_identity(
        &self,
        request: CompletionRequest,
    ) -> anyhow::Result<CompletionRequest> {
        let replay_matches = request.native_replay.as_ref().is_some_and(|replay| {
            let Some(session_id) = request.session_id.as_deref() else {
                return false;
            };
            let Some(route) = self.native_compaction_route(&request.model) else {
                return false;
            };
            replay.matches_route(
                route.capability(),
                session_id,
                route.provider(),
                route.base_route(),
                route.model(),
                route.account_scope(),
                route.auth_epoch(),
            )
        });
        if request.native_replay.is_some() && !replay_matches {
            // A direct provider must never silently reinterpret a request that
            // the mesh classified as native. Surface the route change so the
            // mesh can atomically clear durable replay and perform its single
            // portable retry. Fallback chains deliberately strip replay while
            // constructing a *different* candidate request and return that
            // candidate's exact response receipt instead.
            anyhow::bail!(
                "provider-native replay no longer matches the effective account/model route"
            );
        }
        Ok(request)
    }
}

#[async_trait]
impl LLMProvider for IdentityBoundProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn display_name(&self) -> &str {
        self.inner.display_name()
    }

    fn base_url(&self) -> &str {
        self.inner.base_url()
    }

    fn auth_type(&self) -> AuthType {
        self.inner.auth_type()
    }

    fn env_vars(&self) -> Vec<&str> {
        self.inner.env_vars()
    }

    fn default_headers(&self) -> std::collections::HashMap<String, String> {
        self.inner.default_headers()
    }

    fn has_model(&self, model: &str) -> bool {
        self.inner.has_model(model)
    }

    fn default_model(&self) -> &str {
        self.inner.default_model()
    }

    fn fallback_models(&self) -> Vec<&str> {
        self.inner.fallback_models()
    }

    fn context_window(&self, model: &str) -> Option<u64> {
        self.inner.context_window(model)
    }

    fn native_compaction_capability(&self, model: &str) -> NativeCompactionCapability {
        self.inner.native_compaction_capability(model)
    }

    fn native_compaction_route(&self, model: &str) -> Option<NativeCompactionRoute> {
        let capability = self.inner.native_compaction_capability(model);
        if !capability.is_supported() {
            return None;
        }
        NativeCompactionRoute::try_for_identity(
            capability,
            self.inner.name(),
            self.inner.base_url(),
            model,
            &self.auth_identity,
        )
        .ok()
    }

    fn response_dialect(&self, model: &str) -> ProviderResponseDialect {
        self.inner.response_dialect(model)
    }

    fn response_route(&self, model: &str) -> Option<ProviderResponseRoute> {
        ProviderResponseRoute::try_for_identity(
            self.inner.response_dialect(model),
            self.inner.name(),
            self.inner.base_url(),
            model,
            &self.auth_identity,
        )
        .ok()
    }

    async fn compact_context(
        &self,
        request: NativeCompactionRequest,
    ) -> anyhow::Result<NativeCompactionResult> {
        let expected = self
            .native_compaction_route(request.model())
            .context("native compaction route is unavailable for this provider account")?;
        anyhow::ensure!(
            request.route() == &expected,
            "native compaction request belongs to a different provider route"
        );
        self.inner.compact_context(request).await
    }

    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        self.complete_with_route(request)
            .await
            .map(|routed| routed.into_parts().0)
    }

    async fn stream(&self, request: CompletionRequest) -> anyhow::Result<StreamingResponse> {
        self.stream_with_route(request)
            .await
            .map(|routed| routed.into_parts().0)
    }

    async fn complete_with_route(
        &self,
        request: CompletionRequest,
    ) -> anyhow::Result<RoutedCompletionResponse> {
        let request = self.request_for_identity(request)?;
        let actual_route = self.response_route(&request.model);
        self.inner
            .complete(request)
            .await
            .map(|response| RoutedCompletionResponse::new(response, actual_route))
    }

    async fn stream_with_route(
        &self,
        request: CompletionRequest,
    ) -> anyhow::Result<RoutedStreamingResponse> {
        let request = self.request_for_identity(request)?;
        let actual_route = self.response_route(&request.model);
        self.inner
            .stream(request)
            .await
            .map(|response| RoutedStreamingResponse::new(response, actual_route))
    }

    async fn embeddings(&self, texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
        self.inner.embeddings(texts).await
    }

    async fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        self.inner.list_models().await
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        self.inner.health_check().await
    }

    fn supports_native_images(&self) -> bool {
        self.inner.supports_native_images()
    }
}

pub struct ProviderFactory;

fn fallback_link_timeout_secs(provider_id: &str, configured: u64) -> u64 {
    if provider_id == "tokenrouter" {
        if configured == 0 { 60 } else { configured.clamp(1, 60) }
    } else {
        configured
    }
}

impl ProviderFactory {
    pub fn new() -> Self {
        Self
    }

    pub fn resolve_llm_profile(&self, profile: &LLMProfile) -> Result<ResolvedProvider> {
        let provider = providers_data::get_provider(&profile.provider)
            .context("Configured provider is not in Phoenix's provider catalog")?;
        let auth = resolve_llm_auth(profile, &provider, |name| std::env::var(name).ok())?;
        Ok(ResolvedProvider {
            provider_id: provider.id.to_string(),
            model_id: profile.orchestrator(),
            base_url: provider
                .options
                .base_url
                .unwrap_or(provider.base_url)
                .to_string(),
            auth,
        })
    }

    /// Build a provider for a per-role override (specialist/librarian on a
    /// different provider than the orchestrator). Auth comes from the machine
    /// auth-profile store or env for THAT provider — the main profile's auth
    /// section is intentionally not reused across providers.
    pub fn build_role_provider(
        &self,
        profile: &LLMProfile,
        provider_id: &str,
    ) -> Result<Arc<dyn LLMProvider>> {
        self.build_llm_provider(&profile.probe_for_provider(provider_id))
    }

    pub fn build_llm_provider(&self, profile: &LLMProfile) -> Result<Arc<dyn LLMProvider>> {
        self.build_llm_provider_with_env(profile, |name| std::env::var(name).ok())
    }

    fn build_llm_provider_with_env(
        &self,
        profile: &LLMProfile,
        env_lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Arc<dyn LLMProvider>> {
        let provider = providers_data::get_provider(&profile.provider)
            .context("Configured provider is not in Phoenix's provider catalog")?;
        let resolved = ResolvedProvider {
            provider_id: provider.id.to_string(),
            model_id: profile.orchestrator(),
            base_url: provider
                .options
                .base_url
                .unwrap_or(provider.base_url)
                .to_string(),
            auth: resolve_llm_auth(profile, &provider, env_lookup)?,
        };
        let timeout = Duration::from_secs(profile.timeout_seconds);
        let native_compaction_enabled =
            profile.native_compaction_enabled_for(&resolved.provider_id);
        self.instantiate(
            &resolved.provider_id,
            &resolved.base_url,
            resolved.auth.credential.clone(),
            timeout,
            &resolved.auth.source_summary(),
            resolved.auth.auth_identity.clone(),
            native_compaction_enabled,
        )
    }

    /// Build a runtime client for `provider_id` from an explicit credential.
    /// Shared by the config-resolved path above and the account-fallback
    /// chains (each chain link carries its own stored credential).
    fn instantiate(
        &self,
        provider_id: &str,
        base_url: &str,
        auth_value: Option<String>,
        timeout: Duration,
        auth_source: &str,
        auth_identity: ProviderAuthIdentity,
        native_compaction_enabled: bool,
    ) -> Result<Arc<dyn LLMProvider>> {
        let require_auth = |provider_id: &str| -> Result<String> {
            auth_value.clone().with_context(|| {
                format!(
                    "Provider {} requires auth, but no usable credential was resolved from {}",
                    provider_id, auth_source
                )
            })
        };
        let resolved = ResolvedProviderParts {
            provider_id: provider_id.to_string(),
            base_url: base_url.to_string(),
        };

        let provider: Arc<dyn LLMProvider> = match resolved.provider_id.as_str() {
            "ollama-cloud" => Arc::new(OllamaProvider::with_cloud(
                require_auth("ollama-cloud")?,
                timeout,
            )),
            "ollama" | "sglang" | "vllm" | "lm_studio" | "litellm" | "huggingface" => {
                let url = resolved.base_url.trim_end_matches("v1").to_string();
                Arc::new(OllamaProvider::with_url_and_timeout(url, timeout))
            }
            // Keyless, but NOT on the Ollama path above: that client pins
            // `reasoning: None`, and this endpoint's whole point is a
            // selectable `reasoning_effort` (low · high · max) on top of
            // native OpenAI tool calls. So it takes the OpenAI-compatible
            // client, with a placeholder bearer — the server requires the
            // header to be non-empty but accepts any value.
            "hf-deepseek-v4-free" => Arc::new(OpenAIProvider::with_url_and_timeout(
                resolved.base_url.clone(),
                auth_value
                    .clone()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| "not-needed".to_string()),
                timeout,
            )),
            "openai" => Arc::new(OpenAIProvider::with_url_and_timeout_and_native(
                resolved.base_url.clone(),
                require_auth("openai")?,
                timeout,
                native_compaction_enabled,
            )),
            "groq" | "mistral" | "together" | "fireworks" | "deepinfra" | "moonshot"
            | "kimi-coding" | "tokenrouter" | "zai" | "xai" | "cerebras" | "venice"
            | "kilocode" | "nvidia" | "volcengine" | "byteplus" | "stepfun" | "qianfan"
            | "tencent" | "xiaomi" | "chutes" | "minimax-portal" | "github-copilot" | "meta" => {
                Arc::new(OpenAIProvider::with_url_and_timeout(
                    resolved.base_url.clone(),
                    require_auth(&resolved.provider_id)?,
                    timeout,
                ))
            }
            "openrouter" => Arc::new(OpenRouterProvider::with_timeout(
                require_auth("openrouter")?,
                timeout,
            )),
            "openai-codex" => Arc::new(OpenAICodexProvider::with_url_and_timeout_and_native(
                resolved.base_url.clone(),
                require_auth("openai-codex")?,
                timeout,
                native_compaction_enabled,
            ).with_auth_profile(auth_source.strip_prefix("profile:"), auth_identity.auth_epoch())),
            "grok-cli" => Arc::new(GrokCliProvider::with_url_and_timeout(
                resolved.base_url.clone(),
                require_auth("grok-cli")?,
                timeout,
            )),
            "opencode" => Arc::new(OpenCodeProvider::with_timeout(
                require_auth("opencode")?,
                timeout,
            )),
            "anthropic" => Arc::new(AnthropicProvider::with_timeout_and_native(
                require_auth("anthropic")?,
                timeout,
                native_compaction_enabled,
            )),
            "deepseek" => Arc::new(DeepSeekProvider::with_timeout(
                require_auth("deepseek")?,
                timeout,
            )),
            "google" => Arc::new(GoogleProvider::with_timeout(
                require_auth("google")?,
                timeout,
            )),
            "google-gemini-cli" => Arc::new(GoogleProvider::with_cli_oauth(
                resolved.base_url.clone(),
                require_auth("google-gemini-cli")?,
                timeout,
            )),
            provider => anyhow::bail!(
                "Provider '{}' is in the setup catalog but does not yet have a runtime client",
                provider
            ),
        };

        Ok(Arc::new(IdentityBoundProvider::new(
            provider,
            auth_identity,
        )))
    }

    /// Wrap `primary` with the role's account-fallback chain. Each chain entry
    /// is an auth-profile id resolving to (provider, credential, optional
    /// preferred model); a link that fails to build (missing profile, expired
    /// token) is skipped with a warning — a broken fallback must never break
    /// the primary. Empty chain = `primary` returned untouched.
    pub fn wrap_with_fallback_chain(
        &self,
        primary: Arc<dyn LLMProvider>,
        profile: &LLMProfile,
        role: &str,
        role_provider_id: &str,
        chain: &[String],
    ) -> Arc<dyn LLMProvider> {
        // Infallible entry point: a healthy primary can always answer, so the
        // Result form below cannot fail here.
        self.lane_with_fallback_chain(Ok(primary), profile, role, role_provider_id, chain)
            .expect("a healthy primary always yields a usable lane")
    }

    /// Build the role's lane from a POSSIBLY-DEAD primary plus its fallback
    /// chain.
    ///
    /// The primary arrives as a `Result` on purpose. Building it resolves a
    /// live credential (expired OAuth, revoked key, missing env var all fail
    /// HERE, before a single request), and the old signature took an
    /// `Arc` — so every call site wrote `build_llm_provider(llm)?` and a dead
    /// main account propagated `Err` out of the turn while three perfectly
    /// healthy fallback accounts sat unused. That is the literal "the
    /// fallbacks won't work" bug: the chain was never constructed at all.
    /// A dead primary is now just a missing first link.
    ///
    /// Only when NOTHING in the lane can be built does this return `Err`, and
    /// it returns the primary's own error so the message stays actionable.
    pub fn lane_with_fallback_chain(
        &self,
        primary: Result<Arc<dyn LLMProvider>>,
        profile: &LLMProfile,
        role: &str,
        role_provider_id: &str,
        chain: &[String],
    ) -> Result<Arc<dyn LLMProvider>> {
        // Provider selection selects its connected account pool. Preserve
        // model and effort within that pool, then try explicit cross-provider
        // backups only when the selected provider cannot answer.
        let auto_pool = true;
        if chain.is_empty() && !auto_pool {
            return primary;
        }
        let store = match crate::config::auth_profile::load_auth_profile_store() {
            Ok(store) => store,
            Err(error) => {
                eprintln!(
                    "warning: {role} fallback chain disabled — auth store unreadable ({error:#})"
                );
                return primary;
            }
        };

        // Which stored account the PRIMARY is already using. Without this the
        // chain happily rotates from a dead account straight into the same
        // dead account (gateway logs show `primary` and `ollama-cloud:default`
        // both 401ing on the identical key, 27 times in one run).
        let lane_profile = profile.probe_for_provider(role_provider_id).with_lane_pin(role, role_provider_id);
        let primary_profile_id = self.primary_profile_id(&lane_profile, role_provider_id);

        let effective_chain = effective_fallback_profiles(
            &store,
            role_provider_id,
            chain,
            auto_pool,
            primary_profile_id.as_deref(),
        );

        let mut links: Vec<crate::providers::fallback::FallbackLink> = Vec::new();
        let mut primary_error: Option<anyhow::Error> = None;
        match primary {
            Ok(provider) => links.push(crate::providers::fallback::FallbackLink {
                // Namespaced per role. The label is the key `record_rotation`
                // persists cooldowns under, and a bare "primary" is shared by
                // EVERY role and every account: one judge-lane 401 benched the
                // orchestrator's unrelated (healthy) main account for six
                // hours, across restarts.
                label: primary_profile_id
                    .clone()
                    .unwrap_or_else(|| format!("{role}:primary")),
                provider,
                model_override: None,
                effort_override: None,
            }),
            Err(error) => {
                eprintln!(
                    "warning: {role} primary account unusable ({error:#}) — \
                     falling back to the configured chain"
                );
                primary_error = Some(error);
            }
        }

        // Reasons the configured chain came up short, carried into the
        // exhaustion error so the user learns WHICH account to fix instead of
        // reading "all 2 accounts exhausted" for a 4-account chain.
        let mut skipped: Vec<String> = Vec::new();
        let mut seen: Vec<String> = primary_profile_id.iter().cloned().collect();
        for profile_id in &effective_chain {
            if seen.iter().any(|id| id == profile_id) {
                eprintln!(
                    "warning: {role} fallback profile `{profile_id}` skipped \
                     (already the primary account for this lane — it adds no quota)"
                );
                skipped.push(format!("{profile_id} — same account as the primary"));
                continue;
            }
            seen.push(profile_id.clone());
            match self.build_profile_link(profile, &store, profile_id, role, role_provider_id) {
                Ok(link) => links.push(link),
                Err(error) => {
                    eprintln!(
                        "warning: {role} fallback profile `{profile_id}` skipped ({error:#})"
                    );
                    skipped.push(format!("{profile_id} — {error:#}"));
                }
            }
        }

        apply_provider_account_order(&store, role_provider_id, &mut links);
        match links.len() {
            // Nothing at all: hand back the primary's own error, since that is
            // the one the user can act on.
            0 => Err(primary_error.unwrap_or_else(|| {
                anyhow::anyhow!(
                    "no usable account for the {role} lane: every configured \
                     fallback profile failed to build ({})",
                    skipped.join("; ")
                )
            })),
            // A single survivor still needs the wrapper when it came from the
            // chain — the caller expects a working lane, and the skip reasons
            // must survive into its error messages.
            1 if primary_error.is_none() && skipped.is_empty() => {
                Ok(links.into_iter().next().expect("len checked").provider)
            }
            _ => Ok(Arc::new(
                crate::providers::fallback::FallbackProvider::new(role, links)
                    .with_unusable_accounts(skipped),
            )),
        }
    }

    /// The auth-profile id the role's PRIMARY provider resolves to, when it
    /// comes from the profile store. `None` for env-var or unauthenticated
    /// lanes (nothing to deduplicate against).
    fn primary_profile_id(&self, profile: &LLMProfile, role_provider_id: &str) -> Option<String> {
        let probe = if profile.provider == role_provider_id {
            profile.clone()
        } else {
            profile.probe_for_provider(role_provider_id)
        };
        self.resolve_llm_profile(&probe)
            .ok()
            .and_then(|resolved| resolved.auth.profile_id)
    }

    /// One chain link from a stored auth profile: its own credential, its own
    /// provider client, and model/effort overrides — the profile's per-lane
    /// assignment first (plan 018), then the legacy single-model pin, then
    /// provider-shape defaults.
    fn build_profile_link(
        &self,
        profile: &LLMProfile,
        store: &crate::config::auth_profile::AuthProfileStore,
        profile_id: &str,
        role: &str,
        role_provider_id: &str,
    ) -> Result<crate::providers::fallback::FallbackLink> {
        let credential = store
            .profiles
            .get(profile_id)
            .with_context(|| format!("auth profile '{profile_id}' not found in the store"))?;
        let provider_id = crate::config::auth_profile::profile_provider_id(credential).to_string();
        let provider = providers_data::get_provider(&provider_id)
            .with_context(|| format!("provider '{provider_id}' is not in the catalog"))?;
        let secret = crate::config::auth_profile::extract_profile_secret(credential)?;
        // TokenRouter's free route has repeatedly held a fallback lane while a
        // healthy next link waited behind it. Even when the primary provider
        // has the unbounded default, this known-bad fallback endpoint keeps a
        // short silent-read limit. Other providers preserve the configured
        // value, including zero = no idle-read deadline.
        let link_timeout_secs = fallback_link_timeout_secs(&provider_id, profile.timeout_seconds);
        let inner = self.instantiate(
            &provider_id,
            provider.options.base_url.unwrap_or(provider.base_url),
            Some(secret),
            Duration::from_secs(link_timeout_secs),
            &format!("profile:{profile_id}"),
            profile_auth_identity(profile_id, store.auth_epoch_for_profile(profile_id))?,
            profile.native_compaction_enabled_for(&provider_id),
        )?;
        // Model: the profile's own pick for THIS lane wins (assignments →
        // legacy pin); with no pick, same provider as the role keeps the
        // role's request model, and a different provider gets its recommended
        // model (the role's model id wouldn't exist there).
        let same_provider = provider_id == role_provider_id;
        let assignment = (!same_provider)
            .then(|| store.assignment_for(profile_id, role))
            .flatten();
        let effort_override = assignment.as_ref().and_then(|a| a.effort.clone());
        let model_override = assignment.map(|a| a.model).or_else(|| {
            (!same_provider)
                .then(|| providers_data::recommended_model(&provider_id).to_string())
                .filter(|m| !m.is_empty())
        });
        Ok(crate::providers::fallback::FallbackLink {
            label: profile_id.to_string(),
            provider: inner,
            model_override,
            effort_override,
        })
    }

    pub fn list_available_providers(&self) -> Vec<&'static str> {
        providers_data::all_providers()
            .into_iter()
            .map(|provider| provider.id)
            .collect()
    }

    pub fn get_provider_info(&self, provider_id: &str) -> Option<ProviderModels> {
        providers_data::get_provider(provider_id)
    }

    pub fn requires_auth(&self, provider_id: &str) -> bool {
        providers_data::get_provider(provider_id)
            .map(|provider| !provider.env_vars.is_empty())
            .unwrap_or(false)
    }
}

fn apply_provider_account_order(store: &crate::config::auth_profile::AuthProfileStore, role_provider_id: &str, links: &mut [crate::providers::fallback::FallbackLink]) {
    let available=store.profiles_for_provider(role_provider_id);
    let mut order=store.state.order.get(&format!("provider:{role_provider_id}")).cloned().unwrap_or_default();
    order.retain(|id|available.contains(id));
    for id in available {if !order.contains(&id){order.push(id);}}
    // Use the same complete order the provider settings show, including
    // before the first explicit reorder and after a new account is added.
    links.sort_by_key(|link| {
        let same=store.profiles.get(&link.label).is_some_and(|credential|
            crate::config::auth_profile::profile_provider_id(credential)==role_provider_id);
        (!same,order.iter().position(|id|id==&link.label).unwrap_or(usize::MAX))
    });
}

pub(crate) fn effective_fallback_profiles(
    store: &crate::config::auth_profile::AuthProfileStore,
    role_provider_id: &str,
    configured: &[String],
    auto_pool: bool,
    primary_profile_id: Option<&str>,
) -> Vec<String> {
    let mut effective = configured.to_vec();
    if auto_pool {
        for profile_id in store.profiles_for_provider(role_provider_id) {
            if Some(profile_id.as_str()) != primary_profile_id && !effective.contains(&profile_id) {
                effective.push(profile_id);
            }
        }
    }
    // Drain the selected provider's pool before considering an explicitly
    // configured provider change. Stable ordering preserves account priority.
    effective.sort_by_key(|id| store.profiles.get(id).map_or(true, |credential|
        crate::config::auth_profile::profile_provider_id(credential) != role_provider_id));
    effective.dedup();
    effective
}

impl Default for ProviderFactory {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::auth_profile::{AuthProfileCredential, AuthProfileStore, RoleAssignment};
    use std::collections::HashMap;

    fn token(provider: &str, value: &str) -> AuthProfileCredential {
        AuthProfileCredential::Token {
            provider: provider.to_string(),
            token: value.to_string(),
            expires: None,
        }
    }

    #[tokio::test]
    #[ignore = "requires connected accounts and makes one real Sol medium request"]
    async fn live_provider_pool_keeps_sol_medium_without_lane_backups() {
        use crate::providers::contracts::ChatMessage;
        let factory = ProviderFactory::new();
        let profile = LLMProfile { provider: "openai-codex".into(), model: "gpt-5.6-sol".into(),
            auth: Some(crate::config::LLMAuthConfig { method: Some("oauth".into()), source: Some("profile".into()), profile: Some("openai-codex:2".into()), env_var: None }),
            ..Default::default() };
        let lane = factory.lane_with_fallback_chain(factory.build_llm_provider(&profile), &profile, "school_coach", "openai-codex", &[]).expect("pool builds");
        let mut request = CompletionRequest::new("gpt-5.6-sol", vec![ChatMessage::user("Reply exactly OK")]);
        request.extra_body.insert("reasoning".into(), serde_json::json!({"effort":"medium"}));
        let response = lane.complete(request).await.expect("connected pool should answer");
        assert_eq!(response.content.trim(), "OK");
        println!("LIVE_POOL_OK provider=openai-codex requested_model=gpt-5.6-sol effort=medium explicit_backups=0");
    }

    #[test]
    fn provider_priority_moves_pinned_primary_and_preserves_same_model() {
        let factory=ProviderFactory::new();let mut store=AuthProfileStore::default();
        for id in ["default","2","3","4"] {store.profiles.insert(format!("openai-codex:{id}"),token("openai-codex","test"));}
        store.profiles.insert("openai:backup".into(),token("openai","test"));
        store.state.order.insert("provider:openai-codex".into(),vec!["openai-codex:4".into(),"openai-codex:2".into(),"openai-codex:default".into(),"openai-codex:3".into()]);
        let profile=LLMProfile{provider:"openai-codex".into(),model:"gpt-5.6-sol".into(),reasoning_effort:Some("medium".into()),..Default::default()};
        let mut links=["openai-codex:default","openai:backup","openai-codex:2","openai-codex:3","openai-codex:4"].iter().map(|id|factory.build_profile_link(&profile,&store,id,"specialist","openai-codex").unwrap()).collect::<Vec<_>>();
        apply_provider_account_order(&store,"openai-codex",&mut links);
        assert_eq!(links.iter().map(|l|l.label.as_str()).collect::<Vec<_>>(),vec!["openai-codex:4","openai-codex:2","openai-codex:default","openai-codex:3","openai:backup"]);
        assert!(links[..4].iter().all(|l|l.model_override.is_none()&&l.effort_override.is_none()));
        store.state.order.clear();
        apply_provider_account_order(&store,"openai-codex",&mut links);
        assert_eq!(links.iter().map(|l|l.label.as_str()).collect::<Vec<_>>(),vec!["openai-codex:2","openai-codex:3","openai-codex:4","openai-codex:default","openai:backup"]);
    }

    #[test]
    fn codex_auto_pool_appends_every_other_connected_account_once() {
        let mut store = AuthProfileStore::default();
        for suffix in ["default", "2", "3", "4"] {
            store.profiles.insert(
                format!("openai-codex:{suffix}"),
                token("openai-codex", &format!("token-{suffix}")),
            );
        }
        store
            .profiles
            .insert("openai:default".into(), token("openai", "other-provider"));

        let configured = vec!["openai-codex:2".to_string(), "openai:default".to_string()];
        let effective = effective_fallback_profiles(
            &store,
            "openai-codex",
            &configured,
            true,
            Some("openai-codex:default"),
        );

        assert_eq!(
            effective,
            vec![
                "openai-codex:2",
                "openai-codex:3",
                "openai-codex:4",
                "openai:default",
            ]
        );
    }

    #[test]
    fn same_provider_codex_link_never_changes_the_selected_model_or_effort() {
        let mut store = AuthProfileStore::default();
        let profile_id = "openai-codex:2";
        store
            .profiles
            .insert(profile_id.into(), token("openai-codex", "test-token"));
        store.assignments.insert(
            profile_id.into(),
            HashMap::from([(
                "school_coach".into(),
                RoleAssignment {
                    model: "gpt-5.4-mini".into(),
                    effort: Some("high".into()),
                },
            )]),
        );

        let profile = LLMProfile::new("openai-codex", "gpt-5.6-sol");
        let link = ProviderFactory::new()
            .build_profile_link(&profile, &store, profile_id, "school_coach", "openai-codex")
            .expect("connected Codex account builds");

        assert_eq!(link.model_override, None);
        assert_eq!(link.effort_override, None);
    }

    #[test]
    fn resolves_ollama_profile() {
        let factory = ProviderFactory::new();
        let profile = LLMProfile::new("ollama", "llama3.1:8b");

        let resolved = factory.resolve_llm_profile(&profile).unwrap();
        assert_eq!(resolved.provider_id, "ollama");
        assert_eq!(resolved.model_id, "llama3.1:8b");
        assert_eq!(resolved.auth.source, "none");
    }

    /// The free HF DeepSeek endpoint builds with NO credential configured —
    /// that is the whole point of it — and must not land on the Ollama client,
    /// which pins `reasoning: None` and would silently drop the
    /// `reasoning_effort` this model is worth using for.
    #[test]
    fn builds_the_free_deepseek_endpoint_without_auth_on_the_openai_client() {
        let factory = ProviderFactory::new();
        let profile = LLMProfile::new("hf-deepseek-v4-free", "deepseek-ai/DeepSeek-V4-Flash-0731");

        let resolved = factory.resolve_llm_profile(&profile).unwrap();
        assert_eq!(resolved.auth.source, "none", "keyless by design");

        let provider = factory
            .build_llm_provider(&profile)
            .expect("a keyless endpoint must build with no credential");
        assert_eq!(
            provider.name(),
            "openai",
            "must use the OpenAI-compatible client, not the reasoning-dropping Ollama one"
        );

        // The catalog's context window must be the SERVED figure (393,216),
        // never the model's 1M native window — compaction sizes itself off it,
        // so an aspirational number here overruns the real deployment.
        let model = providers_data::get_provider("hf-deepseek-v4-free")
            .and_then(|p| p.models.into_iter().next())
            .expect("the endpoint serves exactly one model");
        assert_eq!(model.context_window, 393_216);
        assert!(model.reasoning, "reasoning_effort is supported");
    }

    #[test]
    fn builds_ollama_provider_without_auth() {
        let factory = ProviderFactory::new();
        let profile = LLMProfile::new("ollama", "llama3.1:8b");

        let provider = factory
            .build_llm_provider_with_env(&profile, |_| None)
            .unwrap();

        assert_eq!(provider.name(), "ollama");
    }

    #[test]
    fn builds_openai_provider_from_env_lookup() {
        let factory = ProviderFactory::new();
        let profile = LLMProfile::new("openai", "gpt-4o");

        let provider = factory
            .build_llm_provider_with_env(&profile, |name| {
                (name == "OPENAI_API_KEY").then(|| "test-key".to_string())
            })
            .unwrap();

        assert_eq!(provider.name(), "openai");
    }

    #[test]
    fn builds_groq_provider_from_env_lookup() {
        let factory = ProviderFactory::new();
        let profile = LLMProfile::new("groq", "llama-3.3-70b-versatile");

        let provider = factory
            .build_llm_provider_with_env(&profile, |name| {
                (name == "GROQ_API_KEY").then(|| "test-key".to_string())
            })
            .unwrap();

        assert_eq!(provider.name(), "openai");
    }

    #[test]
    fn builds_tokenrouter_provider_from_env_lookup() {
        let factory = ProviderFactory::new();
        let profile = LLMProfile::new("tokenrouter", "moonshotai/kimi-k3-free");

        let provider = factory
            .build_llm_provider_with_env(&profile, |name| {
                (name == "TOKENROUTER_API_KEY").then(|| "test-key".to_string())
            })
            .unwrap();

        assert_eq!(provider.name(), "openai");
    }

    #[test]
    fn every_selectable_provider_has_a_runtime_client() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let factory = ProviderFactory::new();
        let mut failures = Vec::new();
        for provider in providers_data::all_providers() {
            let model = provider
                .models
                .first()
                .map(|entry| entry.id)
                .unwrap_or("catalog-runtime-probe");
            let profile = LLMProfile::new(provider.id, model);
            if let Err(error) =
                factory.build_llm_provider_with_env(&profile, |_| Some("test-key".to_string()))
            {
                failures.push(format!("{}: {error:#}", provider.id));
            }
        }
        assert!(
            failures.is_empty(),
            "selectable providers without executable clients:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    fn tokenrouter_fallback_cannot_hold_a_lane_for_five_minutes() {
        assert_eq!(fallback_link_timeout_secs("tokenrouter", 300), 60);
        assert_eq!(fallback_link_timeout_secs("tokenrouter", 30), 30);
        assert_eq!(fallback_link_timeout_secs("tokenrouter", 0), 60);
        assert_eq!(fallback_link_timeout_secs("nvidia", 300), 300);
        assert_eq!(fallback_link_timeout_secs("nvidia", 0), 0);
    }

    #[test]
    fn reports_missing_provider_auth() {
        let factory = ProviderFactory::new();
        let profile = LLMProfile::new("deepseek", "deepseek-chat");

        let error = match factory.build_llm_provider_with_env(&profile, |_| None) {
            Ok(provider) => panic!("expected missing auth error, got {}", provider.name()),
            Err(error) => error,
        };

        assert!(error.to_string().contains("deepseek"));
    }

    #[test]
    fn reports_missing_openrouter_auth() {
        let factory = ProviderFactory::new();
        let profile = LLMProfile::new("openrouter", "google/gemini-2.5-flash");

        let error = match factory.build_llm_provider_with_env(&profile, |_| None) {
            Ok(provider) => panic!("expected missing auth error, got {}", provider.name()),
            Err(error) => error,
        };

        assert!(error.to_string().contains("openrouter"));
    }

    #[test]
    fn reports_missing_anthropic_auth() {
        let factory = ProviderFactory::new();
        let profile = LLMProfile::new("anthropic", "claude-sonnet-4-6");

        let error = match factory.build_llm_provider_with_env(&profile, |_| None) {
            Ok(provider) => panic!("expected missing auth error, got {}", provider.name()),
            Err(error) => error,
        };

        assert!(error.to_string().contains("anthropic"));
    }

    #[test]
    fn builds_opencode_provider_from_env_lookup() {
        let factory = ProviderFactory::new();
        let profile = LLMProfile::new("opencode", "claude-sonnet-4-6");

        let provider = factory
            .build_llm_provider_with_env(&profile, |name| {
                (name == "OPENCODE_API_KEY").then(|| "test-key".to_string())
            })
            .unwrap();

        assert_eq!(provider.name(), "opencode");
    }

    #[test]
    fn resolves_openai_codex_responses_base_url() {
        let factory = ProviderFactory::new();
        let profile = LLMProfile::new("openai-codex", "gpt-5.4-mini");

        let provider = factory
            .build_llm_provider_with_env(&profile, |name| {
                (name == "OPENAI_OAUTH_TOKEN").then(|| "test-token".to_string())
            })
            .unwrap();

        assert_eq!(provider.base_url(), "https://chatgpt.com/backend-api/codex");
    }

    #[test]
    fn builds_openai_codex_provider_from_env_lookup() {
        let factory = ProviderFactory::new();
        let profile = LLMProfile::new("openai-codex", "gpt-5.4-mini");

        let provider = factory
            .build_llm_provider_with_env(&profile, |name| {
                (name == "OPENAI_OAUTH_TOKEN").then(|| "test-token".to_string())
            })
            .unwrap();

        assert_eq!(provider.name(), "openai-codex");
    }
}
