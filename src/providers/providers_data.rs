//! Curated provider metadata catalog for Phoenix setup and discovery.
//!
//! Model data ported from OpenClaw's plugin manifests (life_stealing_material/deprioritized/openclaw/extensions/)

use super::live_catalogs;
use super::openrouter_catalog;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthType {
    ApiKey,
    Bearer,
    None,
}

#[derive(Debug, Clone)]
pub struct AuthMethod {
    pub method_type: &'static str,
    pub label: &'static str,
    pub env_var: Option<&'static str>,
    pub llm_supported: bool,
    pub unsupported_reason: Option<&'static str>,
    pub prompts: Vec<AuthPrompt>,
}

#[derive(Debug, Clone)]
pub struct AuthPrompt {
    pub prompt_type: &'static str,
    pub key: &'static str,
    pub message: &'static str,
    pub placeholder: Option<&'static str>,
    pub options: Vec<PromptOption>,
}

#[derive(Debug, Clone)]
pub struct PromptOption {
    pub label: &'static str,
    pub value: &'static str,
    pub hint: Option<&'static str>,
}

impl AuthMethod {
    pub fn api_key(env_var: &'static str) -> Self {
        Self {
            method_type: "api",
            label: "API Key",
            env_var: Some(env_var),
            llm_supported: true,
            unsupported_reason: None,
            prompts: vec![],
        }
    }

    pub fn oauth(label: &'static str, env_var: &'static str) -> Self {
        Self {
            method_type: "oauth",
            label,
            env_var: Some(env_var),
            llm_supported: true,
            unsupported_reason: None,
            prompts: vec![],
        }
    }

    pub fn device_code(label: &'static str, env_var: &'static str) -> Self {
        Self::device_code_id("device_code", label, env_var)
    }

    pub fn device_code_id(
        method_type: &'static str,
        label: &'static str,
        env_var: &'static str,
    ) -> Self {
        Self {
            method_type,
            label,
            env_var: Some(env_var),
            llm_supported: true,
            unsupported_reason: None,
            prompts: vec![],
        }
    }

    pub fn unsupported_for_llm(mut self, reason: &'static str) -> Self {
        self.llm_supported = false;
        self.unsupported_reason = Some(reason);
        self
    }

    pub fn none() -> Self {
        Self {
            method_type: "none",
            label: "No API Key Required",
            env_var: None,
            llm_supported: true,
            unsupported_reason: None,
            prompts: vec![],
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ProviderOptions {
    pub base_url: Option<&'static str>,
}

#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub context_window: u32,
    pub reasoning: bool,
}

#[derive(Debug, Clone)]
pub struct ProviderModels {
    pub id: &'static str,
    pub name: &'static str,
    pub base_url: &'static str,
    pub auth_type: AuthType,
    pub auth_methods: Vec<AuthMethod>,
    pub env_vars: Vec<&'static str>,
    pub options: ProviderOptions,
    pub models: Vec<ModelInfo>,
}

pub fn all_providers() -> Vec<ProviderModels> {
    vec![
        anthropic(),
        openai(),
        openai_codex(),
        openrouter(),
        tokenrouter(),
        opencode(),
        google(),
        google_gemini_cli(),
        deepseek(),
        groq(),
        minimax_portal(),
        mistral(),
        together(),
        fireworks(),
        deepinfra(),
        moonshot(),
        kimi_coding(),
        zai(),
        xai(),
        grok_cli(),
        github_copilot(),
        cerebras(),
        venice(),
        kilocode(),
        meta(),
        nvidia(),
        ollama(),
        ollama_cloud(),
        volcengine(),
        byteplus(),
        stepfun(),
        qianfan(),
        tencent(),
        xiaomi(),
        chutes(),
        sglang(),
        vllm(),
        lm_studio(),
        litellm(),
        huggingface(),
        hf_deepseek_v4_free(),
    ]
}

impl ProviderModels {
    pub fn has_model(&self, model_id: &str) -> bool {
        self.models.iter().any(|m| m.id == model_id)
    }

    pub fn supported_auth_methods(&self) -> Vec<AuthMethod> {
        self.auth_methods
            .iter()
            .filter(|method| method.llm_supported)
            .cloned()
            .collect()
    }

    pub fn find_auth_method(&self, method_type: &str) -> Option<&AuthMethod> {
        self.auth_methods.iter().find(|method| {
            method.method_type.eq_ignore_ascii_case(method_type)
                || (method_type.eq_ignore_ascii_case("api_key") && method.method_type == "api")
        })
    }
}

/// How a provider actually transmits "reasoning effort" on the wire.
///
/// This exists because effort is NOT one feature — every vendor spells it
/// differently, and the ladder a picker may offer is a property of the
/// TRANSPORT, not a constant. Handing a provider a low/medium/high ladder it
/// cannot send is how a UI ends up with a control that silently does nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffortTransport {
    /// `reasoning_effort: "<level>"` on the chat/completions body.
    OpenAiChatEffort,
    /// `reasoning: { effort: "<level>" }` on the Responses API.
    OpenAiResponsesEffort,
    /// `thinking: { type: "enabled", budget_tokens: N }` — a token budget, so
    /// the named rungs are a mapping we choose rather than an API enum.
    AnthropicThinkingBudget,
    /// `generationConfig.thinkingConfig.thinkingBudget: N` — also a budget.
    GoogleThinkingBudget,
    /// xAI takes only the two extremes; there is no middle rung to offer.
    XaiLowHigh,
    /// OpenRouter forwards `reasoning: { effort }` to whichever upstream model
    /// is selected, so the ladder is the common denominator.
    OpenRouterReasoning,
    /// Ollama's `think: true|false` — a switch, not a ladder.
    OllamaThinkBool,
    /// Newer Ollama builds accept `think: "low"|"medium"|"high"`.
    OllamaThinkLevel,
    /// The model reasons internally and exposes no knob (DeepSeek reasoner,
    /// Qwen thinking variants, most OSS reasoning models behind OpenAI-shaped
    /// gateways). Effort is not selectable and must not be offered.
    Intrinsic,
    /// No reasoning at all.
    None,
}

/// The wire format this provider uses for effort. Provider-level, because it
/// is a property of the API surface; whether a given MODEL reasons at all is a
/// separate question answered by the catalog's `reasoning` flag.
pub fn effort_transport(provider_id: &str) -> EffortTransport {
    match provider_id {
        "openai-codex" => EffortTransport::OpenAiResponsesEffort,
        "openai" | "github-copilot" | "kilocode" | "opencode" => EffortTransport::OpenAiChatEffort,
        "anthropic" => EffortTransport::AnthropicThinkingBudget,
        "google" | "google-gemini-cli" => EffortTransport::GoogleThinkingBudget,
        "xai" | "grok-cli" => EffortTransport::XaiLowHigh,
        "openrouter" => EffortTransport::OpenRouterReasoning,
        // ollama-cloud runs the same server build as local Ollama but the
        // hosted thinking models accept the named levels.
        "ollama-cloud" => EffortTransport::OllamaThinkLevel,
        "ollama" | "lm-studio" => EffortTransport::OllamaThinkBool,
        // Groq exposes reasoning_effort on its reasoning models only.
        "groq" | "cerebras" => EffortTransport::OpenAiChatEffort,
        // NIM is an OpenAI-compatible surface and its nemotron-3 line accepts
        // reasoning_effort. This sat on the Intrinsic default, so every NIM
        // model was denied a ladder wholesale — including the ones that take
        // one. Which model actually accepts it is decided per model by the
        // generated catalog, not by this line.
        "nvidia" => EffortTransport::OpenAiChatEffort,
        // Everything else that reasons does so internally with no knob.
        _ => EffortTransport::Intrinsic,
    }
}

/// Whether a provider exposes a selectable reasoning effort at all. Gates the
/// `/reasoning` command's visibility.
pub fn supports_reasoning_effort(provider_id: &str) -> bool {
    !matches!(
        effort_transport(provider_id),
        EffortTransport::Intrinsic | EffortTransport::None
    )
}

/// Whether a specific (provider, model) pick should be offered an effort
/// question in setup. Provider-wide support (Codex) always qualifies; on
/// api.openai.com only the reasoning families take the param; elsewhere the
/// catalog's per-model `reasoning` flag decides (the runtime only ever SENDS
/// the field where the API accepts it, so an over-eager pick here is stored
/// but harmless).
pub fn model_supports_effort(provider_id: &str, model_id: &str) -> bool {
    !effort_levels(provider_id, model_id).is_empty()
}

/// Does the catalog mark this specific model as a reasoning model?
///
/// Unknown models (a custom id, a freshly released one not yet in the catalog)
/// answer `true` for providers whose whole surface is reasoning-first, so a new
/// Codex model is not silently denied its effort ladder the day it ships.
fn model_reasons(provider_id: &str, model_id: &str) -> bool {
    if let Some(provider) = get_provider(provider_id) {
        if let Some(model) = provider.models.iter().find(|m| m.id == model_id) {
            return model.reasoning;
        }
    }
    matches!(provider_id, "openai-codex" | "anthropic")
}

/// The effort levels setup offers for a (provider, model) pick, weakest
/// first. Codex and OpenAI's reasoning families take the full ladder;
/// GPT-5.6 Sol and Luna expose the new "max" effort; everything else gets the
/// ladder its transport and live model declaration support.
pub fn effort_levels(provider_id: &str, model_id: &str) -> &'static [&'static str] {
    // A model that does not reason has no ladder, whatever its provider can
    // transmit. This is the check the old implementation lacked: it returned
    // low/medium/high for EVERY provider outside OpenAI, so non-reasoning
    // models were offered a control that could not do anything.
    if !model_reasons(provider_id, model_id) {
        return &[];
    }

    // GPT-6 family (Astra/Sol/Luna): the Codex /models endpoint declares
    // low..max (Sol/Astra also "ultra", which Phoenix has no rung for);
    // reasoning cannot be disabled or minimal.
    if model_id.starts_with("gpt-6-") || model_id.starts_with("gpt-6.") {
        return &["low", "medium", "high", "xhigh", "max"];
    }

    // The GPT-5.6 endpoints that expose the extra "max" rung. Keep this
    // model-specific: sending max to an older family can be accepted and then
    // silently ignored by OpenAI-shaped gateways.
    if model_id.starts_with("gpt-5.6-sol") || model_id.starts_with("gpt-5.6-luna") {
        return &["minimal", "low", "medium", "high", "xhigh", "max"];
    }

    // A NAMED-LEVEL transport can only carry an effort the model itself
    // declares — send `reasoning_effort` to a model that does not list it and
    // the provider accepts the field and drops it, which is precisely how a
    // slider ends up doing nothing. The generated catalogs record the
    // declaration straight from the live APIs, so ask them.
    //
    // BUDGET transports (Anthropic `budget_tokens`, Google `thinkingBudget`)
    // are deliberately exempt: there the rungs are ours to define and we
    // synthesise the number at send time, so an upstream catalog's opinion
    // about a named level says nothing about whether we can vary the budget.
    // `Intrinsic` is the DEFAULT arm of the transport table, so every provider
    // nobody explicitly listed inherited "this model reasons but you may not
    // say how hard" — including Venice, which publishes `supportsReasoningEffort`
    // per model, and the OpenAI-shaped gateways (DeepInfra, Chutes, HF router)
    // that pass the field straight through. That default silently zeroed the
    // ladder for hundreds of models that accept one.
    //
    // The catalog is per MODEL, so this cannot over-reach: it only fires where
    // the model itself declares the parameter. A provider with a real transport
    // of its own is untouched.
    if matches!(effort_transport(provider_id), EffortTransport::Intrinsic)
        && live_catalogs::takes_effort(provider_id, model_id) == Some(true)
    {
        return &["low", "medium", "high"];
    }

    let named_level = matches!(
        effort_transport(provider_id),
        EffortTransport::OpenAiChatEffort
            | EffortTransport::OpenAiResponsesEffort
            | EffortTransport::OpenRouterReasoning
            | EffortTransport::OllamaThinkLevel
    );
    if named_level {
        if provider_id == "openrouter" && !openrouter_catalog::takes_effort(model_id) {
            return &[];
        }
        if let Some(false) = live_catalogs::takes_effort(provider_id, model_id) {
            return &[];
        }
    }

    match effort_transport(provider_id) {
        EffortTransport::OpenAiResponsesEffort => &["minimal", "low", "medium", "high", "xhigh"],
        EffortTransport::OpenAiChatEffort => {
            // On api.openai.com the parameter is only accepted by the
            // reasoning families; other vendors reusing the OpenAI chat shape
            // take the plain three rungs.
            if provider_id == "openai" {
                let m = model_id.trim().to_ascii_lowercase();
                let reasoning_family = m.starts_with("gpt-5")
                    || m.starts_with("o1")
                    || m.starts_with("o3")
                    || m.starts_with("o4");
                if reasoning_family {
                    return &["minimal", "low", "medium", "high", "xhigh"];
                }
                return &[];
            }
            &["low", "medium", "high"]
        }
        // Budget-based transports: the rungs are ours to define, so we expose
        // the same three everywhere and map them to token budgets at send time.
        EffortTransport::AnthropicThinkingBudget
        | EffortTransport::GoogleThinkingBudget
        | EffortTransport::OpenRouterReasoning
        | EffortTransport::OllamaThinkLevel => &["low", "medium", "high"],
        // Two rungs, because that is genuinely all xAI accepts.
        EffortTransport::XaiLowHigh => &["low", "high"],
        // A boolean is not a ladder. Callers render a toggle, not a slider —
        // returning a fake three-rung ladder here is what made the Ollama
        // effort control a no-op.
        EffortTransport::OllamaThinkBool => &["low", "high"],
        EffortTransport::Intrinsic | EffortTransport::None => &[],
    }
}

