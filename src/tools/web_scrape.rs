//! web_scrape — extract readable page content via configured scrape provider.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::web::WebRuntime;
use super::ToolOutput;

const MAX_CHARS: usize = 20_000;
const DEFAULT_MAX_CHARS: usize = 8_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebScrapeInput {
    pub url: String,
    #[serde(default = "default_max_chars")]
    pub max_chars: usize,
}

fn default_max_chars() -> usize {
    DEFAULT_MAX_CHARS
}

pub async fn execute(input: WebScrapeInput, web: WebRuntime) -> Result<ToolOutput> {
    let url = input.url.trim();
    if url.is_empty() {
        bail!("web_scrape url cannot be empty");
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        bail!("web_scrape url must start with http:// or https://");
    }

    let max_chars = input.max_chars.min(MAX_CHARS);
    let page = match super::web::scrape(&web, url, max_chars).await {
        Ok(page) => page,
        Err(provider_error) => {
            // A scrape provider returning no result, hitting quota, or
            // rejecting a site is a routing event, not the end of a public
            // HTML read. Retry the same URL once through Phoenix's bounded
            // plain-HTTP extractor, without re-entering the failed scrape
            // provider. This is the first-party fallback web_fetch already
            // uses and prevents agents from inventing curl/browser detours.
            let mut http_only = web.clone();
            http_only.scrape = None;
            let fetched = super::web_fetch::execute(
                super::web_fetch::WebFetchInput {
                    url: url.to_string(),
                    max_chars,
                },
                http_only,
            )
            .await
            .with_context(|| {
                format!(
                    "scrape provider failed ({provider_error:#}) and the HTTP fallback also failed"
                )
            })?;
            return Ok(ToolOutput {
                summary: format!("Scrape provider failed; {}", fetched.summary),
                content: format!(
                    "Provider scrape unavailable; Phoenix used its bounded HTTP fallback.\n\n{}",
                    fetched.content
                ),
            });
        }
    };
    let provider = web
        .scrape
        .as_ref()
        .map(|s| format!("{:?}", s.provider))
        .unwrap_or_else(|| "unknown".to_string());

    Ok(ToolOutput {
        summary: format!(
            "Scraped {} via {} ({} chars).",
            page.url,
            provider,
            page.content.chars().count()
        ),
        content: format!(
            "URL: {}\nTitle: {}\nProvider: {}\n\n{}",
            page.url, page.title, provider, page.content
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ScrapeProvider, SearchProvider};
    use crate::tools::web::{ScrapeRuntime, SearchRuntime};

    #[tokio::test]
    async fn provider_empty_or_down_falls_back_to_bounded_http() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            let body = "<html><head><title>Fallback page</title></head><body><main>Useful fallback text</main></body></html>";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        let runtime = WebRuntime {
            search: SearchRuntime {
                provider: SearchProvider::DuckDuckGo,
                api_key: None,
                base_url: None,
                default_engine: "google".into(),
                default_limit: 5,
                fallback_keys: vec![],
            },
            crawl: None,
            scrape: Some(ScrapeRuntime {
                provider: ScrapeProvider::Tavily,
                api_key: "fixture".into(),
                base_url: "http://127.0.0.1:9".into(),
                fallback_keys: vec![],
            }),
        };
        let output = execute(
            WebScrapeInput {
                url: format!("http://{address}/page"),
                max_chars: 4_000,
            },
            runtime,
        )
        .await
        .expect("HTTP fallback should preserve the read");
        assert!(
            output.summary.contains("Scrape provider failed"),
            "{}",
            output.summary
        );
        assert!(output.content.contains("bounded HTTP fallback"));
        assert!(output.content.contains("Useful fallback text"));
        server.join().unwrap();
    }
}
