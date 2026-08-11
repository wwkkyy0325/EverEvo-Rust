//! plugin-web-fetch — MCP server for HTTP URL fetching with HTML-to-text conversion.
//!
//! Fetching is a **typed multi-hop chain** so a blocked or anti-bot page still
//! yields content instead of a dead end:
//!
//! ```text
//! live  →  archive (Wayback CDX → raw snapshot)  →  timegate (Memento)  →  snippet
//! ```
//!
//! - `live`: direct fetch (browser UA, proxy-aware). Fast, ~15s budget.
//! - `archive`: Wayback CDX lookup for the newest HTTP-200 snapshot, then the raw
//!   snapshot via `https://web.archive.org/web/{ts}id_/{url}` (the `id_` suffix
//!   returns the original archived bytes without Wayback's injected banner).
//!   ~1 req/s rate limit honoured. Archive-first for anti-bot hosts — never a
//!   blind bypass.
//! - `timegate`: Memento TimeTravel closest-memento lookup as a secondary archive.
//! - `snippet`: terminal — all hops failed; the message tells the model to use
//!   `web_search` snippets or `curl` against the Wayback URL inside the sandbox.
//!
//! Every hop result is typed (`{hop, http_status, anti_bot}`) so the model chains
//! hops without re-prompting.

use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

/// Browser User-Agent — many sites serve 403 or a challenge page to bare or
/// low-reputation agents; a Chrome-like UA is the cheapest anti-bot mitigation.
const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// Live-fetch agent: connect/global timeouts + redirect cap so a blocked or
/// unreachable host fails fast (~15s) instead of hanging for 40+ seconds.
/// When a proxy env var is present the agent routes through it — proxy wiring
/// lives in `everevo-net`, the project's single HTTP egress.
fn agent() -> ureq::Agent {
    everevo_net::ureq_agent(
        Duration::from_secs(5),
        Duration::from_secs(15),
        5,
        Some(BROWSER_UA),
    )
}

/// Archive agent: a longer per-hop budget (~30s) because Wayback CDX through a
/// proxy is slow (measured ~22s one hop) while snapshots themselves are fast.
fn archive_agent() -> ureq::Agent {
    everevo_net::ureq_agent(
        Duration::from_secs(8),
        Duration::from_secs(30),
        5,
        Some(BROWSER_UA),
    )
}

/// Note appended to failure messages so the agent stops burning turns on
/// blocked sites (from this network Wikipedia/Google etc. are unreachable)
/// and instead answers from the search snippets or its own reasoning.
const BLOCKED_HINT: &str =
    "Do not retry this or other sites that may be blocked — use the web_search \
     snippets or your own reasoning to answer instead.";

// ── Hop chain model ────────────────────────────────────────────────────────

/// The four hops of the fetch chain, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hop {
    Live,
    Archive,
    Timegate,
    Snippet,
}

impl Hop {
    /// The hop to try after `self` fails. `Snippet` is terminal.
    fn next(self) -> Hop {
        match self {
            Hop::Live => Hop::Archive,
            Hop::Archive => Hop::Timegate,
            Hop::Timegate => Hop::Snippet,
            Hop::Snippet => Hop::Snippet,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Hop::Live => "live",
            Hop::Archive => "archive",
            Hop::Timegate => "timegate",
            Hop::Snippet => "snippet",
        }
    }
}

/// A typed per-hop failure, matching the plan's `{hop, http_status, anti_bot}`.
#[derive(Debug, Clone)]
struct HopError {
    hop: Hop,
    /// HTTP status when one was observed; `None` for transport-level failures.
    http_status: Option<u16>,
    /// True when the failure smells like anti-bot / a challenge page.
    anti_bot: bool,
    message: String,
}

impl HopError {
    fn typed(&self) -> String {
        let status = self
            .http_status
            .map(|s| s.to_string())
            .unwrap_or_else(|| "0".into());
        format!(
            "[hop={} http={} anti_bot={}] {}",
            self.hop.label(),
            status,
            self.anti_bot,
            self.message
        )
    }
}

// ── Anti-bot detection ─────────────────────────────────────────────────────

/// Status codes that reliably mean "bot detection / rate limit / access denied"
/// (Anubis, Cloudflare, Akamai challenge pages).
fn is_anti_bot_status(status: u16) -> bool {
    matches!(status, 401 | 403 | 429 | 451 | 503)
}