/// Token budget for a named rung on the budget-based transports (Anthropic
/// `budget_tokens`, Google `thinkingBudget`). Returns None for transports that
/// send a named level instead of a number.
pub fn effort_budget_tokens(provider_id: &str, level: &str) -> Option<u32> {
    match effort_transport(provider_id) {
        EffortTransport::AnthropicThinkingBudget | EffortTransport::GoogleThinkingBudget => {
            Some(match level {
                "minimal" => 1024,
                "low" => 4096,
                "medium" => 16384,
                "high" => 32768,
                "xhigh" | "max" => 63999,
                _ => 16384,
            })
        }
        _ => None,
    }
}

/// Image-GENERATION models per provider (the `/images/generations` API —
/// distinct from the chat catalog, so image picks don't pollute chat pickers
/// and vice versa). Providers without a known images API return empty; the
/// wizard falls back to custom-ID entry there.
/// NOTE: requires an API-platform key — a ChatGPT/Codex OAuth subscription
/// token does NOT grant the images endpoint.
pub fn image_models(provider_id: &str) -> &'static [(&'static str, &'static str)] {
    match provider_id {
        "openai" => &[
            ("gpt-image-2", "GPT Image 2 (current, reasoning built in)"),
            ("gpt-image-1", "GPT Image 1 (shuts down 2026-10-23)"),
            ("dall-e-3", "DALL·E 3"),
        ],
        _ => &[],
    }
}

/// Context window (tokens) for a (provider, model) pair from the catalog.
/// None when the provider or model is not in the catalog.
pub fn context_window_for(provider_id: &str, model_id: &str) -> Option<u64> {
    // Exact hit on the named provider.
    if let Some(w) = get_provider(provider_id).and_then(|p| {
        p.models
            .iter()
            .find(|m| m.id == model_id)
            .map(|m| m.context_window as u64)
    }) {
        return Some(w);
    }

    // AGGREGATORS RE-EXPORT OTHER VENDORS' MODELS under a `vendor/model` id,
    // often with a `:free` or `:beta` tier suffix. `nvidia/nemotron-3-ultra…
    // :free` on openrouter missed every lookup, so the runtime fell back to the
    // 200k default while the model actually carries 1M — the context gauge read
    // ~5x high (a real 19% showed as 97%) and compaction triggered against a
    // window a fifth of the true size. Resolve the underlying model instead of
    // guessing.
    // EXACT id under ANY provider first. A tier suffix is not decoration: on
    // OpenRouter `nvidia/nemotron-3-ultra-550b-a55b` is 512288 tokens and the
    // `:free` tier of the same model is 1000000. Stripping the suffix to find a
    // match would confidently return the wrong number, so an exact hit
    // elsewhere in the catalog always beats a fuzzy one here.
    if let Some(w) = all_providers()
        .into_iter()
        .flat_map(|p| p.models)
        .find(|m| m.id == model_id)
        .map(|m| m.context_window as u64)
    {
        return Some(w);
    }

    // Only now fall back to resolving the underlying model, for aggregators
    // whose catalog we do not carry model-by-model.
    let base = model_id.split(':').next().unwrap_or(model_id);
    let leaf = base.rsplit('/').next().unwrap_or(base);
    all_providers()
        .into_iter()
        .flat_map(|p| p.models)
        .find(|m| m.id == base || m.id == leaf)
        .map(|m| m.context_window as u64)
}

pub fn get_provider(id: &str) -> Option<ProviderModels> {
    all_providers().into_iter().find(|p| p.id == id)
}

pub fn get_all_model_ids() -> Vec<&'static str> {
    all_providers()
        .into_iter()
        .flat_map(|p| p.models.into_iter().map(|m| m.id))
        .collect()
}

pub fn get_providers_by_auth(auth_type: AuthType) -> Vec<ProviderModels> {
    all_providers()
        .into_iter()
        .filter(|p| p.auth_type == auth_type)
        .collect()
}

pub fn recommended_providers() -> Vec<ProviderModels> {
    let ids = [
        "anthropic",
        "openai",
        "openrouter",
        "tokenrouter",
        "opencode",
        "google",
        "deepseek",
        "groq",
        "mistral",
        "ollama",
        "ollama-cloud",
    ];
    ids.iter().filter_map(|id| get_provider(id)).collect()
}

pub fn recommended_model(provider_id: &str) -> &'static str {
    match provider_id {
        "anthropic" => "claude-sonnet-4-5",
        // Terra = the balanced 5.6 default; Sol (flagship) is one row up in
        // every picker for the lanes that want the ceiling.
        "openai" | "openai-codex" => "gpt-5.6-terra",
        // A free tier that carries tools, reasoning AND a selectable effort —
        // this project runs on free lanes, so recommending a paid flagship here
        // just sends every new account straight into a 402.
        "openrouter" => "nvidia/nemotron-3-ultra-550b-a55b:free",
        "tokenrouter" => "moonshotai/kimi-k3-free",
        "opencode" => "claude-sonnet-4-6",
        "google" => "gemini-2.5-flash",
        "deepseek" => "deepseek-chat",
        "groq" => "llama-3.3-70b-versatile",
        "mistral" => "mistral-large-latest",
        "together" => "meta-llama/Llama-3.3-70B-Instruct-Turbo",
        "fireworks" => "accounts/fireworks/models/kimi-k2p6",
        "deepinfra" => "deepseek-ai/DeepSeek-V3.2",
        "moonshot" => "kimi-k3",
        "kimi-coding" => "kimi-for-coding",
        "zai" => "glm-4.7",
        "xai" => "grok-4.5",
        "grok-cli" => "grok-4.5",
        "github-copilot" => "claude-sonnet-4.6",
        "cerebras" => "zai-glm-4.7",
        "venice" => "llama-3.3-70b",
        "kilocode" => "kilo/auto",
        "meta" => "llama-4-maverick",
        "nvidia" => "nvidia/nemotron-3-super-120b-a12b",
        "ollama" => "qwen2.5-coder:32b",
        "ollama-cloud" => "gpt-oss:20b",
        "volcengine" => "doubao-seed-1-8-251228",
        "byteplus" => "seed-1-8-251228",
        "stepfun" => "step-3.5-flash",
        "qianfan" => "deepseek-v3.2",
        "tencent" => "hy3-preview",
        "xiaomi" => "mimo-v2-pro",
        "chutes" => "Qwen/Qwen3-32B",
        "sglang" => "",
        "vllm" => "",
        "lm_studio" => "",
        "litellm" => "",
        "huggingface" => "",
        // Single-model endpoint — the recommendation IS the only model.
        "hf-deepseek-v4-free" => "deepseek-ai/DeepSeek-V4-Flash-0731",
        _ => "",
    }
}

fn m(id: &'static str, name: &'static str, ctx: u32, reasoning: bool) -> ModelInfo {
    ModelInfo {
        id,
        name,
        context_window: ctx,
        reasoning,
    }
}

