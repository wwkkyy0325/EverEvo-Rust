use crate::Hit;

/// Move the first word to the end ("a b c" → "b c a").
fn rotate_head(s: &str) -> String {
    let mut words: Vec<&str> = s.split_whitespace().collect();
    if words.len() < 2 {
        return s.to_string();
    }
    words.rotate_left(1);
    words.join(" ")
}

/// True when a result set is dominated by dictionary/definition pages or by
/// Chinese-language pages for an English query — both signs Bing's CN parser
/// failed to find real English results.
pub(crate) fn looks_unusable(hits: &[Hit]) -> bool {
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
pub(crate) fn hits_relevant(query: &str, hits: &[Hit]) -> bool {
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

pub(crate) fn is_cjk(c: char) -> bool {
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
pub(crate) fn keywordize(query: &str) -> String {
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
pub(crate) fn query_variants(query: &str) -> Vec<String> {
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

/// Reorder hits so authoritative, non-reprint results come first (Phase 3b).
/// Down-ranks verbatim reprints of the question — HuggingFace GAIA dataset
/// viewers/mirrors only echo the query text and are useless for answering —
/// to the bottom, and up-ranks live .edu/.gov (and Wikipedia) pages the model
/// should read first. Applied after every engine so the model sees the useful
/// hits before the noise.
pub(crate) fn rank_hits(query: &str, hits: &mut Vec<Hit>) {
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

#[cfg(test)]
mod tests {
    use super::{domain_of, fold_norm, keywordize, query_variants, quote_entity, rank_hits};
    use crate::Hit;

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
