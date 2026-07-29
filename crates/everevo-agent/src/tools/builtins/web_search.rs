//! WebSearch built-in tool — searches the web and returns result blocks.
//!
//! Claude Code equivalent: `WebSearch` tool. Searches the web via DuckDuckGo
//! HTML (no API key required) and returns title + URL + snippet blocks.
//! Distinction: `web_search` asks the web, `web_fetch` reads a specific URL.
//!
//! Future: swap the backend to Brave / SerpAPI / Bing via config.

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
/// Uses DuckDuckGo HTML (no API key) — reliable, rate-limited by DDG's web frontend.
pub struct WebSearchTool;

impl WebSearchTool {
    /// Parse DuckDuckGo HTML results page into (title, url, snippet) tuples.
    fn parse_results(html: &str, limit: usize) -> Vec<(String, String, String)> {
        let mut results = Vec::new();

        // DDG HTML result blocks have this shape:
        // <a rel="nofollow" class="result-link" href="URL">Title</a>
        // <span class="result-snippet">Snippet...</span>
        //
        // Simpler approach: scan for <a> tags with result URLs, then look for
        // nearby snippet text. We use a lightweight state-machine parse.

        let mut i = 0;
        let chars: Vec<char> = html.chars().collect();
        let n = chars.len();

        // Pattern 1: <a class="result-link" ... href="URL">Title</a>
        // Pattern 2: <span class="result-snippet">Snippet</span>
        // Pattern 3: <a class="result-snippet-link" href="URL">Title</a>
        // We do a simpler approach: extract all links that look like external
        // results, then grab surrounding text as snippet.

        while i < n && results.len() < limit {
            // Find next link — look for href=" in a result-like context
            if let Some(link_end) = find_result_link(&chars, i) {
                let (url, title, next_pos) = link_end;
                i = next_pos;

                // Skip ddg internal links and ads
                if url.is_empty()
                    || url.starts_with("//duckduckgo.com")
                    || url.contains("duckduckgo.com/y.js")
                    || url.contains("duckduckgo.com/l/?")
                {
                    continue;
                }

                // Look for snippet text after the link
                let snippet = extract_snippet(&chars, i);

                // Dedup by URL
                if !results.iter().any(|(_, u, _)| u == &url) {
                    results.push((title, url, snippet));
                }
            } else {
                break;
            }
        }

        results
    }
}

/// Find the next result-like link in HTML, returning (url, title, next_pos).
/// Looks for patterns like:
///   <a ... href="http(s)://..." ... >Title</a>
///   <a ... href="//example.com" ... >Title</a>
fn find_result_link(chars: &[char], start: usize) -> Option<(String, String, usize)> {
    let n = chars.len();
    let mut pos = start;

    while pos < n {
        // Find next '<a'
        let tag_start = substr_pos(chars, pos, "<a ")? + pos;
        let tag_body_end = substr_pos(chars, tag_start, ">")? + tag_start + 1;

        // Extract href from within the <a ... > tag
        let tag_body: String = chars[tag_start + 3..tag_body_end].iter().collect();
        let href = extract_href(&tag_body);

        // Skip if no valid href
        let href = match href {
            Some(h) if h.starts_with("http://") || h.starts_with("https://") => h,
            _ => {
                pos = tag_body_end;
                continue;
            }
        };

        // Find closing </a>
        let close_tag = substr_pos(chars, tag_body_end, "</a>")? + tag_body_end;
        let title: String = chars[tag_body_end..close_tag].iter().collect();
        let title = strip_html_tags(&title).trim().to_string();

        // Skip empty titles or javascript: links
        if title.is_empty() || href.starts_with("javascript:") {
            pos = close_tag + 4;
            continue;
        }

        return Some((href, title, close_tag + 4));
    }

    None
}

/// Extract snippet text following a result link.
fn extract_snippet(chars: &[char], pos: usize) -> String {
    let n = chars.len();
    // Look for text within next ~500 chars before another link
    let end = (pos + 800).min(n);

    // Try to find <span class="result-snippet"> or <div class="result__snippet">
    let snippet_tag = ["result-snippet", "result__snippet", "snippet"];
    for tag in &snippet_tag {
        if let Some(start) = substr_pos(chars, pos, tag) {
            let real_start = start + pos + tag.len();
            // find > after class
            if let Some(gt) = substr_pos(chars, real_start, ">") {
                let content_start = real_start + gt + 1;
                // find closing </span> or </div>
                for end_tag in &["</span>", "</div>"] {
                    if let Some(et) = substr_pos(chars, content_start, end_tag) {
                        let end_pos = content_start + et;
                        let text: String = chars[content_start..end_pos].iter().collect();
                        let clean = strip_html_tags(&text).trim().to_string();
                        if clean.len() > 10 {
                            return truncate_at(&clean, 200);
                        }
                    }
                }
            }
        }
    }

    // Fallback: grab plain text near the result
    let mut text = String::new();
    let mut in_tag = false;
    let mut collected = 0usize;

    for &ch in chars[pos..end].iter() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                if collected > 10 {
                    text.push(' ');
                }
            }
            _ if !in_tag && !matches!(ch, '\n' | '\r' | '\t') => {
                text.push(ch);
                collected += 1;
            }
            _ => {}
        }
        if collected > 200 {
            break;
        }
    }

    let clean: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    truncate_at(&clean, 200)
}