/// Heuristic body markers of a challenge / WAF page even when served with a 200.
fn looks_anti_bot_body(html: &str) -> bool {
    let lower = html.to_lowercase();
    [
        "just a moment",
        "checking your browser",
        "cf-chl",
        "challenge-form",
        "anubis",
        "captcha",
        "access denied",
        "unable to access",
        "please enable cookies",
        "attention required",
        "verify you are human",
    ]
    .iter()
    .any(|m| lower.contains(m))
}

// ── Hop 1: live ────────────────────────────────────────────────────────────

/// Map a ureq error to a typed HopError. Status-code errors are exact (ureq 3.3
/// returns `Error::StatusCode` for 4xx/5xx); transport errors are treated as
/// unreachable-without-anti-bot so the archive chain still runs.
fn classify_live_error(url: &str, e: &ureq::Error) -> HopError {
    let (status, anti_bot, message) = match e {
        ureq::Error::StatusCode(code) => (
            Some(*code),
            is_anti_bot_status(*code),
            format!("HTTP {code}"),
        ),
        ureq::Error::Timeout(_) => (None, false, "connection timed out".to_string()),
        ureq::Error::HostNotFound => (None, false, "host not found".to_string()),
        ureq::Error::Io(io) => (None, false, format!("io: {io}")),
        ureq::Error::BadUri(_) => (None, false, "bad URI".to_string()),
        other => (None, false, format!("{other}")),
    };
    HopError {
        hop: Hop::Live,
        http_status: status,
        anti_bot,
        message: format!("{url}: {message}"),
    }
}

/// Hop 1 — direct fetch. Returns readable text or a typed failure.
fn fetch_live(url: &str) -> Result<String, HopError> {
    let resp = agent()
        .get(url)
        .header("User-Agent", BROWSER_UA)
        .header("Accept-Language", "en-US,en;q=0.9")
        .call()
        .map_err(|e| classify_live_error(url, &e))?;
    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        return Err(HopError {
            hop: Hop::Live,
            http_status: Some(status),
            anti_bot: is_anti_bot_status(status),
            message: format!("HTTP {status}"),
        });
    }
    let html = resp.into_body().read_to_string().map_err(|e| HopError {
        hop: Hop::Live,
        http_status: Some(status),
        anti_bot: false,
        message: format!("read failed: {e}"),
    })?;
    let text = strip_html(&html);
    if text.is_empty() {
        return Err(HopError {
            hop: Hop::Live,
            http_status: Some(status),
            anti_bot: looks_anti_bot_body(&html),
            message: "no readable text (blocked or empty page)".to_string(),
        });
    }
    Ok(collapse_ws(&text))
}

// ── Hop 2: archive (Wayback CDX → raw snapshot) ────────────────────────────

/// Wayback CDX API URL for the newest HTTP-200 snapshot of `url`.
/// `output=json` → `[["urlkey","timestamp","original",...], [...row], ...]`;
/// rows come oldest-first, so the last row is the newest snapshot.
fn wayback_cdx_url(url: &str) -> String {
    format!(
        "https://web.archive.org/cdx/search/cdx?url={}&output=json&filter=statuscode:200&collapse=digest&limit=5",
        url_enc(url)
    )
}

/// Extract the newest snapshot timestamp from a CDX JSON body.
/// Returns `None` when no HTTP-200 snapshot exists.
fn newest_cdx_timestamp(body: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let rows = json.as_array()?;
    if rows.len() < 2 {
        return None;
    }
    rows[1..]
        .iter()
        .filter_map(|r| r.get(1).and_then(|t| t.as_str()).map(String::from))
        .max()
}

/// Raw Wayback snapshot URL. The `id_` suffix returns the original archived
/// bytes without Wayback's injected banner.
fn wayback_snapshot_url(ts: &str, url: &str) -> String {
    format!("https://web.archive.org/web/{ts}id_/{url}")
}

/// Rate-limit spacing between Wayback API calls (~1 req/s per IA guidance).
fn archive_rate_limit_delay() -> Duration {
    Duration::from_secs(1)
}

