//! WebSearch built-in tool — searches the web and returns result blocks.
//!
//! Claude Code equivalent: `WebSearch` tool. Multi-engine search with CDP browser
//! bridge fallback. Returns title + URL + snippet blocks.
//! Distinction: `web_search` asks the web, `web_fetch` reads a specific URL.

mod engine;
mod parser;

pub(crate) use engine::ENGINES;
#[cfg(test)]
pub(crate) use engine::SearchEngine;
pub(crate) use parser::*; // all parser functions + encode_url_query (pub)

use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use tokio_util::sync::CancellationToken;

/// Maximum number of results to return.
const MAX_RESULTS: usize = 20;

/// HTTP request timeout for search.
const REQUEST_TIMEOUT_SECS: u64 = 15;

/// Searches the web and returns structured results.
/// Uses multi-engine search (Bing + DuckDuckGo HTML, no API key required).
pub struct WebSearchTool;

impl WebSearchTool {
    /// Parse HTML results page into (title, url, snippet) tuples.
    ///
    /// Tries Bing-specific `b_algo` block parsing first (cleaner, fewer false
    /// positives). Falls back to generic `<a href>` scanning for DDG and other
    /// engines.
    fn parse_results(html: &str, limit: usize) -> Vec<(String, String, String)> {
        if is_challenge_page(html) {
            tracing::debug!("Challenge/anti-bot page detected — skipping");
            return Vec::new();
        }

        let has_bing_blocks = html.contains("b_algo");
        if has_bing_blocks {
            let results = Self::parse_bing_results(html, limit);
            if !results.is_empty() {
                return results;
            }
            return Vec::new();
        }

        Self::parse_generic_results(html, limit)
    }

    /// Parse Bing `b_algo` result blocks.
    fn parse_bing_results(html: &str, limit: usize) -> Vec<(String, String, String)> {
        let mut results = Vec::new();
        let mut pos = 0;

        while pos < html.len() && results.len() < limit {
            let block_start = match html[pos..].find("b_algo") {
                Some(off) => {
                    let slice = &html[..pos + off];
                    match slice.rfind("<li") {
                        Some(li) => li,
                        None => {
                            pos += off + 6;
                            continue;
                        }
                    }
                }
                None => break,
            };

            let block_end = match html[block_start..].find("</li>") {
                Some(off) => block_start + off + 5,
                None => break,
            };
            pos = block_end;

            let block = &html[block_start..block_end];

            let href = match extract_href(block) {
                Some(h) if h.starts_with("http://") || h.starts_with("https://") => h,
                _ => continue,
            };

            if is_internal_link(&href) {
                continue;
            }

            let title = extract_link_text(block);

            if title.is_empty() || title.eq_ignore_ascii_case("here") {
                continue;
            }

            let snippet = extract_bing_snippet(block);

            if !results.iter().any(|(_, u, _)| u == &href) {
                results.push((title, href, snippet));
            }
        }

        results
    }

