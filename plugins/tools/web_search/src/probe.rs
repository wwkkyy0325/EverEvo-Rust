use crate::http::{env_proxy_url, BROWSER_UA};
use crate::research::NEWS_FEEDS;

// ── Engine reachability probe ──────────────────────────────────────────

/// Reachability state for every remote endpoint the plugin can hit, measured
/// at startup and cached. Each benchmark question spawns a fresh plugin
/// process, so the probe runs once per question (~1s); a network switch
/// mid-run is picked up on TTL expiry or when the proxy env changes, without
/// needing the agent to restart. Unreachable engines are skipped by
/// `execute_search` instead of burning 15s timeouts on a dead cascade.
#[derive(Clone, Copy)]
pub(crate) struct ProbeResult {
    pub(crate) sogou: bool,
    pub(crate) bing_rss: bool,
    pub(crate) bing_html: bool,
    pub(crate) ddg: bool,
    pub(crate) arxiv: bool,
    pub(crate) openalex: bool,
    pub(crate) crossref: bool,
    pub(crate) news: bool,
    pub(crate) semantic_scholar: bool,
    pub(crate) pubmed: bool,
}

const PROBE_TTL: std::time::Duration = std::time::Duration::from_secs(300);

static PROBE: std::sync::Mutex<Option<(ProbeResult, std::time::Instant, Option<String>)>> =
    std::sync::Mutex::new(None);

/// Cached probe, refreshed on TTL expiry or proxy-env change.
pub(crate) fn current_probe() -> ProbeResult {
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