/// Hop 2 — find the newest archived snapshot and fetch its raw content.
fn fetch_archive(url: &str) -> Result<String, HopError> {
    let cdx_url = wayback_cdx_url(url);
    let cdx_resp = archive_agent().get(&cdx_url).call().map_err(|e| HopError {
        hop: Hop::Archive,
        http_status: None,
        anti_bot: false,
        message: format!("CDX lookup failed: {e}"),
    })?;
    let status = cdx_resp.status().as_u16();
    if !cdx_resp.status().is_success() {
        return Err(HopError {
            hop: Hop::Archive,
            http_status: Some(status),
            anti_bot: is_anti_bot_status(status),
            message: format!("CDX HTTP {status}"),
        });
    }
    let body = cdx_resp
        .into_body()
        .read_to_string()
        .map_err(|e| HopError {
            hop: Hop::Archive,
            http_status: Some(status),
            anti_bot: false,
            message: format!("CDX read failed: {e}"),
        })?;
    let Some(ts) = newest_cdx_timestamp(&body) else {
        return Err(HopError {
            hop: Hop::Archive,
            http_status: None,
            anti_bot: false,
            message: "no archived HTTP-200 snapshot".to_string(),
        });
    };

    // ~1 req/s rate limit before the snapshot fetch.
    std::thread::sleep(archive_rate_limit_delay());

    let snap_url = wayback_snapshot_url(&ts, url);
    let resp = archive_agent()
        .get(&snap_url)
        .call()
        .map_err(|e| HopError {
            hop: Hop::Archive,
            http_status: None,
            anti_bot: false,
            message: format!("snapshot fetch failed: {e}"),
        })?;
    let snap_status = resp.status().as_u16();
    if !resp.status().is_success() {
        return Err(HopError {
            hop: Hop::Archive,
            http_status: Some(snap_status),
            anti_bot: is_anti_bot_status(snap_status),
            message: format!("snapshot HTTP {snap_status}"),
        });
    }
    let html = resp.into_body().read_to_string().map_err(|e| HopError {
        hop: Hop::Archive,
        http_status: Some(snap_status),
        anti_bot: false,
        message: format!("snapshot read failed: {e}"),
    })?;
    let text = strip_html(&html);
    if text.is_empty() {
        return Err(HopError {
            hop: Hop::Archive,
            http_status: Some(snap_status),
            anti_bot: looks_anti_bot_body(&html),
            message: "archived page has no readable text".to_string(),
        });
    }
    Ok(collapse_ws(&text))
}

// ── Hop 3: timegate (Memento TimeTravel) ───────────────────────────────────

/// Memento TimeTravel JSON API URL — returns the closest known memento.
fn timegate_api_url(url: &str) -> String {
    format!("https://timetravel.mementoweb.org/api/json/{url}")
}

/// Parse the closest memento URI from a TimeTravel JSON body:
/// `{"mementos":{"closest":[{"uri":["http://..."]}]}}`.
fn closest_memento_uri(body: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    json["mementos"]["closest"]
        .as_array()?
        .first()?
        .get("uri")?
        .as_array()?
        .first()?
        .as_str()
        .map(String::from)
}

/// Hop 3 — Memento TimeTravel as a secondary archive (best-effort; this host
/// is frequently unreachable from mainland networks, so it is the first hop to
/// give up on).
fn fetch_timegate(url: &str) -> Result<String, HopError> {
    let api = timegate_api_url(url);
    let resp = archive_agent().get(&api).call().map_err(|e| HopError {
        hop: Hop::Timegate,
        http_status: None,
        anti_bot: false,
        message: format!("timegate lookup failed: {e}"),
    })?;
    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        return Err(HopError {
            hop: Hop::Timegate,
            http_status: Some(status),
            anti_bot: is_anti_bot_status(status),
            message: format!("timegate HTTP {status}"),
        });
    }
    let body = resp.into_body().read_to_string().map_err(|e| HopError {
        hop: Hop::Timegate,
        http_status: Some(status),
        anti_bot: false,
        message: format!("timegate read failed: {e}"),
    })?;
    let Some(memento_uri) = closest_memento_uri(&body) else {
        return Err(HopError {
            hop: Hop::Timegate,
            http_status: None,
            anti_bot: false,
            message: "no closest memento returned".to_string(),
        });
    };

    let resp = archive_agent()
        .get(&memento_uri)
        .call()
        .map_err(|e| HopError {
            hop: Hop::Timegate,
            http_status: None,
            anti_bot: false,
            message: format!("memento fetch failed: {e}"),
        })?;
    let m_status = resp.status().as_u16();
    if !resp.status().is_success() {
        return Err(HopError {
            hop: Hop::Timegate,
            http_status: Some(m_status),
            anti_bot: is_anti_bot_status(m_status),
            message: format!("memento HTTP {m_status}"),
        });
    }
    let html = resp.into_body().read_to_string().map_err(|e| HopError {
        hop: Hop::Timegate,
        http_status: Some(m_status),
        anti_bot: false,
        message: format!("memento read failed: {e}"),
    })?;
    let text = strip_html(&html);
    if text.is_empty() {
        return Err(HopError {
            hop: Hop::Timegate,
            http_status: Some(m_status),
            anti_bot: looks_anti_bot_body(&html),
            message: "memento page has no readable text".to_string(),
        });
    }
    Ok(collapse_ws(&text))
}

