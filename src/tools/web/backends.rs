use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde_json::{json, Value};

use crate::config::{CrawlProvider, ScrapeProvider, SearchProvider};

use super::context::{CrawlRuntime, ScrapeRuntime, SearchRuntime};
use super::types::{PageContent, SearchHit};

const USER_AGENT: &str = "PhoenixAgent/0.1 (web tools)";
const WEB_JSON_MAX_BYTES: usize = 16 * 1024 * 1024;
const WEB_ERROR_PREVIEW_CHARS: usize = 500;
const FIRECRAWL_CRAWL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(180);
const FIRECRAWL_JOB_ID_MAX_CHARS: usize = 512;
const WEB_OPERATION_DEADLINE: std::time::Duration = std::time::Duration::from_secs(240);

type CrawlRateWindows =
    std::collections::HashMap<String, std::collections::VecDeque<tokio::time::Instant>>;

fn crawl_rate_windows() -> &'static tokio::sync::Mutex<CrawlRateWindows> {
    static WINDOWS: std::sync::OnceLock<tokio::sync::Mutex<CrawlRateWindows>> =
        std::sync::OnceLock::new();
    WINDOWS.get_or_init(|| tokio::sync::Mutex::new(std::collections::HashMap::new()))
}

fn reserve_crawl_rate_slot(
    window: &mut std::collections::VecDeque<tokio::time::Instant>,
    now: tokio::time::Instant,
    limit: usize,
) -> Option<std::time::Duration> {
    while window
        .front()
        .is_some_and(|started| now.saturating_duration_since(*started).as_secs() >= 60)
    {
        window.pop_front();
    }
    if window.len() < limit {
        window.push_back(now);
        None
    } else {
        window.front().map(|started| {
            (*started + std::time::Duration::from_secs(60)).saturating_duration_since(now)
        })
    }
}

async fn wait_for_crawl_rate_slot(runtime: &CrawlRuntime) {
    let limit = runtime.rate_limit_per_minute as usize;
    if limit == 0 {
        return;
    }
    let key = format!("{:?}:{}", runtime.provider, runtime.base_url);
    loop {
        let now = tokio::time::Instant::now();
        let wait = {
            let mut windows = crawl_rate_windows().lock().await;
            let window = windows.entry(key.clone()).or_default();
            reserve_crawl_rate_slot(window, now, limit)
        };
        match wait {
            Some(duration) => tokio::time::sleep(duration).await,
            None => return,
        }
    }
}

fn client() -> Result<Client> {
    Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .context("failed to build HTTP client")
}

fn web_preview(value: &str) -> String {
    let preview: String = value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .take(WEB_ERROR_PREVIEW_CHARS)
        .collect();
    if value.chars().count() > WEB_ERROR_PREVIEW_CHARS {
        format!("{preview}...[truncated]")
    } else {
        preview
    }
}

fn web_json_error_preview(body: &Value, raw: &str) -> String {
    let message = body
        .pointer("/error/message")
        .or_else(|| body.get("error"))
        .or_else(|| body.get("message"))
        .or_else(|| body.get("detail"))
        .and_then(Value::as_str)
        .unwrap_or(raw);
    web_preview(message)
}

fn parse_web_json(raw: &str, status: reqwest::StatusCode, label: &str) -> Result<Value> {
    let body: Value = serde_json::from_str(raw).with_context(|| {
        format!(
            "{label} returned invalid JSON (HTTP {status}): {}",
            web_preview(raw)
        )
    })?;
    if status.is_success()
        && (body.get("success") == Some(&Value::Bool(false))
            || body.get("error").is_some_and(|error| !error.is_null()))
    {
        bail!(
            "{label} reported an error despite HTTP {status}: {}",
            web_json_error_preview(&body, raw)
        );
    }
    if !status.is_success() {
        bail!(
            "{label} returned HTTP {status}: {}",
            web_json_error_preview(&body, raw)
        );
    }
    Ok(body)
}

async fn read_web_json(response: reqwest::Response, label: &str) -> Result<Value> {
    let status = response.status();
    let raw = crate::providers::read_response_text(response, WEB_JSON_MAX_BYTES, label)
        .await
        .with_context(|| format!("failed to read {label} (HTTP {status})"))?;
    parse_web_json(&raw, status, label)
}

