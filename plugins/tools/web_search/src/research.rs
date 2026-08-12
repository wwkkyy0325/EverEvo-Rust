use crate::http::{agent, read_body, truncate, urlencoding, Hit};
use crate::probe::{current_probe, ProbeResult};
use crate::web::parse_rss_items;

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
pub(crate) const NEWS_FEEDS: &[&str] = &[
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

/// Merged academic + news search across the source registry: routes by query,
/// dedups, caps per-source and total, tags each hit with its source.
pub(crate) fn research_search_tool(
    query: &str,
    max_results: usize,
    kind: &str,
) -> Result<String, String> {
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
