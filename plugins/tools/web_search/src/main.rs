//! plugin-web-search — MCP server providing web search capability.
//!
//! Communicates via JSON-RPC 2.0 over stdin/stdout (MCP stdio transport).
//! This is a standalone binary — the kernel spawns it as a subprocess.
//!
//! ## Protocol
//!
//! Each stdin line is a JSON-RPC request. Each stdout line is a JSON-RPC response.
//! stderr is used for diagnostics only (never protocol data).
//!
//! ## Supported methods
//!
//! - `initialize`  → MCP handshake (required)
//! - `tools/list`  → discover available tools
//! - `tools/call`  → execute a tool
//! - `ping`        → liveness check

use std::io::{BufRead, BufReader, Write};

// ── Tool: web_search ───────────────────────────────────────────────────

const SEARCH_TOOL_SCHEMA: &str = r#"{
    "type": "object",
    "properties": {
        "query": {
            "type": "string",
            "description": "Search query"
        },
        "max_results": {
            "type": "integer",
            "description": "Maximum number of results (default: 5)",
            "default": 5
        }
    },
    "required": ["query"]
}"#;

/// Parse query from tool call arguments.
fn parse_search_args(args: &serde_json::Value) -> Result<(String, usize), String> {
    let query = args["query"]
        .as_str()
        .ok_or("Missing 'query' parameter")?
        .to_string();
    let max_results = args["max_results"].as_u64().unwrap_or(5) as usize;
    // Clamp to reasonable bounds
    let max_results = max_results.clamp(1, 20);
    Ok((query, max_results))
}

/// Result triple: (title, url, snippet).
type Hit = (String, String, String);

// ── Engine reachability probe ──────────────────────────────────────────

/// Reachability state for every remote endpoint the plugin can hit, measured
/// at startup and cached. Each benchmark question spawns a fresh plugin
/// process, so the probe runs once per question (~1s); a network switch
/// mid-run is picked up on TTL expiry or when the proxy env changes, without
/// needing the agent to restart. Unreachable engines are skipped by
/// `execute_search` instead of burning 15s timeouts on a dead cascade.
#[derive(Clone, Copy)]
struct ProbeResult {
    sogou: bool,
    bing_rss: bool,
    bing_html: bool,
    ddg: bool,
    arxiv: bool,
    openalex: bool,
    crossref: bool,
    news: bool,
    semantic_scholar: bool,
    pubmed: bool,
}

const PROBE_TTL: std::time::Duration = std::time::Duration::from_secs(300);

static PROBE: std::sync::Mutex<Option<(ProbeResult, std::time::Instant, Option<String>)>> =
    std::sync::Mutex::new(None);

/// Cached probe, refreshed on TTL expiry or proxy-env change.
fn current_probe() -> ProbeResult {
    let proxy_now = env_proxy_url();
    let mut g = match PROBE.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(), // poisoned — proceed with fresh probe
    };
    if let Some((res, at, proxy)) = g.as_ref() {
        if at.elapsed() < PROBE_TTL && proxy == &proxy_now {
            return *res;
        }
    }
    let res = run_probe();
    *g = Some((res, std::time::Instant::now(), proxy_now));
    res
}

/// Probe every endpoint in parallel with a hard 4s cap each.
fn run_probe() -> ProbeResult {
    let h_sogou = std::thread::spawn(probe_sogou);
    let h_bing_rss = std::thread::spawn(probe_bing_rss);
    let h_bing_html = std::thread::spawn(probe_bing_html);
    let h_ddg = std::thread::spawn(probe_ddg);
    let h_arxiv = std::thread::spawn(probe_arxiv);
    let h_openalex = std::thread::spawn(probe_openalex);
    let h_crossref = std::thread::spawn(probe_crossref);
    let h_news = std::thread::spawn(probe_news);
    let h_s2 = std::thread::spawn(probe_semantic_scholar);
    let h_pubmed = std::thread::spawn(probe_pubmed);
    let res = ProbeResult {
        sogou: h_sogou.join().unwrap_or(false),
        bing_rss: h_bing_rss.join().unwrap_or(false),
        bing_html: h_bing_html.join().unwrap_or(false),
        ddg: h_ddg.join().unwrap_or(false),
        arxiv: h_arxiv.join().unwrap_or(false),
        openalex: h_openalex.join().unwrap_or(false),
        crossref: h_crossref.join().unwrap_or(false),
        news: h_news.join().unwrap_or(false),
        semantic_scholar: h_s2.join().unwrap_or(false),
        pubmed: h_pubmed.join().unwrap_or(false),
    };
    eprintln!(
        "[web_search] probe: sogou={} bing_rss={} bing_html={} ddg={} arxiv={} openalex={} crossref={} news={} semantic_scholar={} pubmed={}",
        res.sogou, res.bing_rss, res.bing_html, res.ddg, res.arxiv, res.openalex, res.crossref, res.news, res.semantic_scholar, res.pubmed
    );
    res
}

/// Short-timeout agent for probes — the main `agent()` allows 15s, far too
/// slow to probe eight endpoints with. Proxy wiring lives in `everevo-net`.
fn probe_agent() -> ureq::Agent {
    everevo_net::ureq_agent(
        std::time::Duration::from_secs(3),
        std::time::Duration::from_secs(4),
        1,
        Some(BROWSER_UA),
    )
}

fn probe_sogou() -> bool {
    probe_agent()
        .get("https://www.sogou.com/web?query=test")
        .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
        .call()
        .ok()
        .and_then(|r| r.into_body().read_to_string().ok())
        .map(|b| b.contains("vrwrap"))
        .unwrap_or(false)
}

fn probe_bing_rss() -> bool {
    probe_agent()
        .get("https://cn.bing.com/search?q=test&format=rss&ensearch=1&cc=us&mkt=en-US&count=5")
        .call()
        .ok()
        .and_then(|r| r.into_body().read_to_string().ok())
        .map(|b| b.contains("<item>"))
        .unwrap_or(false)
}

fn probe_bing_html() -> bool {
    probe_agent()
        .get("https://cn.bing.com/search?q=test&ensearch=1")
        .call()
        .ok()
        .and_then(|r| r.into_body().read_to_string().ok())
        .map(|b| b.contains("b_algo"))
        .unwrap_or(false)
}

