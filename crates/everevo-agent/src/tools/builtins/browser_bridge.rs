//! Browser Bridge — CDP-based search via the user's real browser.
//!
//! ## Problem
//!
//! Direct HTTP requests to search engines get blocked (datacenter IP).
//! `open::that()` opens the user's browser but returns zero data to the agent.
//!
//! ## Solution
//!
//! Launch Chrome/Edge with `--remote-debugging-port`, connect via CDP
//! (Chrome DevTools Protocol) over WebSocket, navigate to the search engine,
//! and execute a JavaScript scraper in the *rendered* DOM. Results come back
//! as structured JSON through the CDP channel — no local HTTP server needed.
//!
//! The browser uses the user's real fingerprint (User-Agent, TLS stack,
//! viewport) and real IP (plus proxy/VPN if configured), so it naturally
//! bypasses the anti-bot filters that block `reqwest`.
//!
//! ## Architecture
//!
//! ```text
//! Agent                   Browser Bridge              Chrome/Edge
//!   │                          │                          │
//!   │──search("rust error")──→│                          │
//!   │                          │──launch chrome──→        │
//!   │                          │   --remote-debugging-port=PORT
//!   │                          │   --user-data-dir=TEMP   │
//!   │                          │                          │
//!   │                          │──CDP: Page.navigate──→   │
//!   │                          │   url=search_engine      │
//!   │                          │                          │
//!   │                          │←──Page.loadEventFired──  │
//!   │                          │                          │
//!   │                          │──CDP: Runtime.evaluate──→│
//!   │                          │   expression=scraper.js  │
//!   │                          │                          │
//!   │                          │←──[{title,url,snippet}]─ │
//!   │                          │                          │
//!   │←──[SearchResult]─────────│                          │
//!   │                          │──Browser.close──→        │
//! ```

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use everevo_core::EverEvoError;

// ── Result type ───────────────────────────────────────────────────────────

/// A single search result extracted from a rendered search-engine page.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BrowserSearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

// ── CDP types ─────────────────────────────────────────────────────────────

type CdpWs = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct CdpConnection {
    ws: CdpWs,
    next_id: u64,
}

impl CdpConnection {
    fn new(ws: CdpWs) -> Self {
        Self { ws, next_id: 1 }
    }

    /// Send a CDP command and wait for the matching response.
    async fn send_command(&mut self, method: &str, params: Value) -> Result<Value, EverEvoError> {
        let id = self.next_id;
        self.next_id += 1;

        let msg = serde_json::json!({
            "id": id,
            "method": method,
            "params": params,
        });
        let msg_str = msg.to_string();
        self.ws
            .send(Message::Text(msg_str))
            .await
            .map_err(|e| EverEvoError::Internal(format!("CDP send: {e}")))?;

        // Read responses until we get the one matching our id.
        // CDP events (no `id` field) are interleaved — skip them.
        loop {
            let raw = self
                .recv_text_timeout(Duration::from_secs(30))
                .await?;
            let v: Value = serde_json::from_str(&raw)
                .map_err(|e| EverEvoError::Internal(format!("CDP parse: {e}")))?;

            if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                return if let Some(err) = v.get("error") {
                    Err(EverEvoError::Internal(format!(
                        "CDP {method}: {}",
                        err.get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("unknown error")
                    )))
                } else {
                    Ok(v.get("result").cloned().unwrap_or(Value::Null))
                };
            }
            // else: event — skip and continue reading
        }
    }

    /// Read a text message with timeout.
    async fn recv_text_timeout(&mut self, dur: Duration) -> Result<String, EverEvoError> {
        let msg = tokio::time::timeout(dur, self.ws.next())
            .await
            .map_err(|_| EverEvoError::Internal("CDP recv timeout".into()))?
            .ok_or_else(|| EverEvoError::Internal("CDP connection closed".into()))?
            .map_err(|e| EverEvoError::Internal(format!("CDP recv: {e}")))?;

        match msg {
            Message::Text(t) => Ok(t.to_string()),
            Message::Close(_) => Err(EverEvoError::Internal("CDP: browser closed connection".into())),
            other => Ok(other.to_string()),
        }
    }
}

// ── Browser launcher ──────────────────────────────────────────────────────