/// Generated catalog rows win over the hand-written list; anything curated that
/// the generator did not cover is kept and appended.
///
/// Merged rather than replaced on purpose. The generated tables are sourced
/// from live APIs and are right about windows and capabilities, but they only
/// cover what those APIs return — a model someone already has in `config.toml`
/// must not vanish from the picker because an upstream catalog dropped it.
fn merged(provider_id: &str, curated: Vec<ModelInfo>) -> Vec<ModelInfo> {
    let Some(table) = live_catalogs::table_for(provider_id) else {
        return curated;
    };
    let mut out: Vec<ModelInfo> = table
        .iter()
        .map(|&(id, name, ctx, reasoning, _, _, _)| m(id, name, ctx, reasoning))
        .collect();
    for c in curated {
        if !out.iter().any(|x| x.id == c.id) {
            out.push(c);
        }
    }
    out
}

fn anthropic() -> ProviderModels {
    ProviderModels {
        id: "anthropic",
        name: "Anthropic",
        base_url: "https://api.anthropic.com/v1",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![
            AuthMethod::api_key("ANTHROPIC_API_KEY"),
            AuthMethod::oauth("Claude CLI OAuth", "ANTHROPIC_OAUTH_TOKEN").unsupported_for_llm(
                "Claude CLI OAuth is not a verified Anthropic API credential path for Phoenix LLM requests.",
            ),
        ],
        env_vars: vec!["ANTHROPIC_API_KEY"],
        options: ProviderOptions::default(),
        models: merged("anthropic", vec![
            m("claude-sonnet-4-6", "Claude Sonnet 4.6", 1_000_000, true),
            m("claude-sonnet-4-5", "Claude Sonnet 4.5", 1_000_000, true),
            m("claude-sonnet-4", "Claude Sonnet 4", 1_000_000, true),
            m("claude-opus-4-6", "Claude Opus 4.6", 1_000_000, true),
            m("claude-opus-4-5", "Claude Opus 4.5", 200000, true),
            m("claude-haiku-3-5", "Claude Haiku 3.5", 200000, false),
            m("claude-3-7-sonnet", "Claude 3.7 Sonnet", 200000, true),
            m("claude-3-5-haiku", "Claude 3.5 Haiku", 200000, false),
        ]),
    }
}

fn openai() -> ProviderModels {
    ProviderModels {
        id: "openai",
        name: "OpenAI",
        base_url: "https://api.openai.com/v1",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![AuthMethod::api_key("OPENAI_API_KEY")],
        env_vars: vec!["OPENAI_API_KEY"],
        options: ProviderOptions::default(),
        models: merged(
            "openai",
            vec![
                // https://developers.openai.com/api/docs/models/gpt-6-astra
                // Verified 2026-09-04; account entitlement remains provider-enforced.
                m("gpt-6-astra", "GPT-6 Astra", 1_050_000, true),
                // Codex /models listed Sol + Luna 2026-09-22 (272k route window).
                // GPT-6.1 Sol, added 2026-09-29 per the user: 1.05M window.
                m("gpt-6.1-sol", "GPT-6.1 Sol", 1_050_000, true),
                m("gpt-6-sol", "GPT-6 Sol", 1_050_000, true),
                m("gpt-6-luna", "GPT-6 Luna", 1_050_000, true),
                // Model capacity; a selected route may request a smaller window.
                m("gpt-5.6-sol", "GPT-5.6 Sol", 1_050_000, true),
                m("gpt-5.6-terra", "GPT-5.6 Terra", 1_050_000, true),
                m("gpt-5.6-luna", "GPT-5.6 Luna", 1_050_000, true),
                m("gpt-5.5", "GPT-5.5", 1_050_000, true),
                m("gpt-5.4", "GPT-5.4", 1_050_000, true),
                m("gpt-5.4-pro", "GPT-5.4 Pro", 1_050_000, true),
                m("gpt-5.4-mini", "GPT-5.4 mini", 400000, true),
                m("gpt-5.4-nano", "GPT-5.4 nano", 400000, true),
                m("gpt-5.3-codex", "GPT-5.3 Codex", 400000, true),
                m("gpt-5.2", "GPT-5.2", 400000, true),
                m("gpt-5.2-codex", "GPT-5.2 Codex", 400000, true),
                m("gpt-5.1", "GPT-5.1", 400000, true),
                m("gpt-5", "GPT-5", 400000, true),
                m("gpt-5-mini", "GPT-5 Mini", 400000, true),
                m("gpt-5-nano", "GPT-5 Nano", 400000, true),
                m("gpt-5-pro", "GPT-5 Pro", 400000, true),
                m("gpt-4.1", "GPT-4.1", 1047576, false),
                m("gpt-4.1-mini", "GPT-4.1 mini", 1047576, false),
                m("gpt-4.1-nano", "GPT-4.1 nano", 1047576, false),
                m("gpt-4o", "GPT-4o", 128000, false),
                m("gpt-4o-mini", "GPT-4o mini", 128000, false),
                // Audio lanes are selectable from the same provider catalog, but
                // are consumed by Canvas voice commands rather than the text-agent
                // completion loop.
                m("gpt-audio-1.5", "GPT Audio 1.5", 128000, false),
                m("gpt-realtime-2.1", "GPT Realtime 2.1", 128000, false),
                m(
                    "gpt-realtime-2.1-mini",
                    "GPT Realtime 2.1 mini",
                    128000,
                    false,
                ),
                m("gpt-realtime-2", "GPT Realtime 2", 128000, false),
                m("gpt-realtime-1.5", "GPT Realtime 1.5", 128000, false),
                m(
                    "gpt-realtime-whisper",
                    "GPT Realtime Whisper",
                    128000,
                    false,
                ),
                m("gpt-4o-transcribe", "GPT-4o Transcribe", 16000, false),
                m(
                    "gpt-4o-mini-transcribe",
                    "GPT-4o mini Transcribe",
                    16000,
                    false,
                ),
                m("tts-1", "TTS-1", 4096, false),
                m("tts-1-hd", "TTS-1 HD", 4096, false),
                m("o4-mini", "o4-mini", 200000, true),
                m("o3", "o3", 200000, true),
                m("o3-mini", "o3-mini", 200000, true),
                m("o3-pro", "o3-pro", 200000, true),
                m("o1", "o1", 200000, true),
                m("o1-pro", "o1-pro", 200000, true),
            ],
        ),
    }
}

fn openai_codex() -> ProviderModels {
    ProviderModels {
        id: "openai-codex",
        name: "OpenAI Codex",
        base_url: "https://chatgpt.com/backend-api/codex",
        auth_type: AuthType::Bearer,
        auth_methods: vec![
            AuthMethod::oauth("OpenAI Codex OAuth", "OPENAI_OAUTH_TOKEN"),
            AuthMethod::device_code("OpenAI Codex Device Code", "OPENAI_OAUTH_TOKEN"),
        ],
        env_vars: vec!["OPENAI_OAUTH_TOKEN"],
        options: ProviderOptions::default(),
        models: vec![
            // Astra (2026-09-28) and Sol (2026-09-25) serve up to 1.05M on
            // this route per the user, although `codex/models` reported 272k
            // on 2026-09-22. If the route still stops at 272k, the overflow
            // recovery folds and retries instead of failing the turn.
            m("gpt-6-astra", "GPT-6 Astra", 1_050_000, true),
            m("gpt-6.1-sol", "GPT-6.1 Sol", 1_050_000, true),
            m("gpt-6-sol", "GPT-6 Sol", 1_050_000, true),
            m("gpt-6-luna", "GPT-6 Luna", 272_000, true),
            m("gpt-5.6-sol", "GPT-5.6 Sol", 272_000, true),
            m("gpt-5.6-terra", "GPT-5.6 Terra", 272_000, true),
            m("gpt-5.6-luna", "GPT-5.6 Luna", 272_000, true),
            m("gpt-5.5", "GPT-5.5", 256000, true),
            m("gpt-5.5-pro", "GPT-5.5 Pro", 256000, true),
            m("gpt-5.4", "GPT-5.4", 256000, true),
            m("gpt-5.4-pro", "GPT-5.4 Pro", 256000, true),
            m("gpt-5.4-mini", "GPT-5.4 mini", 256000, true),
            m("gpt-5.3-codex", "GPT-5.3 Codex", 256000, true),
            m("gpt-5.2-codex", "GPT-5.2 Codex", 256000, true),
        ],
    }
}

fn openrouter() -> ProviderModels {
    ProviderModels {
        id: "openrouter",
        name: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![AuthMethod::api_key("OPENROUTER_API_KEY")],
        env_vars: vec!["OPENROUTER_API_KEY"],
        options: ProviderOptions::default(),
        // The FULL live catalog, not a shortlist. A hand-written excerpt is
        // what left `nvidia/nemotron-3-ultra-550b-a55b:free` — the model this
        // project actually runs on — absent from every lookup: no context
        // window (so the gauge read ~5x high off the 200k default), and no
        // `reasoning` flag (so the effort control greyed itself out saying the
        // model "takes no reasoning effort" when it takes one just fine).
        models: openrouter_catalog::OPENROUTER_MODELS
            .iter()
            .map(|&(id, name, ctx, reasoning, _, _, _)| m(id, name, ctx, reasoning))
            .collect(),
    }
}

fn tokenrouter() -> ProviderModels {
    ProviderModels {
        id: "tokenrouter",
        name: "TokenRouter",
        base_url: "https://api.tokenrouter.com/v1",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![AuthMethod::api_key("TOKENROUTER_API_KEY")],
        env_vars: vec!["TOKENROUTER_API_KEY"],
        options: ProviderOptions::default(),
        // TokenRouter's public catalog is key-gated. Keep the requested free
        // route as a first-class choice instead of inventing a stale mirror of
        // its fast-moving catalog.
        models: vec![m(
            "moonshotai/kimi-k3-free",
            "Kimi K3 (Free)",
            1_048_576,
            true,
        )],
    }
}

fn opencode() -> ProviderModels {
    ProviderModels {
        id: "opencode",
        name: "OpenCode Zen",
        base_url: "https://opencode.ai/zen/v1",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![AuthMethod::api_key("OPENCODE_API_KEY")],
        env_vars: vec!["OPENCODE_API_KEY"],
        options: ProviderOptions::default(),
        models: merged(
            "opencode",
            vec![
                m("claude-sonnet-4-6", "Claude Sonnet 4.6", 200000, true),
                m("claude-sonnet-4-5", "Claude Sonnet 4.5", 200000, true),
                m("gpt-5.5", "GPT-5.5", 272000, true),
                m("gpt-5.4-mini", "GPT-5.4 mini", 400000, true),
                m("gpt-5.3-codex", "GPT-5.3 Codex", 400000, true),
                m("gemini-3.1-pro", "Gemini 3.1 Pro", 1000000, true),
                m("qwen3.6-plus", "Qwen3.6 Plus", 131072, true),
                m("minimax-m2.5-free", "MiniMax M2.5 Free", 196608, true),
                m(
                    "deepseek-v4-flash-free",
                    "DeepSeek V4 Flash Free",
                    1000000,
                    true,
                ),
                m("big-pickle", "Big Pickle", 128000, false),
            ],
        ),
    }
}

