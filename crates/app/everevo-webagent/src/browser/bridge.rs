//! CDP Browser Bridge — launch + control a real Chrome/Edge browser
//! via the Chrome DevTools Protocol over WebSocket.
//!
//! ## Architecture
//!
//! ```text
//! Agent → MCP tool → BrowserBridge.launch()
//!   ├── find_browser()      Chrome → Edge → Brave
//!   ├── launch_browser()    --remote-debugging-port=PORT
//!   ├── stealth inject      addScriptToEvaluateOnNewDocument
//!   ├── Page.navigate()     search engine URL
//!   ├── wait LoadEventFired
//!   ├── detect_challenge()  CAPTCHA? Turnstile? reCAPTCHA?
//!   ├── CaptchaSolver       Simple (checkbox/Turnstile) or Vision (future)
//!   └── Runtime.evaluate()  DOM scraper JS → structured results
//! ```
//!
//! ## Anti-detection
//!
//! Uses the real Chrome binary for authentic TLS fingerprinting,
//! injects stealth JS before any page script runs, and applies
//! `--disable-blink-features=AutomationControlled` flags.

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use super::stealth;
use crate::captcha::detect::{detect_challenge, ChallengeType};
use crate::captcha::solve::{CaptchaSolution, CaptchaSolver};

// ── CDP WebSocket wrapper ────────────────────────────────────────────────

type CdpWs = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct CdpConnection {
    ws: CdpWs,
    next_id: u64,
}

impl CdpConnection {
    fn new(ws: CdpWs) -> Self { Self { ws, next_id: 1 } }

    async fn send_command(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = serde_json::json!({"id":id,"method":method,"params":params}).to_string();
        self.ws.send(Message::Text(msg)).await.map_err(|e| format!("CDP send: {e}"))?;

        loop {
            let raw = tokio::time::timeout(Duration::from_secs(30), self.ws.next())
                .await.map_err(|_| "CDP recv timeout".to_string())?
                .ok_or_else(|| "CDP connection closed".to_string())?
                .map_err(|e| format!("CDP recv: {e}"))?;

            if let Message::Text(t) = raw {
                let v: Value = serde_json::from_str(&t).map_err(|e| format!("CDP parse: {e}"))?;
                if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                    return if v.get("error").is_some() {
                        Err(format!("CDP {method}: {}", v["error"]["message"].as_str().unwrap_or("unknown")))
                    } else {
                        Ok(v.get("result").cloned().unwrap_or(Value::Null))
                    };
                }
            }
        }
    }
}

// ── Browser discovery ────────────────────────────────────────────────────

fn find_browser() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "windows")]
    {
        for path in &[
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
            r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
            r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe",
        ] {
            let p = std::path::Path::new(path);
            if p.exists() { return Some(p.to_path_buf()); }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        for name in &["google-chrome","chromium","chromium-browser","microsoft-edge","brave-browser"] {
            if let Ok(p) = which::which(name) { return Some(p); }
        }
    }
    which::which("chrome").or_else(|_| which::which("msedge")).or_else(|_| which::which("chromium")).ok()
}