fn probe_ddg() -> bool {
    probe_agent()
        .get("https://lite.duckduckgo.com/lite/?q=test")
        .call()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

fn probe_arxiv() -> bool {
    probe_agent()
        .get("https://export.arxiv.org/api/query?search_query=all:test&max_results=1")
        .call()
        .ok()
        .and_then(|r| r.into_body().read_to_string().ok())
        .map(|b| b.contains("<entry>"))
        .unwrap_or(false)
}

fn probe_openalex() -> bool {
    probe_agent()
        .get("https://api.openalex.org/works?search=test&per-page=1")
        .call()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

fn probe_crossref() -> bool {
    probe_agent()
        .get("https://api.crossref.org/works?query=test&rows=1")
        .call()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

fn probe_news() -> bool {
    probe_agent()
        .get(NEWS_FEEDS[0])
        .call()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

fn probe_semantic_scholar() -> bool {
    probe_agent()
        .get("https://api.semanticscholar.org/graph/v1/paper/search?query=test&limit=1")
        .call()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

fn probe_pubmed() -> bool {
    probe_agent()
        .get("https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esearch.fcgi?db=pubmed&term=test&retmax=1")
        .call()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Execute the search with a multi-engine cascade:
///
/// 1. Keyed Bing Web Search API v7 — when `EVEREVO_BING_API_KEY` is set (real
///    English index, works from mainland China, no proxy).
/// 2. SearXNG JSON API — when `EVEREVO_SEARXNG_URL` is set.
/// 3. Sogou — mainland-China reachable, surfaces rare English entities cn.bing
///    misses. Parked for a cooldown on captcha so the cascade degrades.
/// 4. Bing cn RSS (cn.bing.com + `&format=rss&ensearch=1`) — reachable from
///    mainland China without a proxy.
/// 5. Bing cn HTML `li.b_algo` — fallback if the RSS endpoint changes shape.
/// 6. DuckDuckGo Lite — final fallback for networks where it works.
///
/// Every stage is gated by the startup reachability probe (blocked engines are
/// skipped instead of burning timeouts), and every SERP passes a relevance gate
/// before it is allowed to short-circuit the cascade. All requests carry a
/// browser User-Agent and a hard timeout so a blocked endpoint fails fast
/// instead of hanging the agent for tens of seconds.
/// cn.bing.com serves the SAME fixed Microsoft-support/techcommunity SERP for
/// ANY query from some mainland IPs (observed in the GAIA run: every query →
/// the same garbage, which `hits_relevant` correctly discards). Once BOTH Bing
/// engines return empty-after-relevance-gate in one search, park Bing for the
/// rest of this plugin process so later queries skip the empty variant-retry
/// burn and the cascade lands on Sogou/DDG instead. Cleared on the first
/// relevant Bing hit, so a Bing that starts working self-heals.
static BING_GARBAGE_SESSION: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn execute_search(query: &str, max_results: usize) -> Result<String, String> {
    // Convert natural-language questions into keyword queries up front.
    // Bing's CN parser dictionary-takes-over NL questions ("what is the first
    // song on…" → "FIRST Definition") but serves the real English index for
    // keyword queries. Also helps SearXNG/DDG relevance. No-op for queries
    // that already look like keywords.
    let search_query = {
        let kw = keywordize(query);
        if kw.is_empty() {
            query.to_string()
        } else {
            kw
        }
    };

    // Distinguish "every engine errored" (network failure) from "engines
    // responded but nothing matched" (legitimate empty result). The model must
    // treat empty as a search OUTCOME and change strategy (different query,
    // web_fetch, research_search) instead of retrying a phantom failure (q46).
    let mut any_responded = false;
    let mut tried: Vec<&str> = vec![];

    // 0. Bing Web Search API v7 (keyed) — the definitive fix. Free tier
    //    (1000 req/mo) and, unlike cn.bing.com, serves the REAL English index
    //    from mainland China. Enable by setting EVEREVO_BING_API_KEY on the
    //    server (plugins inherit server env). Azure-managed, no proxy needed.
    if let Some(key) = env_key() {
        tried.push("bing_api");
        match bing_api_search(&key, &search_query, max_results) {
            Ok(hits) if !hits.is_empty() => return Ok(format_search_results(query, hits)),
            Ok(_) => any_responded = true, // empty but reachable
            Err(e) => eprintln!("[web_search] Bing API failed: {e}"),
        }
    }

    // 1. SearXNG JSON API (optional, configured via env).
    if let Some(base) = env_searxng_url() {
        tried.push("searxng");
        match searxng_search(&base, &search_query, max_results) {
            Ok(hits) if !hits.is_empty() => return Ok(format_search_results(query, hits)),
            Ok(_) => any_responded = true, // empty but reachable
            Err(e) => eprintln!("[web_search] SearXNG failed: {e}"),
        }
    }

    // Only attempt engines the startup probe found reachable — a blocked
    // engine would otherwise burn up to its 15s timeout on every search.
    let probe = current_probe();

    // 2. Sogou — reachable from mainland China and its index surfaces rare
    //    English entities (official sites, biography pages) that cn.bing's
    //    local index misses entirely (verified: "Mercedes Sosa" → the singer's
    //    official site, where cn.bing only returns Mercedes-Benz cars).
    //    A relevance gate inside sogou_search falls through to Bing when the
    //    SERP shares no significant token with the query.
    if probe.sogou && !sogou_cooldown_active() {
        tried.push("sogou");
        match sogou_search(&search_query, max_results) {
            Ok(hits) if !hits.is_empty() => return Ok(format_search_results(query, hits)),
            Ok(_) => any_responded = true, // empty but reachable
            Err(e) => eprintln!("[web_search] Sogou failed: {e}"),
        }
    }

    // 3. Bing cn RSS — primary, reachable from China. The RSS endpoint returns
    //    real URLs (no bing.com/ck/ redirects) and — with the full
    //    Accept-Language header + ensearch/cc/mkt params — English results
    //    instead of the localized dictionary/Chinese SERP the plain HTML
    //    endpoint serves from a mainland IP. Skipped while parked for the
    //    session when cn.bing serves a fixed garbage SERP for every query.
    let bing_parked = BING_GARBAGE_SESSION.load(std::sync::atomic::Ordering::Relaxed);
    let mut bing_empty = false;
    if probe.bing_rss && !bing_parked {
        tried.push("bing_rss");
        match bing_rss_search(&search_query, max_results) {
            Ok(hits) if !hits.is_empty() => {
                BING_GARBAGE_SESSION.store(false, std::sync::atomic::Ordering::Relaxed);
                return Ok(format_search_results(query, hits));
            }
            Ok(_) => {
                any_responded = true; // empty but reachable
                bing_empty = true;
            }
            Err(e) => eprintln!("[web_search] Bing RSS failed: {e}"),
        }
    }

    // 4. Bing cn HTML b_algo — fallback if the RSS endpoint changes shape.
    if probe.bing_html && !bing_parked {
        tried.push("bing_html");
        match bing_search(&search_query, max_results) {
            Ok(hits) if !hits.is_empty() => {
                BING_GARBAGE_SESSION.store(false, std::sync::atomic::Ordering::Relaxed);
                return Ok(format_search_results(query, hits));
            }
            Ok(_) => {
                any_responded = true; // empty but reachable
                bing_empty = true;
            }
            Err(e) => eprintln!("[web_search] Bing HTML failed: {e}"),
        }
    }
    if bing_empty {
        // Both Bing engines served no relevant hits for this query — on a
        // mainland IP that means the fixed-garbage-SERP condition. Park Bing
        // for the session so later queries skip the empty variant-retry burn
        // and the cascade lands on Sogou/DDG. Self-heals on any relevant hit.
        BING_GARBAGE_SESSION.store(true, std::sync::atomic::Ordering::Relaxed);
        eprintln!("[web_search] Bing served no relevant results — parked Bing for the session");
    }

    // 5. DuckDuckGo Lite (GFW-blocked from China, but works on other networks).
    let url = format!(
        "https://lite.duckduckgo.com/lite/?q={}",
        urlencoding(&search_query)
    );
    if probe.ddg {
        tried.push("ddg_lite");
        match agent().get(&url).call() {
            Ok(resp) => match read_body(resp) {
                Ok(body) => {
                    let hits = parse_ddg_lite(&body);
                    any_responded = true; // body retrieved — engine responded
                    if !hits.is_empty() {
                        return Ok(format_search_results(query, hits));
                    }
                }
                Err(e) => eprintln!("[web_search] DDG Lite body read failed: {e}"),
            },
            Err(e) => eprintln!("[web_search] DDG Lite request failed: {e}"),
        }
    }

    let tried_list = tried.join(", ");
    if any_responded {
        Ok(format!(
            "No results found for '{query}' (engines tried: {tried_list}). \
             The web is reachable but nothing matched. Try a different query, \
             web_fetch on a known URL, or research_search."
        ))
    } else {
        Err(format!(
            "All search engines failed for '{query}' (engines tried: {tried_list}). \
             Try a different query."
        ))
    }
}

// ── Engine: SearXNG JSON API ─────────────────────────────────────────────

/// SearXNG base URL from `EVEREVO_SEARXNG_URL` (e.g. `http://127.0.0.1:8080`).
fn env_searxng_url() -> Option<String> {
    std::env::var("EVEREVO_SEARXNG_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
}

/// Bing Web Search API v7 subscription key from `EVEREVO_BING_API_KEY`.
fn env_key() -> Option<String> {
    std::env::var("EVEREVO_BING_API_KEY")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Query the Bing Web Search API v7 — real English index, works from
/// mainland China without a proxy. JSON shape:
/// `{"webPages": {"value": [{"name", "url", "snippet"}]}}`.
fn bing_api_search(key: &str, query: &str, max_results: usize) -> Result<Vec<Hit>, String> {
    let url = format!(
        "https://api.bing.microsoft.com/v7.0/search?q={}&count={}&mkt=en-US&textFormat=Raw",
        urlencoding(query),
        max_results.min(20)
    );
    let resp = agent()
        .get(&url)
        .header("Ocp-Apim-Subscription-Key", key)
        .call()
        .map_err(|e| format!("Bing API request: {e}"))?;
    let body = read_body(resp)?;
    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("Bing API parse: {e}"))?;
    let mut hits = Vec::new();
    if let Some(pages) = json["webPages"]["value"].as_array() {
        for p in pages {
            if hits.len() >= max_results {
                break;
            }
            let title = p["name"].as_str().unwrap_or("").trim().to_string();
            let url = p["url"].as_str().unwrap_or("").trim().to_string();
            let snippet = p["snippet"].as_str().unwrap_or("").trim().to_string();
            if title.is_empty() && snippet.is_empty() {
                continue;
            }
            hits.push((title, url, snippet));
        }
    }
    Ok(hits)
}

/// Query SearXNG's `format=json` endpoint and extract result triples.
fn searxng_search(base: &str, query: &str, max_results: usize) -> Result<Vec<Hit>, String> {
    let url = format!(
        "{base}/search?q={}&format=json&language=en&safesearch=0",
        urlencoding(query)
    );
    let resp = agent()
        .get(&url)
        .call()
        .map_err(|e| format!("SearXNG request: {e}"))?;
    let body = read_body(resp)?;
    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("SearXNG parse: {e}"))?;

    let mut hits = Vec::new();
    if let Some(results) = json["results"].as_array() {
        for r in results {
            if hits.len() >= max_results {
                break;
            }
            let title = r["title"].as_str().unwrap_or("").trim().to_string();
            let url = r["url"].as_str().unwrap_or("").trim().to_string();
            let snippet = r["content"].as_str().unwrap_or("").trim().to_string();
            if title.is_empty() && snippet.is_empty() {
                continue;
            }
            hits.push((title, url, snippet));
        }
    }
    Ok(hits)
}

// ── Sogou anti-bot cooldown ─────────────────────────────────────────────

/// Sogou rate-limits by IP after a burst of queries (serves a ~5KB captcha
/// page instead of a SERP). The cooldown parks Sogou for a while after a
/// captcha so the cascade degrades to Bing instead of burning a captcha hit
/// on every single search. Cleared on the first successful Sogou result.
const SOGOU_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(600);
static SOGOU_DOWN_UNTIL: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

fn sogou_cooldown_active() -> bool {
    match SOGOU_DOWN_UNTIL.lock() {
        Ok(g) => g.map(|t| t > std::time::Instant::now()).unwrap_or(false),
        Err(_) => false,
    }
}

fn set_sogou_down() {
    if let Ok(mut g) = SOGOU_DOWN_UNTIL.lock() {
        *g = Some(std::time::Instant::now() + SOGOU_COOLDOWN);
    }
}

fn clear_sogou_down() {
    if let Ok(mut g) = SOGOU_DOWN_UNTIL.lock() {
        *g = None;
    }
}

// ── Engine: Sogou (www.sogou.com) ────────────────────────────────────────

/// Search Sogou and parse result triples from its SERP.
///
/// Sogou is a mainland-China-accessible engine whose index reaches English
/// content cn.bing's local index never serves (rare entities, official sites,
/// biography pages). Results live in `<div class="vrwrap">` blocks: the title
/// link inside `<h3 class="vr-title">`, the snippet in the `space-txt` div.
/// Returns an empty vec on captcha/anti-bot pages so the cascade falls through,
/// and parks Sogou in a cooldown so subsequent searches skip it until the
/// rate-limit window passes.
fn sogou_search(query: &str, max_results: usize) -> Result<Vec<Hit>, String> {
    let url = format!("https://www.sogou.com/web?query={}", urlencoding(query));
    let resp = agent()
        .get(&url)
        .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
        .call()
        .map_err(|e| format!("Sogou request: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Sogou HTTP {}", resp.status()));
    }
    let body = read_body(resp)?;
    if body.len() < 2000 || !body.contains("vrwrap") {
        eprintln!(
            "[web_search] Sogou empty/blocked: {} bytes, vrwrap={}",
            body.len(),
            body.contains("vrwrap")
        );
        set_sogou_down();
        return Ok(Vec::new()); // captcha/security-check page or empty SERP
    }
    let hits = parse_sogou_results(&body, max_results * 2);
    // Sogou mixes Chinese SEO spam with the real English pages. For an
    // English query, surface the English hits first (authoritative sources),
    // keep Chinese pages as fallback — instead of `looks_unusable`, which was
    // tuned for Bing's all-Chinese-spam failure mode and over-filters the mix.
    let hits = if query.is_ascii() {
        let mut english: Vec<Hit> = Vec::new();
        let mut cjk: Vec<Hit> = Vec::new();
        for h in hits {
            if h.0.chars().any(is_cjk) {
                cjk.push(h);
            } else {
                english.push(h);
            }
        }
        english.extend(cjk);
        english.truncate(max_results);
        english
    } else {
        hits.into_iter().take(max_results).collect()
    };

    // Relevance gate: for English queries, Sogou sometimes returns a non-empty
    // but completely irrelevant SERP (long keyword queries tokenize into
    // education-site noise). When none of the hits share a significant query
    // token, treat the result set as empty and fall through to Bing.
    if !hits_relevant(query, &hits) {
        eprintln!("[web_search] Sogou hits irrelevant — falling through to Bing");
        set_sogou_down();
        return Ok(Vec::new());
    }

    clear_sogou_down();
    eprintln!(
        "[web_search] Sogou parsed {} hits from {} bytes",
        hits.len(),
        body.len()
    );
    Ok(hits)
}

/// Extract result triples from Sogou's HTML. Handles both absolute result URLs
/// and Sogou's `/link?url=...` redirect wrappers (kept verbatim — title and
/// snippet still carry the facts even if web_fetch can't follow the JS jump).
fn parse_sogou_results(html: &str, max_results: usize) -> Vec<Hit> {
    let mut hits = Vec::new();
    let mut pos = 0;
    while pos < html.len() && hits.len() < max_results {
        let ws = match html[pos..].find("class=\"vrwrap") {
            Some(o) => pos + o,
            None => break,
        };
        let block_end = match html[ws + 6..].find("class=\"vrwrap") {
            Some(o) => ws + 6 + o,
            None => html.len(),
        };
        let block = &html[ws..block_end];
        pos = block_end;

        // Title link inside <h3 class="vr-title">.
        let h3 = match block.find("<h3") {
            Some(t) => &block[t..],
            None => continue,
        };
        let a = match h3.find("<a ") {
            Some(p) => &h3[p..],
            None => continue,
        };
        let url = first_href(a).unwrap_or_default();
        let title = link_text(a);
        if title.is_empty() || title.eq_ignore_ascii_case("here") {
            continue;
        }
        if !url.is_empty() && !url.starts_with("http") && !url.starts_with("/link?url=") {
            continue; // skip odd internal fragments
        }

        // Snippet in the `space-txt` div.
        let snippet = if let Some(s) = block.find("space-txt") {
            let snip = &block[s..];
            if let Some(gt) = snip.find('>') {
                let content = &snip[gt + 1..];
                let text = match content.find("</div>") {
                    Some(end) => &content[..end],
                    None => content,
                };
                strip_html(text).trim().to_string()
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        let snippet = truncate(&snippet, 200);

        if !hits.iter().any(|h: &Hit| h.1 == url && !url.is_empty()) {
            hits.push((title, url, snippet));
        }
    }
    hits
}

// ── Engine: Bing RSS (cn.bing.com + format=rss) ───────────────────────────

/// Search Bing China via its RSS endpoint and parse result triples.
///
/// Why RSS instead of the HTML SERP? From a mainland-China IP the plain
/// `search` page serves localized results (dictionary/Chinese sites) and links
/// every hit through `bing.com/ck/` redirects. The `format=rss` feed returns
/// real target URLs and — combined with `ensearch=1&cc=us&mkt=en-US` plus the
/// full `Accept-Language` header — English-relevant results. Format is simple
/// RSS 2.0: `<item><title>..</title><link>..</link><description>..</description>`.
fn bing_rss_search(query: &str, max_results: usize) -> Result<Vec<Hit>, String> {
    let mut hits = rss_search(query, max_results)?;
    // Bing's CN parser turns natural-language questions into dictionary
    // lookups ("what is the first song on…" → "FIRST Definition"), or serves
    // random Chinese SEO spam for English queries it has no local results for.
    // When the first SERP looks unusable, retry with stopwords stripped, which
    // empirically gets the real English index (e.g. "first song album Bleach
    // Nirvana" → nirvana.com, Britannica).
    if looks_unusable(&hits) {
        // Query-reformulation ladder (Phase 3b): keywordized → rotated →
        // quoted exact phrase of the distinctive entity. Bing's CN parser
        // dictionary-takes-over natural-language queries ("what is the first
        // song on…" → "FIRST Definition"); the quoted phrase forces an exact
        // entity match ("studio albums mercedes sosa" → the artist's
        // discography pages). First usable variant wins.
        for cand in query_variants(query) {
            if cand == query || cand.is_empty() {
                continue;
            }
            if let Ok(c_hits) = rss_search(&cand, max_results) {
                if !c_hits.is_empty() && !looks_unusable(&c_hits) {
                    eprintln!("[web_search] retried variant query: '{cand}'");
                    hits = c_hits;
                    break;
                }
            }
        }
    }
    // Relevance gate: Bing's CN index can serve a full English garbage SERP
    // (tokenizing on the most common word) that `looks_unusable` misses. Fall
    // through to the next engine when no hit shares a significant query token.
    if !hits_relevant(query, &hits) {
        eprintln!("[web_search] Bing RSS results irrelevant — falling through");
        return Ok(Vec::new());
    }
    Ok(hits)
}

/// Move the first word to the end ("a b c" → "b c a").
fn rotate_head(s: &str) -> String {
    let mut words: Vec<&str> = s.split_whitespace().collect();
    if words.len() < 2 {
        return s.to_string();
    }
    words.rotate_left(1);
    words.join(" ")
}

/// One Bing RSS request for a given query string.
fn rss_search(query: &str, max_results: usize) -> Result<Vec<Hit>, String> {
    let url = format!(
        "https://cn.bing.com/search?q={}&format=rss&ensearch=1&cc=us&mkt=en-US",
        urlencoding(query)
    );
    let resp = agent()
        .get(&url)
        .header("Accept-Language", "en-US,en;q=0.9,zh-CN;q=0.8")
        .call()
        .map_err(|e| format!("Bing RSS request: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Bing RSS HTTP {}", resp.status()));
    }
    let body = read_body(resp)?;
    if body.len() < 400 || !body.contains("<item>") {
        return Ok(Vec::new()); // challenge page or empty feed — fall through
    }
    Ok(parse_rss_items(&body, max_results))
}

/// True when a result set is dominated by dictionary/definition pages or by
/// Chinese-language pages for an English query — both signs Bing's CN parser
/// failed to find real English results.
fn looks_unusable(hits: &[Hit]) -> bool {
    if hits.is_empty() {
        return true;
    }
    let dict = hits
        .iter()
        .filter(|(t, u, _)| {
            let title = t.to_lowercase();
            let domain = u.to_lowercase();
            title.contains("definition")
                || title.contains("meaning of")
                || title.contains("dictionary")
                || domain.contains("merriam-webster")
                || domain.contains("dictionary.cambridge")
                || domain.contains("dictionary.com")
                || domain.contains("collinsdictionary")
                || domain.contains("thefreedictionary")
                || domain.contains("usdictionary")
                || domain.contains("vocabulary.com")
                || domain.contains("oxfordreference")
        })
        .count();
    let cjk = hits
        .iter()
        .filter(|(t, _, _)| t.chars().any(is_cjk))
        .count();
    dict * 2 >= hits.len() || cjk * 2 >= hits.len()
}

/// True when the hits cover at least two DISTINCT significant (>3 char)
/// tokens of the query. Detects a non-empty but completely irrelevant SERP —
/// e.g. cn.bing serving "studio apartments Shanghai" for "studio albums
/// mercedes sosa 2000 2009", which shares only the generic token "studio" —
/// so the caller falls through to the next engine instead of short-circuiting
/// on garbage. A single shared token is not enough to trust a SERP; two or
/// more distinct tokens mean the engine actually found the entities. CJK
/// queries (one long token, no spaces) and short queries are never gated.
fn hits_relevant(query: &str, hits: &[Hit]) -> bool {
    let significant: Vec<String> = query
        .split_whitespace()
        .filter(|w| w.len() > 3)
        .map(|w| w.to_lowercase())
        .collect();
    if significant.len() < 2 {
        return true; // nothing meaningful to match against — don't second-guess
    }
    let mut matched: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (t, _, s) in hits {
        let hay = format!("{} {}", t, s).to_lowercase();
        for q in &significant {
            if hay.contains(q.as_str()) {
                matched.insert(q.clone());
            }
        }
    }
    matched.len() >= 2
}

fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}'   // CJK unified ideographs
        | '\u{3400}'..='\u{4DBF}' // extension A
        | '\u{F900}'..='\u{FAFF}' // compatibility ideographs
    )
}

/// Strip common English stopwords to produce a keyword query — e.g.
/// "what is the first song on the album Bleach by Nirvana"
/// → "first song album Bleach Nirvana", and
/// "How many studio albums were published by Mercedes Sosa between 2000
/// and 2009 (included)?" → "studio albums mercedes sosa 2000 2009".
///
/// Splits every whitespace token into alphanumeric runs so en-dash ranges
/// ("2000–2009") become separate keywords instead of merging into one giant
/// number, and drops single characters plus the function/question words below.
/// Sogou treats long natural-language queries as spam (captcha page) and Bing
/// CN turns them into dictionary lookups, so the shorter the better.
fn keywordize(query: &str) -> String {
    const STOPWORDS: &[&str] = &[
        // determiners / pronouns
        "a",
        "an",
        "the",
        "this",
        "that",
        "these",
        "those",
        "i",
        "you",
        "he",
        "she",
        "it",
        "we",
        "they",
        "my",
        "your",
        "his",
        "her",
        "its",
        "our",
        "their",
        "me",
        "him",
        "us",
        "them",
        "one",
        "some",
        "any",
        "all",
        "each",
        "both",
        "other",
        "another",
        "same",
        // prepositions
        "of",
        "in",
        "on",
        "at",
        "to",
        "for",
        "with",
        "by",
        "from",
        "into",
        "onto",
        "over",
        "under",
        "about",
        "after",
        "before",
        "during",
        "since",
        "until",
        "between",
        "among",
        "through",
        "within",
        "without",
        "against",
        "along",
        "around",
        "across",
        "toward",
        "upon",
        "per",
        "via",
        "than",
        "as",
        // auxiliary / modal / copula
        "am",
        "is",
        "are",
        "was",
        "were",
        "be",
        "been",
        "being",
        "do",
        "does",
        "did",
        "doing",
        "have",
        "has",
        "had",
        "having",
        "can",
        "could",
        "will",
        "would",
        "shall",
        "should",
        "may",
        "might",
        "must",
        // question words
        "what",
        "when",
        "where",
        "why",
        "which",
        "who",
        "whom",
        "whose",
        "how",
        "many",
        "much",
        "whether",
        // coordinators / conjunctions / filler
        "and",
        "or",
        "but",
        "nor",
        "if",
        "then",
        "there",
        "here",
        "also",
        "just",
        "still",
        "even",
        "only",
        "very",
        "more",
        "most",
        "such",
        "not",
        "no",
        "yes",
        "out",
        "off",
        "up",
        "down",
        "again",
        "once",
        "etc",
        // GAIA instruction / generic-verb tails that carry no search value
        "list",
        "name",
        "find",
        "give",
        "take",
        "make",
        "use",
        "get",
        "put",
        "let",
        "call",
        "called",
        "known",
        "found",
        "made",
        "written",
        "wrote",
        "going",
        "went",
        "come",
        "came",
        "publish",
        "published",
        "publishing",
        "release",
        "released",
        "releasing",
        "record",
        "recorded",
        "records",
        "recording",
        "produce",
        "produced",
        "producing",
        "include",
        "included",
        "including",
        "respectively",
        "approximately",
        "following",
        "between",
        "inclusive",
        "average",
        "combined",
    ];
    // Split the whole string into lowercase alphanumeric runs. This turns
    // "2000–2009", "Sosa," and "(included)?" into clean tokens.
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in query.chars() {
        if ch.is_alphanumeric() {
            for low in ch.to_lowercase() {
                cur.push(low);
            }
        } else if !cur.is_empty() {
            words.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words.retain(|w| w.len() > 1 && !STOPWORDS.contains(&w.as_str()));
    words.join(" ")
}

/// Query-reformulation variants for a fact, in preference order (Phase 3b):
/// the original natural-language query, then up to three rephrasings — the
/// keywordized form, a head-rotated form (defeats Bing CN dictionary-takeover),
/// and a quoted exact phrase of the most distinctive entity run. Engines that
/// return an unusable SERP on one variant retry with the next instead of
/// blocking.
fn query_variants(query: &str) -> Vec<String> {
    let kw = keywordize(query);
    let mut out: Vec<String> = Vec::new();
    out.push(query.to_string());
    if !kw.is_empty() && kw != query {
        out.push(kw.clone());
        let rotated = rotate_head(&kw);
        if rotated != kw && rotated != query {
            out.push(rotated);
        }
    }
    if let Some(phrase) = quote_entity(&kw) {
        if !out.contains(&phrase) {
            out.push(phrase);
        }
    }
    out.truncate(4); // original + up to 3 reformulations
    out
}

/// Wrap the most distinctive run of significant words in double quotes so the
/// engine treats it as an exact phrase — e.g. `"studio albums mercedes sosa"`.
/// A leading year or generic adjective ("first", "great" — the words that make
/// Bing's CN parser dictionary-take-over) is skipped so the quoted phrase
/// captures the real entity. Empty when there is nothing distinctive.
fn quote_entity(kw: &str) -> Option<String> {
    let words: Vec<&str> = kw.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }
    let mut start = 0;
    if words.len() > 1 {
        let w0 = words[0].to_lowercase();
        if words[0].chars().all(|c| c.is_ascii_digit())
            || matches!(
                w0.as_str(),
                "first" | "great" | "best" | "top" | "list" | "name"
            )
        {
            start = 1;
        }
    }
    let phrase: Vec<&str> = words[start..].iter().take(4).copied().collect();
    if phrase.is_empty() {
        return None;
    }
    Some(format!("\"{}\"", phrase.join(" ")))
}

/// Extract result triples from an RSS 2.0 feed by splitting on `<item>` blocks.
/// Skips the channel-level `<link>` (Bing's own search URL).
fn parse_rss_items(xml: &str, max_results: usize) -> Vec<Hit> {
    let mut hits = Vec::new();
    for block in xml.split("<item>").skip(1) {
        if hits.len() >= max_results {
            break;
        }
        let title = rss_field(block, "title");
        let url = rss_field(block, "link");
        let snippet = rss_field(block, "description");
        if title.is_empty() || url.is_empty() {
            continue;
        }
        // The channel link (Bing's own search URL) appears before the items;
        // dropping a single stray self-link is harmless and cheap to guard.
        if url.contains("bing.com/search?") || url.contains("bing.com:80/search") {
            continue;
        }
        hits.push((title, url, snippet));
    }
    hits
}

/// Pull the text inside `<name>..</name>`, handling optional CDATA wrapping.
fn rss_field(block: &str, name: &str) -> String {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = match block.find(&open) {
        Some(p) => p + open.len(),
        None => return String::new(),
    };
    let end = match block[start..].find(&close) {
        Some(p) => start + p,
        None => return String::new(),
    };
    let raw = &block[start..end];
    let raw = raw.strip_prefix("<![CDATA[").unwrap_or(raw);
    let raw = raw.strip_suffix("]]>").unwrap_or(raw);
    strip_html(raw).trim().to_string()
}

// ── Engine: Bing (cn.bing.com + ensearch=1) ──────────────────────────────

/// Search Bing China with international-results flag. Returns result triples
/// parsed from `li.b_algo` blocks.
fn bing_search(query: &str, max_results: usize) -> Result<Vec<Hit>, String> {
    let url = format!(
        "https://cn.bing.com/search?q={}&ensearch=1&cc=us&mkt=en-US",
        urlencoding(query)
    );
    let resp = agent()
        .get(&url)
        .header("Accept-Language", "en-US,en;q=0.9,zh-CN;q=0.8")
        .call()
        .map_err(|e| format!("Bing request: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Bing HTTP {}", resp.status()));
    }
    let body = read_body(resp)?;
    let hits = parse_bing_results(&body, max_results);
    // Same relevance gate as the RSS arm: an English garbage SERP that passes
    // `looks_unusable` (no CJK, no dictionary pages) must still not
    // short-circuit the cascade.
    if !hits_relevant(query, &hits) {
        eprintln!("[web_search] Bing HTML results irrelevant — falling through");
        return Ok(Vec::new());
    }
    Ok(hits)
}

/// Extract result blocks from a Bing SERP. Detects challenge pages and returns
/// an empty vec so the caller falls through to the next engine.
fn parse_bing_results(html: &str, max_results: usize) -> Vec<Hit> {
    if is_bing_challenge(html) {
        return Vec::new();
    }

    let mut hits = Vec::new();
    let mut pos = 0;
    while pos < html.len() && hits.len() < max_results {
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
        let url = match first_href(block) {
            Some(h) if h.starts_with("http://") || h.starts_with("https://") => h,
            _ => continue,
        };
        if is_internal_link(&url) {
            continue;
        }
        let title = link_text(block);
        if title.is_empty() || title.eq_ignore_ascii_case("here") {
            continue;
        }
        let snippet = bing_snippet(block);
        if !hits.iter().any(|h: &Hit| h.1 == url) {
            hits.push((title, url, snippet));
        }
    }
    hits
}

/// Extract the first `href="..."` inside an HTML fragment.
fn first_href(fragment: &str) -> Option<String> {
    let href_pos = fragment.find("href=")?;
    let after = &fragment[href_pos + 5..];
    let quote = after.chars().next()?;
    let inner = &after[1..];
    let end = inner.find(quote)?;
    Some(inner[..end].replace("&amp;", "&"))
}

/// Extract the text inside the first `<a ...>...</a>` in a fragment.
fn link_text(fragment: &str) -> String {
    let a_start = match fragment.find("<a ") {
        Some(p) => p,
        None => return String::new(),
    };
    let tag_end = match fragment[a_start..].find('>') {
        Some(p) => a_start + p + 1,
        None => return String::new(),
    };
    let close = match fragment[tag_end..].find("</a>") {
        Some(p) => tag_end + p,
        None => return String::new(),
    };
    strip_html(&fragment[tag_end..close]).trim().to_string()
}

/// Extract a Bing result snippet from `<p>` or `b_caption` block.
fn bing_snippet(block: &str) -> String {
    if let Some(p_start) = block.find("<p") {
        if let Some(gt) = block[p_start..].find('>') {
            let content_start = p_start + gt + 1;
            if let Some(end) = block[content_start..].find("</p>") {
                let text = &block[content_start..content_start + end];
                let clean = strip_html(text).trim().to_string();
                if clean.len() > 10 {
                    return truncate(&clean, 200);
                }
            }
        }
    }
    if let Some(div_start) = block.find("b_caption") {
        if let Some(gt) = block[div_start..].find('>') {
            let content_start = div_start + gt + 1;
            if let Some(end) = block[content_start..].find("</div>") {
                let text = &block[content_start..content_start + end];
                let clean = strip_html(text).trim().to_string();
                if clean.len() > 10 {
                    return truncate(&clean, 200);
                }
            }
        }
    }
    String::new()
}

fn is_bing_challenge(html: &str) -> bool {
    html.contains("challenge-form")
        || html.contains("anomaly.js")
        || html.contains("Just a moment...")
        || html.contains("Checking your browser")
        || html.contains("_cf_chl_opt")
        || html.len() < 2000
}

fn is_internal_link(url: &str) -> bool {
    url.contains("bing.com/ck/")
        || url.contains("bing.com/account")
        || url.contains("go.microsoft.com")
        || url == "https://www.bing.com"
        || url == "https://cn.bing.com"
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "\u{2026}"
    }
}

// ── Shared HTTP agent & formatting ────────────────────────────────────────

/// Browser User-Agent — without it Bing treats the request as a bot and serves
/// junk/localized SEO results instead of the real English SERP.
const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// Resolve an HTTP proxy URL from env. `EVEREVO_HTTP_PROXY` is the explicit
/// override; standard `HTTPS_PROXY`/`HTTP_PROXY`/`ALL_PROXY` follow. Empty when
/// none is set — the agent then connects directly (the mainland-China default).
/// Single egress: proxy env parsing lives in `everevo-net`.
fn env_proxy_url() -> Option<String> {
    everevo_net::env_proxy_url()
}

/// Build a ureq Agent with a connect/global timeout and redirects so a blocked
/// endpoint fails fast instead of hanging (the old code used the global
/// convenience `ureq::get`, which had no timeout). When a proxy env var is
/// present the agent routes through it — proxy wiring lives in `everevo-net`.
fn agent() -> ureq::Agent {
    everevo_net::ureq_agent(
        std::time::Duration::from_secs(8),
        std::time::Duration::from_secs(15),
        3,
        Some(BROWSER_UA),
    )
}

fn read_body(resp: ureq::http::Response<ureq::Body>) -> Result<String, String> {
    resp.into_body()
        .read_to_string()
        .map_err(|e| format!("Read response: {e}"))
}

fn format_search_results(query: &str, hits: Vec<Hit>) -> String {
    let mut hits = hits;
    rank_hits(query, &mut hits);
    let formatted: Vec<String> = hits
        .iter()
        .enumerate()
        .map(|(i, (title, url, snippet))| {
            let snip: String = snippet.chars().take(300).collect();
            format!("{}. {}\n   {}\n   {}", i + 1, title, url, snip)
        })
        .collect();
    format!(
        "Web search results for '{}':\n\n{}\n\n\
         [If the results are not relevant to the question, retry web_search with \
         different keywords (include the exact entity names and years), or fetch \
         one of the result URLs above with web_fetch to read the full page, or \
         answer from what you already know.]",
        query,
        formatted.join("\n\n")
    )
}

/// Reorder hits so authoritative, non-reprint results come first (Phase 3b).
/// Down-ranks verbatim reprints of the question — HuggingFace GAIA dataset
/// viewers/mirrors only echo the query text and are useless for answering —
/// to the bottom, and up-ranks live .edu/.gov (and Wikipedia) pages the model
/// should read first. Applied after every engine so the model sees the useful
/// hits before the noise.
fn rank_hits(query: &str, hits: &mut Vec<Hit>) {
    let norm_q = fold_norm(query);
    let mut scored: Vec<(i32, usize, Hit)> = hits
        .iter()
        .cloned()
        .enumerate()
        .map(|(i, h)| {
            let (title, url, snippet) = &h;
            let url_l = url.to_lowercase();
            let mut score = 0i32;

            // Verbatim HF-dataset reprint of the question.
            let is_hf = url_l.contains("huggingface.co/datasets")
                || url_l.contains("hf.co/datasets")
                || url_l.contains("datasets-server.huggingface.co")
                || (url_l.contains("/datasets/") && url_l.contains("gaia"));
            let is_reprint = is_hf
                || (!norm_q.is_empty() && fold_norm(snippet).contains(&norm_q))
                || (!norm_q.is_empty() && fold_norm(title).contains(&norm_q));
            if is_reprint {
                score -= 10_000; // drop below every real result
            }

            let dom = domain_of(url);
            let authoritative = dom.ends_with(".edu")
                || dom.ends_with(".gov")
                || dom.ends_with(".ac.uk")
                || dom.ends_with(".mil")
                || dom == "wikipedia.org"
                || dom.ends_with(".wikipedia.org");
            if authoritative {
                score += 500;
            }

            (score, i, h)
        })
        .collect();
    // Stable reorder: highest score first, original order within a tie.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    *hits = scored.into_iter().map(|(_, _, h)| h).collect();
}

/// Bare lowercase domain of a URL (scheme and path stripped).
fn domain_of(url: &str) -> String {
    let after = url.split("://").nth(1).unwrap_or(url);
    after
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .to_lowercase()
}

/// Lowercase + fold all whitespace runs to single spaces — for reprint detection.
fn fold_norm(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Simple URL encoding (avoid adding a dependency for this).
fn urlencoding(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            ' ' => "+".to_string(),
            other => format!("%{:02X}", other as u8),
        })
        .collect()
}

/// Parse DuckDuckGo Lite HTML result page.
fn parse_ddg_lite(html: &str) -> Vec<(String, String, String)> {
    let mut results = Vec::new();
    let mut current_title = String::new();
    let mut current_snippet = String::new();
    let mut current_link = String::new();
    let mut in_result = false;
    let mut in_link = false;

    for line in html.lines() {
        let trimmed = line.trim();

        // DDG Lite: result links have class="result-link"
        if trimmed.contains("result-link") || trimmed.contains("class=\"result-snippet\"") {
            // Extract link from <a href="...">
            if let Some(href_start) = trimmed.find("href=\"") {
                let after = &trimmed[href_start + 6..];
                if let Some(href_end) = after.find('"') {
                    current_link = after[..href_end].to_string();
                    // Resolve relative URLs
                    if current_link.starts_with("//") {
                        current_link = format!("https:{current_link}");
                    }
                }
                // Title is the text content
                if let Some(close) = trimmed.find('>') {
                    let after_tag = &trimmed[close + 1..];
                    if let Some(end) = after_tag.find("</a>") {
                        current_title = strip_html(&after_tag[..end]);
                    }
                }
            }
            in_result = true;
            in_link = true;
        } else if trimmed.contains("result-snippet") && in_result {
            // Extract snippet text
            if let Some(close) = trimmed.find('>') {
                let after_tag = &trimmed[close + 1..];
                if let Some(end) = after_tag.find("</") {
                    current_snippet = strip_html(&after_tag[..end]);
                }
            }
            // End of this result
            if !current_title.is_empty() {
                results.push((
                    std::mem::take(&mut current_title),
                    std::mem::take(&mut current_snippet),
                    std::mem::take(&mut current_link),
                ));
            }
            in_result = false;
            in_link = false;
        } else if in_link && (trimmed.starts_with("class=\"") || trimmed.is_empty()) {
            // Continuation of the result block
            continue;
        } else {
            in_link = false;
        }
    }

    results
}

/// Strip HTML tags from text.
fn strip_html(s: &str) -> String {
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

// ── MCP Server ─────────────────────────────────────────────────────────

// ── Tool: arxiv_search ──────────────────────────────────────────────────

/// Search arXiv via its Atom API (export.arxiv.org — reachable from mainland
/// China without a proxy). Used when the question concerns papers/research.
/// Search arXiv via its Atom API (export.arxiv.org — reachable from mainland
/// China without a proxy). Used when the question concerns papers/research.
fn arxiv_hits(query: &str, max_results: usize) -> Result<Vec<Hit>, String> {
    let url = format!(
        "https://export.arxiv.org/api/query?search_query=all:{}&start=0&max_results={}&sortBy=relevance",
        urlencoding(query),
        max_results.min(5)
    );
    let resp = agent()
        .get(&url)
        .call()
        .map_err(|e| format!("arXiv request: {e}"))?;
    let body = read_body(resp)?;
    let mut hits = Vec::new();
    for entry in body.split("<entry>").skip(1) {
        if hits.len() >= max_results {
            break;
        }
        let title = extract_xml_tag(entry, "title").trim().replace('\n', " ");
        if title.is_empty() {
            continue;
        }
        let id = extract_xml_tag(entry, "id").trim().to_string();
        let summary = extract_xml_tag(entry, "summary").trim().replace('\n', " ");
        hits.push((title, id, summary));
    }
    Ok(hits)
}

/// Extract the text of the first `<tag>...</tag>` in an XML fragment.
fn extract_xml_tag(xml: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    if let Some(s) = xml.find(&open) {
        let start = s + open.len();
        if let Some(end_rel) = xml[start..].find(&close) {
            return xml[start..start + end_rel].to_string();
        }
    }
    String::new()
}

// ── Tool: academic_search ───────────────────────────────────────────────

/// Search academic works via OpenAlex (fall back to Crossref). Returns paper
/// titles, years, DOIs, and reconstructed abstracts — authoritative for
/// scientific facts and reachable from mainland China without a proxy.
fn openalex_hits(query: &str, max_results: usize) -> Result<Vec<Hit>, String> {
    let url = format!(
        "https://api.openalex.org/works?search={}&per-page={}",
        urlencoding(query),
        max_results.min(5)
    );
    let resp = agent()
        .get(&url)
        .header("Accept", "application/json")
        .call()
        .map_err(|e| format!("OpenAlex request: {e}"))?;
    let body = read_body(resp)?;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
    let mut hits = Vec::new();
    if let Some(results) = json["results"].as_array() {
        for r in results {
            if hits.len() >= max_results {
                break;
            }
            let title = r["display_name"].as_str().unwrap_or("").trim().to_string();
            if title.is_empty() {
                continue;
            }
            let year = r["publication_year"].as_i64().unwrap_or(0);
            let doi = r["doi"].as_str().unwrap_or("").trim();
            let abstract_text = openalex_abstract(r.get("abstract_inverted_index"));
            let snippet = if abstract_text.is_empty() {
                year.to_string()
            } else {
                format!("{year}: {}", truncate(&abstract_text, 220))
            };
            let url = if doi.is_empty() {
                String::new()
            } else {
                format!("https://doi.org/{doi}")
            };
            hits.push((title, url, snippet));
        }
    }
    if hits.is_empty() {
        return crossref_hits(query, max_results);
    }
    Ok(hits)
}

/// Reconstruct an abstract from OpenAlex's inverted-index representation.
fn openalex_abstract(inv: Option<&serde_json::Value>) -> String {
    let Some(obj) = inv.and_then(|v| v.as_object()) else {
        return String::new();
    };
    let mut pairs: Vec<(i32, &str)> = Vec::new();
    for (word, idxs) in obj {
        if let Some(arr) = idxs.as_array() {
            for i in arr {
                if let Some(pos) = i.as_i64() {
                    pairs.push((pos as i32, word.as_str()));
                }
            }
        }
    }
    pairs.sort_by_key(|(p, _)| *p);
    pairs.iter().map(|(_, w)| *w).collect::<Vec<_>>().join(" ")
}

/// Crossref fallback for OpenAlex (cleaner for humanities/interdisciplinary
/// queries OpenAlex misses).
fn crossref_hits(query: &str, max_results: usize) -> Result<Vec<Hit>, String> {
    let url = format!(
        "https://api.crossref.org/works?query={}&rows={}&select=title,DOI,container-title,issued",
        urlencoding(query),
        max_results.min(5)
    );
    let resp = agent()
        .get(&url)
        .call()
        .map_err(|e| format!("Crossref request: {e}"))?;
    let body = read_body(resp)?;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
    let mut hits = Vec::new();
    if let Some(items) = json["message"]["items"].as_array() {
        for it in items {
            if hits.len() >= max_results {
                break;
            }
            let title = it["title"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if title.is_empty() {
                continue;
            }
            let doi = it["DOI"].as_str().unwrap_or("").trim();
            let journal = it["container-title"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .trim();
            let year = it["issued"]["date-parts"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(|d| d.as_array())
                .and_then(|d| d.first())
                .and_then(|y| y.as_i64())
                .unwrap_or(0);
            let snippet = if journal.is_empty() {
                year.to_string()
            } else {
                format!("{journal}, {year}")
            };
            let url = if doi.is_empty() {
                String::new()
            } else {
                format!("https://doi.org/{doi}")
            };
            hits.push((title, url, snippet));
        }
    }
    Ok(hits)
}

// ── Tool: news_search ───────────────────────────────────────────────────

/// English RSS feeds reachable from mainland China without a proxy
/// (BBC/CNN/Reuters/Guardian feeds are GFW-blocked; Sky News + China Daily
/// work). Fetched and keyword-filtered by news_search.
const NEWS_FEEDS: &[&str] = &[
    "https://feeds.skynews.com/feeds/rss/world.xml",
    "http://www.chinadaily.com.cn/rss/china_rss.xml",
    "http://www.chinadaily.com.cn/rss/world_rss.xml",
];

/// Search recent news by keyword-filtering the reachable English feeds.
fn news_hits(query: &str, max_results: usize) -> Result<Vec<Hit>, String> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| w.len() > 2)
        .collect();
    if terms.is_empty() {
        return Err("news search needs a query with real keywords.".into());
    }

    let mut matched: Vec<Hit> = Vec::new();
    for feed in NEWS_FEEDS {
        let resp = match agent().get(*feed).call() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[news_search] feed {feed} failed: {e}");
                continue;
            }
        };
        let body = match read_body(resp) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[news_search] feed {feed} read failed: {e}");
                continue;
            }
        };
        for hit in parse_rss_items(&body, 50) {
            let text = format!("{} {}", hit.0, hit.2).to_lowercase();
            if terms.iter().any(|t| text.contains(t)) {
                matched.push(hit);
            }
        }
    }
    matched.truncate(max_results);
    Ok(matched)
}

