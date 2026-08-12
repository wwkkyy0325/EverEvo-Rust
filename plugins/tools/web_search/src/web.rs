use crate::http::{agent, read_body, strip_html, truncate, urlencoding, Hit};
use crate::probe::current_probe;
use crate::quality::{
    hits_relevant, is_cjk, keywordize, looks_unusable, query_variants, rank_hits,
};

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

pub(crate) fn execute_search(query: &str, max_results: usize) -> Result<String, String> {
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

/// Extract result triples from an RSS 2.0 feed by splitting on `<item>` blocks.
/// Skips the channel-level `<link>` (Bing's own search URL).
pub(crate) fn parse_rss_items(xml: &str, max_results: usize) -> Vec<Hit> {
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