fn validate_firecrawl_job_id(job_id: &str) -> Result<()> {
    anyhow::ensure!(
        !job_id.is_empty()
            && job_id.chars().take(FIRECRAWL_JOB_ID_MAX_CHARS + 1).count()
                <= FIRECRAWL_JOB_ID_MAX_CHARS
            && job_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')),
        "Firecrawl crawl returned an invalid job id"
    );
    Ok(())
}

async fn with_web_deadline<T>(
    capability: &str,
    deadline: std::time::Duration,
    future: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    tokio::time::timeout(deadline, future)
        .await
        .with_context(|| {
            format!(
                "web {capability} exceeded its {}-second deadline",
                deadline.as_secs()
            )
        })?
}

/// True when a web-provider error means "this ACCOUNT/key is done" — quota,
/// credits, or auth — so the next account in the chain is worth trying.
/// Anything else (bad URL, network, provider outage without status) surfaces
/// unchanged: every key would fail the same way.
fn is_web_quota_error(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("429")
        || m.contains("402")
        || m.contains("401")
        || m.contains("403")
        || m.contains("too many requests")
        || m.contains("rate limit")
        || m.contains("quota")
        || m.contains("credit")
        || m.contains("payment")
        || m.contains("unauthorized")
        || m.contains("usage limit")
}

fn first_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(160)
        .collect()
}

/// Run a web call across the account chain: primary key first, then each
/// fallback key on quota/auth exhaustion. `op` is invoked with one key at a
/// time; non-quota errors surface immediately.
async fn with_key_rotation<T, F, Fut>(
    capability: &str,
    primary: Option<String>,
    fallbacks: &[(String, String)],
    mut op: F,
) -> Result<T>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut candidates: Vec<(String, Option<String>)> = vec![("primary".to_string(), primary)];
    candidates.extend(
        fallbacks
            .iter()
            .map(|(label, key)| (label.clone(), Some(key.clone()))),
    );
    let total = candidates.len();
    let mut last_error: Option<anyhow::Error> = None;
    for (pos, (label, key)) in candidates.into_iter().enumerate() {
        match op(key).await {
            Ok(value) => return Ok(value),
            Err(error) => {
                let message = format!("{error:#}");
                if pos + 1 >= total || !is_web_quota_error(&message) {
                    return Err(error);
                }
                eprintln!(
                    "web fallback [{capability}]: `{label}` exhausted ({}) — rotating to the next account",
                    first_line(&message)
                );
                last_error = Some(error);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("no web accounts available")))
}

pub async fn run_search(
    runtime: &SearchRuntime,
    query: &str,
    limit: usize,
    recency_days: Option<u32>,
) -> Result<Vec<SearchHit>> {
    with_web_deadline(
        "search",
        WEB_OPERATION_DEADLINE,
        with_key_rotation(
            "search",
            runtime.api_key.clone(),
            &runtime.fallback_keys,
            |key| {
                let mut attempt = runtime.clone();
                attempt.api_key = key;
                async move { run_search_once(&attempt, query, limit, recency_days).await }
            },
        ),
    )
    .await
}

