#[derive(Debug, Clone)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[derive(Debug, Clone)]
pub struct PageContent {
    pub url: String,
    pub title: String,
    pub content: String,
}