// ── Fetch entry point ──────────────────────────────────────────────────────

fn fetch_url(url: &str, max: usize) -> Result<String, String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!(
            "Invalid URL (must start with http:// or https://): {url}. {BLOCKED_HINT}"
        ));
    }

    // Walk the typed hop chain: live → archive → timegate → snippet (terminal).
    // Archive-first for anti-bot hosts — never blind-bypass. Each hop's failure
    // is recorded so the terminal hop can report the last typed error.
    let mut hop = Hop::Live;
    let mut last_err: Option<HopError> = None;
    loop {
        match hop {
            Hop::Live | Hop::Archive | Hop::Timegate => {
                let fetched = match hop {
                    Hop::Live => fetch_live(url),
                    Hop::Archive => fetch_archive(url),
                    _ => fetch_timegate(url),
                };
                match fetched {
                    Ok(text) => return Ok(hop_prefix(hop, Some(200), false, &text, max)),
                    Err(e) => {
                        eprintln!("[web_fetch] {} failed: {}", hop.label(), e.typed());
                        last_err = Some(e);
                    }
                }
            }
            Hop::Snippet => {
                // Every preceding hop stored a typed error to report.
                let last = last_err
                    .take()
                    .expect("at least one hop ran before the terminal hop");
                return Err(terminal_message(url, &last));
            }
        }
        hop = hop.next();
    }
}

/// Terminal hop message — tells the model the chain is exhausted and how to
/// proceed (search snippets, or curl against the Wayback URL inside the sandbox).
fn terminal_message(url: &str, last: &HopError) -> String {
    let status = last
        .http_status
        .map(|s| s.to_string())
        .unwrap_or_else(|| "0".into());
    format!(
        "[hop=snippet http={} anti_bot={}]\nAll fetch hops failed for {url} \
         (live → Wayback archive → Memento timegate). Last error: {}\n\
         Next options:\n\
         1) web_search for the fact — the answer is often in the snippets.\n\
         2) Run this inside the sandbox to read the archived page directly:\n\
            curl -sL --max-time 30 'https://web.archive.org/web/2/{url}'\n\
         {BLOCKED_HINT}",
        status, last.anti_bot, last.message
    )
}

/// Wrap successful content with its typed hop header and enforce the truncation
/// cap. The header is a single line so the model can parse hop provenance.
fn hop_prefix(hop: Hop, status: Option<u16>, anti_bot: bool, text: &str, max: usize) -> String {
    let status = status.map(|s| s.to_string()).unwrap_or_else(|| "0".into());
    let header = format!(
        "[hop={} http={} anti_bot={}]",
        hop.label(),
        status,
        anti_bot
    );
    let text = collapse_ws(text);
    if text.chars().count() > max {
        let truncated: String = text.chars().take(max).collect();
        format!(
            "{header}\n{truncated}…\n[truncated at {max} chars — if the fact/value you need \
             is not in the shown text, do NOT re-fetch: use the web_search tool to query for \
             that specific value instead]"
        )
    } else {
        format!("{header}\n{text}")
    }
}

// ── Text helpers ───────────────────────────────────────────────────────────

/// Crude HTML → text: drop tags, unescape a handful of entities, trim.
fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .trim()
        .to_string()
}