// ── Tool: research_search (merged academic + news, source registry) ────

/// Semantic Scholar Graph API — free, no key. Returns paper title/url/abstract.
fn semantic_scholar_hits(query: &str, max_results: usize) -> Result<Vec<Hit>, String> {
    let url = format!(
        "https://api.semanticscholar.org/graph/v1/paper/search?query={}&limit={}&fields=title,year,abstract,url",
        urlencoding(query),
        max_results.min(5)
    );
    let resp = agent()
        .get(&url)
        .header("Accept", "application/json")
        .call()
        .map_err(|e| format!("Semantic Scholar request: {e}"))?;
    let body = read_body(resp)?;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
    let mut hits = Vec::new();
    if let Some(results) = json["data"].as_array() {
        for r in results {
            if hits.len() >= max_results {
                break;
            }
            let title = r["title"].as_str().unwrap_or("").trim().to_string();
            if title.is_empty() {
                continue;
            }
            let url = r["url"].as_str().unwrap_or("").trim().to_string();
            let year = r["year"].as_i64().unwrap_or(0);
            let abstract_text = r["abstract"].as_str().unwrap_or("").trim();
            let snippet = if abstract_text.is_empty() {
                year.to_string()
            } else {
                format!("{year}: {}", truncate(abstract_text, 220))
            };
            hits.push((title, url, snippet));
        }
    }
    Ok(hits)
}

