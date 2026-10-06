//! web_search tool — uses configured search provider (Tavily, Exa, Brave, …) or DuckDuckGo fallback.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::web::WebRuntime;
use super::ToolOutput;

const MAX_RESULTS: usize = 20;
const MAX_BATCH_RESULTS: usize = 10;
const MAX_BATCH_QUERIES: usize = 8;
const MAX_SNIPPET_CHARS: usize = 300;
const MAX_QUERY_CHARS: usize = 4_096;
const DUCKDUCKGO_BODY_MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchInput {
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub queries: Vec<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    /// Provider-enforced freshness when available. The active Tavily backend
    /// maps this to topic=news + days instead of relying on query prose.
    #[serde(default)]
    pub recency_days: Option<u32>,
}

fn resolved_limit(requested: Option<usize>, configured: u32) -> usize {
    requested
        .unwrap_or(configured as usize)
        .clamp(1, MAX_RESULTS)
}

pub async fn execute(input: WebSearchInput, web: WebRuntime) -> Result<ToolOutput> {
    let queries = normalized_queries(input.query, input.queries)?;
    let recency_days = normalized_recency(input.recency_days)?;
    let requested_limit = resolved_limit(input.limit, web.search.default_limit);
    let limit = if queries.len() > 1 {
        requested_limit.min(MAX_BATCH_RESULTS)
    } else {
        requested_limit
    };
    let provider_label = format!("{:?}", web.search.provider);
    if queries.len() == 1 {
        return execute_one(&queries[0], limit, recency_days, &provider_label, &web).await;
    }

    let searches = queries
        .iter()
        .map(|query| super::web::search(&web, query, limit, recency_days));
    let outcomes = futures_util::future::join_all(searches).await;
    batch_output(&queries, outcomes, &provider_label, recency_days)
}

