// ── Shared HTTP agent & formatting ────────────────────────────────────────

/// Browser User-Agent — without it Bing treats the request as a bot and serves
/// junk/localized SEO results instead of the real English SERP.
pub(crate) const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// Resolve an HTTP proxy URL from env. `EVEREVO_HTTP_PROXY` is the explicit
/// override; standard `HTTPS_PROXY`/`HTTP_PROXY`/`ALL_PROXY` follow. Empty when
/// none is set — the agent then connects directly (the mainland-China default).
/// Single egress: proxy env parsing lives in `everevo-net`.
pub(crate) fn env_proxy_url() -> Option<String> {
    everevo_net::env_proxy_url()
}

/// Build a ureq Agent with a connect/global timeout and redirects so a blocked
/// endpoint fails fast instead of hanging (the old code used the global
/// convenience `ureq::get`, which had no timeout). When a proxy env var is
/// present the agent routes through it — proxy wiring lives in `everevo-net`.
pub(crate) fn agent() -> ureq::Agent {
    everevo_net::ureq_agent(
        std::time::Duration::from_secs(8),
        std::time::Duration::from_secs(15),
        3,
        Some(BROWSER_UA),
    )
}

pub(crate) fn read_body(resp: ureq::http::Response<ureq::Body>) -> Result<String, String> {
    resp.into_body()
        .read_to_string()
        .map_err(|e| format!("Read response: {e}"))
}

/// Result triple: (title, url, snippet).
pub(crate) type Hit = (String, String, String);

/// Simple URL encoding (avoid adding a dependency for this).
pub(crate) fn urlencoding(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            ' ' => "+".to_string(),
            other => format!("%{:02X}", other as u8),
        })
        .collect()
}

/// Strip HTML tags from text.
pub(crate) fn strip_html(s: &str) -> String {
    let mut result = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(c),
            _ => {}
        }
    }
    // Decode common HTML entities
    result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&nbsp;", " ")
        .replace("&#039;", "'")
        .trim()
        .to_string()
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "\u{2026}"
    }
}