/// PubMed E-utilities (esearch → esummary). Free, no key; authoritative for
/// biomedical/clinical facts.
fn pubmed_hits(query: &str, max_results: usize) -> Result<Vec<Hit>, String> {
    let url = format!(
        "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esearch.fcgi?db=pubmed&term={}&retmax={}&sort=relevance",
        urlencoding(query),
        max_results.min(5)
    );
    let resp = agent()
        .get(&url)
        .call()
        .map_err(|e| format!("PubMed esearch: {e}"))?;
    let body = read_body(resp)?;
    let ids: Vec<String> = body
        .split("<Id>")
        .skip(1)
        .filter_map(|s| {
            s.split("</Id>")
                .next()
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(String::from)
        })
        .collect();
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let url2 = format!(
        "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esummary.fcgi?db=pubmed&id={}&retmode=json",
        ids.join(",")
    );
    let resp2 = agent()
        .get(&url2)
        .call()
        .map_err(|e| format!("PubMed esummary: {e}"))?;
    let body2 = read_body(resp2)?;
    let json: serde_json::Value = serde_json::from_str(&body2).unwrap_or(serde_json::Value::Null);
    let mut hits = Vec::new();
    for id in &ids {
        if hits.len() >= max_results {
            break;
        }
        let rec = &json["result"][id];
        let title = rec["title"].as_str().unwrap_or("").trim().to_string();
        if title.is_empty() {
            continue;
        }
        let url = format!("https://pubmed.ncbi.nlm.nih.gov/{id}/");
        let pubdate = rec["pubdate"].as_str().unwrap_or("").trim();
        let source = rec["source"].as_str().unwrap_or("").trim();
        let snippet = if source.is_empty() {
            pubdate.to_string()
        } else {
            format!("{source}, {pubdate}")
        };
        hits.push((title, url, snippet));
    }
    Ok(hits)
}

