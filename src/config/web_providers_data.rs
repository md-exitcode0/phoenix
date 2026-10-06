//! Catalog of web search / crawl / scrape providers for onboarding.

#[derive(Debug, Clone, Copy)]
pub struct WebProviderEntry {
    pub id: &'static str,
    pub name: &'static str,
    pub blurb: &'static str,
    pub env_var: &'static str,
    pub signup_url: &'static str,
    pub requires_key: bool,
}

pub const SEARCH_PROVIDERS: &[WebProviderEntry] = &[
    WebProviderEntry {
        id: "duckduckgo",
        name: "DuckDuckGo (no API key)",
        blurb: "Free HTML search fallback; lower quality than paid APIs.",
        env_var: "",
        signup_url: "",
        requires_key: false,
    },
    WebProviderEntry {
        id: "tavily",
        name: "Tavily",
        blurb: "Search + extract + crawl via Tavily API.",
        env_var: "TAVILY_API_KEY",
        signup_url: "https://app.tavily.com/home",
        requires_key: true,
    },
    WebProviderEntry {
        id: "firecrawl",
        name: "Firecrawl (search)",
        blurb: "Search via Firecrawl cloud API (same key as scrape/crawl).",
        env_var: "FIRECRAWL_API_KEY",
        signup_url: "https://firecrawl.dev",
        requires_key: true,
    },
    WebProviderEntry {
        id: "exa",
        name: "Exa",
        blurb: "Neural web search and contents API.",
        env_var: "EXA_API_KEY",
        signup_url: "https://exa.ai",
        requires_key: true,
    },
    WebProviderEntry {
        id: "brave",
        name: "Brave Search",
        blurb: "Brave Search API (free tier available).",
        env_var: "BRAVE_SEARCH_API_KEY",
        signup_url: "https://brave.com/search/api/",
        requires_key: true,
    },
    WebProviderEntry {
        id: "serper",
        name: "Serper (Google)",
        blurb: "Google results via serper.dev.",
        env_var: "SERPER_API_KEY",
        signup_url: "https://serper.dev",
        requires_key: true,
    },
    WebProviderEntry {
        id: "serpapi",
        name: "SerpAPI",
        blurb: "Google and other search engines through SerpAPI.",
        env_var: "SERPAPI_API_KEY",
        signup_url: "https://serpapi.com",
        requires_key: true,
    },
];

pub const CRAWL_PROVIDERS: &[WebProviderEntry] = &[
    WebProviderEntry {
        id: "skip",
        name: "Skip crawl provider",
        blurb: "Do not configure web_crawl yet.",
        env_var: "",
        signup_url: "",
        requires_key: false,
    },
    WebProviderEntry {
        id: "firecrawl",
        name: "Firecrawl",
        blurb: "Site crawl jobs via Firecrawl API.",
        env_var: "FIRECRAWL_API_KEY",
        signup_url: "https://firecrawl.dev",
        requires_key: true,
    },
    WebProviderEntry {
        id: "tavily",
        name: "Tavily",
        blurb: "Crawl via Tavily /crawl endpoint.",
        env_var: "TAVILY_API_KEY",
        signup_url: "https://app.tavily.com/home",
        requires_key: true,
    },
];

pub const SCRAPE_PROVIDERS: &[WebProviderEntry] = &[
    WebProviderEntry {
        id: "skip",
        name: "Skip scrape provider (plain HTTP fetch)",
        blurb: "web_fetch uses direct HTTP unless you configure scrape.",
        env_var: "",
        signup_url: "",
        requires_key: false,
    },
    WebProviderEntry {
        id: "firecrawl",
        name: "Firecrawl",
        blurb: "High-quality page scrape to markdown.",
        env_var: "FIRECRAWL_API_KEY",
        signup_url: "https://firecrawl.dev",
        requires_key: true,
    },
    WebProviderEntry {
        id: "tavily",
        name: "Tavily",
        blurb: "Extract page content via Tavily /extract.",
        env_var: "TAVILY_API_KEY",
        signup_url: "https://app.tavily.com/home",
        requires_key: true,
    },
];

pub fn get_search_provider(id: &str) -> Option<&'static WebProviderEntry> {
    SEARCH_PROVIDERS.iter().find(|p| p.id == id)
}

pub fn get_crawl_provider(id: &str) -> Option<&'static WebProviderEntry> {
    CRAWL_PROVIDERS.iter().find(|p| p.id == id)
}

pub fn get_scrape_provider(id: &str) -> Option<&'static WebProviderEntry> {
    SCRAPE_PROVIDERS.iter().find(|p| p.id == id)
}

pub fn web_profile_id(capability: &str, provider_id: &str) -> String {
    format!("{capability}-{provider_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_visible_capability_catalog_has_unique_ids() {
        for entries in [SEARCH_PROVIDERS, CRAWL_PROVIDERS, SCRAPE_PROVIDERS] {
            let mut ids = std::collections::BTreeSet::new();
            for entry in entries {
                assert!(ids.insert(entry.id), "duplicate web provider {}", entry.id);
                assert!(!entry.name.trim().is_empty());
                assert!(!entry.blurb.trim().is_empty());
                if entry.requires_key {
                    assert!(!entry.env_var.is_empty());
                    assert!(entry.signup_url.starts_with("https://"));
                }
            }
        }
    }

    #[test]
    fn serpapi_is_visible_now_that_its_backend_is_executable() {
        let provider = get_search_provider("serpapi").expect("SerpAPI search catalog entry");
        assert_eq!(provider.env_var, "SERPAPI_API_KEY");
        assert!(provider.requires_key);
    }
}