fn extract_href(tag_body: &str) -> Option<String> {
    let href_pos = tag_body.find("href=")?;
    let after = &tag_body[href_pos + 5..];
    let quote = after.chars().next()?;
    let inner = &after[1..];
    let end = inner.find(quote)?;
    let url = &inner[..end];

    // Decode minimal HTML entities
    let url = url.replace("&amp;", "&");

    Some(url.to_string())
}

fn substr_pos(haystack: &[char], start: usize, needle: &str) -> Option<usize> {
    let needle_chars: Vec<char> = needle.chars().collect();
    if needle_chars.is_empty() || start >= haystack.len() {
        return None;
    }
    haystack[start..]
        .windows(needle_chars.len())
        .position(|window| window == needle_chars.as_slice())
}

fn strip_html_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_at(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

/// Minimal percent-encoding for search query strings.
fn encode_query(s: &str) -> String {
    s.chars().map(|c| match c {
        ' ' => "+".to_string(),
        '&' => "%26".to_string(),
        '#' => "%23".to_string(),
        '+' => "%2B".to_string(),
        '=' => "%3D".to_string(),
        c if c.is_ascii_alphanumeric()
            || matches!(c, '-' | '_' | '.' | '~' | '*' | '"' | '\'' | '(' | ')') =>
        {
            c.to_string()
        }
        c => format!("%{:02X}", c as u8),
    }).collect()
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
            return Ok(ToolOutput { content: "cancelled".into(), is_error: true });
        }

        let query = params["query"]
            .as_str()
            .ok_or_else(|| EverEvoError::InvalidInput("query is required".into()))?;

        if query.trim().is_empty() {
            return Ok(ToolOutput { content: "query is required".into(), is_error: true });
        }

        let limit = params["limit"]
            .as_u64()
            .unwrap_or(8)
            .min(MAX_RESULTS as u64) as usize;

        let allowed_domains: Vec<String> = params["allowed_domains"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        let blocked_domains: Vec<String> = params["blocked_domains"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        // Build DDG HTML search URL
        let search_url = format!(
            "https://html.duckduckgo.com/html/?q={}",
            encode_query(query)
        );

        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()
            .map_err(|e| EverEvoError::LlmProvider(e.to_string()))?;

        let resp = client
            .get(&search_url)
            .send()
            .await
            .map_err(|e| EverEvoError::LlmProvider(format!("Search request failed: {e}")))?;

        let body = resp
            .text()
            .await
            .map_err(|e| EverEvoError::LlmProvider(format!("Read search response: {e}")))?;

        // Parse results
        let results = Self::parse_results(&body, limit);

        // Apply domain filters
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
                    "No results found for '{query}'. Try different keywords or check allowed_domains/blocked_domains filters."
                ),
                is_error: false,
            });
        }

        let lines: Vec<String> = filtered
            .iter()
            .enumerate()
            .map(|(i, (title, url, snippet))| {
                format!(
                    "{}. **{}**\n   {}\n   {}",
                    i + 1,
                    title,
                    url,
                    snippet
                )
            })
            .collect();

        let header = format!("Web search results for '{}':\n\n", query);
        Ok(ToolOutput {
            content: header + &lines.join("\n\n"),
            is_error: false,
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
        // Simulated DDG HTML result snippet
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
            assert!(title.contains("Rust"), "title should mention Rust, got: {title}");
            assert!(url.contains("rust-lang.org") || url.contains("wikipedia.org"),
                "url should be external, got: {url}");
        }
    }

    #[test]
    fn test_parse_skips_ddg_internal() {
        let html = r#"
        <a class="result-link" href="https://duckduckgo.com/y.js?q=test">Internal</a>
        <a class="result-link" href="https://example.com/page">Example</a>
        "#;
        let results = WebSearchTool::parse_results(html, 5);
        // The first result should be example.com, not duckduckgo internal
        if !results.is_empty() {
            assert!(!results[0].1.contains("duckduckgo.com"),
                "should skip ddg internal links, got: {}", results[0].1);
        }
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
        assert_eq!(truncate_at("hello world this is a test", 12), "hello world …");
    }

    #[test]
    fn test_extract_href() {
        assert_eq!(
            extract_href(r#"href="https://example.com" class="link""#),
            Some("https://example.com".into())
        );
        assert_eq!(extract_href("no href here"), None);
    }
}
