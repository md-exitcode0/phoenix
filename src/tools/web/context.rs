use anyhow::Result;

use crate::config::{
    resolve_web_api_key, CrawlProfile, CrawlProvider, PhoenixConfig, ScrapeProfile, ScrapeProvider,
    SearchProfile, SearchProvider,
};

use super::super::web_search;

#[derive(Debug, Clone)]
pub struct SearchRuntime {
    pub provider: SearchProvider,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub default_engine: String,
    pub default_limit: u32,
    /// Extra accounts for the SAME provider: (auth-profile id, key), tried in
    /// order when the active key hits its quota.
    pub fallback_keys: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct CrawlRuntime {
    pub provider: CrawlProvider,
    pub api_key: String,
    pub base_url: String,
    /// Maximum provider requests in a rolling minute. Zero disables the
    /// client-side throttle; provider-side quotas still apply.
    pub rate_limit_per_minute: u32,
    pub fallback_keys: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct ScrapeRuntime {
    pub provider: ScrapeProvider,
    pub api_key: String,
    pub base_url: String,
    pub fallback_keys: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct WebRuntime {
    pub search: SearchRuntime,
    pub crawl: Option<CrawlRuntime>,
    pub scrape: Option<ScrapeRuntime>,
}

impl WebRuntime {
    pub fn load() -> Self {
        match PhoenixConfig::load() {
            Ok(config) => Self::from_config(&config),
            Err(_) => Self::default_fallback(),
        }
    }

    pub fn from_config(config: &PhoenixConfig) -> Self {
        let search = config
            .profile
            .search
            .as_ref()
            .map(|profile| {
                search_runtime_from_profile(profile, &config.profile.web_fallback.search)
            })
            .unwrap_or_else(|| SearchRuntime {
                provider: SearchProvider::DuckDuckGo,
                api_key: None,
                base_url: None,
                default_engine: "google".into(),
                default_limit: 5,
                fallback_keys: Vec::new(),
            });

        let crawl = config.profile.crawl.as_ref().and_then(|profile| {
            crawl_runtime_from_profile(profile, &config.profile.web_fallback.crawl)
        });
        let scrape = config.profile.scrape.as_ref().and_then(|profile| {
            scrape_runtime_from_profile(profile, &config.profile.web_fallback.scrape)
        });

        Self {
            search,
            crawl,
            scrape,
        }
    }

    pub fn default_fallback() -> Self {
        Self {
            search: SearchRuntime {
                provider: SearchProvider::DuckDuckGo,
                api_key: None,
                base_url: None,
                default_engine: "google".into(),
                default_limit: 5,
                fallback_keys: Vec::new(),
            },
            crawl: None,
            scrape: None,
        }
    }
}

fn search_runtime_from_profile(profile: &SearchProfile, chain: &[String]) -> SearchRuntime {
    let env_vars = search_env_vars(&profile.provider);
    let resolved =
        resolve_web_api_key(profile.auth.as_ref(), profile.api_key.as_deref(), &env_vars)
            .ok()
            .flatten();

    SearchRuntime {
        provider: profile.provider.clone(),
        api_key: resolved.map(|r| r.api_key),
        base_url: None,
        default_engine: profile.default_engine.clone(),
        default_limit: profile.num_results.max(1).min(20),
        fallback_keys: resolve_fallback_keys(chain),
    }
}

fn crawl_runtime_from_profile(profile: &CrawlProfile, chain: &[String]) -> Option<CrawlRuntime> {
    let env_vars = crawl_env_vars(&profile.provider);
    let resolved =
        resolve_web_api_key(profile.auth.as_ref(), profile.api_key.as_deref(), &env_vars)
            .ok()
            .flatten()?;
    Some(CrawlRuntime {
        provider: profile.provider.clone(),
        api_key: resolved.api_key,
        base_url: profile
            .base_url
            .clone()
            .unwrap_or_else(|| default_firecrawl_base()),
        rate_limit_per_minute: profile.rate_limit,
        fallback_keys: resolve_fallback_keys(chain),
    })
}

fn scrape_runtime_from_profile(profile: &ScrapeProfile, chain: &[String]) -> Option<ScrapeRuntime> {
    let env_vars = scrape_env_vars(&profile.provider);
    let resolved =
        resolve_web_api_key(profile.auth.as_ref(), profile.api_key.as_deref(), &env_vars)
            .ok()
            .flatten()?;
    Some(ScrapeRuntime {
        provider: profile.provider.clone(),
        api_key: resolved.api_key,
        base_url: profile
            .base_url
            .clone()
            .unwrap_or_else(|| default_scrape_base(&profile.provider)),
        fallback_keys: resolve_fallback_keys(chain),
    })
}

/// Resolve a web capability's fallback chain (auth-profile ids) into usable
/// (profile id, key) pairs. Unknown/expired profiles are skipped with a
/// warning — a broken fallback must never break the primary.
fn resolve_fallback_keys(chain: &[String]) -> Vec<(String, String)> {
    if chain.is_empty() {
        return Vec::new();
    }
    let Ok(store) = crate::config::auth_profile::load_auth_profile_store() else {
        return Vec::new();
    };
    let mut keys = Vec::new();
    for profile_id in chain {
        let Some(credential) = store.profiles.get(profile_id) else {
            eprintln!("warning: web fallback profile `{profile_id}` not found — skipped");
            continue;
        };
        match crate::config::auth_profile::extract_profile_secret(credential) {
            Ok(key) => keys.push((profile_id.clone(), key)),
            Err(error) => {
                eprintln!("warning: web fallback profile `{profile_id}` unusable ({error:#})");
            }
        }
    }
    keys
}

fn default_firecrawl_base() -> String {
    "https://api.firecrawl.dev".to_string()
}

fn default_scrape_base(provider: &ScrapeProvider) -> String {
    match provider {
        ScrapeProvider::Tavily => "https://api.tavily.com".to_string(),
        _ => default_firecrawl_base(),
    }
}

fn search_env_vars(provider: &SearchProvider) -> Vec<&'static str> {
    match provider {
        SearchProvider::Tavily => vec!["TAVILY_API_KEY"],
        SearchProvider::Exa => vec!["EXA_API_KEY"],
        SearchProvider::Brave => vec!["BRAVE_SEARCH_API_KEY"],
        SearchProvider::Serper => vec!["SERPER_API_KEY"],
        SearchProvider::SerpAPI => vec!["SERPAPI_API_KEY"],
        SearchProvider::Firecrawl => vec!["FIRECRAWL_API_KEY"],
        _ => vec![],
    }
}

fn crawl_env_vars(provider: &CrawlProvider) -> Vec<&'static str> {
    match provider {
        CrawlProvider::Firecrawl => vec!["FIRECRAWL_API_KEY"],
        CrawlProvider::Tavily => vec!["TAVILY_API_KEY"],
        _ => vec![],
    }
}

fn scrape_env_vars(provider: &ScrapeProvider) -> Vec<&'static str> {
    match provider {
        ScrapeProvider::Firecrawl => vec!["FIRECRAWL_API_KEY"],
        ScrapeProvider::Tavily => vec!["TAVILY_API_KEY"],
        _ => vec![],
    }
}

pub async fn search_duckduckgo(query: &str, limit: usize) -> Result<Vec<super::types::SearchHit>> {
    web_search::search_duckduckgo_public(query, limit).await
}
