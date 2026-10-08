//! Account-fallback rotation: one logical provider backed by an ordered chain
//! of accounts (auth profiles). When the active account exhausts — quota hit,
//! rate limit that survived the in-provider retries, expired or revoked
//! credentials — the next account in the chain answers instead, and the dead
//! one is benched behind a persisted cooldown. Four Codex accounts keep a
//! session working until all four run out, with zero re-logins.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use anyhow::Context;
use async_trait::async_trait;

use crate::config::auth_profile::{load_auth_profile_store, profile_cooled_down, record_rotation};
use crate::providers::contracts::{
    AuthType, CompletionRequest, CompletionResponse, LLMProvider, ModelInfo,
    NativeCompactionCapability, NativeCompactionRequest, NativeCompactionResult,
    NativeCompactionRoute, ProviderResponseDialect, ProviderResponseRoute,
    RoutedCompletionResponse, RoutedStreamingResponse, StreamingResponse,
};
use crate::providers::retry::is_retryable_error;

/// One account in the chain: the profile id it came from, a ready client, and
/// optional model/effort overrides (a fallback on a different provider than
/// the role's primary can't run the primary's model id; the profile wizard
/// pins a model + reasoning effort per lane).
pub struct FallbackLink {
    pub label: String,
    pub provider: Arc<dyn LLMProvider>,
    pub model_override: Option<String>,
    pub effort_override: Option<String>,
}

/// Bench time after a quota/rate-limit exhaustion: long enough to stop
/// hammering a drained account, short enough to re-probe within a session.
const QUOTA_COOLDOWN_MS: i64 = 15 * 60 * 1000;
/// Bench time after a HARD exhaustion — one only a human action heals:
/// a revoked key, an expired token, a tier gate, or a spent prepaid balance.
/// Re-probing those every 15 minutes just burns turns, so bench them long.
const AUTH_COOLDOWN_MS: i64 = 6 * 60 * 60 * 1000;

/// NVIDIA NIM's account-scoped 404. A NIM account whose org is missing the
/// "Public API Endpoints" permission answers `GET /v1/models` perfectly but
/// 404s EVERY `POST /chat/completions` with
/// `{"status":404,"title":"Not Found","detail":"Function '<uuid>': Not found
/// for account '<acct>'"}`. It reads like a dead model id, but it is the
/// ACCOUNT that is dead — the next NIM account in the chain is usually fine,
/// so this must rotate. A bare 404 (genuinely unknown model) stays
/// unclassified: every account would 404 on it, benching them all for nothing.
const ACCOUNT_SCOPED_404: &str = "not found for account";

/// True when an error means "this ACCOUNT is done for now" — the next account
/// may well succeed, and this one earns a cooldown bench.
pub fn is_account_exhausted_error(message: &str) -> bool {
    if is_retryable_error(message) {
        // 429/overloaded that survived the in-provider retry loop: saturated.
        return true;
    }
    let m = message.to_ascii_lowercase();
    crate::config::auth_profile::is_oauth_login_rejected(message)
        || m.contains("quota")
        || m.contains("insufficient")
        || m.contains("credit")
        || m.contains("usage limit")
        || m.contains("plan limit")
        || m.contains("usage_limit")
        // Prepaid balance spent. NVIDIA NIM's shape is a 402
        // `{"status":402,"title":"Payment Required","detail":"Account '…':
        // Cloud credits expired - Please contact NVIDIA representatives"}` —
        // the whole point of pooling several free NIM accounts is that THIS
        // rotates to the next one.
        || m.contains("402")
        || m.contains("payment required")
        || m.contains("401")
        || m.contains("unauthorized")
        || m.contains("invalid api key")
        || m.contains("invalid_api_key")
        || m.contains("expired")
        || m.contains("revoked")
        // Tier gates: the account authenticates but the plan has no API
        // access (x.ai SuperGrok is the canonical case — a 403, not a 401;
        // NIM answers a revoked/bogus key with `{"status":403,"title":
        // "Forbidden","detail":"Authorization failed"}`). Only a plan change
        // or a new key heals it, so bench it like an auth failure.
        || m.contains("403")
        || m.contains("forbidden")
        || m.contains("no api access")
        || m.contains("permission")
        || m.contains("access denied")
        || m.contains(ACCOUNT_SCOPED_404)
        // A fallback endpoint that consumed its entire read budget without a
        // response must not cost every subsequent agent round the same five
        // minutes. Rotate and short-bench it like a saturated account.
        || m.contains("timed out")
        || m.contains("timeout")
}