/// A merged, source-tagged hit returned by `research_search`.
struct SearchHit {
    source: &'static str,
    title: String,
    url: String,
    snippet: String,
}

/// A single searchable source in the research registry.
///
/// ADDING A NEW SOURCE = write a `*_hits` fn returning `Vec<Hit>` and one
/// registry entry below (key, label, availability gate, run wrapper). Nothing
/// else changes — `tools/list` and `research_search` both iterate the registry.
struct SearchSource {
    key: &'static str,
    label: &'static str,
    available: fn(&ProbeResult) -> bool,
    run: fn(&str, usize) -> Result<Vec<SearchHit>, String>,
}

fn avail_arxiv(p: &ProbeResult) -> bool {
    p.arxiv
}
fn avail_openalex(p: &ProbeResult) -> bool {
    p.openalex
}
fn avail_crossref(p: &ProbeResult) -> bool {
    p.crossref
}
fn avail_s2(p: &ProbeResult) -> bool {
    p.semantic_scholar
}
fn avail_pubmed(p: &ProbeResult) -> bool {
    p.pubmed
}
fn avail_news(p: &ProbeResult) -> bool {
    p.news
}

fn run_arxiv(q: &str, n: usize) -> Result<Vec<SearchHit>, String> {
    Ok(arxiv_hits(q, n)?
        .into_iter()
        .map(|(t, u, s)| SearchHit {
            source: "arXiv",
            title: t,
            url: u,
            snippet: s,
        })
        .collect())
}
fn run_openalex(q: &str, n: usize) -> Result<Vec<SearchHit>, String> {
    Ok(openalex_hits(q, n)?
        .into_iter()
        .map(|(t, u, s)| SearchHit {
            source: "OpenAlex",
            title: t,
            url: u,
            snippet: s,
        })
        .collect())
}
fn run_crossref(q: &str, n: usize) -> Result<Vec<SearchHit>, String> {
    Ok(crossref_hits(q, n)?
        .into_iter()
        .map(|(t, u, s)| SearchHit {
            source: "Crossref",
            title: t,
            url: u,
            snippet: s,
        })
        .collect())
}
fn run_s2(q: &str, n: usize) -> Result<Vec<SearchHit>, String> {
    Ok(semantic_scholar_hits(q, n)?
        .into_iter()
        .map(|(t, u, s)| SearchHit {
            source: "SemanticScholar",
            title: t,
            url: u,
            snippet: s,
        })
        .collect())
}
fn run_pubmed(q: &str, n: usize) -> Result<Vec<SearchHit>, String> {
    Ok(pubmed_hits(q, n)?
        .into_iter()
        .map(|(t, u, s)| SearchHit {
            source: "PubMed",
            title: t,
            url: u,
            snippet: s,
        })
        .collect())
}
fn run_news(q: &str, n: usize) -> Result<Vec<SearchHit>, String> {
    Ok(news_hits(q, n)?
        .into_iter()
        .map(|(t, u, s)| SearchHit {
            source: "News",
            title: t,
            url: u,
            snippet: s,
        })
        .collect())
}