async fn run_search_once(
    runtime: &SearchRuntime,
    query: &str,
    limit: usize,
    recency_days: Option<u32>,
) -> Result<Vec<SearchHit>> {
    match runtime.provider {
        SearchProvider::DuckDuckGo => super::context::search_duckduckgo(query, limit).await,
        SearchProvider::Tavily => {
            let key = runtime
                .api_key
                .as_deref()
                .context("Tavily search requires TAVILY_API_KEY or [profile.search.auth]")?;
            tavily_search(key, runtime.base_url.as_deref(), query, limit, recency_days).await
        }
        SearchProvider::Exa => {
            let key = runtime
                .api_key
                .as_deref()
                .context("Exa search requires EXA_API_KEY or [profile.search.auth]")?;
            exa_search(key, query, limit).await
        }
        SearchProvider::Brave => {
            let key = runtime
                .api_key
                .as_deref()
                .context("Brave search requires BRAVE_SEARCH_API_KEY or [profile.search.auth]")?;
            brave_search(key, query, limit).await
        }
        SearchProvider::Serper => {
            let key = runtime
                .api_key
                .as_deref()
                .context("Serper search requires SERPER_API_KEY or [profile.search.auth]")?;
            serper_search(key, query, limit).await
        }
        SearchProvider::SerpAPI => {
            let key = runtime
                .api_key
                .as_deref()
                .context("SerpAPI search requires SERPAPI_API_KEY or [profile.search.auth]")?;
            serpapi_search(
                key,
                &runtime.default_engine,
                runtime.base_url.as_deref(),
                query,
                limit,
            )
            .await
        }
        SearchProvider::Firecrawl => {
            let key = runtime
                .api_key
                .as_deref()
                .context("Firecrawl search requires FIRECRAWL_API_KEY or [profile.search.auth]")?;
            firecrawl_search(key, runtime.base_url.as_deref(), query, limit).await
        }
        SearchProvider::Google | SearchProvider::Bing | SearchProvider::Custom => {
            bail!(
                "Legacy search provider {:?} is not an available Phoenix backend. Run `phoenix onboard` and pick Tavily, Exa, Brave, Firecrawl, Serper, SerpAPI, or DuckDuckGo.",
                runtime.provider
            );
        }
    }
}

pub async fn run_scrape(
    runtime: &ScrapeRuntime,
    url: &str,
    max_chars: usize,
) -> Result<PageContent> {
    with_web_deadline(
        "scrape",
        WEB_OPERATION_DEADLINE,
        with_key_rotation(
            "scrape",
            Some(runtime.api_key.clone()),
            &runtime.fallback_keys,
            |key| {
                let mut attempt = runtime.clone();
                attempt.api_key = key.expect("scrape rotation always passes a key");
                async move { run_scrape_once(&attempt, url, max_chars).await }
            },
        ),
    )
    .await
}

async fn run_scrape_once(
    runtime: &ScrapeRuntime,
    url: &str,
    max_chars: usize,
) -> Result<PageContent> {
    match runtime.provider {
        ScrapeProvider::Firecrawl => {
            firecrawl_scrape(&runtime.api_key, &runtime.base_url, url, max_chars).await
        }
        ScrapeProvider::Tavily => {
            tavily_extract(&runtime.api_key, &runtime.base_url, url, max_chars).await
        }
        ScrapeProvider::ScraperAPI | ScrapeProvider::ScrapingBee | ScrapeProvider::Custom => {
            bail!(
                "Scrape provider {:?} is not wired yet. Use Firecrawl or Tavily in [profile.scrape].",
                runtime.provider
            );
        }
    }
}

pub async fn run_crawl(
    runtime: &CrawlRuntime,
    url: &str,
    limit: usize,
    max_chars_per_page: usize,
) -> Result<Vec<PageContent>> {
    with_web_deadline(
        "crawl",
        WEB_OPERATION_DEADLINE,
        with_key_rotation(
            "crawl",
            Some(runtime.api_key.clone()),
            &runtime.fallback_keys,
            |key| {
                let mut attempt = runtime.clone();
                attempt.api_key = key.expect("crawl rotation always passes a key");
                async move { run_crawl_once(&attempt, url, limit, max_chars_per_page).await }
            },
        ),
    )
    .await
}

async fn run_crawl_once(
    runtime: &CrawlRuntime,
    url: &str,
    limit: usize,
    max_chars_per_page: usize,
) -> Result<Vec<PageContent>> {
    wait_for_crawl_rate_slot(runtime).await;
    match runtime.provider {
        CrawlProvider::Firecrawl => {
            firecrawl_crawl(
                &runtime.api_key,
                &runtime.base_url,
                url,
                limit,
                max_chars_per_page,
            )
            .await
        }
        CrawlProvider::Tavily => {
            tavily_crawl(
                &runtime.api_key,
                &runtime.base_url,
                url,
                limit,
                max_chars_per_page,
            )
            .await
        }
        CrawlProvider::Crawl4AI
        | CrawlProvider::Scrapfly
        | CrawlProvider::Apify
        | CrawlProvider::Custom => {
            bail!(
                "Crawl provider {:?} is not wired yet. Use Firecrawl or Tavily in [profile.crawl].",
                runtime.provider
            );
        }
    }
}

