//! Shared HTTP egress — the single place that turns the proxy environment into
//! proxy-aware HTTP clients.
//!
//! Every EverEvo component that talks to the outside world (web_fetch /
//! web_search plugins, the download engine, the agent's http_util tool, the LLM
//! client) used to re-implement the same env-var chain and proxy wiring. This
//! crate is that logic, once, so a proxy policy change applies everywhere.
//!
//! `resolve_proxy_url` — beyond the env-var chain — auto-detects a running
//! local proxy (Clash/V2Ray/Shadowsocks common ports) by TCP probe, mirroring
//! the LLM client's detection. This keeps web egress working even when a
//! long-lived server process was started without `HTTP_PROXY` in its
//! environment (e.g. a benchmark harness reusing a running server), which
//! otherwise leaves every web tool DNS-dead on a mainland-China host.

use std::net::{SocketAddr, TcpStream};
use std::sync::OnceLock;
use std::time::Duration;

/// Resolve an HTTP proxy URL from env. `EVEREVO_HTTP_PROXY` is the explicit
/// override; standard `HTTPS_PROXY`/`HTTP_PROXY`/`ALL_PROXY` follow (both
/// cases). Empty when none is set — callers connect directly.
pub fn env_proxy_url() -> Option<String> {
    [
        std::env::var("EVEREVO_HTTP_PROXY").ok(),
        std::env::var("HTTPS_PROXY").ok(),
        std::env::var("https_proxy").ok(),
        std::env::var("HTTP_PROXY").ok(),
        std::env::var("http_proxy").ok(),
        std::env::var("ALL_PROXY").ok(),
        std::env::var("all_proxy").ok(),
    ]
    .into_iter()
    .flatten()
    .map(|s| s.trim().to_string())
    .find(|s| !s.is_empty())
}

/// Resolve the process's outbound proxy: an explicit env-var proxy wins;
/// otherwise auto-detect a running local proxy by TCP-probing common
/// mainland-China proxy ports (Clash 7890, V2Ray 10808/10809, Shadowsocks
/// 1080/7891, Privoxy 8118) — the same policy the LLM client uses. Cached once
/// per process (OnceLock) so a blocking probe never runs per request.
pub fn resolve_proxy_url() -> Option<String> {
    static CACHE: OnceLock<Option<String>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            if let Some(url) = env_proxy_url() {
                return Some(url);
            }
            const CANDIDATE_PORTS: &[u16] = &[7890, 7891, 10808, 10809, 8118, 1080];
            for &port in CANDIDATE_PORTS {
                let addr = format!("127.0.0.1:{port}");
                let sa = match addr.parse::<SocketAddr>() {
                    Ok(sa) => sa,
                    Err(_) => continue,
                };
                // A closed local port fails the connect immediately; only a
                // firewall-dropped probe would wait out the 300ms timeout.
                if TcpStream::connect_timeout(&sa, Duration::from_millis(300)).is_ok() {
                    // HTTP proxies answer on 7890/10808/8118; SOCKS5 proxies on
                    // 7891/10809/1080 (socks5h resolves DNS remotely).
                    let scheme = match port {
                        7891 | 10809 | 1080 => "socks5h",
                        _ => "http",
                    };
                    return Some(format!("{scheme}://{addr}"));
                }
            }
            None
        })
        .clone()
}

/// Build a ureq Agent with connect/global timeouts, a redirect cap and an
/// optional User-Agent, routed through the resolved proxy when one is
/// available. A blocked endpoint then fails fast instead of hanging.
pub fn ureq_agent(
    connect_timeout: Duration,
    global_timeout: Duration,
    max_redirects: u32,
    user_agent: Option<&str>,
) -> ureq::Agent {
    let mut b = ureq::Agent::config_builder()
        .timeout_connect(Some(connect_timeout))
        .timeout_global(Some(global_timeout))
        .max_redirects(max_redirects);
    if let Some(ua) = user_agent {
        b = b.user_agent(ua);
    }
    if let Some(url) = resolve_proxy_url() {
        if let Ok(p) = ureq::Proxy::new(&url) {
            b = b.proxy(Some(p));
        }
    }
    b.build().new_agent()
}

/// Apply the resolved proxy to a reqwest `ClientBuilder`. Returns the builder
/// unchanged when no proxy is configured or the URL is invalid (invalid
/// configs fall back to direct — a misconfigured proxy must not brick the tool).
#[cfg(feature = "reqwest")]
pub fn reqwest_apply_proxy(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    match resolve_proxy_url() {
        Some(url) => match reqwest::Proxy::all(&url) {
            Ok(proxy) => builder.proxy(proxy),
            Err(_) => builder,
        },
        None => builder,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ureq_agent_builds_without_proxy() {
        // No proxy env is the normal case; agent must still build.
        let _agent = ureq_agent(Duration::from_secs(5), Duration::from_secs(15), 5, None);
    }

    #[test]
    #[cfg(feature = "reqwest")]
    fn reqwest_proxy_builder_is_consumable() {
        let builder = reqwest::Client::builder();
        let builder = reqwest_apply_proxy(builder);
        assert!(builder.build().is_ok());
    }

    #[test]
    fn resolve_proxy_url_never_panics() {
        // Auto-detect TCP-probes localhost ports; returns Some only if a local
        // proxy answers (CI without one -> None). Must never panic either way.
        let _ = resolve_proxy_url();
    }
}