/// Find a Chromium-based browser executable on the system.
fn find_browser() -> Option<std::path::PathBuf> {
    // Chrome / Edge / Brave / Chromium — check common install locations.
    #[cfg(target_os = "windows")]
    {
        let candidates: &[&str] = &[
            // Chrome
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
            // Edge
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
            r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
            // Brave
            r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe",
        ];
        for path in candidates {
            let p = std::path::Path::new(path);
            if p.exists() {
                return Some(p.to_path_buf());
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        for name in &["google-chrome", "chromium", "chromium-browser", "microsoft-edge", "brave-browser"]
        {
            if let Ok(p) = which::which(name) {
                return Some(p);
            }
        }
    }
    // Fallback: try PATH lookup
    which::which("chrome")
        .or_else(|_| which::which("msedge"))
        .or_else(|_| which::which("chromium"))
        .or_else(|_| which::which("brave"))
        .ok()
}

/// Launch a browser with CDP remote debugging enabled.
///
/// Uses a temporary user-data directory to avoid profile-lock conflicts
/// with an already-running browser. The real browser binary provides the
/// authentic TLS fingerprint + User-Agent + viewport; the temp profile
/// just means no saved cookies (acceptable — search engines don't require
/// login).
/// Launch a browser process on the **host** (not in sandbox).
/// The whole point of the browser bridge is to use the host browser's
/// real fingerprint — sandboxing would defeat this.
#[allow(clippy::disallowed_methods)]
async fn launch_browser_ex(
    browser_path: &std::path::Path,
    port: u16,
    headless: bool,
) -> Result<tokio::process::Child, EverEvoError> {
    let temp_dir = std::env::temp_dir().join(format!("everevo-cdp-{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir)
        .map_err(|e| EverEvoError::Internal(format!("create cdp temp dir: {e}")))?;

    let mut cmd = tokio::process::Command::new(browser_path);
    cmd.arg(format!("--remote-debugging-port={port}"))
        .arg(format!(
            "--user-data-dir={}",
            temp_dir.display()
        ))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-background-networking")
        .arg("--disable-sync")
        .arg("--disable-extensions")
        .arg("--disable-features=TranslateUI")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);

    if headless {
        // `--headless=new` uses the new headless mode (same rendering path as
        // headful), which is more reliable and harder to detect as a bot than
        // the old `--headless` flag.
        cmd.arg("--headless=new");
    }

    cmd.arg("about:blank"); // start with blank page

    let child = cmd
        .spawn()
        .map_err(|e| EverEvoError::Internal(format!("launch browser ({mode}): {e}", mode = if headless { "headless" } else { "headful" })))?;

    Ok(child)
}

// ── Browser Bridge ────────────────────────────────────────────────────────

pub struct BrowserBridge {
    /// Browser child process handle.
    _process: Option<tokio::process::Child>,
    /// Debugging port.
    port: u16,
    /// Cleanup: temp directory to remove on drop.
    _temp_dir: Option<std::path::PathBuf>,
}

impl BrowserBridge {
    /// Launch a browser with CDP and return the bridge handle.
    ///
    /// First tries the default (headful) mode. If CDP doesn't bind within
    /// the wait window, retries with `--headless=new` which is more reliable
    /// in headless/server environments where GPU/display may be absent.
    pub async fn launch() -> Result<Self, EverEvoError> {
        let browser_path = find_browser().ok_or_else(|| {
            EverEvoError::Internal(
                "No Chromium-based browser found. Install Chrome, Edge, or Brave.".into(),
            )
        })?;

        // Pick a random high port to avoid conflicts
        let port = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
                .hash(&mut h);
            (h.finish() % 20000 + 10000) as u16
        };

        // Try headful first, then headless as fallback.
        for &headless in &[false, true] {
            let mode = if headless { "headless" } else { "headful" };
            tracing::info!(
                browser = %browser_path.display(),
                port,
                mode,
                "Launching browser for CDP bridge"
            );

            let process = launch_browser_ex(&browser_path, port, headless).await?;
            let temp_dir =
                std::env::temp_dir().join(format!("everevo-cdp-{}", std::process::id()));

            // Wait for CDP to become available with exponential backoff
            // (250ms → 500ms → 1s → ... capped at 2s, total ~20s).
            let client = reqwest::Client::new();
            let version_url = format!("http://127.0.0.1:{port}/json/version");
            let mut ws_url: Option<String> = None;

            for attempt in 0..30 {
                let delay_ms = (250u64 * (1u64 << attempt.min(3))).min(2000);
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                match client.get(&version_url).send().await {
                    Ok(resp) => {
                        if let Ok(json) = resp.json::<Value>().await {
                            if let Some(url) = json
                                .get("webSocketDebuggerUrl")
                                .and_then(|v| v.as_str())
                            {
                                ws_url = Some(url.to_string());
                                break;
                            }
                        }
                    }
                    Err(_) => {
                        if attempt % 10 == 9 {
                            tracing::debug!(attempt, mode, "Still waiting for CDP...");
                        }
                    }
                }
            }

            match ws_url {
                Some(url) => {
                    tracing::info!(port, ws_url = %url, mode, "CDP ready");
                    return Ok(Self {
                        _process: Some(process),
                        port,
                        _temp_dir: Some(temp_dir),
                    });
                }
                None if !headless => {
                    tracing::warn!(
                        port,
                        elapsed_approx = "~20s",
                        "CDP not available in headful mode — retrying with --headless=new"
                    );
                    // Kill the headful process before retrying
                    drop(process);
                }
                None => {
                    // Both modes failed — clean up temp dir
                    let _ = std::fs::remove_dir_all(&temp_dir);
                    return Err(EverEvoError::Internal(format!(
                        "Browser launched but CDP not available on port {port} after 20s. \
                         Both headful and headless modes tried. \
                         Check: is Chrome/Edge installed? Is another process using port {port}?"
                    )));
                }
            }
        }

        unreachable!()
    }

    /// Get the CDP WebSocket URL for a blank page.
    async fn page_ws_url(&self) -> Result<String, EverEvoError> {
        let client = reqwest::Client::new();
        let list_url = format!("http://127.0.0.1:{}/json", self.port);
        let resp = client
            .get(&list_url)
            .send()
            .await
            .map_err(|e| EverEvoError::Internal(format!("CDP list: {e}")))?;
        let pages: Vec<Value> = resp
            .json()
            .await
            .map_err(|e| EverEvoError::Internal(format!("CDP list parse: {e}")))?;

        pages
            .first()
            .and_then(|p| p.get("webSocketDebuggerUrl"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| EverEvoError::Internal("No open pages in browser".into()))
    }

    /// Connect to a page via CDP WebSocket.
    async fn connect_page(&self) -> Result<CdpConnection, EverEvoError> {
        let ws_url = self.page_ws_url().await?;
        let (ws, _resp) = connect_async(&ws_url)
            .await
            .map_err(|e| EverEvoError::Internal(format!("CDP connect: {e}")))?;
        Ok(CdpConnection::new(ws))
    }

    /// Navigate to a URL, wait for page load, extract search results.
    pub async fn extract_search_results(
        &self,
        search_url: &str,
        limit: usize,
    ) -> Result<Vec<BrowserSearchResult>, EverEvoError> {
        let mut cdp = self.connect_page().await?;

        // Enable Page domain
        cdp.send_command("Page.enable", serde_json::json!({})).await?;

        // Navigate to search URL
        tracing::debug!(url = %search_url, "CDP navigate");
        cdp.send_command(
            "Page.navigate",
            serde_json::json!({"url": search_url}),
        )
        .await?;

        // Wait for page load by reading CDP events until we get
        // Page.loadEventFired or timeout.
        let load_timeout = Duration::from_secs(20);
        let deadline = tokio::time::Instant::now() + load_timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(EverEvoError::Internal("Page load timeout (20s)".into()));
            }

            let raw = tokio::time::timeout(remaining, cdp.ws.next())
                .await
                .map_err(|_| EverEvoError::Internal("Page load timeout".into()))?
                .ok_or_else(|| EverEvoError::Internal("CDP connection lost during load".into()))?
                .map_err(|e| EverEvoError::Internal(format!("CDP during load: {e}")))?;

            if let Message::Text(t) = raw {
                if t.contains("Page.loadEventFired") {
                    tracing::debug!("Page loaded");
                    break;
                }
            }
        }

        // Small extra delay for any post-load rendering
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Execute the DOM scraper
        let scraper_js = search_result_scraper_js(limit);
        let result = cdp
            .send_command(
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": scraper_js,
                    "returnByValue": true,
                }),
            )
            .await?;

        // Parse the JSON result from the evaluate response
        let json_str = result
            .get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("[]");

        let results: Vec<BrowserSearchResult> = serde_json::from_str(json_str)
            .unwrap_or_default();

        tracing::info!(count = results.len(), "CDP search results extracted");

        Ok(results)
    }
}