/// The source registry — `tools/list` and `research_search` both iterate this.
/// Adding a source: one `SearchSource` entry here (plus its `*_hits`/run fns).
static RESEARCH_SOURCES: &[SearchSource] = &[
    SearchSource {
        key: "arxiv",
        label: "arXiv",
        available: avail_arxiv,
        run: run_arxiv,
    },
    SearchSource {
        key: "openalex",
        label: "OpenAlex",
        available: avail_openalex,
        run: run_openalex,
    },
    SearchSource {
        key: "crossref",
        label: "Crossref",
        available: avail_crossref,
        run: run_crossref,
    },
    SearchSource {
        key: "semantic_scholar",
        label: "SemanticScholar",
        available: avail_s2,
        run: run_s2,
    },
    SearchSource {
        key: "pubmed",
        label: "PubMed",
        available: avail_pubmed,
        run: run_pubmed,
    },
    SearchSource {
        key: "news",
        label: "News",
        available: avail_news,
        run: run_news,
    },
];

/// Deterministic per-question source routing ("具体问题具体分析"): pick source
/// priority by `kind` hint or query keywords. General web search is NOT part of
/// research_search — use the native `web_search` / `web_search_local` for that.
fn route_sources(kind: &str, query: &str) -> Vec<&'static str> {
    let q = query.to_lowercase();
    let paperish = [
        "arxiv",
        "paper",
        "preprint",
        "conference",
        "publication",
        "theorem",
        "abstract",
        "journal",
        "author",
    ]
    .iter()
    .any(|k| q.contains(k));
    let newsish = [
        "news",
        "recent",
        "today",
        "yesterday",
        "this week",
        "breaking",
        "reported",
        "announced",
        "ago",
        "latest",
    ]
    .iter()
    .any(|k| q.contains(k));
    let biomedish = [
        "clinical",
        "patient",
        "disease",
        "drug",
        "treatment",
        "therapy",
        "trial",
        "cancer",
        "study",
    ]
    .iter()
    .any(|k| q.contains(k));
    match kind {
        "news" => vec![
            "news",
            "arxiv",
            "openalex",
            "crossref",
            "semantic_scholar",
            "pubmed",
        ],
        "papers" => vec![
            "arxiv",
            "openalex",
            "semantic_scholar",
            "crossref",
            "pubmed",
            "news",
        ],
        _ => {
            if newsish && !paperish {
                vec![
                    "news",
                    "arxiv",
                    "openalex",
                    "crossref",
                    "semantic_scholar",
                    "pubmed",
                ]
            } else if biomedish {
                vec![
                    "pubmed",
                    "openalex",
                    "crossref",
                    "arxiv",
                    "semantic_scholar",
                    "news",
                ]
            } else {
                vec![
                    "arxiv",
                    "openalex",
                    "crossref",
                    "semantic_scholar",
                    "pubmed",
                    "news",
                ]
            }
        }
    }
}

