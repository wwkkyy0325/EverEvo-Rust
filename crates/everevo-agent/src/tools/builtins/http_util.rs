//! Shared HTTP client builder — browser-grade headers + proxy awareness.
//!
//! Both `web_search` and `web_fetch` use this to:
//! - Send realistic browser headers (reduces 403/blocks from anti-bot systems
//!   like DuckDuckGo's datacenter-IP filtering).
//! - Honor proxy configuration so traffic can be routed around IP blocks.
//!
//! ## Anti-bot mitigation
//!
//! Many lightweight search endpoints (DuckDuckGo HTML/Lite) block datacenter
//! IPs and bare/low-reputation User-Agents with HTTP 403. This builder sends a
//! full Chrome-like header set so requests look like a real browser navigation,
//! which is the single highest-leverage free mitigation
//! (sources: Scrapfly, ZenRows, Bright Data 403-bypass guides).

use std::time::Duration;

use everevo_core::EverEvoError;

/// Realistic Chrome User-Agent string. Keep reasonably current to avoid
/// signature-based blocking. Updated periodically.
pub const BROWSER_UA: &str = concat!(
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) ",
    "AppleWebKit/537.36 (KHTML, like Gecko) ",
    "Chrome/131.0.0.0 Safari/537.36",
);

/// Build a reqwest client with browser-grade headers + proxy awareness.
///
/// # Proxy resolution order
///
/// 1. `EVEREVO_HTTP_PROXY` env var — explicit override. When set, it is the
///    ONLY proxy used (reqwest disables automatic env detection once any
///    explicit `.proxy()` is configured). This lets users force traffic
///    through a residential/VPN proxy to escape a blocked datacenter IP.
/// 2. Standard env vars (`HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY`) — reqwest
///    applies these automatically when no explicit proxy is configured.
///
/// No `.no_proxy()` is ever called, so proxy usage is never suppressed.
pub fn build_browser_client(
    connect_timeout: Duration,
    request_timeout: Duration,
) -> Result<reqwest::Client, EverEvoError> {
    use reqwest::header::{self, HeaderMap, HeaderName, HeaderValue};

    // Assemble browser-grade default headers in one HeaderMap. This avoids the
    // generic-bound friction of chaining `.header()` calls and keeps the
    // request builder setup in a single shot.
    let mut headers = HeaderMap::new();
    headers.insert(
        header::ACCEPT,
        HeaderValue::from_static(
            "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8",
        ),
    );
    headers.insert(
        header::ACCEPT_LANGUAGE,
        HeaderValue::from_static("en-US,en;q=0.9,zh-CN;q=0.8,zh;q=0.7"),
    );
    headers.insert(
        header::ACCEPT_ENCODING,
        HeaderValue::from_static("gzip, deflate, br"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("max-age=0"));
    headers.insert(
        HeaderName::from_static("sec-fetch-dest"),
        HeaderValue::from_static("document"),
    );
    headers.insert(
        HeaderName::from_static("sec-fetch-mode"),
        HeaderValue::from_static("navigate"),
    );
    headers.insert(
        HeaderName::from_static("sec-fetch-site"),
        HeaderValue::from_static("none"),
    );
    headers.insert(
        HeaderName::from_static("sec-fetch-user"),
        HeaderValue::from_static("?1"),
    );
    headers.insert(
        HeaderName::from_static("upgrade-insecure-requests"),
        HeaderValue::from_static("1"),
    );

    let mut builder = reqwest::Client::builder()
        .connect_timeout(connect_timeout)
        .timeout(request_timeout)
        .user_agent(HeaderValue::from_str(BROWSER_UA).expect("static UA is valid"))
        .default_headers(headers)
        .redirect(reqwest::redirect::Policy::limited(3))
        .tcp_nodelay(true);

    // Proxy detection — explicit override, then standard env vars.
    // Reuse the same detection logic as the LLM client.
    let proxy_url: Option<String> = if let Ok(val) = std::env::var("EVEREVO_HTTP_PROXY") {
        let val = val.trim().to_string();
        if !val.is_empty() { Some(val) } else { None }
    } else {
        crate::llm::http::detect_proxy_sync()
    };

    if let Some(ref proxy_url) = proxy_url {
        tracing::debug!(proxy = %proxy_url, "Routing web tool traffic through proxy");
        let proxy = reqwest::Proxy::all(proxy_url).map_err(|e| {
            EverEvoError::Network(format!("Invalid proxy URL '{proxy_url}': {e}"))
        })?;
        builder = builder.proxy(proxy);
    }

    builder
        .build()
        .map_err(|e| EverEvoError::Network(format!("HTTP client build failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_browser_client_succeeds() {
        let client = build_browser_client(Duration::from_secs(5), Duration::from_secs(10));
        assert!(client.is_ok(), "client should build without env proxy");
    }
}
