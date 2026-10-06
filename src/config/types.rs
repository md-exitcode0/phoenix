#[derive(Clone, Debug)]
pub struct Profile {
    pub name: String,
    pub llm: LLMProfile,
    pub search: Option<SearchProfile>,
    pub crawl: Option<CrawlProfile>,
    pub scrape: Option<ScrapeProfile>,
    pub browser: Option<BrowserProfile>,
    /// Account-fallback chains for web capabilities: ordered auth-profile ids
    /// tried after the primary key hits its quota (four free Tavily accounts =
    /// 4× the free scrapes, rotated automatically).
    pub web_fallback: WebFallbackChains,
    /// MCP servers Phoenix can reach as external tool providers — local
    /// (stdio child process, e.g. T3MP3ST's `security_recon`) or remote
    /// (Streamable-HTTP URL). The agent reaches them through the
    /// `mcp_servers`/`mcp_call` meta-tools. Off unless listed here.
    /// Managed via `phoenix configure` → "MCP servers".
    pub mcp_servers: Vec<McpServerConfig>,
}

/// One MCP server registration — local (stdio: `command`+`args`, launched on
/// demand) or remote (`url`, JSON-RPC over Streamable HTTP; `headers` carries
/// auth). Exactly one of `command`/`url` is set. `route` (optional) names the
/// specialist role the server is intended for ("hacker"); discovery surfaces
/// it to that agent (and the orchestrator) but any MCP-capable agent can call
/// it. Empty `route` = available to every MCP-capable agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: std::collections::BTreeMap<String, String>,
    /// Remote server endpoint. Set = HTTP transport, `command` is ignored.
    pub url: Option<String>,
    /// Extra HTTP headers for remote servers (e.g. Authorization).
    pub headers: std::collections::BTreeMap<String, String>,
    pub route: Option<String>,
    pub enabled: bool,
    /// Optional one-line note shown in discovery (what the server is for).
    pub description: Option<String>,
}

impl McpServerConfig {
    pub fn is_remote(&self) -> bool {
        self.url
            .as_deref()
            .map(str::trim)
            .is_some_and(|u| !u.is_empty())
    }
}

/// Ordered account-fallback chains, one per web capability. Entries are
/// auth-profile ids in `~/.phoenix/auth-profiles.json`; the primary configured
/// key always runs first, these run after it in order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WebFallbackChains {
    pub search: Vec<String>,
    pub crawl: Vec<String>,
    pub scrape: Vec<String>,
}

impl WebFallbackChains {
    pub fn is_empty(&self) -> bool {
        self.search.is_empty() && self.crawl.is_empty() && self.scrape.is_empty()
    }
}

/// Ordered account-fallback chains for LLM roles. Entries are auth-profile
/// ids; each resolves to (provider, credential, optional preferred model).
/// The role's primary configured provider always runs first. `agents` maps a
/// specialist role name ("coder", "browser", …) to its own chain, overriding
/// the shared `specialist` chain for that agent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FallbackChains {
    pub orchestrator: Vec<String>,
    pub specialist: Vec<String>,
    pub librarian: Vec<String>,
    /// Vision-sidecar accounts: credential fallback when the primary vision
    /// provider's auth fails (the 2026-07-09 outage class).
    pub vision: Vec<String>,
    /// Image-generation accounts, same contract as `vision`.
    pub image: Vec<String>,
    pub agents: std::collections::BTreeMap<String, Vec<String>>,
}

impl FallbackChains {
    pub fn is_empty(&self) -> bool {
        self.orchestrator.is_empty()
            && self.specialist.is_empty()
            && self.librarian.is_empty()
            && self.vision.is_empty()
            && self.image.is_empty()
            && self.agents.is_empty()
    }