fn batch_output(
    queries: &[String],
    outcomes: Vec<Result<Vec<super::web::SearchHit>>>,
    provider_label: &str,
    recency_days: Option<u32>,
) -> Result<ToolOutput> {
    let mut seen_urls = std::collections::HashSet::new();
    let mut total = 0usize;
    let mut completed = 0usize;
    let sections = queries
        .iter()
        .zip(outcomes)
        .map(|(query, outcome)| match outcome {
            Ok(results) => {
                completed += 1;
                let unique = results
                    .into_iter()
                    .filter(|result| seen_urls.insert(result.url.clone()))
                    .collect::<Vec<_>>();
                total += unique.len();
                if unique.is_empty() {
                    format!(
                        "## {query}\nNo new results (empty or duplicated across earlier lanes)."
                    )
                } else {
                    format!("## {query}\n{}", format_results(&unique))
                }
            }
            Err(error) => format!("## {query}\nSearch provider error: {error}"),
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    if completed == 0 {
        bail!(
            "web_search: all {} search lanes failed via {provider_label}.\n{sections}",
            queries.len()
        );
    }

    Ok(ToolOutput {
        summary: format!(
            "Ran {} search lanes concurrently via {provider_label}{}; {completed} completed with {total} unique result(s).",
            queries.len(),
            recency_days.map_or_else(String::new, |days| format!(" with a {days}-day freshness filter"))
        ),
        content: sections,
    })
}

fn normalized_recency(recency_days: Option<u32>) -> Result<Option<u32>> {
    match recency_days {
        Some(0) => bail!("web_search recency_days must be at least 1"),
        Some(days) if days > 365 => bail!("web_search recency_days cannot exceed 365"),
        value => Ok(value),
    }
}

fn normalized_queries(query: String, queries: Vec<String>) -> Result<Vec<String>> {
    if !query.trim().is_empty() && !queries.is_empty() {
        bail!("web_search accepts either query or queries, not both");
    }
    let candidates = if queries.is_empty() {
        vec![query]
    } else {
        queries
    };
    if candidates.len() > MAX_BATCH_QUERIES {
        bail!("web_search accepts at most {MAX_BATCH_QUERIES} queries per batch");
    }
    let mut seen = std::collections::HashSet::new();
    let mut normalized = Vec::new();
    for candidate in candidates {
        let value = candidate.trim();
        if value.is_empty() {
            bail!("web_search queries cannot be empty");
        }
        if value.chars().take(MAX_QUERY_CHARS + 1).count() > MAX_QUERY_CHARS {
            bail!("web_search query exceeds the {MAX_QUERY_CHARS}-character limit");
        }
        if seen.insert(value.to_ascii_lowercase()) {
            normalized.push(value.to_string());
        }
    }
    if normalized.is_empty() {
        bail!("web_search query cannot be empty");
    }
    Ok(normalized)
}

async fn execute_one(
    query: &str,
    limit: usize,
    recency_days: Option<u32>,
    provider_label: &str,
    web: &WebRuntime,
) -> Result<ToolOutput> {
    single_output(query, super::web::search(web, query, limit, recency_days).await, provider_label, recency_days)
}

fn single_output(
    query: &str,
    outcome: Result<Vec<super::web::SearchHit>>,
    provider_label: &str,
    recency_days: Option<u32>,
) -> Result<ToolOutput> {
    let results = outcome.with_context(|| format!("web_search: search for {query:?} failed via {provider_label}"))?;

    if results.is_empty() {
        return Ok(ToolOutput {
            summary: format!("No results found for \"{query}\" ({provider_label})."),
            content: format!("No search results for \"{query}\"."),
        });
    }

    let formatted = format_results(&results);

    Ok(ToolOutput {
        summary: format!(
            "Found {} result(s) for \"{query}\" via {provider_label}{}.",
            results.len(),
            recency_days.map_or_else(String::new, |days| format!(
                " with a {days}-day freshness filter"
            ))
        ),
        content: formatted,
    })
}

fn format_results(results: &[super::web::SearchHit]) -> String {
    results
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let snippet = truncate(&r.snippet, MAX_SNIPPET_CHARS);
            format!(
                "{}. {}\n   URL: {}\n   {}\n",
                i + 1,
                r.title,
                r.url,
                snippet
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// DuckDuckGo HTML search (no API key). Used as default backend and for tests.
pub async fn search_duckduckgo_public(
    query: &str,
    limit: usize,
) -> Result<Vec<super::web::SearchHit>> {
    let results = search_duckduckgo_html(query, limit).await?;
    Ok(results
        .into_iter()
        .map(|r| super::web::SearchHit {
            title: r.title,
            url: r.url,
            snippet: r.snippet,
        })
        .collect())
}

#[derive(Debug, Clone)]
struct DuckResult {
    title: String,
    url: String,
    snippet: String,
}

async fn search_duckduckgo_html(query: &str, limit: usize) -> Result<Vec<DuckResult>> {
    if query.chars().take(MAX_QUERY_CHARS + 1).count() > MAX_QUERY_CHARS {
        bail!("DuckDuckGo query exceeds the {MAX_QUERY_CHARS}-character limit");
    }
    let client = reqwest::Client::builder()
        .user_agent("PhoenixAgent/0.1 (search tool)")
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let url = format!(
        "https://html.duckduckgo.com/html/?q={}",
        urlencoding::encode(query)
    );

    let response = client.get(&url).send().await?;

    if !response.status().is_success() {
        bail!("DuckDuckGo returned HTTP {}", response.status());
    }

    if response
        .content_length()
        .is_some_and(|length| length > DUCKDUCKGO_BODY_MAX_BYTES as u64)
    {
        bail!("DuckDuckGo response exceeds the {DUCKDUCKGO_BODY_MAX_BYTES}-byte limit");
    }
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > DUCKDUCKGO_BODY_MAX_BYTES {
            bail!("DuckDuckGo response exceeds the {DUCKDUCKGO_BODY_MAX_BYTES}-byte limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    let html = String::from_utf8(bytes).context("DuckDuckGo response is not valid UTF-8")?;
    parse_duckduckgo_html(&html, limit)
}

fn parse_duckduckgo_html(html: &str, limit: usize) -> Result<Vec<DuckResult>> {
    let mut results = Vec::new();
    let mut pos = 0;
    while results.len() < limit {
        let result_start = match html[pos..].find(r#"class="result__body""#) {
            Some(idx) => pos + idx,
            None => break,
        };

        let link_start = match html[result_start..].find(r#"class="result__a""#) {
            Some(idx) => result_start + idx,
            None => {
                pos = result_start + 20;
                continue;
            }
        };

        let href_start = match html[link_start..].find("href=\"") {
            Some(idx) => link_start + idx + 6,
            None => {
                pos = result_start + 20;
                continue;
            }
        };
        let href_end = match html[href_start..].find('"') {
            Some(idx) => href_start + idx,
            None => {
                pos = result_start + 20;
                continue;
            }
        };
        let raw_url = &html[href_start..href_end];

        let url = if raw_url.starts_with("//") {
            format!("https:{raw_url}")
        } else {
            raw_url.to_string()
        };

        let title_close = match html[link_start..].find("</a>") {
            Some(idx) => link_start + idx,
            None => {
                pos = result_start + 20;
                continue;
            }
        };
        let title_text_start = match html[link_start..title_close].find('>') {
            Some(idx) => link_start + idx + 1,
            None => {
                pos = result_start + 20;
                continue;
            }
        };
        let title = strip_html_tags(&html[title_text_start..title_close])
            .trim()
            .to_string();

        if title.is_empty() {
            pos = result_start + 20;
            continue;
        }

        let snippet = match html[result_start..].find(r#"class="result__snippet""#) {
            Some(idx) => {
                let snippet_tag_start = result_start + idx;
                let tag_close = match html[snippet_tag_start..].find('>') {
                    Some(idx) => snippet_tag_start + idx + 1,
                    None => snippet_tag_start,
                };
                let snippet_end = match html[tag_close..].find("</") {
                    Some(idx) => tag_close + idx,
                    // No closing tag: take a bounded window, clamped to the end
                    // of the document so the slice can never run past it.
                    None => (tag_close + 100).min(html.len()),
                };
                // `.get` yields None for an out-of-range or non-char-boundary
                // span instead of panicking — snippets are best-effort.
                let raw_snippet = html
                    .get(tag_close..snippet_end)
                    .map(|s| strip_html_tags(s).trim().to_string())
                    .unwrap_or_default();
                truncate(&raw_snippet, MAX_SNIPPET_CHARS)
            }
            None => String::new(),
        };

        results.push(DuckResult {
            title,
            url,
            snippet,
        });
        pos = result_start + 100;
    }

    if results.is_empty() {
        bail!("no results parsed from DuckDuckGo HTML");
    }

    Ok(results)
}

fn strip_html_tags(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut in_tag = false;

    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => output.push(ch),
            _ => {}
        }
    }

    output = output.replace("&amp;", "&");
    output = output.replace("&lt;", "<");
    output = output.replace("&gt;", ">");
    output = output.replace("&quot;", "\"");
    output = output.replace("&#x27;", "'");
    output = output.replace("&nbsp;", " ");

    output
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value.to_string()
    } else {
        let truncated: String = value.chars().take(max_chars).collect();
        format!("{truncated}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_search_distinguishes_provider_failure_from_no_matches() {
        let failed = single_output("reference", Err(anyhow::anyhow!("HTTP 503")), "fixture", None).unwrap_err();
        assert!(format!("{failed:#}").contains("HTTP 503"));
        let empty = single_output("reference", Ok(vec![]), "fixture", None).unwrap();
        assert!(empty.summary.contains("No results found"));
        let hits = single_output("reference", Ok(vec![super::super::web::SearchHit {
            title: "Actual source".into(), url: "https://example.org/photo".into(), snippet: "Description".into(),
        }]), "fixture", None).unwrap();
        assert!(hits.content.contains("https://example.org/photo"));
        assert!(hits.summary.contains("Found 1 result"));
    }

    #[test]
    fn all_failed_search_lanes_are_an_error_not_empty_success() {
        let error = batch_output(
            &["whole view".into(), "detail".into()],
            vec![
                Err(anyhow::anyhow!("no results parsed from DuckDuckGo HTML")),
                Err(anyhow::anyhow!("HTTP 503")),
            ],
            "DuckDuckGo",
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("all 2 search lanes failed"));
        assert!(error.contains("whole view"));
        assert!(error.contains("HTTP 503"));
    }

    #[test]
    fn partial_search_failure_preserves_and_deduplicates_valid_hits() {
        let hit = super::super::web::SearchHit {
            title: "Source".into(),
            url: "https://example.org/reference".into(),
            snippet: "Photo".into(),
        };
        let result = batch_output(
            &["a".into(), "b".into(), "c".into()],
            vec![
                Err(anyhow::anyhow!("HTTP 503")),
                Ok(vec![hit.clone()]),
                Ok(vec![hit]),
            ],
            "Test",
            None,
        )
        .unwrap();
        assert!(result.summary.contains("2 completed with 1 unique result"));
        assert!(result.content.contains("HTTP 503"));
        assert_eq!(
            result
                .content
                .matches("https://example.org/reference")
                .count(),
            1
        );
    }

    #[test]
    fn successful_empty_search_is_distinct_from_provider_failure() {
        let result = batch_output(
            &["empty".into(), "failed".into()],
            vec![Ok(vec![]), Err(anyhow::anyhow!("HTTP 503"))],
            "Test",
            None,
        )
        .unwrap();
        assert!(result.summary.contains("1 completed with 0 unique result"));
    }

    #[test]
    fn strips_html_tags() {
        assert_eq!(strip_html_tags("<b>Hello</b> World"), "Hello World");
    }

    #[test]
    fn parses_duckduckgo_results() {
        let html = r#"<html>
        <div class="result__body">
          <a class="result__a" href="https://example.com/page">Example Title</a>
          <a class="result__snippet">This is the snippet text for the result.</a>
        </div>
        </html>"#;

        let results = parse_duckduckgo_html(html, 5).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Example Title");
    }

    #[test]
    fn empty_query_rejected() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let web = WebRuntime::default_fallback();
        let result = rt.block_on(execute(
            WebSearchInput {
                query: "   ".to_string(),
                queries: Vec::new(),
                limit: Some(3),
                recency_days: None,
            },
            web,
        ));
        assert!(result.is_err());
    }

    #[test]
    fn oversized_query_is_rejected_before_network_io() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(execute(
            WebSearchInput {
                query: "x".repeat(MAX_QUERY_CHARS + 1),
                queries: Vec::new(),
                limit: Some(3),
                recency_days: None,
            },
            WebRuntime::default_fallback(),
        ));
        assert!(result.is_err());
    }

    #[test]
    fn omitted_limit_uses_configured_default_and_explicit_limit_wins() {
        assert_eq!(resolved_limit(None, 12), 12);
        assert_eq!(resolved_limit(Some(3), 12), 3);
        assert_eq!(resolved_limit(None, 99), MAX_RESULTS);
        assert_eq!(resolved_limit(Some(0), 12), 1);
    }

    #[test]
    fn batch_queries_are_deduplicated_and_bounded_before_network_io() {
        assert_eq!(
            normalized_queries(
                String::new(),
                vec![
                    "OpenAI news".into(),
                    " openai news ".into(),
                    "Anthropic".into()
                ]
            )
            .unwrap(),
            vec!["OpenAI news", "Anthropic"]
        );
        assert!(normalized_queries(
            String::new(),
            (0..=MAX_BATCH_QUERIES)
                .map(|index| format!("q{index}"))
                .collect()
        )
        .is_err());
        assert!(normalized_queries("one".into(), vec!["two".into()]).is_err());
        assert_eq!(normalized_recency(Some(3)).unwrap(), Some(3));
        assert!(normalized_recency(Some(0)).is_err());
        assert!(normalized_recency(Some(366)).is_err());
    }
}
