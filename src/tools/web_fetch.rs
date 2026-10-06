//! web_fetch tool - fetch URL content and extract readable text

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::web::WebRuntime;
use super::ToolOutput;

const MAX_FETCH_CHARS: usize = 20_000;
const DEFAULT_MAX_CHARS: usize = 8_000;
const MAX_FETCH_URL_CHARS: usize = 8_192;
const MAX_FETCH_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_FETCH_ERROR_BODY_BYTES: usize = 64 * 1024;
const MAX_FETCH_ERROR_PREVIEW_CHARS: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebFetchInput {
    pub url: String,
    #[serde(default = "default_max_chars")]
    pub max_chars: usize,
}

fn default_max_chars() -> usize {
    DEFAULT_MAX_CHARS
}

pub async fn execute(input: WebFetchInput, web: WebRuntime) -> Result<ToolOutput> {
    let url = input.url.trim();
    if url.is_empty() {
        bail!("web_fetch url cannot be empty");
    }
    if url.chars().take(MAX_FETCH_URL_CHARS + 1).count() > MAX_FETCH_URL_CHARS {
        bail!("web_fetch url exceeds the {MAX_FETCH_URL_CHARS}-character limit");
    }

    let parsed_url = url::Url::parse(url).context("web_fetch url is invalid")?;
    if !matches!(parsed_url.scheme(), "http" | "https") {
        bail!("web_fetch url must start with http:// or https://");
    }
    if parsed_url.host().is_none() {
        bail!("web_fetch url must include a host");
    }
    if !parsed_url.username().is_empty() || parsed_url.password().is_some() {
        bail!("web_fetch url must not contain user credentials");
    }

    let max_chars = input.max_chars.min(MAX_FETCH_CHARS);

    if web.scrape.is_some() {
        match super::web::scrape(&web, url, max_chars).await {
            Ok(page) => {
                let provider = web
                    .scrape
                    .as_ref()
                    .map(|s| format!("{:?}", s.provider))
                    .unwrap_or_default();
                return Ok(ToolOutput {
                    summary: format!(
                        "Fetched {} via scrape provider {} ({} chars).",
                        page.url,
                        provider,
                        page.content.chars().count()
                    ),
                    content: format!(
                        "URL: {}\nTitle: {}\nProvider: {}\n\n{}",
                        page.url, page.title, provider, page.content
                    ),
                });
            }
            Err(error) => {
                // Expected for URLs the provider can't extract (404 pages, JS
                // walls) — the HTTP fallback below handles it; only a failure
                // of BOTH paths is worth surfacing, and that one bails loudly.
                tracing::debug!("scrape provider failed, falling back to HTTP: {error}");
            }
        }
    }

    let client = reqwest::Client::builder()
        .user_agent("PhoenixAgent/0.1 (fetch tool; research)")
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("failed to build HTTP client")?;

    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("failed to fetch {url}"))?;

    let status = response.status();

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let content_type_lower = content_type.to_ascii_lowercase();
    let media_type = content_type_lower.split(';').next().unwrap_or("").trim();
    let is_json = media_type == "application/json" || media_type.ends_with("+json");
    let is_xml = media_type == "application/xml" || media_type.ends_with("+xml");

    if !status.is_success() {
        let detail = match crate::providers::read_response_text(
            response,
            MAX_FETCH_ERROR_BODY_BYTES,
            "web_fetch error response",
        )
        .await
        {
            Ok(body) => fetch_error_preview(&body),
            Err(error) => format!("response body unavailable: {error}"),
        };
        bail!("fetch returned HTTP {status} for {url}: {detail}");
    }

    // Only process text content.
    if !media_type.is_empty() && !media_type.starts_with("text/") && !is_json && !is_xml {
        bail!(
            "web_fetch cannot process content type: {content_type}. Use it for HTML, XML, JSON, or text pages."
        );
    }

    let body =
        crate::providers::read_response_text(response, MAX_FETCH_BODY_BYTES, "web_fetch response")
            .await
            .context("failed to read response body")?;

    let extracted = if media_type == "text/html" || media_type.is_empty() {
        extract_readable_text(&body, max_chars)
    } else if is_json {
        serde_json::from_str::<serde_json::Value>(&body)
            .context("web_fetch response declared JSON but was invalid")?;
        truncate(&body, max_chars)
    } else {
        // Plain text and XML (including sitemap and Atom/RSS +xml types).
        truncate(&body, max_chars)
    };

    if extracted.trim().is_empty() {
        bail!("no readable text extracted from {url}");
    }

    Ok(ToolOutput {
        summary: format!(
            "Fetched {} ({}) {} chars extracted.",
            url,
            content_type,
            extracted.chars().count()
        ),
        content: format!("URL: {url}\nContent-Type: {content_type}\n\n{extracted}"),
    })
}

