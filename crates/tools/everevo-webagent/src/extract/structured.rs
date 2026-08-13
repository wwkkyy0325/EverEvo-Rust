//! Structured data extraction from HTML pages.
//!
//! Extracts JSON-LD, microdata, and Open Graph / Twitter Card metadata.
//! Used to pull rich snippets from search results and product pages.

/// Extract JSON-LD blocks from HTML.
pub fn extract_json_ld(html: &str) -> Vec<String> {
    let mut results = Vec::new();
    let mut pos = 0;
    while let Some(start) = html[pos..].find(r#"type="application/ld+json""#) {
        let abs_start = pos + start;
        // Find the > after the <script tag
        if let Some(gt) = html[abs_start..].find('>') {
            let content_start = abs_start + gt + 1;
            if let Some(end) = html[content_start..].find("</script>") {
                let json = html[content_start..content_start + end].trim().to_string();
                if !json.is_empty() {
                    results.push(json);
                }
                pos = content_start + end + 9;
                continue;
            }
        }
        pos = abs_start + 1;
    }
    results
}

/// Extract Open Graph and Twitter Card meta tags.
/// Returns (property, content) pairs.
pub fn extract_meta_tags(html: &str) -> Vec<(String, String)> {
    let mut results = Vec::new();
    // og:title, og:description, og:url, twitter:title, etc.
    for prefix in &["og:", "twitter:"] {
        let search = format!(r#"property="{prefix}"#);
        let mut pos = 0;
        while let Some(start) = html[pos..].find(&search) {
            let abs_start = pos + start;
            let content_start = abs_start + search.len();
            // Find content="..."
            if let Some(content_attr) = html[content_start..].find("content=\"") {
                let val_start = content_start + content_attr + 9; // skip content="
                if let Some(quote) = html[val_start..].find('"') {
                    let value = html[val_start..val_start + quote].to_string();
                    // Extract the property name
                    let prop = html[abs_start..]
                        .chars()
                        .take_while(|&c| c != '"')
                        .collect::<String>();
                    results.push((prop, value));
                    pos = val_start + quote + 1;
                    continue;
                }
            }
            pos = abs_start + 1;
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_json_ld() {
        let html =
            r#"<script type="application/ld+json">{"@type":"Article","name":"Test"}</script>"#;
        let items = extract_json_ld(html);
        assert_eq!(items.len(), 1);
        assert!(items[0].contains("Article"));
    }

    #[test]
    fn test_extract_og_tags() {
        let html = r#"<meta property="og:title" content="My Page"><meta property="og:description" content="A test">"#;
        let tags = extract_meta_tags(html);
        assert_eq!(tags.len(), 2);
    }
}
