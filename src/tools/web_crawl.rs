//! web_crawl — crawl a site via configured crawl provider (Firecrawl, Tavily).

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use super::web::WebRuntime;
use super::ToolOutput;

const MAX_PAGES: usize = 30;
const MAX_CHARS_PER_PAGE: usize = 6_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebCrawlInput {
    pub url: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default = "default_max_chars")]
    pub max_chars_per_page: usize,
}

fn default_limit() -> usize {
    10
}

fn default_max_chars() -> usize {
    MAX_CHARS_PER_PAGE
}

pub async fn execute(input: WebCrawlInput, web: WebRuntime) -> Result<ToolOutput> {
    let url = input.url.trim();
    if url.is_empty() {
        bail!("web_crawl url cannot be empty");
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        bail!("web_crawl url must start with http:// or https://");
    }

    let limit = input.limit.min(MAX_PAGES).max(1);
    let max_chars = input.max_chars_per_page.min(20_000);

    let pages = super::web::crawl(&web, url, limit, max_chars).await?;
    let provider = web
        .crawl
        .as_ref()
        .map(|c| format!("{:?}", c.provider))
        .unwrap_or_else(|| "unknown".to_string());

    if pages.is_empty() {
        return Ok(ToolOutput {
            summary: format!("Crawl of {url} returned no pages ({provider})."),
            content: format!("No pages returned for seed URL: {url}"),
        });
    }

    let formatted = pages
        .iter()
        .enumerate()
        .map(|(i, page)| {
            format!(
                "## Page {} — {}\nURL: {}\n\n{}\n",
                i + 1,
                if page.title.is_empty() {
                    "(untitled)"
                } else {
                    page.title.as_str()
                },
                page.url,
                page.content
            )
        })
        .collect::<Vec<_>>()
        .join("\n---\n\n");

    Ok(ToolOutput {
        summary: format!("Crawled {} — {} page(s) via {provider}.", url, pages.len()),
        content: formatted,
    })
}