fn google() -> ProviderModels {
    ProviderModels {
        id: "google",
        name: "Google Gemini",
        base_url: "https://generativelanguage.googleapis.com/v1beta",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![AuthMethod::api_key("GOOGLE_API_KEY")],
        env_vars: vec!["GOOGLE_API_KEY"],
        options: ProviderOptions::default(),
        models: merged(
            "google",
            vec![
                m("gemini-3.1-pro", "Gemini 3.1 Pro", 1048576, true),
                m("gemini-3.0-pro", "Gemini 3 Pro", 1048576, true),
                m("gemini-2.5-pro", "Gemini 2.5 Pro", 1048576, true),
                m("gemini-2.5-flash", "Gemini 2.5 Flash", 1048576, true),
                m("gemini-2.0-flash", "Gemini 2.0 Flash", 1048576, false),
            ],
        ),
    }
}

fn google_gemini_cli() -> ProviderModels {
    ProviderModels {
        id: "google-gemini-cli",
        name: "Gemini CLI OAuth",
        base_url: "https://cloudcode-pa.googleapis.com",
        auth_type: AuthType::Bearer,
        auth_methods: vec![AuthMethod::oauth("Google OAuth", "GEMINI_CLI_OAUTH_TOKEN")],
        env_vars: vec![
            "GEMINI_CLI_OAUTH_TOKEN",
            "OPENCLAW_GEMINI_OAUTH_CLIENT_ID",
            "OPENCLAW_GEMINI_OAUTH_CLIENT_SECRET",
            "GEMINI_CLI_OAUTH_CLIENT_ID",
            "GEMINI_CLI_OAUTH_CLIENT_SECRET",
        ],
        options: ProviderOptions::default(),
        models: merged(
            "google-gemini-cli",
            vec![
                m(
                    "google/gemini-3.1-pro-preview",
                    "Gemini 3.1 Pro Preview",
                    1048576,
                    true,
                ),
                m("google/gemini-2.5-pro", "Gemini 2.5 Pro", 1048576, true),
                m("google/gemini-2.5-flash", "Gemini 2.5 Flash", 1048576, true),
            ],
        ),
    }
}

fn deepseek() -> ProviderModels {
    ProviderModels {
        id: "deepseek",
        name: "DeepSeek",
        base_url: "https://api.deepseek.com/v1",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![AuthMethod::api_key("DEEPSEEK_API_KEY")],
        env_vars: vec!["DEEPSEEK_API_KEY"],
        options: ProviderOptions::default(),
        models: merged(
            "deepseek",
            vec![
                m("deepseek-v4-flash", "DeepSeek V4 Flash", 1000000, true),
                m("deepseek-v4-pro", "DeepSeek V4 Pro", 1000000, true),
                m("deepseek-chat", "DeepSeek Chat (V3)", 131072, false),
                m("deepseek-reasoner", "DeepSeek Reasoner (R1)", 131072, true),
            ],
        ),
    }
}

fn groq() -> ProviderModels {
    ProviderModels {
        id: "groq",
        name: "Groq (LPU Inference)",
        base_url: "https://api.groq.com/openai/v1",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![AuthMethod::api_key("GROQ_API_KEY")],
        env_vars: vec!["GROQ_API_KEY"],
        options: ProviderOptions::default(),
        models: merged(
            "groq",
            vec![
                m("llama-3.3-70b-versatile", "Llama 3.3 70B", 131072, false),
                m("llama-3.1-8b-instant", "Llama 3.1 8B", 131072, false),
                // Speech-to-text (free tier). Only the Speech to text lane
                // offers these; the text lanes filter them out.
                m("whisper-large-v3-turbo", "Whisper Large v3 Turbo", 448, false),
                m("whisper-large-v3", "Whisper Large v3", 448, false),
                m(
                    "deepseek-r1-distill-llama-70b",
                    "DeepSeek R1 Distill Llama 70B",
                    131072,
                    true,
                ),
                m("qwen-qwq-32b", "Qwen QwQ 32B", 131072, true),
                m("qwen/qwen3-32b", "Qwen3 32B", 131072, true),
                m("mistral-saba-24b", "Mistral Saba 24B", 32768, false),
                m(
                    "meta-llama/llama-4-maverick-17b-128e-instruct",
                    "Llama 4 Maverick 17B",
                    131072,
                    false,
                ),
                m(
                    "meta-llama/llama-4-scout-17b-16e-instruct",
                    "Llama 4 Scout 17B",
                    131072,
                    false,
                ),
            ],
        ),
    }
}

fn mistral() -> ProviderModels {
    ProviderModels {
        id: "mistral",
        name: "Mistral AI",
        base_url: "https://api.mistral.ai/v1",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![AuthMethod::api_key("MISTRAL_API_KEY")],
        env_vars: vec!["MISTRAL_API_KEY"],
        options: ProviderOptions::default(),
        models: merged(
            "mistral",
            vec![
                m("mistral-large-latest", "Mistral Large", 262144, false),
                m("mistral-small-latest", "Mistral Small", 128000, true),
                m("mistral-medium-3-5", "Mistral Medium 3.5", 262144, true),
                m("codestral-latest", "Codestral", 256000, false),
                m("pixtral-large-latest", "Pixtral Large", 128000, false),
                m("magistral-small", "Magistral Small", 128000, true),
            ],
        ),
    }
}

fn minimax_portal() -> ProviderModels {
    ProviderModels {
        id: "minimax-portal",
        name: "MiniMax Portal",
        base_url: "https://api.minimax.io/v1",
        auth_type: AuthType::Bearer,
        auth_methods: vec![
            AuthMethod::device_code("MiniMax OAuth (Global)", "MINIMAX_OAUTH_TOKEN"),
            AuthMethod::device_code_id(
                "device_code_cn",
                "MiniMax OAuth (CN)",
                "MINIMAX_OAUTH_TOKEN",
            ),
        ],
        env_vars: vec!["MINIMAX_OAUTH_TOKEN", "MINIMAX_API_KEY"],
        options: ProviderOptions::default(),
        models: merged(
            "minimax-portal",
            vec![
                // M3 advertises up to 1M context with 512K guaranteed — use the
                // guarantee so auto-compaction never overruns the real window.
                m("MiniMax-M3", "MiniMax M3", 524288, true),
                m("MiniMax-M2.7", "MiniMax M2.7", 204800, true),
                m("MiniMax-M2.5", "MiniMax M2.5", 196608, true),
            ],
        ),
    }
}

fn together() -> ProviderModels {
    ProviderModels {
        id: "together",
        name: "Together AI",
        base_url: "https://api.together.xyz/v1",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![AuthMethod::api_key("TOGETHER_API_KEY")],
        env_vars: vec!["TOGETHER_API_KEY"],
        options: ProviderOptions::default(),
        models: merged(
            "together",
            vec![
                m(
                    "meta-llama/Llama-3.3-70B-Instruct-Turbo",
                    "Llama 3.3 70B Turbo",
                    131072,
                    false,
                ),
                m(
                    "meta-llama/Llama-4-Scout-17B-16E-Instruct",
                    "Llama 4 Scout 17B",
                    10000000,
                    false,
                ),
                m(
                    "meta-llama/Llama-4-Maverick-17B-128E-Instruct-FP8",
                    "Llama 4 Maverick 17B",
                    20000000,
                    false,
                ),
                m("deepseek-ai/DeepSeek-V3.1", "DeepSeek V3.1", 131072, false),
                m("deepseek-ai/DeepSeek-R1", "DeepSeek R1", 131072, true),
                m("moonshotai/Kimi-K2.5", "Kimi K2.5", 262144, true),
                m(
                    "moonshotai/Kimi-K2-Instruct-0905",
                    "Kimi K2 Instruct 0905",
                    262144,
                    false,
                ),
            ],
        ),
    }
}

fn fireworks() -> ProviderModels {
    ProviderModels {
        id: "fireworks",
        name: "Fireworks AI",
        base_url: "https://api.fireworks.ai/inference/v1",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![AuthMethod::api_key("FIREWORKS_API_KEY")],
        env_vars: vec!["FIREWORKS_API_KEY"],
        options: ProviderOptions::default(),
        models: merged(
            "fireworks",
            vec![
                m(
                    "accounts/fireworks/models/kimi-k2p6",
                    "Kimi K2.6",
                    262144,
                    false,
                ),
                m(
                    "accounts/fireworks/routers/kimi-k2p5-turbo",
                    "Kimi K2.5 Turbo",
                    256000,
                    false,
                ),
            ],
        ),
    }
}

fn deepinfra() -> ProviderModels {
    ProviderModels {
        id: "deepinfra",
        name: "DeepInfra",
        base_url: "https://api.deepinfra.com/v1/openai",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![AuthMethod::api_key("DEEPINFRA_API_KEY")],
        env_vars: vec!["DEEPINFRA_API_KEY"],
        options: ProviderOptions::default(),
        models: merged(
            "deepinfra",
            vec![
                m("deepseek-ai/DeepSeek-V3.2", "DeepSeek V3.2", 163840, false),
                m("zai-org/GLM-5.1", "GLM-5.1", 202752, true),
                m("moonshotai/Kimi-K2.5", "Kimi K2.5", 262144, true),
                m("stepfun-ai/Step-3.5-Flash", "Step 3.5 Flash", 262144, false),
                m("MiniMaxAI/MiniMax-M2.5", "MiniMax M2.5", 196608, true),
                m(
                    "meta-llama/Llama-3.3-70B-Instruct-Turbo",
                    "Llama 3.3 70B",
                    131072,
                    false,
                ),
            ],
        ),
    }
}

