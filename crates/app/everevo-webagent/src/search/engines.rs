//! Multi-engine web search — Bing + DuckDuckGo with fallback cascade.
//!
//! Bing (cn.bing.com) is tried first because it's reachable from mainland
//! China without a proxy. DDG lite/html follow as fallback for when Bing is
//! rate-limited or blocked.
//!
//! No API key required — parses HTML search result pages.

use std::time::Duration;

// ── Result type ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

// ── Search engines ───────────────────────────────────────────────────────

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

    async fn fetch_html(&self, client: &reqwest::Client, query: &str) -> Result<String, String> {
        match self {
            Self::BingCn => {
                let url = format!("https://cn.bing.com/search?q={}", encode_url_query(query));
                let resp = client
                    .get(&url)
                    .send()
                    .await
                    .map_err(|e| format!("bing request: {e}"))?;
                if !resp.status().is_success() {
                    return Err(format!("bing HTTP {}", resp.status()));
                }
                resp.text()
                    .await
                    .map_err(|e| format!("bing body: {e}"))
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
                    .map_err(|e| format!("ddg request: {e}"))?;
                if !resp.status().is_success() {
                    return Err(format!("ddg HTTP {}", resp.status()));
                }
                resp.text()
                    .await
                    .map_err(|e| format!("ddg body: {e}"))
            }
        }
    }
}

// ── Public API ───────────────────────────────────────────────────────────

/// Search the web and return formatted results text.
pub async fn search(query: &str, limit: usize) -> Result<String, String> {
    let client = build_search_client()?;

    let mut results: Vec<SearchResult> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for engine in ENGINES {
        match engine.fetch_html(&client, query).await {
            Ok(body) => {
                let parsed = parse_results(&body, limit);
                if !parsed.is_empty() {
                    results = parsed;
                    break;
                }
            }
            Err(e) => {
                errors.push(format!("{}: {}", engine.label(), e));
            }
        }
    }

    if results.is_empty() {
        let engine_list: Vec<&str> = ENGINES.iter().map(|e| e.label()).collect();
        return Err(format!(
            "All search engines failed: {}. Errors: {}",
            engine_list.join(", "),
            errors.join("; ")
        ));
    }

    // Format results
    let lines: Vec<String> = results
        .iter()
        .enumerate()
        .map(|(i, r)| format!("{}. **{}**\n   {}\n   {}", i + 1, r.title, r.url, r.snippet))
        .collect();

    Ok(format!(
        "Web search results for '{}':\n\n{}",
        query,
        lines.join("\n\n")
    ))
}

// ── HTTP client builder ─────────────────────────────────────────────────

fn build_search_client() -> Result<reqwest::Client, String> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static(
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        ),
    );
    headers.insert(
        reqwest::header::ACCEPT_LANGUAGE,
        reqwest::header::HeaderValue::from_static("en-US,en;q=0.9,zh-CN;q=0.8"),
    );

    let ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

    // Proxy detection
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .user_agent(ua)
        .default_headers(headers)
        .redirect(reqwest::redirect::Policy::limited(3))
        .tcp_nodelay(true);

    if let Some(proxy_url) = detect_proxy() {
        if let Ok(proxy) = reqwest::Proxy::all(&proxy_url) {
            builder = builder.proxy(proxy);
        }
    }

    builder.build().map_err(|e| format!("client build: {e}"))
}

fn detect_proxy() -> Option<String> {
    for var in &[
        "EVEREVO_HTTP_PROXY",
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        if let Ok(val) = std::env::var(var) {
            let val = val.trim().to_string();
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

// ── HTML parsing ─────────────────────────────────────────────────────────

fn parse_results(html: &str, limit: usize) -> Vec<SearchResult> {
    if is_challenge_page(html) {
        return Vec::new();
    }

    let has_bing_blocks = html.contains("b_algo");
    if has_bing_blocks {
        let results = parse_bing_results(html, limit);
        if !results.is_empty() {
            return results;
        }
        return Vec::new();
    }

    parse_generic_results(html, limit)
}

fn parse_bing_results(html: &str, limit: usize) -> Vec<SearchResult> {
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
        if !results.iter().any(|r: &SearchResult| r.url == href) {
            results.push(SearchResult { title, url: href, snippet });
        }
    }

    results
}

fn parse_generic_results(html: &str, limit: usize) -> Vec<SearchResult> {
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
                if title.is_empty() || is_internal_link(&url) || title.eq_ignore_ascii_case("here") {
                    continue;
                }
                let snippet = extract_snippet(&chars, i);
                if !results.iter().any(|r: &SearchResult| r.url == url) {
                    results.push(SearchResult { title, url, snippet });
                }
            }
            None => break,
        }
    }

    results
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn extract_href(tag_body: &str) -> Option<String> {
    let href_pos = tag_body.find("href=")?;
    let after = &tag_body[href_pos + 5..];
    let quote = after.chars().next()?;
    let inner = &after[1..];
    let end = inner.find(quote)?;
    let url = &inner[..end];
    Some(url.replace("&amp;", "&"))
}