/// Launch a browser on the HOST (not sandboxed). The whole purpose of the
/// browser bridge is to use the host's real browser fingerprint.
#[allow(clippy::disallowed_methods)]
async fn launch_browser(browser_path: &std::path::Path, port: u16, headless: bool) -> Result<tokio::process::Child, String> {
    let temp_dir = std::env::temp_dir().join(format!("everevo-cdp-{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir).map_err(|e| format!("create temp dir: {e}"))?;

    let mut cmd = tokio::process::Command::new(browser_path);
    cmd.arg(format!("--remote-debugging-port={port}"))
        .arg(format!("--user-data-dir={}", temp_dir.display()))
        .args(stealth::STEALTH_FLAGS)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);

    if headless { cmd.arg("--headless=new"); }
    cmd.arg("about:blank");

    cmd.spawn().map_err(|e| format!("launch browser: {e}"))
}

// ── Public API ───────────────────────────────────────────────────────────

pub struct BrowserBridge {
    _process: Option<tokio::process::Child>,
    port: u16,
}

impl BrowserBridge {
    pub async fn launch() -> Result<Self, String> {
        let browser_path = find_browser()
            .ok_or_else(|| "No Chromium browser found. Install Chrome/Edge/Brave.".to_string())?;

        let port = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default().as_nanos().hash(&mut h);
            (h.finish() % 20000 + 10000) as u16
        };

        for &headless in &[false, true] {
            let _mode = if headless { "headless" } else { "headful" };
            let process = launch_browser(&browser_path, port, headless).await?;
            let client = reqwest::Client::new();
            let version_url = format!("http://127.0.0.1:{port}/json/version");
            let mut ws_url: Option<String> = None;

            for attempt in 0..30 {
                let delay_ms = (250u64 * (1u64 << attempt.min(3))).min(2000);
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                if let Ok(resp) = client.get(&version_url).send().await {
                    if let Ok(json) = resp.json::<Value>().await {
                        if let Some(url) = json.get("webSocketDebuggerUrl").and_then(|v| v.as_str()) {
                            ws_url = Some(url.to_string());
                            break;
                        }
                    }
                }
            }

            match ws_url {
                Some(_) => {
                    return Ok(Self { _process: Some(process), port });
                }
                None if !headless => {
                    drop(process);
                }
                None => {
                    return Err(format!("CDP not available on port {port} after 20s (both modes tried)"));
                }
            }
        }
        unreachable!()
    }

    async fn page_ws_url(&self) -> Result<String, String> {
        let resp = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{}/json", self.port))
            .send().await.map_err(|e| format!("CDP list: {e}"))?;
        let pages: Vec<Value> = resp.json().await.map_err(|e| format!("CDP list parse: {e}"))?;
        pages.first()
            .and_then(|p| p.get("webSocketDebuggerUrl"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "No open pages".to_string())
    }

    async fn connect_page(&self) -> Result<CdpConnection, String> {
        let ws_url = self.page_ws_url().await?;
        let (ws, _) = connect_async(&ws_url).await.map_err(|e| format!("CDP connect: {e}"))?;
        Ok(CdpConnection::new(ws))
    }

    /// Navigate to a URL, handle CAPTCHA, and extract search results.
    pub async fn search(
        &self,
        search_url: &str,
        limit: usize,
        solver: Option<&dyn CaptchaSolver>,
    ) -> Result<Vec<crate::search::engines::SearchResult>, String> {
        let mut cdp = self.connect_page().await?;

        // Select a coherent fingerprint profile for this session
        let fp = super::fingerprint::select_fingerprint(std::process::id() as u64);

        // Enable Page + Network domains
        cdp.send_command("Page.enable", serde_json::json!({})).await?;
        cdp.send_command("Network.enable", serde_json::json!({})).await?;

        // Override User-Agent at the network level (TLS handshake sees this)
        cdp.send_command("Network.setUserAgentOverride", serde_json::json!({
            "userAgent": fp.user_agent,
            "platform": fp.platform,
            "acceptLanguage": fp.language,
        })).await?;

        // Inject stealth JS + fingerprint BEFORE any page script loads
        cdp.send_command(
            "Page.addScriptToEvaluateOnNewDocument",
            stealth::cdp_inject_stealth(),
        ).await?;
        // Inject coherent fingerprint profile (OS-consistent: Win/Mac + screen + timezone)
        let fp_js = super::fingerprint::fingerprint_injection_js(fp);
        cdp.send_command(
            "Page.addScriptToEvaluateOnNewDocument",
            serde_json::json!({"source": fp_js}),
        ).await?;

        // Navigate
        cdp.send_command("Page.navigate", serde_json::json!({"url": search_url})).await?;

        // Wait for page load
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() { return Err("Page load timeout".to_string()); }

            let raw = tokio::time::timeout(remaining, cdp.ws.next()).await
                .map_err(|_| "Page load timeout".to_string())?
                .ok_or_else(|| "CDP lost during load".to_string())?
                .map_err(|e| format!("CDP: {e}"))?;

            if let Message::Text(t) = raw {
                if t.contains("Page.loadEventFired") { break; }
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;

        // ── Check for CAPTCHA ──────────────────────────────────────
        let dom_state = cdp.send_command(
            "Runtime.evaluate",
            serde_json::json!({
                "expression": "JSON.stringify({title:document.title,html:document.documentElement.outerHTML.substring(0,5000)})",
                "returnByValue": true,
            }),
        ).await?;

        let page_json: String = dom_state["result"]["value"].as_str().unwrap_or("{}").to_string();
        let page_data: Value = serde_json::from_str(&page_json).unwrap_or(Value::Null);
        let title = page_data["title"].as_str().unwrap_or("");
        let html = page_data["html"].as_str().unwrap_or("");

        if let Some(challenge) = detect_challenge(html, title) {
            match &challenge {
                ChallengeType::Turnstile | ChallengeType::RecaptchaV3 => {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
                ChallengeType::RecaptchaV2Checkbox => {
                    // Click the reCAPTCHA iframe checkbox
                    let _ = cdp.send_command("Runtime.evaluate", serde_json::json!({
                        "expression": "document.querySelector('.g-recaptcha') ? 'found' : (document.querySelector('iframe[src*=\"recaptcha\"]') ? 'iframe' : 'none')",
                        "returnByValue": true,
                    })).await;
                }
                _ => {
                    // Try external solver if available
                    if let Some(s) = solver {
                        // Capture screenshot for vision-based solvers (CDP Page.captureScreenshot)
                        let screenshot = capture_screenshot(&mut cdp).await;
                        let result = s.solve(screenshot, &challenge, "", html).await;
                        match result {
                            CaptchaSolution::WaitAndRetry => {
                                tokio::time::sleep(Duration::from_secs(5)).await;
                            }
                            CaptchaSolution::Unsolvable(reason) => {
                                return Err(format!("CAPTCHA blocked: {reason}"));
                            }
                            _ => {}
                        }
                    } else {
                        return Err(format!("CAPTCHA detected but no solver configured: {challenge:?}"));
                    }
                }
            }
        }

        // ── Extract results ─────────────────────────────────────────
        let scraper = search_result_scraper_js(limit);
        let result = cdp.send_command("Runtime.evaluate", serde_json::json!({
            "expression": scraper,
            "returnByValue": true,
        })).await?;

        let json_str = result["result"]["value"].as_str().unwrap_or("[]");
        let items: Vec<Value> = serde_json::from_str(json_str).unwrap_or_default();

        Ok(items.into_iter().map(|v| crate::search::engines::SearchResult {
            title: v["title"].as_str().unwrap_or("").to_string(),
            url: v["url"].as_str().unwrap_or("").to_string(),
            snippet: v["snippet"].as_str().unwrap_or("").to_string(),
        }).collect())
    }

    /// Fetch a page and return its full rendered text.
    pub async fn fetch_page_text(&self, url: &str) -> Result<String, String> {
        let mut cdp = self.connect_page().await?;
        let fp = super::fingerprint::select_fingerprint(std::process::id() as u64);
        cdp.send_command("Page.enable", serde_json::json!({})).await?;
        cdp.send_command("Network.enable", serde_json::json!({})).await?;
        cdp.send_command("Network.setUserAgentOverride", serde_json::json!({
            "userAgent": fp.user_agent,
            "platform": fp.platform,
            "acceptLanguage": fp.language,
        })).await?;
        cdp.send_command("Page.addScriptToEvaluateOnNewDocument", stealth::cdp_inject_stealth()).await?;
        cdp.send_command("Page.addScriptToEvaluateOnNewDocument",
            serde_json::json!({"source": super::fingerprint::fingerprint_injection_js(fp)})).await?;
        cdp.send_command("Page.navigate", serde_json::json!({"url": url})).await?;

        // Wait for load
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() { return Err("Page load timeout".to_string()); }
            let raw = tokio::time::timeout(remaining, cdp.ws.next()).await
                .map_err(|_| "Timeout".to_string())?
                .ok_or_else(|| "CDP lost".to_string())?
                .map_err(|e| format!("CDP: {e}"))?;
            if let Message::Text(t) = raw {
                if t.contains("Page.loadEventFired") { break; }
            }
        }
        tokio::time::sleep(Duration::from_millis(300)).await;

        let result = cdp.send_command("Runtime.evaluate", serde_json::json!({
            "expression": "document.body ? document.body.innerText : document.documentElement.innerText",
            "returnByValue": true,
        })).await?;

        Ok(result["result"]["value"].as_str().unwrap_or("").to_string())
    }
}

/// Capture a full-page screenshot via CDP Page.captureScreenshot.
/// Returns PNG image bytes, or None if the capture fails.
async fn capture_screenshot(cdp: &mut CdpConnection) -> Option<Vec<u8>> {
    match cdp.send_command("Page.captureScreenshot", serde_json::json!({
        "format": "png",
        "captureBeyondViewport": true,
    })).await {
        Ok(result) => {
            let data = result["data"].as_str().unwrap_or("");
            // CDP returns base64-encoded PNG; decode to bytes
            base64_decode(data).ok()
        }
        Err(e) => {
            tracing::debug!(error = %e, "CDP screenshot capture failed");
            None
        }
    }
}

/// Simple base64 decode (no external dep).
fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let clean: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    let mut output = Vec::with_capacity(clean.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for c in clean.chars() {
        if c == '=' { break; }
        let val = TABLE.iter().position(|&x| x == c as u8)
            .ok_or_else(|| format!("Invalid base64 char: {c}"))? as u32;
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buf >> bits) as u8);
        }
    }
    Ok(output)
}