/// Kimi SUBSCRIPTION lane ("Kimi for Coding", 2026): plan members mint up to 5
/// keys in the Kimi Code Console; those keys only work against the coding
/// endpoint, not the pay-per-token platform. OpenAI-compatible. With K3's
/// launch (2026-07-16) the coding alias serves the current flagship.
fn kimi_coding() -> ProviderModels {
    ProviderModels {
        id: "kimi-coding",
        name: "Kimi for Coding (subscription)",
        base_url: "https://api.kimi.com/coding/v1",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![AuthMethod::api_key("KIMI_CODING_API_KEY")],
        env_vars: vec!["KIMI_CODING_API_KEY", "KIMI_API_KEY"],
        options: ProviderOptions::default(),
        models: merged(
            "kimi-coding",
            vec![
                m(
                    "kimi-for-coding",
                    "Kimi for Coding (K3-class)",
                    262144,
                    true,
                ),
                m(
                    "kimi-for-coding-highspeed",
                    "Kimi for Coding HighSpeed",
                    262144,
                    true,
                ),
            ],
        ),
    }
}

fn moonshot() -> ProviderModels {
    ProviderModels {
        id: "moonshot",
        name: "Moonshot AI (Kimi)",
        base_url: "https://api.moonshot.ai/v1",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![AuthMethod::api_key("MOONSHOT_API_KEY")],
        env_vars: vec!["MOONSHOT_API_KEY", "KIMI_API_KEY"],
        options: ProviderOptions::default(),
        models: merged(
            "moonshot",
            vec![
                // K3 (released 2026-07-16): Moonshot's flagship — 1M-token context
                // (Kimi Delta Attention), native vision, agentic/reasoning tier.
                m("kimi-k3", "Kimi K3", 1_048_576, true),
                m("kimi-k2.6", "Kimi K2.6", 262144, false),
                m("kimi-k2.5", "Kimi K2.5", 262144, false),
                m("kimi-k2-thinking", "Kimi K2 Thinking", 262144, true),
                m("kimi-k2-turbo", "Kimi K2 Turbo (free)", 256000, false),
            ],
        ),
    }
}

fn zai() -> ProviderModels {
    ProviderModels {
        id: "zai",
        name: "Z.AI (GLM)",
        base_url: "https://api.z.ai/api/paas/v4",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![AuthMethod::api_key("ZAI_API_KEY")],
        env_vars: vec!["ZAI_API_KEY", "Z_AI_API_KEY"],
        options: ProviderOptions::default(),
        models: merged(
            "zai",
            vec![
                m("glm-5.1", "GLM-5.1", 202800, true),
                m("glm-5", "GLM-5", 202800, true),
                m("glm-4.7", "GLM-4.7", 204800, true),
                m("glm-4.7-flash", "GLM-4.7 Flash", 200000, true),
                m("glm-4.6", "GLM-4.6", 204800, true),
                m("glm-4.5-flash", "GLM-4.5 Flash (free)", 131072, true),
            ],
        ),
    }
}

fn xai() -> ProviderModels {
    ProviderModels {
        id: "xai",
        name: "xAI (Grok)",
        base_url: "https://api.x.ai/v1",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![
            // SuperGrok / X Premium subscription login — PKCE flow against
            // accounts.x.ai; the bearer rides the same api.x.ai/v1 surface.
            AuthMethod::oauth("xAI Grok OAuth (SuperGrok / X Premium)", "XAI_OAUTH_TOKEN"),
            AuthMethod::api_key("XAI_API_KEY"),
        ],
        env_vars: vec!["XAI_API_KEY", "XAI_OAUTH_TOKEN"],
        options: ProviderOptions::default(),
        models: merged(
            "xai",
            vec![
                // docs.x.ai 2026-07-11: grok-4.5 = "the most intelligent and
                // fastest model we've built", 500k window; grok-4.3 carries 1M.
                m("grok-4.5", "Grok 4.5", 500_000, true),
                m("grok-4.3", "Grok 4.3", 1_000_000, true),
                m("grok-4", "Grok 4", 131072, true),
                m("grok-3", "Grok 3", 131072, true),
                m("grok-3-mini", "Grok 3 Mini", 131072, true),
                m("grok-2", "Grok 2", 131072, false),
            ],
        ),
    }
}

/// xAI Grok via the SuperGrok CLI proxy — the subscription lane that actually
/// works ($30 SuperGrok). Same OIDC login as `xai` (identical client), but the
/// bearer rides `cli-chat-proxy.grok.com` (Grok Build's real endpoint) instead
/// of the tier-gated `api.x.ai`. Models mirror the live proxy `/v1/models`.
fn grok_cli() -> ProviderModels {
    ProviderModels {
        id: "grok-cli",
        name: "xAI Grok (SuperGrok)",
        base_url: crate::providers::grok_cli::PROXY_BASE_URL,
        auth_type: AuthType::Bearer,
        auth_methods: vec![
            // SuperGrok / X Premium subscription login — the SAME PKCE OIDC flow
            // as `xai` (client b1a00492, grok-cli:access scope); here the token
            // is used against the CLI proxy, where the subscription is honored.
            AuthMethod::oauth("SuperGrok subscription (grok login)", "XAI_OAUTH_TOKEN"),
        ],
        env_vars: vec!["XAI_OAUTH_TOKEN"],
        options: ProviderOptions::default(),
        models: merged(
            "grok-cli",
            vec![
                m("grok-4.5", "Grok 4.5", 500_000, true),
                m(
                    "grok-composer-2.5-fast",
                    "Composer 2.5 (fast)",
                    200_000,
                    false,
                ),
            ],
        ),
    }
}

fn github_copilot() -> ProviderModels {
    ProviderModels {
        id: "github-copilot",
        name: "GitHub Copilot",
        base_url: "https://api.individual.githubcopilot.com",
        auth_type: AuthType::Bearer,
        auth_methods: vec![
            AuthMethod::device_code("GitHub Device Login", "COPILOT_GITHUB_TOKEN"),
            AuthMethod::api_key("COPILOT_GITHUB_TOKEN"),
        ],
        env_vars: vec!["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"],
        options: ProviderOptions::default(),
        models: merged(
            "github-copilot",
            vec![
                m("claude-sonnet-4.6", "Claude Sonnet 4.6", 128000, true),
                m("claude-sonnet-4.5", "Claude Sonnet 4.5", 128000, true),
                m("claude-opus-4.6", "Claude Opus 4.6", 128000, true),
                // GPT-5.6 in Copilot per the 2026-07-09 GitHub changelog.
                m("gpt-5.6-sol", "GPT-5.6 Sol", 128000, true),
                m("gpt-5.6-terra", "GPT-5.6 Terra", 128000, true),
                m("gpt-5.6-luna", "GPT-5.6 Luna", 128000, true),
                m("gpt-5.5", "GPT-5.5", 400000, true),
                m("gpt-5.4", "GPT-5.4", 128000, true),
                m("gpt-5.3-codex", "GPT-5.3 Codex", 128000, true),
                m("gpt-5.2-codex", "GPT-5.2 Codex", 128000, true),
                m("gpt-5-mini", "GPT-5 Mini", 128000, true),
                m("gemini-3.1-pro", "Gemini 3.1 Pro", 128000, true),
                m("gemini-2.5-pro", "Gemini 2.5 Pro", 128000, true),
            ],
        ),
    }
}

fn cerebras() -> ProviderModels {
    ProviderModels {
        id: "cerebras",
        name: "Cerebras",
        base_url: "https://api.cerebras.ai/v1",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![AuthMethod::api_key("CEREBRAS_API_KEY")],
        env_vars: vec!["CEREBRAS_API_KEY"],
        options: ProviderOptions::default(),
        models: merged(
            "cerebras",
            vec![
                m("zai-glm-4.7", "GLM 4.7", 128000, true),
                m("gpt-oss-120b", "GPT OSS 120B", 128000, true),
                m(
                    "qwen-3-235b-a22b-instruct-2507",
                    "Qwen3 235B",
                    128000,
                    false,
                ),
                m("llama3.1-8b", "Llama 3.1 8B", 128000, false),
            ],
        ),
    }
}

fn venice() -> ProviderModels {
    ProviderModels {
        id: "venice",
        name: "Venice AI",
        base_url: "https://api.venice.ai/api/v1",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![AuthMethod::api_key("VENICE_API_KEY")],
        env_vars: vec!["VENICE_API_KEY"],
        options: ProviderOptions::default(),
        models: merged(
            "venice",
            vec![
                m("llama-3.3-70b", "Llama 3.3 70B", 128000, false),
                m(
                    "qwen3-235b-a22b-thinking-2507",
                    "Qwen3 235B Thinking",
                    128000,
                    true,
                ),
                m("kimi-k2-5", "Kimi K2.5", 256000, true),
                m("deepseek-v3.2", "DeepSeek V3.2", 160000, true),
            ],
        ),
    }
}

fn kilocode() -> ProviderModels {
    ProviderModels {
        id: "kilocode",
        name: "KiloCode",
        base_url: "https://api.kilo.ai/api/gateway/",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![AuthMethod::api_key("KILOCODE_API_KEY")],
        env_vars: vec!["KILOCODE_API_KEY"],
        options: ProviderOptions {
            base_url: Some("https://api.kilo.ai/api/gateway/"),
        },
        models: merged(
            "kilocode",
            vec![m("kilo/auto", "Kilo Auto (multi-modal)", 1000000, true)],
        ),
    }
}

/// NVIDIA NIM — the hosted OpenAI-compatible surface at build.nvidia.com.
///
/// Every id below came from the LIVE catalog, not from memory:
/// `GET https://integrate.api.nvidia.com/v1/models` needs no auth and returned
/// 118 models on 2026-07-25. Repeat that check before editing this list,
/// because three of the four ids this function used to carry DO NOT EXIST and
/// would 404 on first use — `moonshotai/kimi-k2.5` (real id is `k2.6`),
/// `minimaxai/minimax-m2.5` (`m2.7`/`m3`), and `z-ai/glm5` (`z-ai/glm-5.2`).
/// A catalog entry naming a model the provider does not serve is worse than a
/// missing one: setup succeeds, and then every turn fails.
///
/// NIM matters here beyond being one more provider. Accounts are free and each
/// carries its own inference credits, so several NIM profiles (`nvidia:1`,
/// `nvidia:2`, …) on different accounts are a legitimate way to hold real
/// capacity — which is exactly what the account-fallback chain in
/// `factory::wrap_with_fallback_chain` exists to rotate through.
fn meta() -> ProviderModels {
    ProviderModels {
        id: "meta",
        name: "Meta Llama",
        // api.llama.com ships an OpenAI-compatible surface at /compat/v1, so
        // this rides the same openai_compat client as everything else.
        base_url: "https://api.llama.com/compat/v1",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![AuthMethod::api_key("LLAMA_API_KEY")],
        env_vars: vec!["LLAMA_API_KEY"],
        options: ProviderOptions {
            base_url: Some("https://api.llama.com/compat/v1"),
        },
        // Llama was reachable only second-hand before this — through OpenRouter,
        // Together, NIM — so "which Llama models exist" had no single answer in
        // the catalog. The generated table supplies them.
        models: merged("meta", vec![]),
    }
}