async fn tavily_search(
    api_key: &str,
    base_url: Option<&str>,
    query: &str,
    limit: usize,
    recency_days: Option<u32>,
) -> Result<Vec<SearchHit>> {
    let base = base_url.unwrap_or("https://api.tavily.com");
    let body = tavily_search_body(api_key, query, limit, recency_days);
    let response = client()?
        .post(format!("{base}/search"))
        .json(&body)
        .send()
        .await?;
    let response = read_web_json(response, "Tavily search").await?;

    let mut hits = Vec::new();
    if let Some(results) = response.get("results").and_then(|v| v.as_array()) {
        for item in results {
            hits.push(SearchHit {
                title: item
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                url: item
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                snippet: item
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }
    Ok(hits)
}

fn tavily_search_body(
    api_key: &str,
    query: &str,
    limit: usize,
    recency_days: Option<u32>,
) -> Value {
    let mut body = json!({
        "api_key": api_key,
        "query": query,
        "max_results": limit.min(20),
        "include_raw_content": false,
    });
    if let Some(days) = recency_days {
        // Date words inside a semantic query are not a freshness filter.
        // Tavily enforces `days` only on its news topic, so send both.
        body["topic"] = json!("news");
        body["days"] = json!(days.clamp(1, 365));
    }
    body
}

async fn tavily_extract_once(
    api_key: &str,
    base_url: &str,
    url: &str,
    depth: &str,
) -> Result<Value> {
    let body = json!({
        "api_key": api_key,
        "urls": [url],
        "extract_depth": depth,
    });
    let response = client()?
        .post(format!("{base_url}/extract"))
        .json(&body)
        .send()
        .await?;
    read_web_json(response, "Tavily extract").await
}

async fn tavily_extract(
    api_key: &str,
    base_url: &str,
    url: &str,
    max_chars: usize,
) -> Result<PageContent> {
    // Basic depth first (cheap); a stubborn page gets one advanced-depth
    // retry before this errors into the caller's HTTP fallback.
    let mut response = tavily_extract_once(api_key, base_url, url, "basic").await?;
    let empty = response
        .get("results")
        .and_then(|v| v.as_array())
        .map_or(true, |arr| arr.is_empty());
    if empty {
        response = tavily_extract_once(api_key, base_url, url, "advanced").await?;
    }

    let result = response
        .get("results")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .context("Tavily extract returned no results")?;

    let content = result
        .get("raw_content")
        .or_else(|| result.get("content"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    Ok(PageContent {
        url: result
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or(url)
            .to_string(),
        title: result
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        content: truncate(content, max_chars),
    })
}

async fn tavily_crawl(
    api_key: &str,
    base_url: &str,
    url: &str,
    limit: usize,
    max_chars_per_page: usize,
) -> Result<Vec<PageContent>> {
    let body = json!({
        "api_key": api_key,
        "url": url,
        "limit": limit.min(50),
        "extract_depth": "basic",
    });
    let response = client()?
        .post(format!("{base_url}/crawl"))
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&body)
        .send()
        .await?;
    let response = read_web_json(response, "Tavily crawl").await?;

    let mut pages = Vec::new();
    if let Some(results) = response.get("results").and_then(|v| v.as_array()) {
        for item in results {
            let content = item
                .get("raw_content")
                .or_else(|| item.get("content"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            pages.push(PageContent {
                url: item
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or(url)
                    .to_string(),
                title: item
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                content: truncate(content, max_chars_per_page),
            });
        }
    }
    Ok(pages)
}

async fn exa_search(api_key: &str, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
    let body = json!({
        "query": query,
        "numResults": limit.min(20),
        "type": "auto",
    });
    let response = client()?
        .post("https://api.exa.ai/search")
        .header("x-api-key", api_key)
        .json(&body)
        .send()
        .await?;
    let response = read_web_json(response, "Exa search").await?;

    let mut hits = Vec::new();
    if let Some(results) = response.get("results").and_then(|v| v.as_array()) {
        for item in results {
            let highlights = item
                .get("highlights")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|h| h.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            hits.push(SearchHit {
                title: item
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                url: item
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                snippet: highlights,
            });
        }
    }
    Ok(hits)
}

async fn brave_search(api_key: &str, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
    let response = client()?
        .get("https://api.search.brave.com/res/v1/web/search")
        .header("X-Subscription-Token", api_key)
        .query(&[("q", query), ("count", &limit.min(20).to_string())])
        .send()
        .await?;
    let response = read_web_json(response, "Brave search").await?;

    let mut hits = Vec::new();
    if let Some(results) = response
        .get("web")
        .and_then(|v| v.get("results"))
        .and_then(|v| v.as_array())
    {
        for item in results {
            hits.push(SearchHit {
                title: item
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                url: item
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                snippet: item
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }
    Ok(hits)
}

async fn serper_search(api_key: &str, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
    let body = json!({ "q": query, "num": limit.min(20) });
    let response = client()?
        .post("https://google.serper.dev/search")
        .header("X-API-KEY", api_key)
        .json(&body)
        .send()
        .await?;
    let response = read_web_json(response, "Serper search").await?;

    let mut hits = Vec::new();
    if let Some(organic) = response.get("organic").and_then(|v| v.as_array()) {
        for item in organic {
            hits.push(SearchHit {
                title: item
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                url: item
                    .get("link")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                snippet: item
                    .get("snippet")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }
    Ok(hits)
}

fn serpapi_hits(response: &Value, limit: usize) -> Vec<SearchHit> {
    response
        .get("organic_results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let url = item.get("link").and_then(Value::as_str)?.trim();
            if url.is_empty() {
                return None;
            }
            Some(SearchHit {
                title: item
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                url: url.to_string(),
                snippet: item
                    .get("snippet")
                    .or_else(|| item.get("snippet_highlighted_words"))
                    .and_then(|value| match value {
                        Value::String(text) => Some(text.clone()),
                        Value::Array(words) => Some(
                            words
                                .iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join(" "),
                        ),
                        _ => None,
                    })
                    .unwrap_or_default(),
            })
        })
        .take(limit.min(20))
        .collect()
}

async fn serpapi_search(
    api_key: &str,
    engine: &str,
    base_url: Option<&str>,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchHit>> {
    let engine = engine.trim();
    anyhow::ensure!(
        !engine.is_empty()
            && engine.len() <= 64
            && engine
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')),
        "SerpAPI search engine must be a simple 1..=64 character id"
    );
    let endpoint = base_url.unwrap_or("https://serpapi.com/search.json");
    let response = client()?
        .get(endpoint)
        .query(&[
            ("api_key", api_key),
            ("engine", engine),
            ("q", query),
            ("num", &limit.min(20).to_string()),
        ])
        .send()
        .await?;
    let response = read_web_json(response, "SerpAPI search").await?;
    Ok(serpapi_hits(&response, limit))
}

fn firecrawl_headers(api_key: &str) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {api_key}").parse().expect("valid bearer"),
    );
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        "application/json".parse().expect("valid content-type"),
    );
    headers
}

async fn firecrawl_search(
    api_key: &str,
    base_url: Option<&str>,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchHit>> {
    let base = base_url.unwrap_or("https://api.firecrawl.dev");
    let body = json!({ "query": query, "limit": limit.min(20) });
    let response = client()?
        .post(format!("{base}/v1/search"))
        .headers(firecrawl_headers(api_key))
        .json(&body)
        .send()
        .await?;
    let response = read_web_json(response, "Firecrawl search").await?;

    let data = response
        .get("data")
        .or_else(|| response.get("results"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut hits = Vec::new();
    for item in data {
        hits.push(SearchHit {
            title: item
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            url: item
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            snippet: item
                .get("description")
                .or_else(|| item.get("markdown"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        });
    }
    Ok(hits)
}

async fn firecrawl_scrape(
    api_key: &str,
    base_url: &str,
    url: &str,
    max_chars: usize,
) -> Result<PageContent> {
    let body = json!({
        "url": url,
        "formats": ["markdown"],
    });
    let response = client()?
        .post(format!("{base_url}/v1/scrape"))
        .headers(firecrawl_headers(api_key))
        .json(&body)
        .send()
        .await?;
    let response = read_web_json(response, "Firecrawl scrape").await?;

    let data = response.get("data").unwrap_or(&response);
    let markdown = data
        .get("markdown")
        .and_then(|v| v.as_str())
        .or_else(|| data.get("content").and_then(|v| v.as_str()))
        .unwrap_or("");
    let metadata = data.get("metadata").unwrap_or(&Value::Null);
    Ok(PageContent {
        url: metadata
            .get("sourceURL")
            .or_else(|| metadata.get("url"))
            .and_then(|v| v.as_str())
            .unwrap_or(url)
            .to_string(),
        title: metadata
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        content: truncate(markdown, max_chars),
    })
}

async fn firecrawl_crawl(
    api_key: &str,
    base_url: &str,
    url: &str,
    limit: usize,
    max_chars_per_page: usize,
) -> Result<Vec<PageContent>> {
    let body = json!({
        "url": url,
        "limit": limit.min(50),
        "scrapeOptions": { "formats": ["markdown"] },
    });
    let deadline = tokio::time::Instant::now() + FIRECRAWL_CRAWL_DEADLINE;
    let start_response = tokio::time::timeout_at(
        deadline,
        client()?
            .post(format!("{base_url}/v1/crawl"))
            .headers(firecrawl_headers(api_key))
            .json(&body)
            .send(),
    )
    .await
    .with_context(|| {
        format!(
            "Firecrawl crawl exceeded its {}-second deadline",
            FIRECRAWL_CRAWL_DEADLINE.as_secs()
        )
    })??;
    let start = tokio::time::timeout_at(
        deadline,
        read_web_json(start_response, "Firecrawl crawl start"),
    )
    .await
    .with_context(|| {
        format!(
            "Firecrawl crawl exceeded its {}-second deadline",
            FIRECRAWL_CRAWL_DEADLINE.as_secs()
        )
    })??;

    let job_id = start
        .get("id")
        .or_else(|| start.get("jobId"))
        .and_then(|v| v.as_str())
        .context("Firecrawl crawl did not return a job id")?;
    validate_firecrawl_job_id(job_id)?;

    let client = client()?;
    let status_url = format!("{base_url}/v1/crawl/{job_id}");
    for _ in 0..60 {
        let response = tokio::time::timeout_at(deadline, async {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            client
                .get(&status_url)
                .headers(firecrawl_headers(api_key))
                .send()
                .await
        })
        .await
        .with_context(|| {
            format!(
                "Firecrawl crawl {job_id} exceeded its {}-second deadline",
                FIRECRAWL_CRAWL_DEADLINE.as_secs()
            )
        })??;
        let status =
            tokio::time::timeout_at(deadline, read_web_json(response, "Firecrawl crawl status"))
                .await
                .with_context(|| {
                    format!(
                        "Firecrawl crawl {job_id} exceeded its {}-second deadline",
                        FIRECRAWL_CRAWL_DEADLINE.as_secs()
                    )
                })??;

        let state = status.get("status").and_then(|v| v.as_str()).unwrap_or("");
        if state.eq_ignore_ascii_case("completed") || status.get("success") == Some(&json!(true)) {
            let data = status
                .get("data")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let mut pages = Vec::new();
            for item in data {
                let markdown = item
                    .get("markdown")
                    .and_then(|v| v.as_str())
                    .or_else(|| item.get("content").and_then(|v| v.as_str()))
                    .unwrap_or("");
                let metadata = item.get("metadata").unwrap_or(&Value::Null);
                pages.push(PageContent {
                    url: metadata
                        .get("sourceURL")
                        .or_else(|| metadata.get("url"))
                        .or_else(|| item.get("url"))
                        .and_then(|v| v.as_str())
                        .unwrap_or(url)
                        .to_string(),
                    title: metadata
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    content: truncate(markdown, max_chars_per_page),
                });
            }
            return Ok(pages);
        }
        if state.eq_ignore_ascii_case("failed") {
            let detail = status
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            bail!("Firecrawl crawl failed: {}", web_preview(detail));
        }
    }
    bail!("Firecrawl crawl timed out waiting for job {job_id}");
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value.to_string()
    } else {
        let truncated: String = value.chars().take(max_chars).collect();
        format!("{truncated}\n...[truncated]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_json_parser_keeps_status_and_bounded_provider_detail() {
        let raw = serde_json::json!({
            "error": {"message": format!("quota {}", "x".repeat(2_000))}
        })
        .to_string();
        let error = parse_web_json(&raw, reqwest::StatusCode::TOO_MANY_REQUESTS, "fixture")
            .expect_err("non-success must fail")
            .to_string();
        assert!(error.contains("429"));
        assert!(error.contains("quota"));
        assert!(error.contains("[truncated]"));
        assert!(error.chars().count() < 700);
    }

    #[test]
    fn web_json_parser_rejects_non_json_with_status_context() {
        let error = parse_web_json(
            "<html>bad gateway</html>",
            reqwest::StatusCode::BAD_GATEWAY,
            "fixture",
        )
        .expect_err("HTML must not be accepted as JSON")
        .to_string();
        assert!(error.contains("502"));
        assert!(error.contains("invalid JSON"));
    }

    #[test]
    fn firecrawl_job_ids_are_bounded_path_components() {
        assert!(validate_firecrawl_job_id("job_123-abc").is_ok());
        assert!(validate_firecrawl_job_id("../admin").is_err());
        assert!(validate_firecrawl_job_id(&"x".repeat(FIRECRAWL_JOB_ID_MAX_CHARS + 1)).is_err());
    }

    #[test]
    fn web_json_parser_rejects_success_status_error_envelopes() {
        let error = parse_web_json(
            r#"{"success":false,"error":"logical failure"}"#,
            reqwest::StatusCode::OK,
            "fixture",
        )
        .expect_err("HTTP 200 logical errors must fail")
        .to_string();
        assert!(error.contains("logical failure"));
    }

    #[test]
    fn tavily_recency_is_a_real_news_filter_not_query_decoration() {
        let body = tavily_search_body("secret", "official AI posts", 60, Some(3));
        assert_eq!(body["topic"], "news");
        assert_eq!(body["days"], 3);
        assert_eq!(body["max_results"], 20);

        let general = tavily_search_body("secret", "evergreen topic", 5, None);
        assert!(general.get("topic").is_none());
        assert!(general.get("days").is_none());
    }

    #[test]
    fn serpapi_parser_returns_only_bounded_linked_organic_results() {
        let response = serde_json::json!({
            "organic_results": [
                {"title": "Phoenix", "link": "https://example.com/phoenix", "snippet": "first"},
                {"title": "No link", "snippet": "must be dropped"},
                {"title": "Second", "link": "https://example.com/two", "snippet_highlighted_words": ["two", "words"]}
            ]
        });
        let hits = serpapi_hits(&response, 2);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "Phoenix");
        assert_eq!(hits[1].snippet, "two words");
    }

    #[test]
    fn crawl_rate_window_is_rolling_and_reserves_exactly_the_configured_slots() {
        let start = tokio::time::Instant::now();
        let mut window = std::collections::VecDeque::new();
        assert_eq!(reserve_crawl_rate_slot(&mut window, start, 2), None);
        assert_eq!(reserve_crawl_rate_slot(&mut window, start, 2), None);
        assert_eq!(
            reserve_crawl_rate_slot(&mut window, start, 2),
            Some(std::time::Duration::from_secs(60))
        );
        let later = start + std::time::Duration::from_secs(61);
        assert_eq!(reserve_crawl_rate_slot(&mut window, later, 2), None);
        assert_eq!(window.len(), 1);
    }

    #[tokio::test]
    async fn web_deadline_cancels_stalled_operations() {
        let error = with_web_deadline(
            "fixture",
            std::time::Duration::ZERO,
            std::future::pending::<Result<()>>(),
        )
        .await
        .expect_err("stalled operation must time out")
        .to_string();
        assert!(error.contains("deadline"));
    }
}