// ── JavaScript scrapers ──────────────────────────────────────────────────

fn search_result_scraper_js(limit: usize) -> String {
    format!(r#"(function(){{
  var results=[],hostname=window.location.hostname;
  if(hostname.indexOf('bing.com')!==-1){{
    var items=document.querySelectorAll('.b_algo');
    for(var i=0;i<items.length&&results.length<{limit};i++){{
      var el=items[i],a=el.querySelector('h2 a'),p=el.querySelector('p,.b_caption p,.b_lineclamp2');
      if(a&&a.href&&a.href.indexOf('http')===0){{
        var url=a.href;
        if(url.indexOf('bing.com/ck/')!==-1||url.indexOf('go.microsoft.com')!==-1||url.indexOf('bing.com/account')!==-1)continue;
        results.push({{title:(a.textContent||'').trim(),url:url,snippet:p?(p.textContent||'').trim():''}});
      }}
    }}
  }}else if(hostname.indexOf('duckduckgo.com')!==-1){{
    var links=document.querySelectorAll('a.result__a,a.result-link');
    for(var i=0;i<links.length&&results.length<{limit};i++){{
      var a=links[i];
      if(a.href&&a.href.indexOf('http')===0&&a.href.indexOf('duckduckgo.com')===-1){{
        var snippet='',parent=a.closest('.result,.result__body,tr');
        if(parent){{var s=parent.querySelector('.result__snippet,.result-snippet,td:last-child');if(s)snippet=(s.textContent||'').trim();}}
        results.push({{title:(a.textContent||'').trim(),url:a.href,snippet:snippet}});
      }}
    }}
  }}
  if(results.length===0){{
    var all=document.querySelectorAll('a[href^="http"]');
    for(var i=0;i<all.length&&results.length<{limit};i++){{
      var a=all[i];
      if(a.textContent.trim().length>10&&!a.href.includes('duckduckgo.com')&&!a.href.includes('bing.com')){{
        var p=a.closest('li,div,article,tr'),s='';
        if(p)s=(p.textContent||'').replace(a.textContent,'').trim().substring(0,200);
        results.push({{title:a.textContent.trim(),url:a.href,snippet:s}});
      }}
    }}
  }}
  return JSON.stringify(results);
}})()"#, limit=limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_browser() {
        // At minimum on a dev machine, one browser should be findable
        if let Some(p) = find_browser() {
            assert!(p.exists(), "{p:?} should exist");
        }
    }

    #[test]
    fn test_scraper_js_contains_key_selectors() {
        let js = search_result_scraper_js(5);
        assert!(js.contains("b_algo"));
        assert!(js.contains("querySelectorAll"));
        assert!(js.contains("JSON.stringify"));
    }
}