    /// The named role lanes, label → chain, in display order. `agents` rides
    /// separately (keyed by agent name).
    pub fn role_lanes(&self) -> [(&'static str, &Vec<String>); 5] {
        [
            ("orchestrator", &self.orchestrator),
            ("specialist", &self.specialist),
            ("librarian", &self.librarian),
            ("vision", &self.vision),
            ("image", &self.image),
        ]
    }

    /// Mutable access to a named role lane (not `agents`).
    pub fn role_lane_mut(&mut self, lane: &str) -> Option<&mut Vec<String>> {
        match lane {
            "orchestrator" => Some(&mut self.orchestrator),
            "specialist" => Some(&mut self.specialist),
            "librarian" | "indexer" => Some(&mut self.librarian),
            "vision" => Some(&mut self.vision),
            "image" => Some(&mut self.image),
            _ => None,
        }
    }

    /// Every profile id referenced anywhere in the chains.
    pub fn referenced_profiles(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for id in self
            .orchestrator
            .iter()
            .chain(self.specialist.iter())
            .chain(self.librarian.iter())
            .chain(self.vision.iter())
            .chain(self.image.iter())
            .chain(self.agents.values().flatten())
        {
            if !out.contains(id) {
                out.push(id.clone());
            }
        }
        out
    }

    /// Remove a profile id from every chain (used when a profile is deleted).
    pub fn remove_profile(&mut self, profile_id: &str) {
        self.orchestrator.retain(|id| id != profile_id);
        self.specialist.retain(|id| id != profile_id);
        self.librarian.retain(|id| id != profile_id);
        self.vision.retain(|id| id != profile_id);
        self.image.retain(|id| id != profile_id);
        for chain in self.agents.values_mut() {
            chain.retain(|id| id != profile_id);
        }
        self.agents.retain(|_, chain| !chain.is_empty());
    }
}

/// Browser agent preferences — set once (config), used on every session.
/// Env vars (PHOENIX_BROWSER_*) still override for one-off runs.
#[derive(Clone, Debug, Default)]
pub struct BrowserProfile {
    /// "chrome" = the user's real Chrome (attach if running with the debug
    /// port, else launch with its profile — copying it if locked);
    /// "phoenix"/unset = sandboxed persistent Phoenix profile;
    /// any other value = an explicit user-data-dir path.
    pub source: Option<String>,
    /// CDP debug port to probe for attaching to a live browser (default 9222).
    pub attach_port: Option<u16>,
    pub headless: Option<bool>,
    /// Explicit browser executable. Any Chromium-compatible build works —
    /// CloakBrowser, Brave, ungoogled-chromium. Unset = autodetect Chrome.
    pub binary: Option<String>,
    /// Extra command-line flags passed to the browser at launch.
    pub extra_args: Option<Vec<String>>,
    /// Port logins from this source browser into the session at start
    /// (Firefox-family: "zen", "firefox", …). None/"none" = off.
    pub login_source: Option<String>,
    /// Cleanly suspend Phoenix-owned Chromium after this many idle seconds.
    /// Zero disables suspension; omitted uses Phoenix's efficient default.
    pub suspend_after_seconds: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct LLMProfile {
    pub provider: String,
    pub model: String,
    pub orchestrator_model: Option<String>,
    pub specialist_model: Option<String>,
    pub librarian_model: Option<String>,
    /// Cheap image model for screenshot grounding (None = vision disabled).
    pub vision_model: Option<String>,
    /// Provider for the vision model (None = same as `provider`).
    pub vision_provider: Option<String>,
    /// Enabled by default; explicit false keeps the caption sidecar.
    /// When the acting provider supports images, attach the latest screenshot to its
    /// context as a real image instead of round-tripping through the caption
    /// sidecar. One model call per perception step instead of two, and the
    /// agent reads the actual pixels, not a paraphrase. Requires an
    /// provider that advertises native image support (including Codex);
    /// other providers fall back to the caption sidecar.
    pub native_vision: bool,
    /// Image GENERATION model (None = image_gen tool disabled). Distinct from
    /// the vision sidecar (which reads images): this one creates them — hero
    /// images, textures, design references — a first-class capability for
    /// frontend/design work (Iris).
    pub image_model: Option<String>,
    /// Provider for the image-generation model (None = same as `provider`).
    /// Must speak the OpenAI-compatible `/images/generations` API.
    pub image_provider: Option<String>,
    /// Provider for specialist agents (None = same as `provider`).
    pub specialist_provider: Option<String>,
    /// Provider for the librarian (None = same as `provider`).
    pub librarian_provider: Option<String>,
    /// Model for memory graph-building (cognee cognify: entity/relationship
    /// extraction over saved memories). None = the librarian model. This lane
    /// runs in the background on every ingested memory — cheap and reliable
    /// beats smart here.
    pub memory_model: Option<String>,
    /// Provider for memory graph-building (None = librarian's, then main).
    pub memory_provider: Option<String>,
    /// Speech-to-text lane used by Canvas native dictation.
    pub stt_model: Option<String>,
    pub stt_provider: Option<String>,
    /// Text-to-speech lane and provider-specific voice name.
    pub tts_model: Option<String>,
    pub tts_provider: Option<String>,
    pub tts_voice: Option<String>,
    /// Full-duplex speech-to-speech lane (Realtime/WebRTC-capable models).
    pub realtime_model: Option<String>,
    pub realtime_provider: Option<String>,
    pub realtime_voice: Option<String>,
    pub auth: Option<LLMAuthConfig>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    /// Provider idle-read timeout in seconds. Zero disables the idle-read
    /// deadline; positive values are explicit user-configured limits.
    pub timeout_seconds: u64,
    /// Selectable reasoning effort (minimal/low/medium/high) for providers that
    /// support it (currently openai-codex). None = provider default.
    /// Global default — `[profile.llm.efforts]` overrides it per lane.
    pub reasoning_effort: Option<String>,
    /// `[profile.llm.efforts]` — reasoning effort per lane. Keys: role names
    /// (`orchestrator`, `specialist`, `librarian`) or an
    /// individual agent name (`coder`). Missing key = `reasoning_effort`.
    pub efforts: std::collections::BTreeMap<String, String>,
    /// `[profile.llm.agent_models]` — primary model per individual specialist
    /// (`coder = "glm-5.2"`). Unset agents ride `specialist_model` → `model`.
    pub agent_models: std::collections::BTreeMap<String, String>,
    /// `[profile.llm.auth_by_lane]` — WHICH stored account is a lane's PRIMARY.
    /// Keys are role names (`orchestrator`, `specialist`, `librarian`) or an
    /// individual agent name (`coder`); values are auth-profile ids
    /// (`nvidia:4`).
    ///
    /// Without this, a provider holding several accounts resolves its primary
    /// through `resolve_legacy_profile`, which returns the ALPHABETICALLY FIRST
    /// compatible profile — so a pool of `nvidia:2 … nvidia:6` always ran on
    /// `nvidia:2`, an accident of ASCII that nobody chose. The dashboard's
    /// chain editor writes this table when you drag an account into position 0.
    pub auth_by_lane: std::collections::BTreeMap<String, String>,
    /// Provider-specific context policy.  Missing providers are deliberately
    /// Phoenix-only so adding this field cannot silently turn on an opaque
    /// provider-native protocol for an existing configuration.
    ///
    /// `[profile.llm.compaction_modes]` maps provider ids to `native_preferred`
    /// or `phoenix_only`.  The native path always retains Phoenix's portable
    /// transcript and falls back to it on route, auth, or API failure.
    pub compaction_modes: std::collections::BTreeMap<String, CompactionMode>,
    /// Override the model's context window (tokens). None = resolve from the
    /// provider catalog (`providers_data`) for the configured provider/model.
    /// Kept as the legacy company-wide fallback; new desktop edits use the
    /// per-lane table below so one coworker's smaller window does not silently
    /// constrain every other model.
    pub context_window: Option<u64>,
    /// `[profile.llm.context_windows]` — explicit usable context ceilings per
    /// model lane. Keys mirror `efforts`/`agent_models` (`orchestrator`,
    /// `specialist`, or a coworker id). Values may be lower than, never higher
    /// than, the selected model's catalog maximum.
    pub context_windows: std::collections::BTreeMap<String, u64>,
    /// Account-fallback chains per role: when a role's provider exhausts an
    /// account (quota, rate limit, expired auth), the runtime rotates to the
    /// next profile in that role's chain automatically — four Codex accounts
    /// keep working until all four run out, no re-login.
    pub fallback: FallbackChains,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactionMode {
    PhoenixOnly,
    NativePreferred,
}

impl Default for CompactionMode {
    fn default() -> Self {
        Self::PhoenixOnly
    }
}

impl CompactionMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "phoenix_only" | "portable" | "phoenix" => Some(Self::PhoenixOnly),
            "native_preferred" | "native" => Some(Self::NativePreferred),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::PhoenixOnly => "phoenix_only",
            Self::NativePreferred => "native_preferred",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct LLMAuthConfig {
    pub method: Option<String>,
    pub source: Option<String>,
    pub profile: Option<String>,
    pub env_var: Option<String>,
}

impl LLMProfile {
    /// A probe profile for resolving auth AGAINST A DIFFERENT PROVIDER
    /// (vision lane, image gen, per-role providers, cognee's memory LLM).
    /// The main lane's pinned `auth` belongs to the MAIN provider — carrying
    /// it into another provider's resolution can only fail ("Auth profile
    /// 'ollama-cloud:2' belongs to provider 'ollama-cloud' but config
    /// expects 'openai-codex'" — the live 2026-07-09 vision outage, which
    /// survived a codex re-login because the pin short-circuits resolution
    /// before the provider's own stored profiles are even consulted).
    /// Cross-provider probes drop the pin so resolution falls back to env
    /// vars, then the TARGET provider's own profiles; a same-provider probe
    /// keeps the pin (it is valid there, and deliberate).
    pub fn probe_for_provider(&self, provider_id: &str) -> LLMProfile {
        let mut probe = self.clone();
        if probe.provider != provider_id {
            probe.auth = None;
        }
        probe.provider = provider_id.to_string();
        probe
    }

    /// The account `[profile.llm.auth_by_lane]` pins as `lane`'s primary, but
    /// ONLY when that account belongs to `provider_id`. The guard is the whole
    /// point: a pin carried into another provider's resolution can only fail
    /// ("Auth profile 'nvidia:4' belongs to provider 'nvidia' but config
    /// expects 'openai-codex'"), which is the same cross-provider pin disease
    /// `probe_for_provider` exists to dodge. A stale pin left behind by a
    /// provider switch is therefore ignored, not fatal.
    pub fn lane_pin(&self, lane: &str, provider_id: &str) -> Option<&str> {
        let pinned = self.auth_by_lane.get(lane)?.trim();
        if pinned.is_empty() {
            return None;
        }
        let owner = pinned.split(':').next().unwrap_or("");
        let compatible = owner == provider_id
            || matches!(
                (owner, provider_id),
                ("xai", "grok-cli") | ("grok-cli", "xai")
            );
        compatible.then_some(pinned)
    }

    /// This profile with `lane`'s pinned account declared as the auth source,
    /// so `resolve_declared_auth` short-circuits to exactly that credential
    /// instead of "whichever of the provider's accounts sorts first". No pin
    /// (or a pin for another provider) returns a clone unchanged.
    pub fn with_lane_pin(&self, lane: &str, provider_id: &str) -> LLMProfile {
        let mut out = self.clone();
        if let Some(profile_id) = self.lane_pin(lane, provider_id) {
            out.auth = Some(LLMAuthConfig {
                method: None,
                source: Some("profile".to_string()),
                profile: Some(profile_id.to_string()),
                env_var: None,
            });
        }
        out
    }

    pub fn orchestrator(&self) -> String {
        self.orchestrator_model
            .clone()
            .unwrap_or_else(|| self.model.clone())
    }

    pub fn specialist(&self) -> String {
        self.specialist_model
            .clone()
            .unwrap_or_else(|| self.model.clone())
    }

    pub fn librarian(&self) -> String {
        self.librarian_model
            .clone()
            .unwrap_or_else(|| self.model.clone())
    }

    /// Memory graph-building model: explicit → librarian.
    pub fn memory(&self) -> String {
        self.memory_model
            .clone()
            .unwrap_or_else(|| self.librarian())
    }

    /// Reasoning effort for a lane: exact key → the global default.
    /// Lane is a role name ("orchestrator", "librarian") or an agent name.
    pub fn effort_for(&self, lane: &str) -> Option<String> {
        self.efforts
            .get(lane)
            .cloned()
            .or_else(|| self.reasoning_effort.clone())
            .filter(|e| !e.trim().is_empty())
    }

    /// Native compaction is an explicit provider-by-provider opt-in.  A
    /// missing entry is Phoenix-only, including for newly added providers.
    pub fn compaction_mode_for(&self, provider_id: &str) -> CompactionMode {
        self.compaction_modes
            .get(provider_id)
            .copied()
            .unwrap_or_default()
    }

    pub fn native_compaction_enabled_for(&self, provider_id: &str) -> bool {
        self.compaction_mode_for(provider_id) == CompactionMode::NativePreferred
    }

    /// Effort for an individual specialist: agent key → specialist → global.
    pub fn agent_effort(&self, agent: &str) -> Option<String> {
        self.efforts
            .get(agent)
            .or_else(|| self.efforts.get("specialist"))
            .cloned()
            .or_else(|| self.reasoning_effort.clone())
            .filter(|e| !e.trim().is_empty())
    }

    /// Primary model for an individual specialist: agent_models → specialist.
    pub fn agent_model(&self, agent: &str) -> String {
        self.agent_models
            .get(agent)
            .cloned()
            .unwrap_or_else(|| self.specialist())
    }
}

#[derive(Clone, Debug, Default)]
pub struct WebAuthConfig {
    pub source: Option<String>,
    pub profile: Option<String>,
    pub env_var: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SearchProfile {
    pub provider: SearchProvider,
    pub default_engine: String,
    pub api_key: Option<String>,
    pub auth: Option<WebAuthConfig>,
    pub num_results: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchProvider {
    Google,
    Bing,
    DuckDuckGo,
    SerpAPI,
    Serper,
    Tavily,
    Exa,
    Brave,
    Firecrawl,
    Custom,
}

#[derive(Clone, Debug)]
pub struct CrawlProfile {
    pub provider: CrawlProvider,
    pub api_key: Option<String>,
    pub auth: Option<WebAuthConfig>,
    pub base_url: Option<String>,
    pub rate_limit: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CrawlProvider {
    Firecrawl,
    Tavily,
    Crawl4AI,
    Scrapfly,
    Apify,
    Custom,
}

#[derive(Clone, Debug)]
pub struct ScrapeProfile {
    pub provider: ScrapeProvider,
    pub api_key: Option<String>,
    pub auth: Option<WebAuthConfig>,
    pub base_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScrapeProvider {
    Firecrawl,
    Tavily,
    ScraperAPI,
    ScrapingBee,
    Custom,
}

#[derive(Clone, Debug, Default)]
pub struct OAuthConfig {
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub auth_url: Option<String>,
    pub token_url: Option<String>,
    pub scopes: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct APIKeyConfig {
    pub name: String,
    pub key: Option<String>,
    pub env_var: String,
    pub keychain: bool,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            name: "default".to_string(),
            llm: LLMProfile::default(),
            search: None,
            crawl: None,
            scrape: None,
            browser: None,
            web_fallback: WebFallbackChains::default(),
            mcp_servers: Vec::new(),
        }
    }
}

impl Default for LLMProfile {
    fn default() -> Self {
        Self {
            provider: "openai".to_string(),
            model: "gpt-4o".to_string(),
            orchestrator_model: None,
            specialist_model: None,
            librarian_model: None,
            vision_model: None,
            vision_provider: None,
            native_vision: true,
            image_model: None,
            image_provider: None,
            specialist_provider: None,
            librarian_provider: None,
            memory_model: None,
            memory_provider: None,
            stt_model: None,
            stt_provider: None,
            tts_model: None,
            tts_provider: None,
            tts_voice: None,
            realtime_model: None,
            realtime_provider: None,
            realtime_voice: None,
            auth: None,
            temperature: Some(0.0),
            max_tokens: None,
            timeout_seconds: 0,
            reasoning_effort: None,
            efforts: Default::default(),
            agent_models: Default::default(),
            auth_by_lane: Default::default(),
            compaction_modes: Default::default(),
            context_window: None,
            context_windows: Default::default(),
            fallback: FallbackChains::default(),
        }
    }
}

impl LLMProfile {
    pub fn new(provider: &str, model: &str) -> Self {
        Self {
            provider: provider.to_string(),
            model: model.to_string(),
            orchestrator_model: None,
            specialist_model: None,
            librarian_model: None,
            vision_model: None,
            vision_provider: None,
            native_vision: true,
            image_model: None,
            image_provider: None,
            specialist_provider: None,
            librarian_provider: None,
            memory_model: None,
            memory_provider: None,
            stt_model: None,
            stt_provider: None,
            tts_model: None,
            tts_provider: None,
            tts_voice: None,
            realtime_model: None,
            realtime_provider: None,
            realtime_voice: None,
            auth: None,
            temperature: Some(0.0),
            max_tokens: None,
            timeout_seconds: 0,
            reasoning_effort: None,
            efforts: Default::default(),
            agent_models: Default::default(),
            auth_by_lane: Default::default(),
            compaction_modes: Default::default(),
            context_window: None,
            context_windows: Default::default(),
            fallback: FallbackChains::default(),
        }
    }

    pub fn with_orchestrator(mut self, model: &str) -> Self {
        self.orchestrator_model = Some(model.to_string());
        self
    }

    pub fn with_specialist(mut self, model: &str) -> Self {
        self.specialist_model = Some(model.to_string());
        self
    }

    pub fn with_librarian(mut self, model: &str) -> Self {
        self.librarian_model = Some(model.to_string());
        self
    }

    pub fn with_temperature(mut self, temp: f32) -> Self {
        self.temperature = Some(temp);
        self
    }

    pub fn with_max_tokens(mut self, tokens: u32) -> Self {
        self.max_tokens = Some(tokens);
        self
    }
}
