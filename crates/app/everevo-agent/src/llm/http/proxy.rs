//! Proxy detection and HTTP client construction.
//!
//! Free functions with no dependency on [`HttpClient`]. Kept in a separate
//! module from the client coordinator so `http.rs` stays focused on the
//! request drivers (`chat` / `stream_chat`).

// ── Proxy detection ───────────────────────────────────────────────────────

/// Detect proxy URL from environment variables.
///
/// Checks in order: `EVEREVO_HTTP_PROXY`, `HTTPS_PROXY`, `https_proxy`,
/// `HTTP_PROXY`, `http_proxy`, `ALL_PROXY`, `all_proxy`.
///
/// Falls back to auto-detecting common local proxy ports (Clash: 7890,
/// V2Ray: 10808) by attempting a TCP connect. Returns `None` only if
/// no proxy is configured and no known proxy port responds.
pub async fn detect_proxy() -> Option<String> {
    // 1+2. Explicit override then standard env vars — single egress in
    // everevo-net. Fall through to port auto-detect when none is set.
    if let Some(val) = everevo_net::env_proxy_url() {
        tracing::info!(proxy = %val, "Using proxy from env");
        return Some(val);
    }
    // 3. Auto-detect common local proxy ports (Clash / V2Ray / Shadowsocks)
    // These are the most common in mainland China; a TCP connect to the
    // proxy port confirms the proxy is running.
    const CANDIDATE_PORTS: &[u16] = &[7890, 7891, 10808, 10809, 8118, 1080];
    for &port in CANDIDATE_PORTS {
        let addr = format!("127.0.0.1:{port}");
        if tokio::net::TcpStream::connect(&addr).await.is_ok() {
            // Assume HTTP proxy on this port. SOCKS5 ports (7891, 10809, 1080)
            // get a socks5h:// prefix; HTTP ports get http://.
            let scheme = match port {
                7891 | 10809 | 1080 => "socks5h",
                _ => "http",
            };
            let proxy_url = format!("{scheme}://{addr}");
            tracing::info!(%proxy_url, "Auto-detected local proxy");
            return Some(proxy_url);
        }
    }
    None
}

/// Sync proxy detection from env vars only (no network I/O). Delegates to
/// `everevo-net` — the project's single HTTP egress.
pub fn detect_proxy_sync() -> Option<String> {
    everevo_net::env_proxy_url()
}

/// Build a reqwest client with optional proxy.
pub fn build_llm_http_client(proxy_url: Option<&str>) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(600))
        .user_agent(format!("EverEvo/{}", env!("CARGO_PKG_VERSION")));

    // Apply proxy from env vars (sync)
    let env_proxy = detect_proxy_sync();
    let proxy_src = proxy_url.or(env_proxy.as_deref());
    if let Some(url) = proxy_src {
        match reqwest::Proxy::all(url) {
            Ok(proxy) => {
                builder = builder.proxy(proxy);
                tracing::info!(%url, "LLM client using proxy");
            }
            Err(e) => {
                tracing::warn!(%url, error = %e, "Invalid proxy URL — proceeding without proxy");
            }
        }
    }

    builder.build().unwrap_or_default()
}