// ── JavaScript Scrapers ───────────────────────────────────────────────────

/// Build a JavaScript snippet that extracts search results from the current
/// page's DOM. Handles Bing, DuckDuckGo, and generic fallback.
fn search_result_scraper_js(limit: usize) -> String {
    format!(
        r#"(function() {{
  var results = [];
  var hostname = window.location.hostname;

  // ── Bing (cn.bing.com / www.bing.com) ──────────────────────────
  if (hostname.indexOf('bing.com') !== -1) {{
    var items = document.querySelectorAll('.b_algo');
    for (var i = 0; i < items.length && results.length < {limit}; i++) {{
      var el = items[i];
      var a = el.querySelector('h2 a');
      var p = el.querySelector('p, .b_caption p, .b_lineclamp2');
      if (a && a.href && a.href.indexOf('http') === 0) {{
        var url = a.href;
        // Skip internal Bing links
        if (url.indexOf('bing.com/ck/') !== -1 ||
            url.indexOf('go.microsoft.com') !== -1 ||
            url.indexOf('bing.com/account') !== -1) continue;
        results.push({{
          title: (a.textContent || '').trim(),
          url: url,
          snippet: p ? (p.textContent || '').trim() : ''
        }});
      }}
    }}
  }}

  // ── DuckDuckGo (html/lite) ─────────────────────────────────────
  else if (hostname.indexOf('duckduckgo.com') !== -1) {{
    var links = document.querySelectorAll('a.result__a, a.result-link');
    for (var i = 0; i < links.length && results.length < {limit}; i++) {{
      var a = links[i];
      if (a.href && a.href.indexOf('http') === 0 &&
          a.href.indexOf('duckduckgo.com') === -1) {{
        var snippet = '';
        var parent = a.closest('.result, .result__body, tr');
        if (parent) {{
          var s = parent.querySelector('.result__snippet, .result-snippet, td:last-child');
          if (s) snippet = (s.textContent || '').trim();
        }}
        results.push({{
          title: (a.textContent || '').trim(),
          url: a.href,
          snippet: snippet
        }});
      }}
    }}
  }}

  // ── Generic fallback — extract links with surrounding text ─────
  if (results.length === 0) {{
    var allLinks = document.querySelectorAll('a[href^="http"]');
    for (var i = 0; i < allLinks.length && results.length < {limit}; i++) {{
      var a = allLinks[i];
      if (a.textContent.trim().length > 10 &&
          !a.href.includes('duckduckgo.com') &&
          !a.href.includes('bing.com')) {{
        // Try to find nearby paragraph text
        var parent = a.closest('li, div, article, tr');
        var snippet = '';
        if (parent) {{
          snippet = (parent.textContent || '').replace(a.textContent, '').trim().substring(0, 200);
        }}
        results.push({{
          title: a.textContent.trim(),
          url: a.href,
          snippet: snippet
        }});
      }}
    }}
  }}

  return JSON.stringify(results);
}})()"#,
        limit = limit
    )
}