const RESEARCH_TOOL_SCHEMA: &str = r#"{
    "type": "object",
    "properties": {
        "query": {
            "type": "string",
            "description": "Search query"
        },
        "max_results": {
            "type": "integer",
            "description": "Maximum number of merged results (default: 8)",
            "default": 8
        },
        "kind": {
            "type": "string",
            "enum": ["auto", "papers", "news"],
            "description": "Source priority hint (default: auto — routed by query keywords)"
        }
    },
    "required": ["query"]
}"#;

/// Parse research_search arguments: (query, max_results, kind).
fn parse_research_args(args: &serde_json::Value) -> Result<(String, usize, String), String> {
    let query = args["query"]
        .as_str()
        .ok_or("Missing 'query' parameter")?
        .to_string();
    let max_results = args["max_results"].as_u64().unwrap_or(8) as usize;
    let max_results = max_results.clamp(1, 15);
    let kind = args["kind"].as_str().unwrap_or("auto").to_string();
    Ok((query, max_results, kind))
}

/// Merged academic + news search across the source registry: routes by query,
/// dedups, caps per-source and total, tags each hit with its source.
fn research_search_tool(query: &str, max_results: usize, kind: &str) -> Result<String, String> {
    let probe = current_probe();
    let mut merged: Vec<SearchHit> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let per_source = 2usize.max(max_results / 3);
    for key in route_sources(kind, query) {
        let Some(src) = RESEARCH_SOURCES.iter().find(|s| s.key == key) else {
            continue;
        };
        if !(src.available)(&probe) {
            continue;
        }
        match (src.run)(query, per_source) {
            Ok(hits) => {
                for h in hits {
                    let dedup_key: String = h
                        .title
                        .to_lowercase()
                        .chars()
                        .filter(|c| c.is_alphanumeric())
                        .collect();
                    if dedup_key.is_empty() || seen.contains(&dedup_key) {
                        continue;
                    }
                    seen.insert(dedup_key);
                    merged.push(h);
                    if merged.len() >= max_results {
                        break;
                    }
                }
            }
            Err(e) => eprintln!("[research_search] {} failed: {e}", src.label),
        }
        if merged.len() >= max_results {
            break;
        }
    }
    if merged.is_empty() {
        return Ok(format!(
            "No research results found for '{query}'. Use web_search for general web results."
        ));
    }
    let mut out = format!("Research results for '{query}':\n\n");
    for (i, h) in merged.iter().enumerate() {
        let url = if h.url.is_empty() {
            String::new()
        } else {
            format!("   {}\n", h.url)
        };
        out.push_str(&format!(
            "{}. [{}] {}\n{}{}\n\n",
            i + 1,
            h.source,
            h.title,
            url,
            truncate(&h.snippet, 200)
        ));
    }
    Ok(out)
}