/// Hard exhaustion gets the long bench; quota/rate-limit shapes the short one.
fn cooldown_for(message: &str) -> i64 {
    let m = message.to_ascii_lowercase();
    let hard = crate::config::auth_profile::is_oauth_login_rejected(message)
        || m.contains("401")
        || m.contains("unauthorized")
        || m.contains("invalid api key")
        || m.contains("invalid_api_key")
        || m.contains("expired")
        || m.contains("revoked")
        || m.contains("403")
        || m.contains("forbidden")
        || m.contains("no api access")
        || m.contains("permission")
        || m.contains("access denied")
        // A spent prepaid balance does not refill in 15 minutes, and a
        // permission-gated account never heals on its own.
        || m.contains("402")
        || m.contains("payment required")
        || m.contains(ACCOUNT_SCOPED_404);
    if hard {
        AUTH_COOLDOWN_MS
    } else {
        QUOTA_COOLDOWN_MS
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub struct FallbackProvider {
    role: String,
    links: Vec<FallbackLink>,
    /// In-memory cooldown mirror (profile id → benched-until epoch ms),
    /// seeded from the persisted store so restarts remember drained accounts.
    cooldowns: Mutex<HashMap<String, i64>>,
    /// Configured accounts that never made it into `links` (missing profile,
    /// expired token, duplicate of the primary), with the reason. Reported
    /// when the chain exhausts: "all 2 accounts exhausted" on a chain the user
    /// configured with four is a lie that sends them debugging the wrong lane.
    unusable: Vec<String>,
}

impl FallbackProvider {
    pub fn new(role: impl Into<String>, links: Vec<FallbackLink>) -> Self {
        assert!(
            !links.is_empty(),
            "a fallback chain needs at least one link"
        );
        let mut cooldowns = HashMap::new();
        if let Ok(store) = load_auth_profile_store() {
            let now = now_ms();
            for link in &links {
                if profile_cooled_down(&store.state, &link.label, now) {
                    if let Some(until) = store.state.cooldown_until.get(&link.label) {
                        cooldowns.insert(link.label.clone(), *until);
                    }
                }
            }
        }
        Self {
            role: role.into(),
            links,
            cooldowns: Mutex::new(cooldowns),
            unusable: Vec::new(),
        }
    }

    /// Record configured-but-unbuildable accounts for the exhaustion message.
    pub fn with_unusable_accounts(mut self, unusable: Vec<String>) -> Self {
        self.unusable = unusable;
        self
    }

    /// The error surfaced once every link has failed. Names the dropped
    /// accounts so a short chain is self-explaining.
    fn exhausted_error(&self, last_error: Option<anyhow::Error>, failures: Vec<String>) -> anyhow::Error {
        let mut summary = format!(
            "None of the {} loaded account(s) could serve the {} request",
            self.links.len(),
            self.role
        );
        if !failures.is_empty() {
            summary.push_str(&format!(": {}", failures.join("; ")));
        }
        if failures.len() < self.links.len() {
            summary.push_str(&format!("; {} account(s) skipped while cooling down", self.links.len()-failures.len()));
        }
        if !self.unusable.is_empty() {
            summary.push_str(&format!(
                " ({} configured account(s) never loaded: {})",
                self.unusable.len(),
                self.unusable.join("; ")
            ));
        }
        last_error
            .unwrap_or_else(|| anyhow::anyhow!("fallback chain has no usable accounts"))
            .context(summary)
    }

    pub fn chain_labels(&self) -> Vec<&str> {
        self.links.iter().map(|l| l.label.as_str()).collect()
    }

    fn account_failure(&self, index: usize, message: &str) -> String {
        let lower = message.to_ascii_lowercase();
        let reason = if crate::config::auth_profile::is_oauth_login_rejected(message) {
            "sign in again (saved login rejected; not a usage limit)"
        } else if lower.contains("401") || lower.contains("unauthorized") || lower.contains("invalid_api_key") {
            "authentication rejected (not a usage-limit result)"
        } else if lower.contains("token") && (lower.contains("expired") || lower.contains("revoked")) {
            "credential expired or revoked"
        } else if lower.contains("usage_limit") || lower.contains("insufficient_quota") {
            "usage limit reached"
        } else if lower.contains("429") || lower.contains("rate limit") {
            "rate limited"
        } else if lower.contains("403") || lower.contains("forbidden") {
            "access denied"
        } else if lower.contains("timeout") || lower.contains("timed out") {
            "request timed out"
        } else {
            "provider request failed"
        };
        format!("{} — {}", self.links[index].label, reason)
    }

    /// Attempt order for this call: every link not on cooldown, in chain
    /// order; if the whole chain is benched, try everyone anyway — a stale
    /// cooldown beats refusing to work.
    fn attempt_order(&self) -> Vec<usize> {
        let now = now_ms();
        let mut cooldowns = self.cooldowns.lock().unwrap_or_else(|p| p.into_inner());
        if let Ok(store) = load_auth_profile_store() {
            for link in &self.links {
                if store.profiles.contains_key(&link.label) {
                    match store.state.cooldown_until.get(&link.label) {
                        Some(until) => { cooldowns.insert(link.label.clone(), *until); }
                        None => { cooldowns.remove(&link.label); }
                    }
                }
            }
        }
        let live: Vec<usize> = (0..self.links.len())
            .filter(|i| {
                cooldowns
                    .get(&self.links[*i].label)
                    .map_or(true, |until| *until <= now)
            })
            .collect();
        if live.is_empty() {
            (0..self.links.len()).collect()
        } else {
            live
        }
    }

    /// Bench a drained account (in memory + persisted) and log the rotation.
    fn bench(&self, index: usize, error: &str, next: Option<usize>) {
        let link = &self.links[index];
        let until = now_ms() + cooldown_for(error);
        self.cooldowns
            .lock()
            .expect("cooldown lock")
            .insert(link.label.clone(), until);
        let next_label = next.map(|i| self.links[i].label.as_str());
        record_rotation(&self.role, Some((&link.label, until)), next_label);
        let onward = next_label
            .map(|l| format!(" — rotating to `{l}`"))
            .unwrap_or_else(|| " — no accounts left in the chain".to_string());
        eprintln!(
            "account fallback [{}]: `{}` unavailable ({}){}",
            self.role,
            link.label,
            first_line(error),
            onward
        );
        crate::debug_session::log(
            "A",
            "fallback.rs:bench",
            "account exhausted, rotating",
            serde_json::json!({
                "role": self.role,
                "exhausted": link.label,
                "next": next_label,
                "cooldown_until_ms": until,
                "error": first_line(error),
            }),
        );
    }

    fn request_for(&self, index: usize, request: &CompletionRequest) -> CompletionRequest {
        let mut request = request.clone();
        if let Some(model) = &self.links[index].model_override {
            request.model = model.clone();
        }
        if let Some(effort) = &self.links[index].effort_override {
            request.extra_body.insert(
                "reasoning".to_string(),
                serde_json::json!({ "effort": effort }),
            );
        }
        // A Codex subscription speed preference must not leak to another
        // provider or an unsupported model when an account fallback rotates.
        let speed_supported = self.links[index].provider.name() == "openai-codex"
            && request.extra_body.get("service_tier").and_then(|v| v.as_str())
                .and_then(|tier| crate::providers::openai_codex::codex_service_tier(&request.model, tier)).is_some();
        if !speed_supported { request.extra_body.remove("service_tier"); }
        // Replay is bound to the candidate AFTER its model override is
        // applied. Every attempt starts from the original portable request, so
        // a mismatching candidate loses only its native accelerator; it can
        // never receive another account's opaque prefix.
        let replay_matches = request.native_replay.as_ref().is_some_and(|replay| {
            let Some(session_id) = request.session_id.as_deref() else {
                return false;
            };
            let Some(route) = self.candidate_route(index, &request.model) else {
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
            request.native_replay = None;
        }
        request
    }

    fn candidate_route(
        &self,
        index: usize,
        requested_model: &str,
    ) -> Option<NativeCompactionRoute> {
        let link = self.links.get(index)?;
        let model = link.model_override.as_deref().unwrap_or(requested_model);
        link.provider.native_compaction_route(model)
    }

    fn candidate_response_route(
        &self,
        index: usize,
        requested_model: &str,
    ) -> Option<ProviderResponseRoute> {
        let link = self.links.get(index)?;
        let model = link.model_override.as_deref().unwrap_or(requested_model);
        link.provider.response_route(model)
    }

    async fn complete_routed(
        &self,
        request: CompletionRequest,
    ) -> anyhow::Result<RoutedCompletionResponse> {
        let order = self.attempt_order();
        let mut last_error: Option<anyhow::Error> = None;
        let mut failures = Vec::new();
        for (pos, index) in order.iter().copied().enumerate() {
            let link_request = self.request_for(index, &request);
            let actual_route = self.candidate_response_route(index, &link_request.model);
            match self.links[index]
                .provider
                .complete_with_route(link_request)
                .await
            {
                Ok(routed) => {
                    if pos > 0 {
                        record_rotation(&self.role, None, Some(&self.links[index].label));
                    }
                    let (response, inner_route) = routed.into_parts();
                    return Ok(RoutedCompletionResponse::new(
                        response,
                        inner_route.or(actual_route),
                    ));
                }
                Err(error) => {
                    let message = format!("{error:#}");
                    failures.push(self.account_failure(index, &message));
                    let next = order.get(pos + 1).copied();
                    if is_account_exhausted_error(&message) {
                        self.bench(index, &message, next);
                    } else if let Some(next_index) = next {
                        // Unclassified failure (odd provider error, network
                        // blip, model quirk): the next link is a DIFFERENT
                        // account — often a different provider entirely — so
                        // it may be fine. Rotate WITHOUT benching this one.
                        eprintln!(
                            "account fallback [{}]: `{}` failed ({}) — trying `{}` (no bench)",
                            self.role,
                            self.links[index].label,
                            first_line(&message),
                            self.links[next_index].label
                        );
                    }
                    last_error = Some(error);
                }
            }
        }
        Err(self.exhausted_error(last_error, failures))
    }

    async fn stream_routed(
        &self,
        request: CompletionRequest,
    ) -> anyhow::Result<RoutedStreamingResponse> {
        let order = self.attempt_order();
        let mut last_error: Option<anyhow::Error> = None;
        let mut failures = Vec::new();
        for (pos, index) in order.iter().copied().enumerate() {
            let link_request = self.request_for(index, &request);
            let actual_route = self.candidate_response_route(index, &link_request.model);
            match self.links[index]
                .provider
                .stream_with_route(link_request)
                .await
            {
                Ok(routed) => {
                    if pos > 0 {
                        record_rotation(&self.role, None, Some(&self.links[index].label));
                    }
                    let (response, inner_route) = routed.into_parts();
                    return Ok(RoutedStreamingResponse::new(
                        response,
                        inner_route.or(actual_route),
                    ));
                }
                Err(error) => {
                    let message = format!("{error:#}");
                    failures.push(self.account_failure(index, &message));
                    let next = order.get(pos + 1).copied();
                    if is_account_exhausted_error(&message) {
                        self.bench(index, &message, next);
                    } else if let Some(next_index) = next {
                        eprintln!(
                            "account fallback [{}]: `{}` failed ({}) — trying `{}` (no bench)",
                            self.role,
                            self.links[index].label,
                            first_line(&message),
                            self.links[next_index].label
                        );
                    }
                    last_error = Some(error);
                }
            }
        }
        Err(self.exhausted_error(last_error, failures))
    }
}

fn first_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(200)
        .collect()
}

#[async_trait]
impl LLMProvider for FallbackProvider {
    fn name(&self) -> &str {
        self.links[0].provider.name()
    }
    fn display_name(&self) -> &str {
        self.links[0].provider.display_name()
    }
    fn base_url(&self) -> &str {
        self.links[0].provider.base_url()
    }
    fn auth_type(&self) -> AuthType {
        self.links[0].provider.auth_type()
    }
    fn env_vars(&self) -> Vec<&str> {
        self.links[0].provider.env_vars()
    }
    fn default_headers(&self) -> std::collections::HashMap<String, String> {
        self.links[0].provider.default_headers()
    }
    fn has_model(&self, model: &str) -> bool {
        self.links.iter().any(|l| l.provider.has_model(model))
    }
    fn default_model(&self) -> &str {
        self.links[0].provider.default_model()
    }
    fn fallback_models(&self) -> Vec<&str> {
        self.links[0].provider.fallback_models()
    }
    fn context_window(&self, model: &str) -> Option<u64> {
        // A request may rotate to any live link. Size the prompt for the
        // smallest known route so a 1M primary cannot send 622k to a 500k Grok
        // fallback before compaction gets a chance to run.
        self.links
            .iter()
            .filter_map(|link| {
                let route_model = link.model_override.as_deref().unwrap_or(model);
                link.provider.context_window(route_model)
            })
            .min()
    }
    fn native_compaction_capability(&self, model: &str) -> NativeCompactionCapability {
        self.native_compaction_route(model)
            .map_or(NativeCompactionCapability::Unsupported, |route| {
                route.capability()
            })
    }
    fn native_compaction_route(&self, model: &str) -> Option<NativeCompactionRoute> {
        // Advertise native compaction only for the same first live account that
        // `complete`/`stream` will try. Skipping an unsupported earlier link to
        // compact on a later account would either change configured cost order
        // or let the earlier link succeed portably and strand the new replay.
        let first = self.attempt_order().into_iter().next()?;
        self.candidate_route(first, model)
    }
    fn response_dialect(&self, model: &str) -> ProviderResponseDialect {
        let Some(first) = self.attempt_order().into_iter().next() else {
            return ProviderResponseDialect::Generic;
        };
        let link = &self.links[first];
        let model = link.model_override.as_deref().unwrap_or(model);
        link.provider.response_dialect(model)
    }
    fn response_route(&self, model: &str) -> Option<ProviderResponseRoute> {
        let first = self.attempt_order().into_iter().next()?;
        self.candidate_response_route(first, model)
    }
    fn supports_native_images(&self) -> bool {
        // The request may rotate to any eligible link after an account/model
        // failure. Advertise image parts only when every possible route can
        // consume them; otherwise the mesh uses its text/vision sidecar path.
        self.links
            .iter()
            .all(|link| link.provider.supports_native_images())
    }

    async fn compact_context(
        &self,
        request: NativeCompactionRequest,
    ) -> anyhow::Result<NativeCompactionResult> {
        let order = self.attempt_order();
        let index = order
            .first()
            .copied()
            .context("fallback chain has no candidate for native compaction")?;
        let current_route = self
            .candidate_route(index, request.model())
            .with_context(|| {
                format!(
                    "first candidate in the {} fallback chain does not support native compaction",
                    self.role
                )
            })?;
        anyhow::ensure!(
            &current_route == request.route(),
            "native compaction route no longer matches the first candidate in the {} fallback chain",
            self.role
        );

        // A compaction request's provenance is already fixed to this exact
        // route. If it fails, return to portable compaction; never retarget the
        // same opaque-history request at a different account or model.
        match self.links[index].provider.compact_context(request).await {
            Ok(result) => Ok(result),
            Err(error) => {
                let message = format!("{error:#}");
                if is_account_exhausted_error(&message) {
                    let next = order.get(1).copied();
                    self.bench(index, &message, next);
                }
                Err(error)
            }
        }
    }

    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        self.complete_routed(request)
            .await
            .map(|routed| routed.into_parts().0)
    }

    async fn stream(&self, request: CompletionRequest) -> anyhow::Result<StreamingResponse> {
        self.stream_routed(request)
            .await
            .map(|routed| routed.into_parts().0)
    }

    async fn complete_with_route(
        &self,
        request: CompletionRequest,
    ) -> anyhow::Result<RoutedCompletionResponse> {
        self.complete_routed(request).await
    }

    async fn stream_with_route(
        &self,
        request: CompletionRequest,
    ) -> anyhow::Result<RoutedStreamingResponse> {
        self.stream_routed(request).await
    }

    async fn embeddings(&self, texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
        let index = self.attempt_order()[0];
        self.links[index].provider.embeddings(texts).await
    }

    async fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        let index = self.attempt_order()[0];
        self.links[index].provider.list_models().await
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        let index = self.attempt_order()[0];
        self.links[index].provider.health_check().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::contracts::{ChatMessage, TokenUsage};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Mock account: either always fails with `fail` or answers with its name.
    struct MockAccount {
        name: &'static str,
        fail: Option<&'static str>,
        calls: Arc<AtomicU32>,
        seen_model: Arc<Mutex<Option<String>>>,
        native_images: bool,
    }

    #[async_trait]
    impl LLMProvider for MockAccount {
        fn name(&self) -> &str {
            self.name
        }
        fn display_name(&self) -> &str {
            self.name
        }
        fn base_url(&self) -> &str {
            "mock://account"
        }
        fn auth_type(&self) -> AuthType {
            AuthType::None
        }
        fn env_vars(&self) -> Vec<&str> {
            vec![]
        }
        fn default_headers(&self) -> std::collections::HashMap<String, String> {
            Default::default()
        }
        fn has_model(&self, _model: &str) -> bool {
            true
        }
        fn default_model(&self) -> &str {
            "mock-model"
        }
        fn fallback_models(&self) -> Vec<&str> {
            vec![]
        }
        fn context_window(&self, model: &str) -> Option<u64> {
            match model {
                "alt-provider-model" => Some(500_000),
                _ => Some(1_000_000),
            }
        }
        fn supports_native_images(&self) -> bool {
            self.native_images
        }
        async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.seen_model.lock().unwrap() = Some(request.model.clone());
            if let Some(message) = self.fail {
                anyhow::bail!("{message}");
            }
            Ok(CompletionResponse {
                content: self.name.to_string(),
                model: request.model,
                usage: TokenUsage::new(1, 1),
                reasoning: None,
                stop_reason: Some("stop".to_string()),
                tool_calls: vec![],
                provider_replay: None,
            })
        }
        async fn stream(&self, _request: CompletionRequest) -> anyhow::Result<StreamingResponse> {
            anyhow::bail!("mock has no stream")
        }
        async fn embeddings(&self, _texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
            anyhow::bail!("mock has no embeddings")
        }
        async fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
            Ok(vec![])
        }
        async fn health_check(&self) -> anyhow::Result<bool> {
            Ok(true)
        }
    }

    fn link(
        label: &str,
        fail: Option<&'static str>,
        model_override: Option<&str>,
    ) -> (FallbackLink, Arc<AtomicU32>, Arc<Mutex<Option<String>>>) {
        let calls = Arc::new(AtomicU32::new(0));
        let seen_model = Arc::new(Mutex::new(None));
        let link = FallbackLink {
            label: label.to_string(),
            provider: Arc::new(MockAccount {
                name: "mock",
                fail,
                calls: calls.clone(),
                seen_model: seen_model.clone(),
                native_images: false,
            }),
            model_override: model_override.map(str::to_string),
            effort_override: None,
        };
        (link, calls, seen_model)
    }

    #[test]
    fn codex_speed_does_not_leak_across_provider_or_model_fallbacks() {
        let codex: Arc<dyn LLMProvider> = Arc::new(crate::providers::openai_codex::OpenAICodexProvider::with_url_and_timeout(
            "http://127.0.0.1:9".into(), "fixture-only".into(), std::time::Duration::from_secs(1)));
        let primary = FallbackLink { label: "codex".into(), provider: codex.clone(), model_override: None, effort_override: None };
        let (other, _, _) = link("other", None, Some("other-model"));
        let unsupported = FallbackLink { label: "luna".into(), provider: codex, model_override: Some("gpt-6-luna".into()), effort_override: None };
        let lane = FallbackProvider::new("fixture", vec![primary, other, unsupported]);
        let mut request = CompletionRequest::new("gpt-6.1-sol", vec![]);
        request.extra_body.insert("service_tier".into(), serde_json::json!("ultrafast"));
        assert_eq!(lane.request_for(0, &request).extra_body["service_tier"], "ultrafast");
        assert!(!lane.request_for(1, &request).extra_body.contains_key("service_tier"));
        assert!(!lane.request_for(2, &request).extra_body.contains_key("service_tier"));
    }

    fn image_link(label: &str, native_images: bool) -> FallbackLink {
        FallbackLink {
            label: label.to_string(),
            provider: Arc::new(MockAccount {
                name: "mock",
                fail: None,
                calls: Arc::new(AtomicU32::new(0)),
                seen_model: Arc::new(Mutex::new(None)),
                native_images,
            }),
            model_override: None,
            effort_override: None,
        }
    }

    fn request() -> CompletionRequest {
        CompletionRequest::new("primary-model", Vec::<ChatMessage>::new())
    }

    #[test]
    fn heterogeneous_fallback_chain_never_overpromises_native_images() {
        let all_images = FallbackProvider::new(
            "vision",
            vec![image_link("primary", true), image_link("fallback", true)],
        );
        assert!(all_images.supports_native_images());

        let mixed = FallbackProvider::new(
            "vision",
            vec![image_link("primary", true), image_link("fallback", false)],
        );
        assert!(!mixed.supports_native_images());
        let reverse = FallbackProvider::new(
            "vision",
            vec![image_link("primary", false), image_link("fallback", true)],
        );
        assert!(!reverse.supports_native_images());
    }

    #[tokio::test]
    async fn quota_exhaustion_rotates_to_the_next_account() {
        // Cooldowns re-sync from the process-global auth store; hold a
        // private home so a parallel test's store cannot un-bench a link.
        let home = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let (dead, dead_calls, _) = link("account-1", Some("HTTP 429 Too Many Requests"), None);
        let (alive, alive_calls, _) = link("account-2", None, None);
        let chain = FallbackProvider::new("orchestrator", vec![dead, alive]);
        let response = chain
            .complete(request())
            .await
            .expect("second account answers");
        assert_eq!(response.content, "mock");
        assert_eq!(dead_calls.load(Ordering::SeqCst), 1);
        assert_eq!(alive_calls.load(Ordering::SeqCst), 1);
        // The drained account is benched: the next call skips straight to #2.
        chain.complete(request()).await.expect("still answering");
        assert_eq!(
            dead_calls.load(Ordering::SeqCst),
            1,
            "benched account not re-hit"
        );
        assert_eq!(alive_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn rejected_oauth_login_is_skipped_until_reconnected_across_provider_rebuilds() {
        use crate::config::auth_profile::{AuthProfileCredential, AuthProfileStore, update_auth_profile_store};
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let mut store = AuthProfileStore::default();
        store.profiles.insert("rejected-login".into(), AuthProfileCredential::Token {
            provider:"openai-codex".into(), token:"synthetic-old".into(), expires:None,
        });
        update_auth_profile_store(|saved| { *saved = store; Ok(()) }).unwrap();
        const REJECTED: &str = "OAuth token refresh returned HTTP 400: the provider rejected the saved login; sign in again";
        let (rejected, rejected_calls, _) = link("rejected-login", Some(REJECTED), None);
        let (working, working_calls, _) = link("working-login", None, None);
        let chain = FallbackProvider::new("specialist", vec![rejected, working]);
        for _ in 0..3 { chain.complete(request()).await.unwrap(); }
        assert_eq!(rejected_calls.load(Ordering::SeqCst), 1);
        assert_eq!(working_calls.load(Ordering::SeqCst), 3);
        assert_eq!(cooldown_for(REJECTED), AUTH_COOLDOWN_MS);
        assert!(chain.account_failure(0, REJECTED).contains("sign in again"));
        let (reconnected, fresh_calls, _) = link("rejected-login", None, None);
        let (spare, spare_calls, _) = link("working-login", None, None);
        let rebuilt = FallbackProvider::new("librarian", vec![reconnected, spare]);
        rebuilt.complete(request()).await.unwrap();
        assert_eq!(fresh_calls.load(Ordering::SeqCst), 0, "persisted cooldown survives rebuilding for another role");
        assert_eq!(spare_calls.load(Ordering::SeqCst), 1);
        let mut store = load_auth_profile_store().unwrap();
        store.profiles.insert("rejected-login".into(), AuthProfileCredential::Token {
            provider:"openai-codex".into(), token:"synthetic-reconnected".into(), expires:None,
        });
        update_auth_profile_store(|saved| { *saved = store; Ok(()) }).unwrap();
        rebuilt.complete(request()).await.unwrap();
        assert_eq!(fresh_calls.load(Ordering::SeqCst), 1, "reconnect clears cooldown immediately");
        assert_eq!(spare_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn four_codex_accounts_rotate_without_changing_the_model() {
        let (first, first_calls, first_model) =
            link("openai-codex:default", Some("usage_limit_reached"), None);
        let (second, second_calls, second_model) =
            link("openai-codex:2", Some("HTTP 429 Too Many Requests"), None);
        let (third, third_calls, third_model) =
            link("openai-codex:3", Some("quota exceeded"), None);
        let (fourth, fourth_calls, fourth_model) = link("openai-codex:4", None, None);
        let chain = FallbackProvider::new("school_coach", vec![first, second, third, fourth]);

        chain
            .complete(request())
            .await
            .expect("fourth account answers");

        for calls in [first_calls, second_calls, third_calls, fourth_calls] {
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
        for seen_model in [first_model, second_model, third_model, fourth_model] {
            assert_eq!(seen_model.lock().unwrap().as_deref(), Some("primary-model"));
        }
    }

    #[tokio::test]
    async fn unclassified_errors_rotate_without_benching() {
        // A weird 400 on one link says nothing about a DIFFERENT account on a
        // different provider — the chain must keep going (the xAI tier-gate
        // 403 killing whole turns was exactly this, 2026-07-17). But the
        // failed link is NOT benched: it stays first in line next call.
        let (broken, broken_calls, _) =
            link("account-1", Some("400 bad request: unknown field"), None);
        let (spare, spare_calls, _) = link("account-2", None, None);
        let chain = FallbackProvider::new("orchestrator", vec![broken, spare]);
        let response = chain.complete(request()).await.expect("spare answers");
        assert_eq!(response.content, "mock");
        assert_eq!(spare_calls.load(Ordering::SeqCst), 1);
        chain
            .complete(request())
            .await
            .expect("spare answers again");
        assert_eq!(
            broken_calls.load(Ordering::SeqCst),
            2,
            "an unclassified failure must not bench the account"
        );
    }

    #[tokio::test]
    async fn fallback_links_run_their_own_model() {
        let (dead, _, _) = link("account-1", Some("quota exceeded"), None);
        let (alt, _, alt_model) = link("account-2", None, Some("alt-provider-model"));
        let chain = FallbackProvider::new("coder", vec![dead, alt]);
        chain.complete(request()).await.expect("alt answers");
        assert_eq!(
            alt_model.lock().unwrap().as_deref(),
            Some("alt-provider-model"),
            "the fallback account runs its own model, not the primary's"
        );
    }

    #[test]
    fn chain_uses_the_smallest_fallback_context_window() {
        let (primary, _, _) = link("account-1", None, None);
        let (smaller, _, _) = link("account-2", None, Some("alt-provider-model"));
        let chain = FallbackProvider::new("coder", vec![primary, smaller]);
        assert_eq!(chain.context_window("primary-model"), Some(500_000));
    }

    #[tokio::test]
    async fn all_exhausted_reports_the_chain_clearly() {
        let (a, _, _) = link("account-1", Some("429"), None);
        let (b, _, _) = link("account-2", Some("quota exceeded"), None);
        let chain = FallbackProvider::new("scout", vec![a, b]);
        let error = chain.complete(request()).await.expect_err("all drained");
        assert!(
            format!("{error:#}").contains("None of the 2 loaded account(s)"),
            "{error:#}"
        );
    }

    #[test]
    fn classifies_exhaustion_vs_real_errors() {
        // Account-done errors → rotate.
        assert!(is_account_exhausted_error("HTTP 429 Too Many Requests"));
        assert!(is_account_exhausted_error("insufficient credits on plan"));
        assert!(is_account_exhausted_error("monthly quota exceeded"));
        assert!(is_account_exhausted_error("401 Unauthorized"));
        assert!(is_account_exhausted_error("Stored OAuth token is expired"));
        assert!(is_account_exhausted_error("usage_limit_reached"));
        assert!(is_account_exhausted_error("operation timed out"));
        // Tier gates: authenticated but the plan has no API access → benched.
        assert!(is_account_exhausted_error(
            "xAI rejected the call (403 Forbidden)"
        ));
        assert!(is_account_exhausted_error("your plan has no API access"));
        // Unclassified errors → not benched (but rotation still tries the next link).
        assert!(!is_account_exhausted_error(
            "400 bad request: unknown field"
        ));
        assert!(!is_account_exhausted_error("connection refused"));
        assert!(!is_account_exhausted_error("model not found (404)"));
    }

    #[test]
    fn auth_failures_bench_longer_than_quota_hits() {
        assert_eq!(cooldown_for("401 unauthorized"), AUTH_COOLDOWN_MS);
        assert_eq!(cooldown_for("token expired"), AUTH_COOLDOWN_MS);
        assert_eq!(cooldown_for("429 rate limit"), QUOTA_COOLDOWN_MS);
        assert_eq!(cooldown_for("quota exceeded"), QUOTA_COOLDOWN_MS);
    }

    // ---- NVIDIA NIM: the exact wire errors a pooled-account chain must
    // survive. Bodies are copied verbatim from live NIM responses (403) and
    // from NVIDIA's developer forum reports (402 / account-scoped 404);
    // `OpenAIProvider` wraps them as `OpenAI API error ({status}): {body}`.

    /// A revoked / mistyped NIM key. Verified live 2026-07-25 against
    /// `POST https://integrate.api.nvidia.com/v1/chat/completions`.
    const NIM_403: &str = r#"OpenAI API error (403 Forbidden): {"status":403,"title":"Forbidden","detail":"Authorization failed"}"#;
    /// A NIM account whose free inference credits are spent.
    const NIM_402: &str = r#"OpenAI API error (402 Payment Required): {"status":402,"title":"Payment Required","detail":"Account 'Li-rEEodKCkt1Tz2UIQC4zUEyvPSeTdxIzZXq5jI5sU': Cloud credits expired - Please contact NVIDIA representatives"}"#;
    /// A NIM org missing the "Public API Endpoints" permission: /v1/models
    /// works, every completion 404s. Account-scoped, so the chain rotates.
    const NIM_404_ACCOUNT: &str = r#"OpenAI API error (404 Not Found): {"status":404,"title":"Not Found","detail":"Function '23d4f03a-b8a6-4adb-a183-7daa083a09cc': Not found for account 'QPJ8xxpqmG-NHcVSEeCWOf1HbKLXvOy3sD7YVa5qIDI'"}"#;

    #[test]
    fn nvidia_nim_exhaustion_shapes_all_rotate() {
        assert!(
            is_account_exhausted_error(NIM_402),
            "NIM credit exhaustion must rotate to the next account — that IS the pooling"
        );
        assert!(
            is_account_exhausted_error(NIM_403),
            "a revoked NIM key must rotate, not abort the chain"
        );
        assert!(
            is_account_exhausted_error(NIM_404_ACCOUNT),
            "NIM's account-scoped 404 (missing Public API Endpoints permission) must rotate"
        );
        // None of these heal in 15 minutes: spent credits, a dead key, and a
        // missing org permission all need a human. Long bench.
        assert_eq!(cooldown_for(NIM_402), AUTH_COOLDOWN_MS);
        assert_eq!(cooldown_for(NIM_403), AUTH_COOLDOWN_MS);
        assert_eq!(cooldown_for(NIM_404_ACCOUNT), AUTH_COOLDOWN_MS);
        // A NIM 429 is a real rate limit (~40 req/min), not a dead account:
        // short bench so the account comes back inside the session.
        assert!(is_account_exhausted_error(
            "OpenAI rate limit (429): Too Many Requests"
        ));
        assert_eq!(
            cooldown_for("OpenAI rate limit (429): Too Many Requests"),
            QUOTA_COOLDOWN_MS
        );
        // A GENERIC 404 is a bad model id, not a dead account — every account
        // would 404 on it. Benching the whole chain for that is the failure
        // mode this guard exists to prevent.
        assert!(!is_account_exhausted_error(
            r#"OpenAI API error (404 Not Found): 404 page not found"#
        ));
    }

    #[tokio::test]
    async fn credit_exhaustion_walks_the_whole_nim_account_pool() {
        // Cooldowns re-sync from the process-global auth store; hold a
        // private home so a parallel test's store cannot un-bench a link.
        let home = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        // Three free NIM accounts. #1 is out of credits, #2's key was revoked,
        // #3 still has quota — the turn must land on #3, not die on #1.
        let (spent, spent_calls, _) = link("nvidia:default", Some(NIM_402), None);
        let (revoked, revoked_calls, _) = link("nvidia:2", Some(NIM_403), None);
        let (fresh, fresh_calls, _) = link("nvidia:3", None, None);
        let chain = FallbackProvider::new("orchestrator", vec![spent, revoked, fresh]);

        chain.complete(request()).await.expect("account 3 answers");
        assert_eq!(spent_calls.load(Ordering::SeqCst), 1);
        assert_eq!(revoked_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fresh_calls.load(Ordering::SeqCst), 1);

        // Both dead accounts are now benched: the next call goes straight to
        // #3 instead of re-burning two round trips on known-dead credentials.
        chain.complete(request()).await.expect("still on account 3");
        assert_eq!(
            spent_calls.load(Ordering::SeqCst),
            1,
            "spent account re-hit"
        );
        assert_eq!(
            revoked_calls.load(Ordering::SeqCst),
            1,
            "revoked account re-hit"
        );
        assert_eq!(fresh_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_benched_nim_account_returns_when_its_cooldown_lapses() {
        // Cooldowns are a bench, not a delete: once the window passes the
        // account is back at the front of the chain (credits do get topped up,
        // keys do get re-issued). Simulated by expiring the stored deadline.
        let (rate_limited, calls, _) = link("nvidia:default", Some("429 Too Many Requests"), None);
        let (spare, _, _) = link("nvidia:2", None, None);
        let chain = FallbackProvider::new("orchestrator", vec![rate_limited, spare]);
        chain.complete(request()).await.expect("spare answers");
        assert_eq!(chain.attempt_order(), vec![1], "benched account skipped");

        let until = *chain
            .cooldowns
            .lock()
            .unwrap()
            .get("nvidia:default")
            .expect("account benched");
        assert!(
            until - now_ms() > QUOTA_COOLDOWN_MS - 60_000,
            "a 429 must earn the SHORT bench so quota comes back this session"
        );
        chain
            .cooldowns
            .lock()
            .unwrap()
            .insert("nvidia:default".to_string(), now_ms() - 1);
        assert_eq!(
            chain.attempt_order(),
            vec![0, 1],
            "a lapsed cooldown puts the account back at the head of the chain"
        );
        chain.complete(request()).await.expect("answers");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "re-probed after the bench lapsed"
        );
    }

    #[tokio::test]
    async fn mixed_auth_and_quota_failures_are_not_all_reported_as_exhausted() {
        let (quota, _, _) = link("diagnostic-quota", Some("usage_limit_reached"), None);
        let (auth, _, _) = link("diagnostic-auth", Some("401 Unauthorized"), None);
        let chain = FallbackProvider::new("specialist", vec![quota, auth])
            .with_unusable_accounts(vec!["diagnostic-expired — Stored OAuth token is expired".into()]);
        let error = chain.complete(request()).await.unwrap_err();
        let visible = error.to_string();
        assert!(visible.contains("diagnostic-quota — usage limit reached"), "{visible}");
        assert!(visible.contains("diagnostic-auth — authentication rejected"), "{visible}");
        assert!(visible.contains("diagnostic-expired"), "{visible}");
        assert!(!visible.contains("are exhausted"), "{visible}");
    }

    #[tokio::test]
    async fn a_fully_drained_pool_names_nvidia_not_a_generic_failure() {
        let (a, _, _) = link("nvidia:default", Some(NIM_402), None);
        let (b, _, _) = link("nvidia:2", Some(NIM_402), None);
        let chain = FallbackProvider::new("orchestrator", vec![a, b])
            .with_unusable_accounts(vec!["nvidia:3 — auth profile 'nvidia:3' not found".into()]);
        let error = chain.complete(request()).await.expect_err("pool drained");
        let text = format!("{error:#}");
        assert!(text.contains("None of the 2 loaded account(s)"), "{text}");
        // The account that never loaded is named, so "2 accounts" on a chain
        // the user configured with 3 does not send them debugging the wrong one.
        assert!(text.contains("nvidia:3"), "{text}");
        assert!(text.contains("Cloud credits expired"), "{text}");
    }
}