// ── Fallback wrapper — used by web_search when direct HTTP fails ──────────

/// Try to extract search results via CDP browser bridge.
///
/// Returns `Ok(results)` on success, `Err` if browser launch or CDP fails
/// (caller should fall back to `open::that()`).
pub async fn search_via_browser(
    query: &str,
    limit: usize,
) -> Result<Vec<BrowserSearchResult>, EverEvoError> {
    // Build the search URL — prefer Bing (mainland-China-friendly)
    let engine = std::env::var("EVEREVO_SEARCH_BROWSER_URL")
        .unwrap_or_else(|_| "https://cn.bing.com/search?q=".to_string());
    let search_url = format!("{}{}", engine, super::web_search::encode_url_query(query));

    let bridge = BrowserBridge::launch().await?;
    let results = bridge.extract_search_results(&search_url, limit).await?;

    // Browser process is killed on drop (kill_on_drop was set)
    drop(bridge);
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_browser_returns_some() {
        // On any dev machine, at least one browser should be found.
        // This test is informational — it won't fail if no browser is installed.
        if let Some(path) = find_browser() {
            assert!(path.exists(), "found browser path should exist: {path:?}");
        }
    }

    #[test]
    fn test_scraper_js_is_valid_javascript() {
        let js = search_result_scraper_js(5);
        // Basic sanity: the JS snippet must contain key function calls
        assert!(js.contains("querySelectorAll"));
        assert!(js.contains("JSON.stringify"));
        assert!(js.contains("b_algo"));
    }

    #[test]
    fn test_scraper_js_contains_bing_selector() {
        let js = search_result_scraper_js(10);
        assert!(js.contains("b_algo"), "must handle Bing results");
        assert!(js.contains("duckduckgo.com"), "must handle DDG results");
    }
}
