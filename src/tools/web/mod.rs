mod backends;
mod context;
mod types;

pub use context::WebRuntime;
#[cfg(test)]
pub(crate) use context::{ScrapeRuntime, SearchRuntime};
pub use types::{PageContent, SearchHit};

pub async fn search(
    runtime: &WebRuntime,
    query: &str,
    limit: usize,
    recency_days: Option<u32>,
) -> anyhow::Result<Vec<SearchHit>> {
    backends::run_search(&runtime.search, query, limit, recency_days).await
}

pub async fn scrape(
    runtime: &WebRuntime,
    url: &str,
    max_chars: usize,
) -> anyhow::Result<PageContent> {
    let scrape = runtime
        .scrape
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No scrape provider configured. Run `phoenix onboard` and set [profile.scrape], or use web_fetch for plain HTTP."))?;
    backends::run_scrape(scrape, url, max_chars).await
}

pub async fn crawl(
    runtime: &WebRuntime,
    url: &str,
    limit: usize,
    max_chars_per_page: usize,
) -> anyhow::Result<Vec<PageContent>> {
    let crawl = runtime.crawl.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "No crawl provider configured. Run `phoenix onboard` and set [profile.crawl]."
        )
    })?;
    backends::run_crawl(crawl, url, limit, max_chars_per_page).await
}