fn nvidia() -> ProviderModels {
    ProviderModels {
        id: "nvidia",
        name: "NVIDIA NIM",
        base_url: "https://integrate.api.nvidia.com/v1",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![AuthMethod::api_key("NVIDIA_API_KEY")],
        env_vars: vec!["NVIDIA_API_KEY"],
        options: ProviderOptions::default(),
        models: merged(
            "nvidia",
            vec![
                // Frontier-class, in rough order of capability.
                m(
                    "nvidia/nemotron-3-ultra-550b-a55b",
                    "Nemotron 3 Ultra 550B",
                    262144,
                    true,
                ),
                m("qwen/qwen3.5-397b-a17b", "Qwen 3.5 397B", 262144, true),
                m(
                    "nvidia/nemotron-3-super-120b-a12b",
                    "Nemotron 3 Super 120B",
                    262144,
                    true,
                ),
                // GLM-5.2 is the model this project ran on ollama-cloud, so a
                // config that already names it keeps its behaviour on NIM.
                m("z-ai/glm-5.2", "GLM-5.2", 202752, true),
                m("moonshotai/kimi-k2.6", "Kimi K2.6", 262144, true),
                m(
                    "deepseek-ai/deepseek-v4-pro",
                    "DeepSeek V4 Pro",
                    163840,
                    true,
                ),
                m(
                    "deepseek-ai/deepseek-v4-flash",
                    "DeepSeek V4 Flash",
                    163840,
                    true,
                ),
                m("minimaxai/minimax-m3", "MiniMax M3", 204800, true),
                m("minimaxai/minimax-m2.7", "MiniMax M2.7", 204800, true),
                m("openai/gpt-oss-120b", "GPT-OSS 120B", 131072, true),
                m(
                    "mistralai/mistral-medium-3.5-128b",
                    "Mistral Medium 3.5",
                    131072,
                    false,
                ),
                m(
                    "meta/llama-3.3-70b-instruct",
                    "Llama 3.3 70B",
                    131072,
                    false,
                ),
                m(
                    "meta/llama-4-maverick-17b-128e-instruct",
                    "Llama 4 Maverick 17B",
                    131072,
                    false,
                ),
                // Small + fast: the right pick for the judge, librarian and sweep
                // lanes, which are called constantly and want to stay cheap.
                m(
                    "nvidia/nemotron-3-nano-30b-a3b",
                    "Nemotron 3 Nano 30B",
                    131072,
                    true,
                ),
                m("openai/gpt-oss-20b", "GPT-OSS 20B", 131072, true),
                m(
                    "nvidia/nvidia-nemotron-nano-9b-v2",
                    "Nemotron Nano 9B v2",
                    131072,
                    true,
                ),
            ],
        ),
    }
}

fn ollama() -> ProviderModels {
    ProviderModels {
        id: "ollama",
        name: "Ollama",
        base_url: "http://localhost:11434/v1",
        auth_type: AuthType::None,
        auth_methods: vec![AuthMethod::none()],
        env_vars: vec![],
        options: ProviderOptions {
            base_url: Some("http://localhost:11434/v1"),
        },
        models: merged(
            "ollama",
            vec![
                m("llama3.1:8b", "Llama 3.1 8B", 131072, false),
                m("llama3.1:70b", "Llama 3.1 70B", 131072, false),
                m("qwen2.5-coder:32b", "Qwen 2.5 Coder 32B", 32768, false),
                m("deepseek-r1:14b", "DeepSeek R1 14B", 32768, true),
                m("llama3.3:70b", "Llama 3.3 70B", 131072, false),
                m("mistral:7b", "Mistral 7B", 32768, false),
            ],
        ),
    }
}

fn ollama_cloud() -> ProviderModels {
    ProviderModels {
        id: "ollama-cloud",
        name: "Ollama Cloud",
        base_url: "https://ollama.com",
        auth_type: AuthType::Bearer,
        auth_methods: vec![AuthMethod::api_key("OLLAMA_API_KEY")],
        env_vars: vec!["OLLAMA_API_KEY"],
        options: ProviderOptions {
            base_url: Some("https://ollama.com"),
        },
        models: merged(
            "ollama-cloud",
            vec![
                // gpt-oss takes Ollama's string reasoning levels (think:
                // "low"/"medium"/"high") — flagged reasoning so the effort
                // question appears; OllamaProvider maps effort → think.
                m("gpt-oss:20b", "GPT-OSS 20B", 131072, true),
                m("gpt-oss:120b", "GPT-OSS 120B", 131072, true),
                m("gemma3:27b", "Gemma 3 27B", 131072, false),
                // GLM-5.2 and DeepSeek V4 Flash carry 1M windows. When a model is
                // MISSING here, resolve_context_window falls back to the 200k
                // default — the TUI gauge lies AND auto-compaction fires ~5×
                // early (live 2026-07-09: "109.1k/200.0k" on the 1M glm-5.2).
                m("glm-5.2:cloud", "GLM-5.2 Cloud", 1_000_000, true),
                m("glm-5.2", "GLM-5.2", 1_000_000, true),
                m("deepseek-v4-flash", "DeepSeek V4 Flash", 1_000_000, true),
                m(
                    "deepseek-v3.1:671b-cloud",
                    "DeepSeek V3.1 Cloud",
                    131072,
                    false,
                ),
                m(
                    "qwen3-coder:480b-cloud",
                    "Qwen3 Coder 480B Cloud",
                    131072,
                    false,
                ),
            ],
        ),
    }
}

fn volcengine() -> ProviderModels {
    ProviderModels {
        id: "volcengine",
        name: "Volcengine (Doubao)",
        base_url: "https://ark.cn-beijing.volces.com/api/v3",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![AuthMethod::api_key("VOLCANO_ENGINE_API_KEY")],
        env_vars: vec!["VOLCANO_ENGINE_API_KEY"],
        options: ProviderOptions::default(),
        models: merged(
            "volcengine",
            vec![
                m("doubao-seed-1-8-251228", "Doubao Seed 1.8", 256000, false),
                m(
                    "doubao-seed-code-preview-251028",
                    "Doubao Seed Code",
                    256000,
                    false,
                ),
                m("kimi-k2-5-260127", "Kimi K2.5", 256000, false),
                m("deepseek-v3-2-251201", "DeepSeek V3.2", 128000, false),
            ],
        ),
    }
}

fn byteplus() -> ProviderModels {
    ProviderModels {
        id: "byteplus",
        name: "BytePlus (International)",
        base_url: "https://ark.ap-southeast.bytepluses.com/api/v3",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![AuthMethod::api_key("BYTEPLUS_API_KEY")],
        env_vars: vec!["BYTEPLUS_API_KEY"],
        options: ProviderOptions::default(),
        models: merged(
            "byteplus",
            vec![
                m("seed-1-8-251228", "Seed 1.8", 256000, false),
                m("kimi-k2-5-260127", "Kimi K2.5", 256000, false),
                m("glm-4-7-251222", "GLM 4.7", 200000, false),
            ],
        ),
    }
}

fn stepfun() -> ProviderModels {
    ProviderModels {
        id: "stepfun",
        name: "StepFun",
        base_url: "https://api.stepfun.ai/v1",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![AuthMethod::api_key("STEPFUN_API_KEY")],
        env_vars: vec!["STEPFUN_API_KEY"],
        options: ProviderOptions::default(),
        models: merged(
            "stepfun",
            vec![m("step-3.5-flash", "Step 3.5 Flash (free)", 262144, false)],
        ),
    }
}

fn qianfan() -> ProviderModels {
    ProviderModels {
        id: "qianfan",
        name: "Qianfan (Baidu)",
        base_url: "https://qianfan.baidubce.com/v2",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![AuthMethod::api_key("QIANFAN_API_KEY")],
        env_vars: vec!["QIANFAN_API_KEY"],
        options: ProviderOptions::default(),
        models: merged(
            "qianfan",
            vec![
                m("deepseek-v3.2", "DeepSeek V3.2", 98304, true),
                m(
                    "ernie-5.0-thinking-preview",
                    "ERNIE 5.0 Thinking",
                    119000,
                    true,
                ),
            ],
        ),
    }
}

fn tencent() -> ProviderModels {
    ProviderModels {
        id: "tencent",
        name: "Tencent TokenHub",
        base_url: "https://tokenhub.tencentmaas.com/v1",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![AuthMethod::api_key("TOKENHUB_API_KEY")],
        env_vars: vec!["TOKENHUB_API_KEY"],
        options: ProviderOptions::default(),
        models: merged(
            "tencent",
            vec![m("hy3-preview", "Hy3 Preview", 256000, true)],
        ),
    }
}

fn xiaomi() -> ProviderModels {
    ProviderModels {
        id: "xiaomi",
        name: "Xiaomi MiMo",
        base_url: "https://api.xiaomimimo.com/v1",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![AuthMethod::api_key("XIAOMI_API_KEY")],
        env_vars: vec!["XIAOMI_API_KEY"],
        options: ProviderOptions::default(),
        models: merged(
            "xiaomi",
            vec![
                m("mimo-v2-flash", "MiMo V2 Flash (free)", 262144, false),
                m("mimo-v2-pro", "MiMo V2 Pro", 1048576, true),
                m("mimo-v2-omni", "MiMo V2 Omni", 262144, true),
            ],
        ),
    }
}

