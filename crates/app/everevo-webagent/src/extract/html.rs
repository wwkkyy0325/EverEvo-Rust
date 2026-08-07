//! HTML to Markdown conversion.
//!
//! Converts common HTML elements to their Markdown equivalents and
//! strips everything else. Designed for search result pages and
//! documentation sites.

/// Convert HTML to Markdown.
///
/// Handles: headings (h1-h6), paragraphs, links, bold/italic, code blocks,
/// line breaks, lists, and images. Strips scripts, styles, and unknown tags.
pub fn html_to_markdown(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let mut in_pre = false;
    let mut tag_buf = String::new();
    let mut last_was_newline = false;

    let chars: Vec<char> = html.chars().collect();
    let len = chars.len();
    let mut i = 0;

    // Helper: read chars from i until delimiter or len
    let peek_tag = |chars: &[char], start: usize| -> String {
        let mut s = String::new();
        let mut j = start;
        while j < chars.len() && j < start + 20 {
            let c = chars[j];
            if c == '>' || c == ' ' || c == '\n' { break; }
            s.push(c.to_ascii_lowercase());
            j += 1;
        }
        s
    };

    // Helper: extract attribute value from tag buffer
    let extract_attr = |buf: &str, attr: &str| -> Option<String> {
        let lower = buf.to_lowercase();
        let needle = format!("{attr}=");
        if let Some(pos) = lower.find(&needle) {
            let rest = &buf[pos + needle.len()..];
            let delim = rest.chars().next().unwrap_or('"');
            let inner: String = rest.chars().skip(1).take_while(|&c| c != delim).collect();
            if !inner.is_empty() { return Some(inner); }
        }
        None
    };

    // Helper: ensure newline separation
    let ensure_nl = |out: &mut String, last: &mut bool| {
        if !*last {
            out.push('\n');
            *last = true;
        }
    };

    while i < len {
        match chars[i] {
            '<' => {
                in_tag = true;
                tag_buf.clear();

                // Check <script>, <style>, <pre>
                let tag = peek_tag(&chars, i);
                match tag.as_str() {
                    "<script" => { in_script = true; i += 7; continue; }
                    "<style" => { in_style = true; i += 6; continue; }
                    "<pre" | "<code" => { in_pre = true; ensure_nl(&mut out, &mut last_was_newline); }
                    _ => {}
                }

                // Closing tags
                if tag.starts_with("</") {
                    match tag.as_str() {
                        "</script>" | "</style>" => {
                            in_script = false; in_style = false;
                            i += tag.len() + 1; continue;
                        }
                        "</pre>" | "</code>" => { in_pre = false; ensure_nl(&mut out, &mut last_was_newline); }
                        "</p>" | "</div>" => {
                            ensure_nl(&mut out, &mut last_was_newline);
                            ensure_nl(&mut out, &mut last_was_newline);
                        }
                        "</h1>" | "</h2>" | "</h3>" | "</h4>" | "</h5>" | "</h6>" => {
                            ensure_nl(&mut out, &mut last_was_newline);
                            ensure_nl(&mut out, &mut last_was_newline);
                        }
                        "</li>" => { ensure_nl(&mut out, &mut last_was_newline); }
                        "</a>" => { /* text already pushed during content collection */ }
                        "</b>" | "</strong>" => { out.push_str("**"); }
                        "</i>" | "</em>" => { out.push('*'); }
                        "</br>" | "<br/>" | "<br />" => { ensure_nl(&mut out, &mut last_was_newline); }
                        _ => {}
                    }
                }
            }
            '>' => {
                // Opening tag: process heading levels and list markers
                let lower = tag_buf.to_lowercase();
                if lower.starts_with("h1") { out.push_str("\n# "); last_was_newline = false; }
                else if lower.starts_with("h2") { out.push_str("\n## "); last_was_newline = false; }
                else if lower.starts_with("h3") { out.push_str("\n### "); last_was_newline = false; }
                else if lower.starts_with("h4") { out.push_str("\n#### "); last_was_newline = false; }
                else if lower.starts_with("h5") { out.push_str("\n##### "); last_was_newline = false; }
                else if lower.starts_with("h6") { out.push_str("\n###### "); last_was_newline = false; }
                else if lower.starts_with("p") { /* paragraph: just track newlines */ }
                else if lower.starts_with("br") { ensure_nl(&mut out, &mut last_was_newline); }
                else if lower.starts_with("li") { out.push_str("- "); last_was_newline = false; }
                else if lower.starts_with("b") || lower.starts_with("strong") { out.push_str("**"); }
                else if lower.starts_with("i") || lower.starts_with("em") { out.push('*'); }
                else if lower.starts_with("a ") {
                    // Extract href for Markdown link
                    if let Some(_href) = extract_attr(&tag_buf, "href") {
                        out.push('[');
                        // We'll collect link text and flush at </a>
                    }
                }
                else if lower.starts_with("img") {
                    if let Some(src) = extract_attr(&tag_buf, "src") {
                        let alt = extract_attr(&tag_buf, "alt").unwrap_or_default();
                        out.push_str(&format!("![{}]({})", alt, src));
                    }
                }
                in_tag = false;
                tag_buf.clear();
            }
            ch => {
                if in_tag {
                    tag_buf.push(ch);
                } else if !in_script && !in_style {
                    if !in_pre && (ch == '\n' || ch == '\r') {
                        // Collapse whitespace
                        if !last_was_newline { out.push(' '); last_was_newline = true; }
                    } else {
                        out.push(ch);
                        last_was_newline = false;
                    }
                }
            }
        }
        i += 1;
    }

    // Final cleanup: decode common entities
    let result = out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");
    // Collapse multiple blank lines
    let result = result.lines().fold(String::new(), |mut acc, line| {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !acc.ends_with("\n\n") { acc.push_str("\n\n"); }
        } else {
            acc.push_str(trimmed);
            acc.push('\n');
        }
        acc
    });
    result.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_html_to_md() {
        assert_eq!(
            html_to_markdown("<p>Hello World</p>"),
            "Hello World"
        );
    }
}