/// Collapse runs of whitespace into single spaces — makes HTML-stripped text
/// much more readable for the model.
fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        match c {
            '\n' | '\r' | '\t' | ' ' => {
                if !prev_space {
                    out.push(' ');
                    prev_space = true;
                }
            }
            other => {
                out.push(other);
                prev_space = false;
            }
        }
    }
    out
}

/// Minimal percent-encoding for the CDX `url=` query parameter. Keeps `/`/`:`
/// unencoded so Wayback routes the exact URL; escapes spaces and reserved
/// characters like `&`, `?`, `#`.
fn url_enc(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' | '/' | ':' => c.to_string(),
            ' ' => "%20".to_string(),
            other => format!("%{:02X}", other as u8),
        })
        .collect()
}

// ── MCP line protocol ──────────────────────────────────────────────────────

fn main() {
    let stdin = BufReader::new(std::io::stdin());
    let mut stdout = std::io::stdout();
    for line in stdin.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let req: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let _ = writeln!(
                    stdout,
                    r#"{{"jsonrpc":"2.0","error":{{"code":-32700,"message":"{}"}},"id":null}}"#,
                    e
                );
                let _ = stdout.flush();
                continue;
            }
        };
        let method = req["method"].as_str().unwrap_or("").to_string();
        let id = req["id"].clone();

        if method == "notifications/initialized" {
            continue;
        }

        let response = match method.as_str() {
            "initialize" => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2025-03-26",
                    "serverInfo": { "name": "web_fetch", "version": env!("CARGO_PKG_VERSION") },
                    "capabilities": { "tools": {} }
                }
            }),
            "tools/list" => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "tools": [
                    {
                        "name": "web_fetch",
                        "description": "Fetch a URL and return its content as text. If the live \
                         site is blocked or missing, falls back automatically to the Wayback \
                         Machine archive, then Memento. Each result starts with a \
                         [hop=... http=... anti_bot=...] header.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "url": { "type": "string" },
                                "max_chars": {
                                    "type": "integer",
                                    "description": "Max characters (default: 20000)"
                                }
                            },
                            "required": ["url"]
                        }
                    }
                ] }
            }),
            "tools/call" => {
                let args = &req["params"]["arguments"];
                let url = args["url"].as_str().unwrap_or("").to_string();
                let max = args["max_chars"].as_u64().unwrap_or(20_000) as usize;
                match fetch_url(&url, max) {
                    Ok(text) => serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "content": [{ "type": "text", "text": text }] }
                    }),
                    Err(err) => serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "content": [{ "type": "text", "text": err }], "isError": true }
                    }),
                }
            }
            "ping" => serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
            _ => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("Unknown method: {method}") }
            }),
        };
        let _ = writeln!(stdout, "{}", serde_json::to_string(&response).unwrap());
        let _ = stdout.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Hop-selection order ──
    #[test]
    fn hop_chain_order_is_live_archive_timegate_snippet() {
        assert_eq!(Hop::Live.next(), Hop::Archive);
        assert_eq!(Hop::Archive.next(), Hop::Timegate);
        assert_eq!(Hop::Timegate.next(), Hop::Snippet);
        assert_eq!(Hop::Snippet.next(), Hop::Snippet); // terminal
    }

    #[test]
    fn hop_labels_are_stable() {
        assert_eq!(Hop::Live.label(), "live");
        assert_eq!(Hop::Archive.label(), "archive");
        assert_eq!(Hop::Timegate.label(), "timegate");
        assert_eq!(Hop::Snippet.label(), "snippet");
    }

    // ── CDX URL construction ──
    #[test]
    fn wayback_cdx_url_encodes_and_filters_200() {
        let u = wayback_cdx_url("https://www.example.com/page?a=1&b=two words");
        assert!(u.starts_with("https://web.archive.org/cdx/search/cdx?url="));
        assert!(u.contains("filter=statuscode:200"));
        assert!(u.contains("collapse=digest"));
        assert!(u.contains("output=json"));
        // reserved '&' from the original query escaped; spaces percent-encoded
        assert!(u.contains("%26"));
        assert!(u.contains("%20"));
    }

    #[test]
    fn wayback_cdx_url_keeps_slashes_and_colons() {
        let u = wayback_cdx_url("https://en.wikipedia.org/wiki/Example");
        assert!(u.contains("/wiki/Example"));
        assert!(u.contains("https://en.wikipedia.org"));
    }

    #[test]
    fn newest_cdx_timestamp_picks_max_row() {
        let body = r#"[["urlkey","timestamp","original","mimetype","statuscode","digest","length"],
["com,example)/","20220101000000","http://example.com/","text/html","200","abc","100"],
["com,example)/","20230615120000","http://example.com/","text/html","200","def","200"]]"#;
        assert_eq!(
            newest_cdx_timestamp(body).as_deref(),
            Some("20230615120000")
        );
    }

    #[test]
    fn newest_cdx_timestamp_none_on_empty_or_single_row() {
        assert_eq!(newest_cdx_timestamp("[]"), None);
        assert_eq!(newest_cdx_timestamp("[[\"a\",\"b\"]]"), None);
        assert_eq!(newest_cdx_timestamp("garbage"), None);
    }

    #[test]
    fn wayback_snapshot_url_uses_id_suffix() {
        assert_eq!(
            wayback_snapshot_url("20230615120000", "https://example.com/page"),
            "https://web.archive.org/web/20230615120000id_/https://example.com/page"
        );
    }

    // ── Per-hop error typing ──
    #[test]
    fn live_error_403_is_typed_anti_bot() {
        let e = classify_live_error("https://x.com", &ureq::Error::StatusCode(403));
        assert_eq!(e.hop, Hop::Live);
        assert_eq!(e.http_status, Some(403));
        assert!(e.anti_bot);
        assert!(e.typed().contains("[hop=live http=403 anti_bot=true]"));
    }

    #[test]
    fn live_error_404_is_typed_not_anti_bot() {
        let e = classify_live_error("https://x.com", &ureq::Error::StatusCode(404));
        assert_eq!(e.http_status, Some(404));
        assert!(!e.anti_bot);
    }

    #[test]
    fn anti_bot_status_codes_are_recognized() {
        for s in [401, 403, 429, 451, 503] {
            assert!(is_anti_bot_status(s), "status {s} should be anti-bot");
        }
        for s in [200, 301, 404, 410, 500] {
            assert!(!is_anti_bot_status(s), "status {s} should not be anti-bot");
        }
    }

    #[test]
    fn challenge_bodies_are_detected() {
        assert!(looks_anti_bot_body("<title>Just a moment...</title>"));
        assert!(looks_anti_bot_body("Attention Required! | Cloudflare"));
        assert!(!looks_anti_bot_body("<h1>Welcome</h1>"));
    }

    #[test]
    fn transport_error_is_typed_not_anti_bot() {
        let e = classify_live_error(
            "https://x.com",
            &ureq::Error::Io(std::io::Error::from(std::io::ErrorKind::ConnectionRefused)),
        );
        assert_eq!(e.hop, Hop::Live);
        assert_eq!(e.http_status, None);
        assert!(!e.anti_bot);
        assert!(e.message.contains("x.com"));
    }

    // ── Rate-limit spacing ──
    #[test]
    fn archive_rate_limit_delay_is_about_one_second() {
        assert_eq!(archive_rate_limit_delay(), Duration::from_secs(1));
    }

    // ── Memento timegate ──
    #[test]
    fn closest_memento_uri_parses() {
        let body = r#"{"mementos":{"closest":[{"datetime":"2023-01-01T00:00:00Z","uri":["http://archive.example/2023/https://x.com"]}]}}"#;
        assert_eq!(
            closest_memento_uri(body).as_deref(),
            Some("http://archive.example/2023/https://x.com")
        );
    }

    #[test]
    fn closest_memento_uri_none_on_missing() {
        assert_eq!(closest_memento_uri("{}"), None);
        assert_eq!(closest_memento_uri("garbage"), None);
    }

    // ── Hop prefix / truncation ──
    #[test]
    fn hop_prefix_adds_typed_header() {
        let out = hop_prefix(Hop::Archive, Some(200), false, "hello world", 20000);
        assert!(out.starts_with("[hop=archive http=200 anti_bot=false]\nhello world"));
    }

    #[test]
    fn hop_prefix_truncates_at_max() {
        let out = hop_prefix(Hop::Live, Some(200), false, &"x".repeat(300), 100);
        assert!(out.contains("[truncated at 100 chars"));
    }
}
