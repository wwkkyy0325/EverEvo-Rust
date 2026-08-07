//! HTML parsing utilities — extract structured search results from raw HTML.
//!
//! Supports both Bing `b_algo` blocks and generic `<a href>` scanning for DDG.
//! All functions in this module are stateless and testable in isolation.

/// Find the next result-like link in HTML, returning (url, title, next_pos).
/// Looks for patterns like:
///   <a ... href="http(s)://..." ... >Title</a>
///   <a ... href="//example.com" ... >Title</a>
pub(crate) fn find_result_link(chars: &[char], start: usize) -> Option<(String, String, usize)> {
    let n = chars.len();
    let mut pos = start;

    while pos < n {
        let tag_start = substr_pos(chars, pos, "<a ")? + pos;
        let tag_body_end = substr_pos(chars, tag_start, ">")? + tag_start + 1;

        let tag_body: String = chars[tag_start + 3..tag_body_end].iter().collect();
        let href = extract_href(&tag_body);

        let href = match href {
            Some(h)
                if h.starts_with("http://") || h.starts_with("https://") || h.starts_with("//") =>
            {
                h
            }
            _ => {
                pos = tag_body_end;
                continue;
            }
        };

        let close_tag = substr_pos(chars, tag_body_end, "</a>")? + tag_body_end;
        let title: String = chars[tag_body_end..close_tag].iter().collect();
        let title = strip_html_tags(&title).trim().to_string();

        if title.is_empty() || href.starts_with("javascript:") {
            pos = close_tag + 4;
            continue;
        }

        return Some((href, title, close_tag + 4));
    }

    None
}

/// Extract snippet text following a result link.
pub(crate) fn extract_snippet(chars: &[char], pos: usize) -> String {
    let n = chars.len();
    let end = (pos + 800).min(n);

    let snippet_tag = ["result-snippet", "result__snippet", "snippet"];
    for tag in &snippet_tag {
        if let Some(start) = substr_pos(chars, pos, tag) {
            let real_start = start + pos + tag.len();
            if let Some(gt) = substr_pos(chars, real_start, ">") {
                let content_start = real_start + gt + 1;
                for end_tag in &["</span>", "</div>"] {
                    if let Some(et) = substr_pos(chars, content_start, end_tag) {
                        let end_pos = content_start + et;
                        let text: String = chars[content_start..end_pos].iter().collect();
                        let clean = strip_html_tags(&text).trim().to_string();
                        if clean.len() > 10 {
                            return truncate_at(&clean, 200);
                        }
                    }
                }
            }
        }
    }

    let mut text = String::new();
    let mut in_tag = false;
    let mut collected = 0usize;

    for &ch in chars[pos..end].iter() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                if collected > 10 {
                    text.push(' ');
                }
            }
            _ if !in_tag && !matches!(ch, '\n' | '\r' | '\t') => {
                text.push(ch);
                collected += 1;
            }
            _ => {}
        }
        if collected > 200 {
            break;
        }
    }

    let clean: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_at(&clean, 200)
}

pub(crate) fn extract_href(tag_body: &str) -> Option<String> {
    let href_pos = tag_body.find("href=")?;
    let after = &tag_body[href_pos + 5..];
    let quote = after.chars().next()?;
    let inner = &after[1..];
    let end = inner.find(quote)?;
    let url = &inner[..end];

    let url = url.replace("&amp;", "&");

    Some(url.to_string())
}

/// Detect a search-engine anti-bot / challenge page — these contain no real
/// results and must return empty so the caller falls back to the next endpoint
/// instead of parsing garbage links from the challenge footer.
///
/// Covers both DuckDuckGo and Bing block pages.
pub(crate) fn is_challenge_page(html: &str) -> bool {
    html.contains("anomaly.js")
        || html.contains("challenge-form")
        || html.contains("/check.")
        || html.contains("ddg_ptoken")
        || html.contains("Get the full-JS version here")
        || html.contains("not enabled JavaScript")
        || html.contains("captcha-delivery.com")
        || html.contains("g-recaptcha")
        || html.contains("hCaptcha")
        || html.contains("challenge-platform")
        || html.contains("tr.bing.com")
        || (html.contains("id=\"b_sb_preview\"") && !html.contains("b_algo"))
        || html.contains("Just a moment...")
        || html.contains("Checking your browser")
        || html.contains("DDoS protection")
        || html.contains("cf-browser-verification")
        || html.contains("cf-challenge-running")
        || html.contains("_cf_chl_opt")
        || html.contains("cf-spinner")
        || html.contains("Please turn JavaScript on")
        || html.contains("please enable JavaScript")
        || html.contains("Attention Required! | Cloudflare")
        || html.contains("akamai")
        || html.contains("distil_r_captcha")
        || html.contains("imperva")
        || (html.len() < 200 && !html.contains("<a "))
}