fn main() {
    let stdin = BufReader::new(std::io::stdin());
    let mut stdout = std::io::stdout();

    for line in stdin.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break, // stdin closed → exit
        };

        if line.trim().is_empty() {
            continue;
        }

        let req: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let err = serde_json::json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32700, "message": format!("Parse error: {e}")},
                    "id": null
                });
                let _ = writeln!(stdout, "{}", serde_json::to_string(&err).unwrap());
                let _ = stdout.flush();
                continue;
            }
        };

        let method = req["method"].as_str().unwrap_or("");
        let id = req["id"].clone();

        let response = match method {
            "initialize" => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2025-03-26",
                    "serverInfo": {
                        "name": "web_search",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "capabilities": {
                        "tools": {}
                    }
                }
            }),

            "notifications/initialized" => {
                // No response needed for notifications
                continue;
            }

            "tools/list" => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": [
                        {
                            "name": "web_search_local",
                            "description": "Local web search fallback (Sogou → Bing → DuckDuckGo/Google). Use ONLY when web_search (the server-side search executed by the API) failed, returned nothing useful, or you need raw engine results to verify. IMPORTANT: use SHORT KEYWORD queries, not full sentences — e.g. 'Mercedes Sosa studio albums list' instead of 'How many studio albums did Mercedes Sosa publish?'. Returns numbered results with title, URL, and snippet.",
                            "inputSchema": serde_json::from_str::<serde_json::Value>(SEARCH_TOOL_SCHEMA).unwrap()
                        },
                        {
                            "name": "research_search",
                            "description": "Merged academic + news search (arXiv, OpenAlex, Crossref, Semantic Scholar, PubMed, news feeds). Use for scientific papers, biomedical/clinical facts, statistics with citable sources, or recent news events. NOT for general web lookups — use web_search for that. Auto-routes source priority by query; pass 'kind' ('papers' | 'news') to bias routing. Returns deduplicated results tagged with their source.",
                            "inputSchema": serde_json::from_str::<serde_json::Value>(RESEARCH_TOOL_SCHEMA).unwrap()
                        }
                    ]
                }
            }),

            "tools/call" => {
                let params = &req["params"];
                let tool_name = params["name"].as_str().unwrap_or("");
                let arguments = &params["arguments"];

                let result = match tool_name {
                    "web_search_local" => match parse_search_args(arguments) {
                        Ok((query, max_results)) => execute_search(&query, max_results),
                        Err(e) => Err(e),
                    },
                    "research_search" => match parse_research_args(arguments) {
                        Ok((query, max_results, kind)) => {
                            research_search_tool(&query, max_results, &kind)
                        }
                        Err(e) => Err(e),
                    },
                    other => Err(format!("Unknown tool: {other}")),
                };

                match result {
                    Ok(content) => serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{"type": "text", "text": content}]
                        }
                    }),
                    Err(e) => serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{"type": "text", "text": e}],
                            "isError": true
                        }
                    }),
                }
            }

            "ping" => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {}
            }),

            _ => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("Method not found: {method}")
                }
            }),
        };

        let _ = writeln!(stdout, "{}", serde_json::to_string(&response).unwrap());
        let _ = stdout.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Query reformulation (Phase 3b) ──
    #[test]
    fn query_variants_include_keywordized_and_quoted() {
        let q = "How many studio albums were published by Mercedes Sosa between 2000 and 2009?";
        let vs = query_variants(q);
        assert_eq!(vs[0], q); // original first
        assert!(vs.contains(&"studio albums mercedes sosa 2000 2009".to_string()));
        assert!(vs.iter().any(|v| v.contains('"'))); // quoted variant present
        assert!(vs.len() <= 4); // original + up to 3 reformulations
    }

    #[test]
    fn query_variants_on_keyword_query_still_offers_quoted() {
        // Already-keywordized (lowercase) query: keywordize is a no-op, so the
        // quoted exact-phrase variant is the only reformulation.
        let q = "mercedes sosa studio albums 2000 2009";
        let vs = query_variants(q);
        assert_eq!(vs[0], q);
        assert!(vs.iter().any(|v| v.contains('"') && v.contains("mercedes")));
    }

    #[test]
    fn quote_entity_skips_leading_year_and_generic_word() {
        assert_eq!(
            quote_entity("2000 studio albums mercedes sosa").as_deref(),
            Some("\"studio albums mercedes sosa\"")
        );
        assert_eq!(
            quote_entity("studio albums mercedes sosa 2000 2009").as_deref(),
            Some("\"studio albums mercedes sosa\"")
        );
        assert_eq!(
            quote_entity("first song album bleach nirvana").as_deref(),
            Some("\"song album bleach nirvana\"")
        );
        assert_eq!(quote_entity("").as_deref(), None);
    }

    // ── Result ranking (Phase 3b) ──
    #[test]
    fn rank_hits_downranks_hf_reprint_and_ups_edu_gov() {
        let q = "How many studio albums were published by Mercedes Sosa between 2000 and 2009?";
        let mut hits: Vec<Hit> = vec![
            (
                "hf viewer".into(),
                "https://huggingface.co/datasets/gaia-benchmark/GAIA/viewer".into(),
                format!("{q} — the exact question text echoed back."),
            ),
            (
                "nasa page".into(),
                "https://www.nasa.gov/news/".into(),
                "Mercedes Sosa discography".into(),
            ),
            (
                "mit page".into(),
                "https://music.mit.edu/artist".into(),
                "Sosa albums 2000-2009".into(),
            ),
            (
                "normal blog".into(),
                "https://example.com/foo".into(),
                "Sosa recorded albums".into(),
            ),
        ];
        rank_hits(q, &mut hits);
        // Both authoritative hits (+500) keep engine order: nasa (idx 1) before
        // mit (idx 2); the normal blog (0) follows; the HF reprint sinks last.
        assert_eq!(hits[0].1, "https://www.nasa.gov/news/");
        assert_eq!(hits[1].1, "https://music.mit.edu/artist");
        assert_eq!(hits[2].1, "https://example.com/foo");
        assert_eq!(
            hits[3].1,
            "https://huggingface.co/datasets/gaia-benchmark/GAIA/viewer" // reprint last
        );
    }

    #[test]
    fn rank_hits_detects_verbatim_snippet_reprint_even_on_non_hf_url() {
        let q = "What was the first song on the album Bleach by Nirvana?";
        let mut hits: Vec<Hit> = vec![
            (
                "mirror".into(),
                "https://example.com/mirror".into(),
                q.to_string(), // verbatim question in the snippet
            ),
            (
                "real source".into(),
                "https://example.com/real".into(),
                "Bleach opened with Blew".into(),
            ),
        ];
        rank_hits(q, &mut hits);
        assert_eq!(hits[0].1, "https://example.com/real");
        assert_eq!(hits[1].1, "https://example.com/mirror");
    }

    // ── Helpers ──
    #[test]
    fn domain_of_strips_scheme_and_path() {
        assert_eq!(domain_of("https://www.nasa.gov/news/"), "www.nasa.gov");
        assert_eq!(
            domain_of("http://example.com:8080/a?b=1"),
            "example.com:8080"
        );
        assert_eq!(domain_of("example.edu"), "example.edu");
        assert_eq!(domain_of(""), "");
    }

    #[test]
    fn fold_norm_folds_whitespace_and_lowercases() {
        assert_eq!(
            fold_norm("  Mercedes   Sosa \n\t 2000 "),
            "mercedes sosa 2000"
        );
    }

    #[test]
    fn keywordize_splits_dash_ranges_and_drops_stopwords() {
        assert_eq!(
            keywordize("How many studio albums between 2000 and 2009 (included)?"),
            "studio albums 2000 2009"
        );
        assert_eq!(keywordize("Mercedes Sosa"), "mercedes sosa");
    }
}