fn chutes() -> ProviderModels {
    ProviderModels {
        id: "chutes",
        name: "Chutes",
        base_url: "https://llm.chutes.ai/v1",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![
            AuthMethod::oauth("Chutes OAuth", "CHUTES_OAUTH_TOKEN"),
            AuthMethod::api_key("CHUTES_API_KEY"),
        ],
        env_vars: vec!["CHUTES_API_KEY", "CHUTES_OAUTH_TOKEN"],
        options: ProviderOptions::default(),
        models: merged(
            "chutes",
            vec![
                m("Qwen/Qwen3-32B", "Qwen3 32B", 40960, true),
                m(
                    "deepseek-ai/DeepSeek-V3.2-TEE",
                    "DeepSeek V3.2 TEE",
                    131072,
                    true,
                ),
                m("moonshotai/Kimi-K2.5-TEE", "Kimi K2.5 TEE", 262144, true),
                m(
                    "deepseek-ai/DeepSeek-R1-0528-TEE",
                    "DeepSeek R1 TEE",
                    163840,
                    true,
                ),
                m("zai-org/GLM-5-TEE", "GLM-5 TEE", 202752, true),
            ],
        ),
    }
}

fn sglang() -> ProviderModels {
    ProviderModels {
        id: "sglang",
        name: "SGLang (Self-hosted)",
        base_url: "http://localhost:30000/v1",
        auth_type: AuthType::None,
        auth_methods: vec![AuthMethod::none()],
        env_vars: vec![],
        options: ProviderOptions::default(),
        models: vec![],
    }
}

fn vllm() -> ProviderModels {
    ProviderModels {
        id: "vllm",
        name: "vLLM (Self-hosted)",
        base_url: "http://localhost:8000/v1",
        auth_type: AuthType::None,
        auth_methods: vec![AuthMethod::none()],
        env_vars: vec![],
        options: ProviderOptions::default(),
        models: vec![],
    }
}

fn lm_studio() -> ProviderModels {
    ProviderModels {
        id: "lm_studio",
        name: "LM Studio (Local)",
        base_url: "http://localhost:1234/v1",
        auth_type: AuthType::None,
        auth_methods: vec![AuthMethod::none()],
        env_vars: vec![],
        options: ProviderOptions {
            base_url: Some("http://localhost:1234/v1"),
        },
        models: vec![],
    }
}

fn litellm() -> ProviderModels {
    ProviderModels {
        id: "litellm",
        name: "LiteLLM Gateway",
        base_url: "http://localhost:4000/v1",
        auth_type: AuthType::ApiKey,
        auth_methods: vec![AuthMethod::api_key("LITELLM_API_KEY")],
        env_vars: vec!["LITELLM_API_KEY"],
        options: ProviderOptions {
            base_url: Some("http://localhost:4000/v1"),
        },
        models: vec![],
    }
}

fn huggingface() -> ProviderModels {
    ProviderModels {
        id: "huggingface",
        name: "HuggingFace Inference",
        base_url: "https://api-inference.huggingface.co/v1",
        auth_type: AuthType::Bearer,
        auth_methods: vec![AuthMethod::api_key("HUGGINGFACE_HUB_TOKEN")],
        env_vars: vec!["HUGGINGFACE_HUB_TOKEN", "HF_TOKEN"],
        options: ProviderOptions::default(),
        models: merged("huggingface", vec![]),
    }
}