/// Resolve a DDG result href to the real destination URL.
/// DDG wraps results as `//duckduckgo.com/l/?uddg=<percent-encoded real url>`.
pub(crate) fn resolve_real_url(href: &str) -> Option<String> {
    if let Some(pos) = href.find("uddg=") {
        let after = &href[pos + "uddg=".len()..];
        let end = after.find('&').unwrap_or(after.len());
        let decoded = percent_decode(&after[..end]);
        if decoded.starts_with("http://") || decoded.starts_with("https://") {
            return Some(decoded);
        }
    }
    if let Some(rest) = href.strip_prefix("//") {
        if !rest.starts_with("duckduckgo.com") && rest.contains('.') {
            return Some(format!("https:{href}"));
        }
    }
    if href.starts_with("http://") || href.starts_with("https://") {
        return Some(href.replace("&amp;", "&"));
    }
    None
}

/// Is this a search-engine-internal link (ads, nav, redirect)?
pub(crate) fn is_internal_link(url: &str) -> bool {
    url.contains("duckduckgo.com/y.js")
        || url.contains("duckduckgo.com/l/?")
        || url.contains("duckduckgo.com/ai")
        || url.starts_with("https://duckduckgo.com")
        || url.starts_with("http://duckduckgo.com")
        || url.contains("://www.bing.com/ck/")
        || url.contains("go.microsoft.com/fwlink")
        || url.contains("://www.bing.com/account")
        || url.contains("://www.bing.com/feedback")
        || url.contains("://cn.bing.com/ck/")
        || url == "https://www.bing.com"
        || url == "https://cn.bing.com"
}

/// Minimal percent-decoding for `uddg=` params: `+` → space, `%XX` → byte.
pub(crate) fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                if let Ok(byte) =
                    u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16)
                {
                    out.push(byte);
                    i += 3;
                    continue;
                } else {
                    out.push(b[i]);
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Extract the text content between `<a ...>` and `</a>`.
pub(crate) fn extract_link_text(html_fragment: &str) -> String {
    let a_start = match html_fragment.find("<a ") {
        Some(pos) => pos,
        None => return String::new(),
    };
    let tag_end = match html_fragment[a_start..].find('>') {
        Some(pos) => a_start + pos + 1,
        None => return String::new(),
    };
    let close = match html_fragment[tag_end..].find("</a>") {
        Some(pos) => tag_end + pos,
        None => return String::new(),
    };
    strip_html_tags(&html_fragment[tag_end..close])
        .trim()
        .to_string()
}

/// Extract snippet text from a Bing `b_algo` block.
pub(crate) fn extract_bing_snippet(block: &str) -> String {
    if let Some(p_start) = block.find("<p") {
        if let Some(gt) = block[p_start..].find('>') {
            let content_start = p_start + gt + 1;
            if let Some(end) = block[content_start..].find("</p>") {
                let text = &block[content_start..content_start + end];
                let clean = strip_html_tags(text).trim().to_string();
                if clean.len() > 10 {
                    return truncate_at(&clean, 200);
                }
            }
        }
    }
    if let Some(div_start) = block.find("b_caption") {
        if let Some(gt) = block[div_start..].find('>') {
            let content_start = div_start + gt + 1;
            if let Some(end) = block[content_start..].find("</div>") {
                let text = &block[content_start..content_start + end];
                let clean = strip_html_tags(text).trim().to_string();
                if clean.len() > 10 {
                    return truncate_at(&clean, 200);
                }
            }
        }
    }
    String::new()
}

pub(crate) fn substr_pos(haystack: &[char], start: usize, needle: &str) -> Option<usize> {
    let needle_chars: Vec<char> = needle.chars().collect();
    if needle_chars.is_empty() || start >= haystack.len() {
        return None;
    }
    haystack[start..]
        .windows(needle_chars.len())
        .position(|window| window == needle_chars.as_slice())
}

pub(crate) fn strip_html_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn truncate_at(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "\u{2026}"
    }
}

/// Percent-encode a search query for embedding in a browser URL.
/// Space → `+` (form-encoded), unreserved chars pass through, all else → `%XX`.
pub fn encode_url_query(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ' ' => "+".to_string(),
            c if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') => c.to_string(),
            c => format!("%{:02X}", c as u8),
        })
        .collect()
}