    /// Generic `<a href>` parser for DDG and other engines.
    fn parse_generic_results(html: &str, limit: usize) -> Vec<(String, String, String)> {
        let mut results = Vec::new();
        let chars: Vec<char> = html.chars().collect();
        let n = chars.len();
        let mut i = 0;

        while i < n && results.len() < limit {
            match find_result_link(&chars, i) {
                Some((href, title, next_pos)) => {
                    i = next_pos;

                    let url = match resolve_real_url(&href) {
                        Some(u) => u,
                        None => continue,
                    };

                    if title.is_empty()
                        || is_internal_link(&url)
                        || title.eq_ignore_ascii_case("here")
                    {
                        continue;
                    }

                    let snippet = extract_snippet(&chars, i);

                    if !results.iter().any(|(_, u, _)| u == &url) {
                        results.push((title, url, snippet));
                    }
                }
                None => break,
            }
        }

        results
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web and return result blocks with titles and URLs. \
         Use this for finding documentation, error solutions, library docs, \
         or any information that requires up-to-date web knowledge. \
         For reading the full content of a specific page, use web_fetch instead. \
         Parameters: query (required — search keywords), \
         limit (optional — max results, default 8, max 20), \
         allowed_domains (optional — comma-separated domains to include), \
         blocked_domains (optional — comma-separated domains to exclude)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query keywords"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results to return (default: 8, max: 20)"
                },
                "allowed_domains": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Only include results from these domains (optional)"
                },
                "blocked_domains": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Exclude results from these domains (optional)"
                }
            },
            "required": ["query"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        cancel: Option<&CancellationToken>,
    ) -> Result<ToolOutput, EverEvoError> {
        if cancel.is_some_and(|c| c.is_cancelled()) {
            return Ok(ToolOutput {
                content: "cancelled".into(),
                is_error: true,
                ..Default::default()
            });
        }

        let query = params["query"]
            .as_str()
            .ok_or_else(|| EverEvoError::InvalidInput("query is required".into()))?;

        if query.trim().is_empty() {
            return Ok(ToolOutput {
                content: "query is required".into(),
                is_error: true,
                ..Default::default()
            });
        }

        let limit = params["limit"]
            .as_u64()
            .unwrap_or(8)
            .min(MAX_RESULTS as u64) as usize;

        let allowed_domains: Vec<String> = params["allowed_domains"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let blocked_domains: Vec<String> = params["blocked_domains"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let client = super::http_util::build_browser_client(
            std::time::Duration::from_secs(10),
            std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS),
        )?;

        let mut results: Vec<(String, String, String)> = Vec::new();
        let mut last_error = String::new();
        let mut tried: Vec<&str> = Vec::new();

        for engine in ENGINES {
            let label = engine.label();
            tried.push(label);
            match engine.fetch_html(&client, query).await {
                Ok(body) => {
                    let parsed = Self::parse_results(&body, limit);
                    tracing::debug!(
                        engine = label,
                        parsed = parsed.len(),
                        "search engine responded"
                    );
                    if !parsed.is_empty() {
                        results = parsed;
                        break;
                    }
                }
                Err(e) => {
                    last_error = format!("{label}: {e}");
                    tracing::debug!(
                        engine = label,
                        error = %e,
                        "search engine failed — trying next"
                    );
                }
            }
        }

        let mut bridge_error: Option<String> = None;

        if results.is_empty() {
            match super::browser_bridge::search_via_browser(query, limit).await {
                Ok(browser_results) if !browser_results.is_empty() => {
                    results = browser_results
                        .into_iter()
                        .map(|r| (r.title, r.url, r.snippet))
                        .collect();
                }
                Ok(_) => {
                    bridge_error = Some(
                        "CDP browser bridge: navigation succeeded but 0 results extracted"
                            .into(),
                    );
                    tracing::debug!(
                        "CDP browser bridge: navigation succeeded but 0 results extracted"
                    );
                }
                Err(ref e) => {
                    bridge_error = Some(format!("CDP browser bridge: {e}"));
                    tracing::warn!(error = %e, "Browser bridge failed");
                }
            }
        }

        if results.is_empty() {
            let tried_str = tried.join(", ");
            let engine = std::env::var("EVEREVO_SEARCH_BROWSER_URL")
                .unwrap_or_else(|_| "https://cn.bing.com/search?q=".to_string());
            let browser_url = format!("{}{}", engine, encode_url_query(query));

            let _ = open::that(&browser_url);

            let bridge_detail = bridge_error
                .as_deref()
                .unwrap_or("CDP browser bridge: not attempted");

            return Ok(ToolOutput {
                content: format!(
                    "Web search failed — all endpoints and browser bridge exhausted.\n\n\
                     Tried: {tried_str}\n\
                     {bridge_detail}\n\
                     Manual fallback: opened {browser_url}\n\
                     HTTP last error: {last_error}\n\n\
                     If you see search results in the opened browser tab, \
                     copy the relevant links and use `web_fetch` to read them."
                ),
                is_error: true,
                ..Default::default()
            });
        }

        let filtered: Vec<_> = results
            .into_iter()
            .filter(|(_, url, _)| {
                if !allowed_domains.is_empty() {
                    allowed_domains.iter().any(|d| url.contains(d))
                } else {
                    true
                }
            })
            .filter(|(_, url, _)| {
                if !blocked_domains.is_empty() {
                    !blocked_domains.iter().any(|d| url.contains(d))
                } else {
                    true
                }
            })
            .collect();

        if filtered.is_empty() {
            return Ok(ToolOutput {
                content: format!(
                    "No results found for '{query}'. Try different keywords or check domain filters."
                ),
                is_error: false,
                ..Default::default()
            });
        }

        let lines: Vec<String> = filtered
            .iter()
            .enumerate()
            .map(|(i, (title, url, snippet))| {
                format!("{}. **{}**\n   {}\n   {}", i + 1, title, url, snippet)
            })
            .collect();

        let header = format!("Web search results for '{}':\n\n", query);
        Ok(ToolOutput {
            content: header + &lines.join("\n\n"),
            is_error: false,
            ..Default::default()
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_empty_html() {
        let results = WebSearchTool::parse_results("", 5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_parse_real_ddg_html() {
        let html = r#"
        <html><body>
        <div class="result">
            <a class="result-link" href="https://www.rust-lang.org/">Rust Programming Language</a>
            <span class="result-snippet">A language empowering everyone to build reliable and efficient software.</span>
        </div>
        <div class="result">
            <a class="result-link" href="https://en.wikipedia.org/wiki/Rust_(programming_language)">Rust - Wikipedia</a>
            <span class="result-snippet">Rust is a general-purpose programming language emphasizing performance, type safety, and concurrency.</span>
        </div>
        </body></html>
        "#;

        let results = WebSearchTool::parse_results(html, 5);
        assert!(results.len() >= 1, "should find at least one result");
        if let Some((title, url, _snippet)) = results.first() {
            assert!(
                title.contains("Rust"),
                "title should mention Rust, got: {title}"
            );
            assert!(
                url.contains("rust-lang.org") || url.contains("wikipedia.org"),
                "url should be external, got: {url}"
            );
        }
    }

    #[test]
    fn test_parse_skips_ddg_internal() {
        let html = r#"
        <a class="result-link" href="https://duckduckgo.com/y.js?q=test">Internal</a>
        <a class="result-link" href="https://example.com/page">Example</a>
        "#;
        let results = WebSearchTool::parse_results(html, 5);
        if !results.is_empty() {
            assert!(
                !results[0].1.contains("duckduckgo.com"),
                "should skip ddg internal links, got: {}",
                results[0].1
            );
        }
    }

    #[test]
    fn test_parse_ddg_redirect_unwraps_uddg() {
        let html = r#"<a rel="nofollow" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Frust-lang.org%2F">Rust Programming Language</a>"#;
        let results = WebSearchTool::parse_results(html, 5);
        assert_eq!(results.len(), 1);
        let (title, url, _) = &results[0];
        assert!(title.contains("Rust"), "got: {title}");
        assert_eq!(url, "https://rust-lang.org/");
    }

    #[test]
    fn test_parse_challenge_returns_empty() {
        let html = r#"<html><body>
            <script src="anomaly.js"></script>
            <form id="challenge-form"></form>
            <a href="https://duckduckgo.com/?iai=1">Get the full-JS version here</a>
        </body></html>"#;
        let results = WebSearchTool::parse_results(html, 5);
        assert!(results.is_empty(), "challenge page should yield no results");
    }

    #[test]
    fn test_resolve_real_url_direct() {
        assert_eq!(
            resolve_real_url("https://example.com/page"),
            Some("https://example.com/page".into())
        );
    }

    #[test]
    fn test_resolve_real_url_uddg() {
        let href = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fdoc.rust-lang.org%2F&rut=abc";
        assert_eq!(
            resolve_real_url(href),
            Some("https://doc.rust-lang.org/".into())
        );
    }

    #[test]
    fn test_percent_decode_basic() {
        assert_eq!(percent_decode("https%3A%2F%2Fx.org%2F"), "https://x.org/");
        assert_eq!(percent_decode("a+b"), "a b");
    }

    #[test]
    fn test_name_and_schema() {
        let tool = WebSearchTool;
        assert_eq!(tool.name(), "web_search");
        assert_eq!(tool.risk_level(), RiskLevel::Low);
        let schema = tool.parameters_schema();
        assert_eq!(schema["required"][0], "query");
    }

    #[test]
    fn test_strip_html_tags() {
        assert_eq!(strip_html_tags("<b>hello</b> world"), "hello world");
        assert_eq!(strip_html_tags("plain text"), "plain text");
    }

    #[test]
    fn test_truncate_at() {
        assert_eq!(truncate_at("hello", 10), "hello");
        assert_eq!(
            truncate_at("hello world this is a test", 12),
            "hello world \u{2026}"
        );
    }

    #[test]
    fn test_extract_href() {
        assert_eq!(
            extract_href(r#"href="https://example.com" class="link""#),
            Some("https://example.com".into())
        );
        assert_eq!(extract_href("no href here"), None);
    }

    #[test]
    fn test_encode_url_query_plain() {
        assert_eq!(encode_url_query("hello world"), "hello+world");
    }

    #[test]
    fn test_encode_url_query_special_chars() {
        assert_eq!(encode_url_query("rust & crates"), "rust+%26+crates");
        assert_eq!(encode_url_query("c# vs rust"), "c%23+vs+rust");
    }

    #[test]
    fn test_encode_url_query_cjk() {
        let encoded = encode_url_query("你好");
        assert!(
            encoded.starts_with("%"),
            "CJK should be percent-encoded, got {encoded}"
        );
    }

    #[test]
    fn test_engines_defined() {
        assert!(ENGINES.len() >= 2);
        assert_eq!(ENGINES[0], SearchEngine::BingCn);
    }

    #[test]
    fn test_parse_bing_html() {
        let html = r#"<li class="b_algo">
            <h2><a href="https://www.rust-lang.org/" h="ID=SERP,5000.1">Rust Programming Language</a></h2>
            <p>A language empowering everyone to build reliable and efficient software.</p>
        </li>
        <li class="b_algo">
            <h2><a href="https://doc.rust-lang.org/book/">The Rust Book</a></h2>
            <p>Official guide to Rust.</p>
        </li>"#;
        let results = WebSearchTool::parse_results(html, 5);
        assert!(results.len() >= 2, "got {} results", results.len());
        let urls: Vec<&str> = results.iter().map(|(_, u, _)| u.as_str()).collect();
        assert!(
            urls.contains(&"https://www.rust-lang.org/"),
            "urls: {urls:?}"
        );
        assert!(
            urls.contains(&"https://doc.rust-lang.org/book/"),
            "urls: {urls:?}"
        );
    }

    #[test]
    fn test_parse_filters_bing_internal() {
        let html = r#"<a href="https://www.bing.com/ck/a?...">Ad</a>
        <a href="https://www.bing.com/account/preferences">Settings</a>
        <a href="https://example.com/real">Real Result</a>"#;
        let results = WebSearchTool::parse_results(html, 5);
        assert!(results
            .iter()
            .any(|(_, u, _)| u == "https://example.com/real"));
        assert!(!results.iter().any(|(_, u, _)| u.contains("bing.com/ck/")));
        assert!(!results
            .iter()
            .any(|(_, u, _)| u.contains("bing.com/account")));
    }

    // ── Edge case / crash resistance ─────────────────────────────────

    #[test]
    fn test_extract_href_malformed() {
        assert_eq!(extract_href(""), None);
        assert_eq!(extract_href("no equals sign"), None);
        assert_eq!(extract_href(r#"href=""#,), None); // unclosed quote
        assert_eq!(extract_href(r#"href=""#), None);         // empty value
    }

    #[test]
    fn test_strip_html_tags_nested() {
        assert_eq!(strip_html_tags("<div><span>text</span></div>"), "text");
        assert_eq!(strip_html_tags("<a href='x'>link</a>"), "link");
    }

    #[test]
    fn test_truncate_at_edge_cases() {
        assert_eq!(truncate_at("", 5), "");
        assert_eq!(truncate_at("hi", 5), "hi");
        // Unicode: each char counts as 1
        assert_eq!(truncate_at("你好世界", 2), "你好\u{2026}");
    }

    #[test]
    fn test_encode_url_query_roundtrip() {
        // Alphanumeric should pass through unchanged
        assert_eq!(encode_url_query("hello123"), "hello123");
        // Special chars should be encoded
        let encoded = encode_url_query("test&query=1");
        assert!(!encoded.contains('&'));
        assert!(!encoded.contains('='));
    }

    #[test]
    fn test_is_challenge_page_never_panics() {
        // Very long input, binary-ish content — must not panic
        let big = "x".repeat(10000);
        let _ = is_challenge_page(&big);
        // Empty
        let _ = is_challenge_page("");
        // Garbage
        let _ = is_challenge_page("\x00\x01\x02\x03");
    }

    #[test]
    fn test_substr_pos_edge_cases() {
        let chars: Vec<char> = "hello world".chars().collect();
        assert_eq!(substr_pos(&chars, 0, "hello"), Some(0));
        assert_eq!(substr_pos(&chars, 0, "world"), Some(6));
        assert_eq!(substr_pos(&chars, 0, "xyz"), None);
        assert_eq!(substr_pos(&chars, 100, "hello"), None);
        assert_eq!(substr_pos(&[], 0, "hello"), None);
    }
}