fn fetch_error_preview(body: &str) -> String {
    let preview: String = body
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .take(MAX_FETCH_ERROR_PREVIEW_CHARS)
        .collect();
    if body.chars().count() > MAX_FETCH_ERROR_PREVIEW_CHARS {
        format!("{preview}...[truncated]")
    } else {
        preview
    }
}

/// ASCII case-insensitive `starts_with`. HTML tag names are ASCII, so this
/// avoids allocating a fully-lowercased copy of the page — whose byte length can
/// differ from the original (e.g. `İ`→`i̇`), which silently desyncs byte offsets
/// and panics on a non-char-boundary slice.
fn starts_with_ci(haystack: &str, prefix_lower_ascii: &str) -> bool {
    let hb = haystack.as_bytes();
    let pb = prefix_lower_ascii.as_bytes();
    hb.len() >= pb.len()
        && hb[..pb.len()]
            .iter()
            .zip(pb)
            .all(|(h, p)| h.to_ascii_lowercase() == *p)
}

/// ASCII case-insensitive substring search returning the byte index into
/// `haystack`. The needle is lowercase ASCII, so matches only land on
/// ASCII-aligned runs — the returned index is always a valid char boundary.
fn find_ci(haystack: &str, needle_lower_ascii: &str) -> Option<usize> {
    let hb = haystack.as_bytes();
    let nb = needle_lower_ascii.as_bytes();
    if nb.is_empty() || hb.len() < nb.len() {
        return None;
    }
    (0..=hb.len() - nb.len()).find(|&i| {
        hb[i..i + nb.len()]
            .iter()
            .zip(nb)
            .all(|(h, n)| h.to_ascii_lowercase() == *n)
    })
}

fn extract_readable_text(html: &str, max_chars: usize) -> String {
    // Remove script and style blocks. All tag detection is done in-place on the
    // original bytes (ASCII case-insensitive) — never against a separately
    // lowercased copy, whose length can differ and corrupt byte offsets.
    let mut cleaned = String::with_capacity(html.len());
    let mut pos = 0;

    while pos < html.len() {
        // Find next tag
        let tag_start = match html[pos..].find('<') {
            Some(idx) => pos + idx,
            None => {
                cleaned.push_str(&html[pos..]);
                break;
            }
        };

        // Add text before this tag
        cleaned.push_str(&html[pos..tag_start]);

        let remaining = &html[tag_start..];

        // Skip script blocks entirely
        if starts_with_ci(remaining, "<script") {
            if let Some(end) = find_ci(remaining, "</script>") {
                pos = tag_start + end + "</script>".len();
                cleaned.push(' ');
                continue;
            }
        }

        // Skip style blocks entirely
        if starts_with_ci(remaining, "<style") {
            if let Some(end) = find_ci(remaining, "</style>") {
                pos = tag_start + end + "</style>".len();
                cleaned.push(' ');
                continue;
            }
        }

        // Find end of this tag
        let tag_end = match html[tag_start..].find('>') {
            Some(idx) => tag_start + idx + 1,
            None => {
                cleaned.push_str(&html[tag_start..]);
                break;
            }
        };

        // Handle common block elements - add newlines
        let tag = &html[tag_start..tag_end];
        if starts_with_ci(tag, "</p")
            || starts_with_ci(tag, "</div")
            || starts_with_ci(tag, "</h1")
            || starts_with_ci(tag, "</h2")
            || starts_with_ci(tag, "</h3")
            || starts_with_ci(tag, "</h4")
            || starts_with_ci(tag, "<br")
            || starts_with_ci(tag, "</li")
            || starts_with_ci(tag, "</tr")
        {
            cleaned.push('\n');
        }
        if starts_with_ci(tag, "<p") || starts_with_ci(tag, "<div") {
            if !cleaned.ends_with('\n') && !cleaned.is_empty() {
                cleaned.push('\n');
            }
        }
        // Add space for inline tags
        if starts_with_ci(tag, "</a")
            || starts_with_ci(tag, "</span")
            || starts_with_ci(tag, "</strong")
            || starts_with_ci(tag, "</em")
            || starts_with_ci(tag, "<img")
        {
            cleaned.push(' ');
        }

        pos = tag_end;
    }

    // Decode HTML entities
    let decoded = decode_html_entities(&cleaned);

    // Collapse whitespace
    let collapsed = decoded
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    truncate(&collapsed, max_chars)
}

