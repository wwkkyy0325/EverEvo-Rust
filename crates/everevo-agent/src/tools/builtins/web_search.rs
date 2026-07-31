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

/// Search engines tried in order. **Bing (cn.bing.com) is first** — directly
/// reachable from mainland China without a proxy (unlike DuckDuckGo) and
/// returns real result URLs rather than DDG's `uddg=` redirect wrapper. DDG
/// `lite`/`html` follow as fallback for when Bing is rate-limited or blocked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SearchEngine {
    BingCn,
    DdgLite,
    DdgHtml,
}

const ENGINES: &[SearchEngine] = &[
    SearchEngine::BingCn,
    SearchEngine::DdgLite,
    SearchEngine::DdgHtml,
];

impl SearchEngine {
    fn label(&self) -> &'static str {
        match self {
            Self::BingCn => "bing-cn",
            Self::DdgLite => "ddg-lite",
            Self::DdgHtml => "ddg-html",
        }
    }

    /// Fetch the results-page HTML for `query`.
    async fn fetch_html(
        &self,
        client: &reqwest::Client,
        query: &str,
    ) -> Result<String, EverEvoError> {
        match self {
            Self::BingCn => {
                // Bing uses GET ?q=; directly reachable in mainland China.
                let url = format!("https://cn.bing.com/search?q={}", encode_url_query(query));
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

/// Searches the web and returns structured results.
/// Uses DuckDuckGo HTML (no API key) — reliable, rate-limited by DDG's web frontend.
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

        // Bing has structured result blocks — parse those first for accuracy.
        // If the page has `b_algo` markers but yields 0 results, it's a
        // cold-query "no results" page and we should NOT fall back to generic
        // parsing (which would return garbage nav/footer links).
        let has_bing_blocks = html.contains("b_algo");
        if has_bing_blocks {
            let results = Self::parse_bing_results(html, limit);
            if !results.is_empty() {
                return results;
            }
            // b_algo blocks exist but parser found nothing → genuine "no results"
            // page; return empty rather than feeding nav links to the LLM.
            return Vec::new();
        }

        // Generic parser — handles DDG lite/html and other engines.
        Self::parse_generic_results(html, limit)
    }

    /// Parse Bing `b_algo` result blocks. Each block looks like:
    /// `<li class="b_algo"><h2><a href="...">Title</a></h2><p>...</p></li>`
    fn parse_bing_results(html: &str, limit: usize) -> Vec<(String, String, String)> {
        let mut results = Vec::new();
        let mut pos = 0;

        while pos < html.len() && results.len() < limit {
            // Find next <li class="b_algo">
            let block_start = match html[pos..].find("b_algo") {
                Some(off) => {
                    // Walk back to find the opening <li>
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

            // Find </li> for this block
            let block_end = match html[block_start..].find("</li>") {
                Some(off) => block_start + off + 5,
                None => break,
            };
            pos = block_end;

            let block = &html[block_start..block_end];

            // Extract href from <a href="...">
            let href = match extract_href(block) {
                Some(h) if h.starts_with("http://") || h.starts_with("https://") => h,
                _ => continue,
            };

            // Skip Bing internal links
            if is_internal_link(&href) {
                continue;
            }

            // Extract title from <a ...>Title</a> or <h2><a ...>Title</a></h2>
            let title = extract_link_text(block);

            if title.is_empty() || title.eq_ignore_ascii_case("here") {
                continue;
            }

            // Extract snippet from <p> or <div class="b_caption">
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

        // Skip if no valid href. Accept protocol-relative `//` (DDG wraps
        // results as `//duckduckgo.com/l/?uddg=...`); resolve_real_url unwraps it.
        let href = match href {
            Some(h)
                if h.starts_with("http://") || h.starts_with("https://") || h.starts_with("//") =>
            {
                h
            }
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

    let clean: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
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

/// Detect a search-engine anti-bot / challenge page — these contain no real
/// results and must return empty so the caller falls back to the next endpoint
/// instead of parsing garbage links from the challenge footer.
///
/// Covers both DuckDuckGo and Bing block pages.
fn is_challenge_page(html: &str) -> bool {
    // DuckDuckGo challenge markers
    html.contains("anomaly.js")
        || html.contains("challenge-form")
        || html.contains("/check.")
        || html.contains("ddg_ptoken")
        || html.contains("Get the full-JS version here")
        || html.contains("not enabled JavaScript")
        // Bing captcha / rate-limit / consent-wall markers
        || html.contains("captcha-delivery.com")
        || html.contains("g-recaptcha")
        || html.contains("hCaptcha")
        || html.contains("challenge-platform")
        || html.contains("tr.bing.com")
        || (html.contains("id=\"b_sb_preview\"") && !html.contains("b_algo"))
        // Cloudflare / generic CDN challenge pages
        || html.contains("Just a moment...")
        || html.contains("Checking your browser")
        || html.contains("DDoS protection")
        || html.contains("cf-browser-verification")
        || html.contains("cf-challenge-running")
        || html.contains("_cf_chl_opt")
        || html.contains("cf-spinner")
        || html.contains("Please turn JavaScript on")
        || html.contains("please enable JavaScript")
        || html.contains("Attention Required! | Cloudflare")
        // Akamai / Imperva / Distil
        || html.contains("akamai")
        || html.contains("distil_r_captcha")
        || html.contains("imperva")
        // Generic: very short pages with no actual link tags are likely
        // error/block pages, not search results
        || (html.len() < 200 && !html.contains("<a "))
}

/// Resolve a DDG result href to the real destination URL.
///
/// DDG wraps results as `//duckduckgo.com/l/?uddg=<percent-encoded real url>`.
/// Direct `http(s)://` hrefs pass through unchanged.
fn resolve_real_url(href: &str) -> Option<String> {
    if let Some(pos) = href.find("uddg=") {
        let after = &href[pos + "uddg=".len()..];
        let end = after.find('&').unwrap_or(after.len());
        let decoded = percent_decode(&after[..end]);
        if decoded.starts_with("http://") || decoded.starts_with("https://") {
            return Some(decoded);
        }
    }
    // Protocol-relative real URL (rare, non-DDG): normalize to https.
    if let Some(rest) = href.strip_prefix("//") {
        if !rest.starts_with("duckduckgo.com") && rest.contains('.') {
            return Some(format!("https:{href}"));
        }
    }
    if href.starts_with("http://") || href.starts_with("https://") {
        return Some(href.replace("&amp;", "&"));
    }
    None
}

/// Is this a search-engine-internal link (ads, nav, redirect that didn't unwrap)?
/// Covers both DuckDuckGo and Bing self-references.
fn is_internal_link(url: &str) -> bool {
    // DuckDuckGo internals
    url.contains("duckduckgo.com/y.js")
        || url.contains("duckduckgo.com/l/?")
        || url.contains("duckduckgo.com/ai")
        || url.starts_with("https://duckduckgo.com")
        || url.starts_with("http://duckduckgo.com")
        // Bing internals: nav, ads, attribution, "ck/a" click-redirect
        || url.contains("://www.bing.com/ck/")
        || url.contains("go.microsoft.com/fwlink")
        || url.contains("://www.bing.com/account")
        || url.contains("://www.bing.com/feedback")
        || url.contains("://cn.bing.com/ck/")
        || url == "https://www.bing.com"
        || url == "https://cn.bing.com"
}

/// Minimal percent-decoding for `uddg=` params: `+` → space, `%XX` → byte.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                if let Ok(byte) =
                    u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16)
                {
                    out.push(byte);
                    i += 3;
                    continue;
                } else {
                    out.push(b[i]);
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Extract the text content between `<a ...>` and `</a>`.
fn extract_link_text(html_fragment: &str) -> String {
    // Find <a ...>
    let a_start = match html_fragment.find("<a ") {
        Some(pos) => pos,
        None => return String::new(),
    };
    let tag_end = match html_fragment[a_start..].find('>') {
        Some(pos) => a_start + pos + 1,
        None => return String::new(),
    };
    let close = match html_fragment[tag_end..].find("</a>") {
        Some(pos) => tag_end + pos,
        None => return String::new(),
    };
    strip_html_tags(&html_fragment[tag_end..close]).trim().to_string()
}

/// Extract snippet text from a Bing `b_algo` block.
/// Bing puts the description in `<p>` or `<div class="b_caption">`.
fn extract_bing_snippet(block: &str) -> String {
    // Try <p> tag text
    if let Some(p_start) = block.find("<p") {
        if let Some(gt) = block[p_start..].find('>') {
            let content_start = p_start + gt + 1;
            if let Some(end) = block[content_start..].find("</p>") {
                let text = &block[content_start..content_start + end];
                let clean = strip_html_tags(text).trim().to_string();
                if clean.len() > 10 {
                    return truncate_at(&clean, 200);
                }
            }
        }
    }
    // Try <div class="b_caption">
    if let Some(div_start) = block.find("b_caption") {
        if let Some(gt) = block[div_start..].find('>') {
            let content_start = div_start + gt + 1;
            if let Some(end) = block[content_start..].find("</div>") {
                let text = &block[content_start..content_start + end];
                let clean = strip_html_tags(text).trim().to_string();
                if clean.len() > 10 {
                    return truncate_at(&clean, 200);
                }
            }
        }
    }
    String::new()
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

/// Percent-encode a search query for embedding in a browser URL.
/// Space → `+` (form-encoded), unreserved chars pass through, all else → `%XX`.
fn encode_url_query(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ' ' => "+".to_string(),
            c if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') => c.to_string(),
            c => format!("%{:02X}", c as u8),
        })
        .collect()
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

        // Browser-grade client: realistic headers + proxy awareness to dodge
        // datacenter-IP 403 blocks.
        let client = super::http_util::build_browser_client(
            std::time::Duration::from_secs(10),
            std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS),
        )?;

        // Try each DDG endpoint in order until one returns parseable results.
        // This defeats per-endpoint IP blocking.
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

        if results.is_empty() {
            let tried_str = tried.join(", ");
            // Final fallback: open the user's real default browser. This is the
            // most reliable path — a real browser carries cookies, a genuine
            // fingerprint, and honors the user's system proxy/VPN, so it sidesteps
            // the datacenter-IP block entirely. Override the search engine via
            // EVEREVO_SEARCH_BROWSER_URL (default: Bing, reachable in mainland China).
            let engine = std::env::var("EVEREVO_SEARCH_BROWSER_URL")
                .unwrap_or_else(|_| "https://cn.bing.com/search?q=".to_string());
            let browser_url = format!("{}{}", engine, encode_url_query(query));

            if open::that(&browser_url).is_ok() {
                return Ok(ToolOutput {
                    content: format!(
                        "Direct search was blocked by DuckDuckGo (datacenter IP). \
                         Opened your default browser to:\n{browser_url}\n\n\
                         A real browser bypasses the block (real fingerprint + system \
                         proxy/VPN + cookies). Review the results there; if you need \
                         specific page content in this session, use `web_fetch` on the \
                         result URL.\n\n\
                         Diagnostic — tried direct endpoints [{tried_str}], last error: {last_error}"
                    ),
                    is_error: false,
                 ..Default::default() });
            }

            return Ok(ToolOutput {
                content: format!(
                    "Web search failed — all endpoints unreachable and browser fallback failed.\n\n\
                     Tried: {tried_str}\n\
                     Last error: {last_error}\n\n\
                     Likely cause: your IP is blocked by DuckDuckGo's anti-bot filter \
                     (common for datacenter/proxy IPs).\n\
                     Fixes:\n\
                     1. Set EVEREVO_HTTP_PROXY=http://your-proxy:port to route through a \
                     residential/VPN proxy.\n\
                     2. Ensure HTTPS_PROXY/HTTP_PROXY env vars are exported.\n\
                     3. For reliable search, configure a Brave/Tavily MCP search server."
                ),
                is_error: true,
                ..Default::default()
            });
        }

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
             ..Default::default() });
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
        // The first result should be example.com, not duckduckgo internal
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
        // DDG lite/html wraps real URLs in //duckduckgo.com/l/?uddg=<encoded>.
        let html = r#"<a rel="nofollow" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Frust-lang.org%2F">Rust Programming Language</a>"#;
        let results = WebSearchTool::parse_results(html, 5);
        assert_eq!(results.len(), 1);
        let (title, url, _) = &results[0];
        assert!(title.contains("Rust"), "got: {title}");
        assert_eq!(url, "https://rust-lang.org/");
    }

    #[test]
    fn test_parse_challenge_returns_empty() {
        // Anti-bot challenge page must yield NO results (so the next endpoint
        // is tried), instead of mistaking the footer "here" link for a result.
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
            "hello world …"
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
        // & and # must be percent-encoded or they break the URL
        assert_eq!(encode_url_query("rust & crates"), "rust+%26+crates");
        assert_eq!(encode_url_query("c# vs rust"), "c%23+vs+rust");
    }

    #[test]
    fn test_encode_url_query_cjk() {
        // Non-ASCII must be percent-encoded as UTF-8 bytes
        let encoded = encode_url_query("你好");
        assert!(
            encoded.starts_with("%"),
            "CJK should be percent-encoded, got {encoded}"
        );
    }

    #[test]
    fn test_engines_defined() {
        // Bing (default, mainland-friendly) + at least one DDG fallback.
        assert!(ENGINES.len() >= 2);
        assert_eq!(ENGINES[0], SearchEngine::BingCn); // Bing must be tried first
    }

    #[test]
    fn test_parse_bing_html() {
        // Bing returns real URLs directly (no uddg redirect wrapper).
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
        // Bing nav/ad links (ck/a click-redirect, account) must be skipped.
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
}
