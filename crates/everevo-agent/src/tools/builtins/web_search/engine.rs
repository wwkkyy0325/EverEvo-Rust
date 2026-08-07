//! Search engine backends — Bing (cn.bing.com) + DuckDuckGo (lite/html).
//!
//! Bing is tried first: directly reachable from mainland China without a proxy,
//! and returns real result URLs rather than DDG's `uddg=` redirect wrapper.
//! DDG `lite`/`html` follow as fallback for when Bing is rate-limited or blocked.

use everevo_core::EverEvoError;

/// Search engines tried in order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SearchEngine {
    BingCn,
    DdgLite,
    DdgHtml,
}

pub(crate) const ENGINES: &[SearchEngine] = &[
    SearchEngine::BingCn,
    SearchEngine::DdgLite,
    SearchEngine::DdgHtml,
];

impl SearchEngine {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::BingCn => "bing-cn",
            Self::DdgLite => "ddg-lite",
            Self::DdgHtml => "ddg-html",
        }
    }

    /// Fetch the results-page HTML for `query`.
    pub(crate) async fn fetch_html(
        &self,
        client: &reqwest::Client,
        query: &str,
    ) -> Result<String, EverEvoError> {
        match self {
            Self::BingCn => {
                let url = format!(
                    "https://cn.bing.com/search?q={}",
                    super::encode_url_query(query)
                );
                let resp = client
                    .get(&url)
                    .send()
                    .await
                    .map_err(|e| EverEvoError::Network(format!("bing request: {e}")))?;
                if !resp.status().is_success() {
                    return Err(EverEvoError::Network(format!(
                        "bing HTTP {}",
                        resp.status()
                    )));
                }
                resp.text()
                    .await
                    .map_err(|e| EverEvoError::Network(format!("bing body: {e}")))
            }
            Self::DdgLite | Self::DdgHtml => {
                let endpoint = match self {
                    Self::DdgLite => "https://lite.duckduckgo.com/lite/",
                    _ => "https://html.duckduckgo.com/html/",
                };
                let resp = client
                    .post(endpoint)
                    .form(&[("q", query), ("kp", "-2"), ("kl", "us-en")])
                    .send()
                    .await
                    .map_err(|e| EverEvoError::Network(format!("ddg request: {e}")))?;
                if !resp.status().is_success() {
                    return Err(EverEvoError::Network(format!("ddg HTTP {}", resp.status())));
                }
                resp.text()
                    .await
                    .map_err(|e| EverEvoError::Network(format!("ddg body: {e}")))
            }
        }
    }
}