fn decode_html_entities(input: &str) -> String {
    input
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&lsquo;", "'")
        .replace("&rsquo;", "'")
        .replace("&ldquo;", "\"")
        .replace("&rdquo;", "\"")
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
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn serve_once(content_type: &str, body: &str) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let content_type = content_type.to_string();
        let body = body.to_string();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        (format!("http://{address}/test"), server)
    }

    #[test]
    fn extracts_readable_text_from_html() {
        let html = r#"<html><head><script>console.log('x');</script><style>body{}</style></head>
        <body><h1>Title</h1><p>First paragraph with <strong>bold</strong> text.</p>
        <p>Second paragraph.</p></body></html>"#;

        let text = extract_readable_text(html, 2000);
        assert!(text.contains("Title"));
        assert!(text.contains("First paragraph with bold text"));
        assert!(text.contains("Second paragraph"));
        assert!(!text.contains("console.log"));
        assert!(!text.contains("<script>"));
    }

    #[test]
    fn non_ascii_before_tags_does_not_panic_or_corrupt() {
        // `İ` (U+0130) lowercases to 2 chars / 3 bytes, so the old parallel
        // `html.to_lowercase()` desynced byte offsets and panicked on a
        // non-char-boundary slice. The in-place ASCII matcher must be immune.
        let html = "<p>İstanbul İ İ</p><script>var x = \"İ çödé\";</script><p>after</p>";
        let text = extract_readable_text(html, 2000);
        assert!(text.contains("İstanbul"), "{text}");
        assert!(text.contains("after"), "{text}");
        assert!(!text.contains("var x"), "script not stripped: {text}");
    }

    #[test]
    fn decodes_html_entities() {
        assert_eq!(
            decode_html_entities("Hello &amp; welcome &mdash; test"),
            "Hello & welcome — test"
        );
    }

    #[test]
    fn fetches_application_xml_sitemaps() {
        let sitemap = r#"<?xml version="1.0"?><urlset><url><loc>https://example.test/docs</loc></url></urlset>"#;
        let (url, server) = serve_once("application/xml; charset=utf-8", sitemap);
        let result = tokio::runtime::Runtime::new().unwrap().block_on(execute(
            WebFetchInput {
                url,
                max_chars: 2_000,
            },
            WebRuntime::default_fallback(),
        ));
        server.join().unwrap();
        let output = result.unwrap();
        assert!(output.content.contains("https://example.test/docs"));
        assert!(output.content.contains("application/xml"));
    }

    #[test]
    fn fetches_vendor_xml_feeds() {
        let feed = r#"<feed><title>Phoenix updates</title></feed>"#;
        let (url, server) = serve_once("application/atom+xml", feed);
        let result = tokio::runtime::Runtime::new().unwrap().block_on(execute(
            WebFetchInput {
                url,
                max_chars: 2_000,
            },
            WebRuntime::default_fallback(),
        ));
        server.join().unwrap();
        assert!(result.unwrap().content.contains("Phoenix updates"));
    }

    #[test]
    fn rejects_non_http_urls() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let web = WebRuntime::default_fallback();
        let result = rt.block_on(execute(
            WebFetchInput {
                url: "file:///etc/passwd".to_string(),
                max_chars: 1000,
            },
            web,
        ));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("http://"));
    }

    #[test]
    fn rejects_empty_url() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let web = WebRuntime::default_fallback();
        let result = rt.block_on(execute(
            WebFetchInput {
                url: "   ".to_string(),
                max_chars: 1000,
            },
            web,
        ));
        assert!(result.is_err());
    }

    #[test]
    fn rejects_url_credentials_before_network_io() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(execute(
            WebFetchInput {
                url: "https://user:pass@example.test/".to_string(),
                max_chars: 1000,
            },
            WebRuntime::default_fallback(),
        ));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("credentials"));
    }

    #[test]
    fn rejects_oversized_url_before_network_io() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let web = WebRuntime::default_fallback();
        let result = rt.block_on(execute(
            WebFetchInput {
                url: format!("https://example.test/{}", "x".repeat(MAX_FETCH_URL_CHARS)),
                max_chars: 1000,
            },
            web,
        ));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("character limit"));
    }

    #[test]
    fn error_preview_is_bounded_and_sanitized() {
        let body = format!("nope\0{}", "x".repeat(MAX_FETCH_ERROR_PREVIEW_CHARS + 50));
        let preview = fetch_error_preview(&body);
        assert!(!preview.contains('\0'));
        assert!(preview.ends_with("...[truncated]"));
        assert!(preview.chars().count() <= MAX_FETCH_ERROR_PREVIEW_CHARS + 14);
    }
}