fn extract_link_text(html_fragment: &str) -> String {
    let a_start = match html_fragment.find("<a ") {
        Some(p) => p,
        None => return String::new(),
    };
    let tag_end = match html_fragment[a_start..].find('>') {
        Some(p) => a_start + p + 1,
        None => return String::new(),
    };
    let close = match html_fragment[tag_end..].find("</a>") {
        Some(p) => tag_end + p,
        None => return String::new(),
    };
    strip_html_tags(&html_fragment[tag_end..close]).trim().to_string()
}

fn extract_bing_snippet(block: &str) -> String {
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

fn find_result_link(chars: &[char], start: usize) -> Option<(String, String, usize)> {
    let n = chars.len();
    let mut pos = start;

    while pos < n {
        let tag_start = substr_pos(chars, pos, "<a ")? + pos;
        let tag_body_end = substr_pos(chars, tag_start, ">")? + tag_start + 1;
        let tag_body: String = chars[tag_start + 3..tag_body_end].iter().collect();
        let href = extract_href(&tag_body);

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

        let close_tag = substr_pos(chars, tag_body_end, "</a>")? + tag_body_end;
        let title: String = chars[tag_body_end..close_tag].iter().collect();
        let title = strip_html_tags(&title).trim().to_string();

        if title.is_empty() || href.starts_with("javascript:") {
            pos = close_tag + 4;
            continue;
        }

        return Some((href, title, close_tag + 4));
    }
    None
}

fn extract_snippet(chars: &[char], pos: usize) -> String {
    let n = chars.len();
    let end = (pos + 800).min(n);
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

fn resolve_real_url(href: &str) -> Option<String> {
    if let Some(pos) = href.find("uddg=") {
        let after = &href[pos + "uddg=".len()..];
        let end = after.find('&').unwrap_or(after.len());
        let decoded = percent_decode(&after[..end]);
        if decoded.starts_with("http://") || decoded.starts_with("https://") {
            return Some(decoded);
        }
    }
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

fn is_challenge_page(html: &str) -> bool {
    html.contains("anomaly.js")
        || html.contains("challenge-form")
        || html.contains("/check.")
        || html.contains("ddg_ptoken")
        || html.contains("captcha-delivery.com")
        || html.contains("g-recaptcha")
        || html.contains("hCaptcha")
        || html.contains("challenge-platform")
        || html.contains("tr.bing.com")
        || html.contains("Just a moment...")
        || html.contains("Checking your browser")
        || html.contains("cf-browser-verification")
        || html.contains("_cf_chl_opt")
        || html.contains("Please turn JavaScript on")
        || (html.len() < 200 && !html.contains("<a "))
}

fn is_internal_link(url: &str) -> bool {
    url.contains("duckduckgo.com/y.js")
        || url.contains("duckduckgo.com/l/?")
        || url.contains("duckduckgo.com/ai")
        || url.starts_with("https://duckduckgo.com")
        || url.contains("://www.bing.com/ck/")
        || url.contains("go.microsoft.com/fwlink")
        || url.contains("://www.bing.com/account")
        || url.contains("://cn.bing.com/ck/")
        || url == "https://www.bing.com"
        || url == "https://cn.bing.com"
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
        s.chars().take(max).collect::<String>() + "\u{2026}"
    }
}

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
                }
                out.push(b[i]);
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn encode_url_query(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ' ' => "+".to_string(),
            c if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') => c.to_string(),
            c => format!("%{:02X}", c as u8),
        })
        .collect()
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_bing_html() {
        let html = r#"<li class="b_algo">
            <h2><a href="https://www.rust-lang.org/">Rust Programming Language</a></h2>
            <p>A language empowering everyone to build reliable software.</p>
        </li>"#;
        let results = parse_results(html, 5);
        assert!(results.len() >= 1);
        assert_eq!(results[0].url, "https://www.rust-lang.org/");
    }

    #[test]
    fn test_parse_challenge_returns_empty() {
        let html = r#"<html><body><script src="anomaly.js"></script>
            <form id="challenge-form"></form>
            <a href="https://duckduckgo.com/">here</a></body></html>"#;
        assert!(parse_results(html, 5).is_empty());
    }

    #[test]
    fn test_encode_url_query() {
        assert_eq!(encode_url_query("hello world"), "hello+world");
        assert_eq!(encode_url_query("rust & crates"), "rust+%26+crates");
    }

    #[test]
    fn test_strip_html_tags() {
        assert_eq!(strip_html_tags("<b>hello</b> world"), "hello world");
    }
}