/// A free, public, no-auth DeepSeek-V4-Flash endpoint donated by a HF user
/// (`victor`), announced 2026-07-31. OpenAI-compatible Chat Completions on
/// vLLM, with native `tools`/`tool_calls` and a `reasoning_effort` knob
/// (low · high · max).
///
/// Verified live before wiring: a chat round-trip returned in 0.67s and a
/// tool-call probe came back as proper OpenAI `tool_calls` JSON, which is the
/// part Phoenix cannot work without.
///
/// Two honest caveats, both load-bearing:
/// - It is a COMMUNITY endpoint, rate-limited to roughly a 20-request burst
///   refilling at ~12 req/min PER IP. That is fine for one agent lane and far
///   too tight for the whole mesh — see the fallback note in `configure`.
/// - The context window below is the 393,216 the deployment actually serves,
///   NOT the model's 1,048,576 native window. Compaction sizes itself off this
///   number, so the served figure is the only safe one to publish here.
/// - No key is needed, but the client must still send a non-empty bearer, so
///   this is `AuthType::None` with the request-time placeholder.
fn hf_deepseek_v4_free() -> ProviderModels {
    ProviderModels {
        id: "hf-deepseek-v4-free",
        name: "DeepSeek V4 Flash (free HF endpoint)",
        base_url: "https://q5dh1rfszfym23hj.us-east-2.aws.endpoints.huggingface.cloud/v1",
        auth_type: AuthType::None,
        auth_methods: vec![AuthMethod::none()],
        env_vars: vec![],
        options: ProviderOptions::default(),
        models: vec![m(
            "deepseek-ai/DeepSeek-V4-Flash-0731",
            "DeepSeek V4 Flash 0731",
            393_216,
            true,
        )],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn astra_catalog_exposes_full_window_and_only_supported_efforts() {
        for provider_id in ["openai", "openai-codex"] {
            let provider = get_provider(provider_id).unwrap();
            let astra = provider
                .models
                .iter()
                .find(|model| model.id == "gpt-6-astra")
                .unwrap();
            assert_eq!(astra.context_window, 1_050_000);
            assert!(astra.reasoning);
            assert_eq!(
                effort_levels(provider_id, astra.id),
                &["low", "medium", "high", "xhigh", "max"]
            );
            let sol61 = provider.models.iter().find(|m| m.id == "gpt-6.1-sol").unwrap();
            assert_eq!(sol61.context_window, 1_050_000);
            assert_eq!(effort_levels(provider_id, sol61.id), &["low", "medium", "high", "xhigh", "max"]);
            let sol = provider.models.iter().find(|m| m.id == "gpt-6-sol").unwrap();
            assert_eq!(sol.context_window, 1_050_000, "GPT-6 Sol offers the 1M window on every route");
            for id in ["gpt-6-sol", "gpt-6-luna"] {
                let model = provider.models.iter().find(|m| m.id == id).unwrap();
                assert!(model.reasoning);
                assert_eq!(effort_levels(provider_id, id), &["low", "medium", "high", "xhigh", "max"]);
            }
        }
    }

    /// Every provider's recommended model must exist in that provider's own
    /// model list.
    ///
    /// This is the check that would have caught the NIM breakage: three of the
    /// four ids under `nvidia` named models NVIDIA does not serve, so onboarding
    /// completed happily and then every single turn 404'd. A model id is a
    /// remote contract, and the one part of it we CAN verify offline is that the
    /// catalog agrees with itself.
    ///
    /// Providers with an empty recommendation are self-hosted (`vllm`, `ollama`,
    /// `lm_studio`, …) where the model set is whatever the user is running, so
    /// there is nothing to pin.
    #[test]
    fn every_recommended_model_exists_in_its_own_catalog() {
        for provider in all_providers() {
            let want = recommended_model(provider.id);
            if want.is_empty() {
                continue;
            }
            assert!(
                provider.models.iter().any(|m| m.id == want),
                "provider `{}` recommends `{}`, which is not in its own model list: {:?}",
                provider.id,
                want,
                provider.models.iter().map(|m| m.id).collect::<Vec<_>>(),
            );
        }
    }

    #[test]
    fn tokenrouter_exposes_the_requested_free_kimi_route() {
        let provider = get_provider("tokenrouter").expect("tokenrouter provider");
        assert_eq!(provider.name, "TokenRouter");
        assert_eq!(provider.base_url, "https://api.tokenrouter.com/v1");
        assert_eq!(provider.env_vars, vec!["TOKENROUTER_API_KEY"]);
        assert_eq!(recommended_model("tokenrouter"), "moonshotai/kimi-k3-free");

        let model = provider
            .models
            .iter()
            .find(|model| model.id == "moonshotai/kimi-k3-free")
            .expect("requested Kimi K3 free route");
        assert_eq!(model.context_window, 1_048_576);
        assert!(model.reasoning);
    }

    /// NIM ids are `namespace/model` and were taken from the live, unauthenticated
    /// `GET /v1/models`. Pinning the shape stops a bare `glm-5.2` (the ollama-cloud
    /// spelling) from being pasted back in, which is the exact mistake that made
    /// three of the previous four entries dead.
    #[test]
    fn nvidia_nim_models_are_namespaced_and_include_the_workhorses() {
        let p = get_provider("nvidia").expect("nvidia provider");
        assert_eq!(p.base_url, "https://integrate.api.nvidia.com/v1");
        for model in &p.models {
            assert!(
                model.id.contains('/'),
                "NIM model id `{}` is missing its `namespace/` prefix",
                model.id
            );
        }
        for want in [
            "nvidia/nemotron-3-ultra-550b-a55b",
            "nvidia/nemotron-3-super-120b-a12b",
            "z-ai/glm-5.2",
            "moonshotai/kimi-k2.6",
            "qwen/qwen3.5-397b-a17b",
        ] {
            assert!(
                p.models.iter().any(|m| m.id == want),
                "NIM catalog lost `{want}`"
            );
        }
        // The dead ids this entry used to ship. None may ever come back.
        for dead in [
            "moonshotai/kimi-k2.5",
            "minimaxai/minimax-m2.5",
            "z-ai/glm5",
        ] {
            assert!(
                !p.models.iter().any(|m| m.id == dead),
                "`{dead}` does not exist on NIM and would 404 on first use"
            );
        }
    }

    #[test]
    fn xai_supports_supergrok_oauth_and_grok_45() {
        let p = get_provider("xai").expect("xai provider");
        // OAuth (SuperGrok subscription) is the default method; API key stays.
        assert_eq!(p.auth_methods[0].method_type, "oauth");
        assert!(p.auth_methods.iter().any(|m| m.method_type == "api"));
        assert_eq!(context_window_for("xai", "grok-4.5"), Some(500_000));
        assert_eq!(context_window_for("xai", "grok-4.3"), Some(1_000_000));
        assert_eq!(recommended_model("xai"), "grok-4.5");
    }

    #[test]
    fn codex_context_windows_match_plus_plan() {
        // ChatGPT Plus-plan Codex caps context at 256k regardless of model;
        // the 1M tier is a config `context_window` override, never a default.
        assert_eq!(context_window_for("openai-codex", "gpt-5.4"), Some(256_000));
        assert_eq!(
            context_window_for("openai-codex", "gpt-5.4-mini"),
            Some(256_000)
        );
        assert_eq!(recommended_model("openai-codex"), "gpt-5.6-terra");
    }

    #[test]
    fn context_window_lookup_misses_cleanly() {
        // An unknown MODEL still misses cleanly — the caller applies its own
        // default knowingly rather than being handed a wrong number.
        assert_eq!(context_window_for("openai-codex", "not-a-model"), None);
        assert_eq!(context_window_for("not-a-provider", "not-a-model"), None);
        // A KNOWN model resolves even under an unfamiliar provider label: the
        // context window is a property of the model, not of who is serving it.
        // This is what lets aggregator re-exports resolve instead of silently
        // taking the 200k default.
        assert!(context_window_for("not-a-provider", "gpt-5.4").is_some());
    }

    #[test]
    fn non_reasoning_models_get_no_effort_ladder() {
        // The bug this replaces: effort_levels() returned low/medium/high for
        // every provider outside OpenAI, so a picker offered an effort control
        // on models that cannot use one and the setting went nowhere.
        for provider in all_providers() {
            for model in &provider.models {
                if !model.reasoning {
                    assert!(
                        effort_levels(provider.id, model.id).is_empty(),
                        "{}/{} does not reason but was offered an effort ladder",
                        provider.id,
                        model.id
                    );
                    assert!(!model_supports_effort(provider.id, model.id));
                }
            }
        }
    }

    #[test]
    fn effort_ladders_match_the_transport() {
        // xAI genuinely accepts only the two extremes.
        assert_eq!(effort_levels("xai", "grok-4.5"), &["low", "high"]);
        // Codex takes the full Responses ladder.
        assert!(effort_levels("openai-codex", "gpt-5.4").contains(&"xhigh"));
        // Sol and Luna carry the extra max rung through both the API and
        // ChatGPT/Codex transports.
        assert!(effort_levels("openai", "gpt-5.6-sol").contains(&"max"));
        assert_eq!(
            effort_levels("openai-codex", "gpt-5.6-sol"),
            &["minimal", "low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(
            effort_levels("openai-codex", "gpt-5.6-luna"),
            &["minimal", "low", "medium", "high", "xhigh", "max"]
        );
        assert!(!effort_levels("openai-codex", "gpt-5.4").contains(&"max"));
        // A provider whose models reason internally exposes no knob.
        assert_eq!(effort_transport("deepseek"), EffortTransport::Intrinsic);
        assert!(effort_levels("deepseek", "deepseek-reasoner").is_empty());
    }

    #[test]
    fn budget_transports_map_rungs_to_token_budgets() {
        // Anthropic and Google take a token count, not a named level, so every
        // rung the picker offers must resolve to a number.
        for provider in ["anthropic", "google"] {
            for level in effort_levels(provider, &recommended_model(provider).to_string()) {
                assert!(
                    effort_budget_tokens(provider, level).is_some(),
                    "{provider} rung {level} has no token budget"
                );
            }
        }
        // Budgets must increase with the rung, or the slider lies.
        let low = effort_budget_tokens("anthropic", "low").unwrap();
        let med = effort_budget_tokens("anthropic", "medium").unwrap();
        let high = effort_budget_tokens("anthropic", "high").unwrap();
        assert!(low < med && med < high);
        // Named-level transports must NOT claim a budget.
        assert_eq!(effort_budget_tokens("openai-codex", "high"), None);
    }

    #[test]
    fn every_effort_rung_is_a_known_level() {
        // The UI slider maps these strings onto its own ladder; an unknown rung
        // would render as a dead notch.
        const KNOWN: &[&str] = &["minimal", "low", "medium", "high", "xhigh", "max"];
        for provider in all_providers() {
            for model in &provider.models {
                for level in effort_levels(provider.id, model.id) {
                    assert!(
                        KNOWN.contains(level),
                        "{}/{} offers unknown effort rung {level}",
                        provider.id,
                        model.id
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod aggregator_window_tests {
    use super::*;

    #[test]
    fn an_aggregator_reexport_resolves_to_the_real_context_window() {
        // The live failure: this exact id on openrouter missed the catalog, so
        // the runtime used the 200k default against a 1M model. The gauge read
        // 97% when the truth was nearer 19%, and compaction fired against a
        // window a fifth of the real one.
        let w = context_window_for("openrouter", "nvidia/nemotron-3-ultra-550b-a55b:free")
            .expect("aggregator re-export must resolve");
        // The point is that it resolves to the CATALOG rather than silently
        // falling back to the 200k default, which is what skewed the gauge.
        assert_ne!(
            w,
            crate::config::DEFAULT_CONTEXT_WINDOW_TOKENS,
            "still falling back to the default instead of resolving the model"
        );
        assert!(w >= 262_144, "resolved to an implausibly small window: {w}");
    }

    #[test]
    fn a_declared_effort_is_never_zeroed_by_the_transport_default() {
        // Regression: `Intrinsic` is the transport table's default arm, so every
        // unlisted provider silently denied an effort ladder to models that
        // publish one. Venice states `supportsReasoningEffort` per model, and
        // the OpenAI-shaped gateways pass the field through.
        for pid in ["venice", "deepinfra", "chutes", "huggingface"] {
            let table = live_catalogs::table_for(pid).unwrap_or_else(|| panic!("{pid}"));
            let declared: Vec<_> = table.iter().filter(|r| r.4).collect();
            if declared.is_empty() {
                continue;
            }
            for row in declared {
                assert!(
                    !effort_levels(pid, row.0).is_empty(),
                    "{pid}/{} declares reasoning_effort but was offered no ladder",
                    row.0
                );
            }
        }
    }

    #[test]
    fn a_model_that_declares_nothing_still_gets_nothing() {
        // The other half of the same rule: the per-model catalog is what makes
        // the fallback above safe, so a provider-wide ladder must never appear.
        let table = live_catalogs::table_for("venice").expect("venice");
        for row in table.iter().filter(|r| !r.4 && !r.3) {
            assert!(
                effort_levels("venice", row.0).is_empty(),
                "venice/{} declares no effort yet was offered one",
                row.0
            );
        }
    }

    #[test]
    fn every_generated_table_reaches_its_provider() {
        // The wiring, not the data: a table that exists but is never merged in
        // is the same as no table at all, and that failure is silent.
        for (pid, table) in [
            ("openai", live_catalogs::OPENAI_MODELS),
            ("anthropic", live_catalogs::ANTHROPIC_MODELS),
            ("google", live_catalogs::GOOGLE_MODELS),
            ("xai", live_catalogs::XAI_MODELS),
            ("meta", live_catalogs::META_MODELS),
            ("nvidia", live_catalogs::NVIDIA_MODELS),
            ("ollama-cloud", live_catalogs::OLLAMA_CLOUD_MODELS),
        ] {
            let p = get_provider(pid).unwrap_or_else(|| panic!("{pid} is not a provider"));
            for row in table {
                let found = p.models.iter().find(|m| m.id == row.0);
                let found = found.unwrap_or_else(|| panic!("{pid} is missing {}", row.0));
                assert_eq!(
                    found.context_window, row.2,
                    "{pid}/{} window drifted from the generated table",
                    row.0
                );
            }
        }
    }

    #[test]
    fn the_windows_that_drifted_are_the_served_ones() {
        // Each of these was hand-written at a figure ~4-5x too small, and each
        // one made the gauge read high and compaction fire early. Named
        // explicitly so a future "tidy-up" cannot quietly revert them.
        assert_eq!(
            context_window_for("anthropic", "claude-sonnet-4-6"),
            Some(1_000_000)
        );
        assert_eq!(context_window_for("openai", "gpt-5.6-sol"), Some(1_050_000));
        assert_eq!(context_window_for("openai", "gpt-5.5"), Some(1_050_000));
    }

    #[test]
    fn merging_never_drops_a_curated_model() {
        // Someone's config.toml may already name a model the upstream catalog
        // no longer lists. Losing it from the picker would look like the model
        // was removed rather than merely uncatalogued.
        let p = get_provider("openai").expect("openai");
        for id in ["o1-pro", "gpt-4o", "dall-e-3"].iter().take(2) {
            assert!(
                p.models.iter().any(|m| &m.id == id),
                "curated {id} was dropped"
            );
        }
    }

    #[test]
    fn the_tier_suffix_selects_a_different_window_and_must_be_kept() {
        // This test asserted the OPPOSITE until the live OpenRouter catalog was
        // pulled in, and the assumption was simply wrong: one model, three
        // windows. Serving config is per endpoint and per tier, so a lookup
        // that "helpfully" strips `:free` to find a match returns a confidently
        // wrong number — the exact failure mode the gauge bug came from.
        let nim = context_window_for("nvidia", "nvidia/nemotron-3-ultra-550b-a55b");
        let paid = context_window_for("openrouter", "nvidia/nemotron-3-ultra-550b-a55b");
        let free = context_window_for("openrouter", "nvidia/nemotron-3-ultra-550b-a55b:free");
        assert_eq!(free, Some(1_000_000), "the free tier is the 1M one");
        assert_eq!(paid, Some(512_288), "the paid tier is served at 512k");
        assert_ne!(nim, free, "NIM and OpenRouter serve this model differently");
    }

    #[test]
    fn a_model_that_cannot_take_an_effort_is_offered_no_ladder() {
        // Ground truth is the model's own `supported_parameters`. Offering a
        // ladder a model does not accept is how the effort control became a
        // no-op; denying one to a model that does accept it is how the same
        // control greyed itself out on the model this project runs on.
        assert!(
            !effort_levels("openrouter", "nvidia/nemotron-3-ultra-550b-a55b:free").is_empty(),
            "this model advertises reasoning_effort and must get a ladder"
        );
        // Stated as an invariant over the whole table rather than one example,
        // because the examples move: `openrouter/auto` looked like the obvious
        // no-effort model and in fact advertises one.
        for row in openrouter_catalog::OPENROUTER_MODELS {
            let ladder = effort_levels("openrouter", row.0);
            assert_eq!(
                row.4,
                !ladder.is_empty(),
                "{} advertises reasoning_effort={} but was offered {:?}",
                row.0,
                row.4,
                ladder
            );
        }
    }

    #[test]
    fn the_openrouter_catalog_is_the_whole_live_list() {
        // A shortlist is what hid the running model from every lookup. If this
        // ever shrinks back to a curated handful, the gauge and the effort
        // control start lying again for anything off the list.
        let p = get_provider("openrouter").expect("openrouter is in the catalog");
        assert!(
            p.models.len() > 300,
            "openrouter carries the full catalog, got {}",
            p.models.len()
        );
    }

    #[test]
    fn a_genuinely_unknown_model_still_reports_nothing() {
        // Falling back to a wrong number is what caused the bug; None lets the
        // caller apply its own default knowingly.
        assert_eq!(
            context_window_for("openrouter", "acme/not-a-real-model"),
            None
        );
    }
}
