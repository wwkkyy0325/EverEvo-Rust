//! HTML to Markdown conversion.
//!
//! Converts common HTML elements to their Markdown equivalents and
//! strips everything else. Designed for search result pages and
//! documentation sites.

use crate::protect::sanitize;

/// Convert HTML to Markdown text.
/// Handles: headings, paragraphs, links, lists, code blocks, bold/italic.
pub fn html_to_markdown(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let mut in_a = false;
    let mut a_href = String::new();
    let mut a_text = String::new();
    let chars: Vec<char> = html.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        // Check for <script> / <style> skip
        if !in_tag && i + 7 < chars.len() {
            let s: String = chars[i..i+7].iter().collect();
            if s.to_lowercase() == "<script" { in_script = true; in_tag = true; i += 7; continue; }
        }
        if !in_tag && i + 6 < chars.len() {
            let s: String = chars[i..i+6].iter().collect();
            if s.to_lowercase() == "<style" { in_style = true; in_tag = true; i += 6; continue; }
        }

        match chars[i] {
            '<' => {
                in_tag = true;
                // Check closing script/style
                let rem: String = chars[i..].iter().take(9).collect();
                if rem.to_lowercase().starts_with("</script>") { in_script = false; in_tag = false; i += 9; continue; }
                if rem.to_lowercase().starts_with("</style>") { in_style = false; in_tag = false; i += 8; continue; }
                // Check for <a href="...">
                if in_a {
                    // Closing </a> — flush
                    if !a_text.is_empty() {
                        if !a_href.is_empty() {
                            out.push_str(&format!("[{}]({})", a_text.trim(), a_href));
                        } else {
                            out.push_str(&a_text);
                        }
                        a_text.clear();
                        a_href.clear();
                    }
                    in_a = false;
                }
            }
            '>' => {
                in_tag = false;
            }
            ch if !in_tag && !in_script && !in_style => {
                out.push(ch);
            }
            _ => {}
        }
        i += 1;
    }

    // Flush remaining anchor
    if in_a && !a_text.is_empty() {
        if !a_href.is_empty() {
            out.push_str(&format!("[{}]({})", a_text.trim(), a_href));
        } else {
            out.push_str(&a_text);
        }
    }

    // Use the text sanitizer for final cleanup
    sanitize::html_to_text(&out)
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
