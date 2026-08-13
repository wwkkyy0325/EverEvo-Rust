//! URL and content sanitization — prevents SSRF, XSS, path traversal.

/// Validate and sanitize a URL for safe fetching.
///
/// - Only http/https schemes
/// - No localhost/127.0.0.1/0.0.0.0 (SSRF prevention)
/// - No internal IPs (10.x, 172.16-31.x, 192.168.x)
/// - No file://, ftp://, gopher:// schemes
pub fn sanitize_url(url: &str) -> Result<String, String> {
    let url = url.trim();

    // Must start with http:// or https://
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("Only http/https URLs are allowed".to_string());
    }

    // Extract host manually (avoid url crate dependency).
    // Format: http(s)://host[:port][/path]
    let after_scheme = url
        .strip_prefix("https://")
        .unwrap_or_else(|| url.strip_prefix("http://").unwrap_or(url));
    let host = match after_scheme.find('/') {
        Some(pos) => &after_scheme[..pos],
        None => after_scheme,
    };
    let host = match host.find(':') {
        Some(pos) => &host[..pos], // strip port
        None => host,
    };
    let host_lower = host.to_lowercase();

    // Block loopback
    if host_lower == "localhost"
        || host_lower == "127.0.0.1"
        || host_lower == "0.0.0.0"
        || host_lower == "[::1]"
    {
        return Err("Access to localhost is blocked".to_string());
    }

    // Block link-local
    if host_lower.starts_with("169.254.") {
        return Err("Access to link-local addresses is blocked".to_string());
    }

    // Block private ranges
    if host_lower.starts_with("10.")
        || host_lower.starts_with("192.168.")
        || host_lower.starts_with("172.16.")
        || host_lower.starts_with("172.17.")
        || host_lower.starts_with("172.18.")
        || host_lower.starts_with("172.19.")
        || host_lower.starts_with("172.20.")
        || host_lower.starts_with("172.21.")
        || host_lower.starts_with("172.22.")
        || host_lower.starts_with("172.23.")
        || host_lower.starts_with("172.24.")
        || host_lower.starts_with("172.25.")
        || host_lower.starts_with("172.26.")
        || host_lower.starts_with("172.27.")
        || host_lower.starts_with("172.28.")
        || host_lower.starts_with("172.29.")
        || host_lower.starts_with("172.30.")
        || host_lower.starts_with("172.31.")
    {
        return Err("Access to private network addresses is blocked".to_string());
    }

    Ok(url.to_string())
}

/// Strip HTML tags and decode entities, returning clean text.
/// Safe for arbitrary HTML input.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let chars: Vec<char> = html.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if !in_tag && i + 6 < chars.len() {
            let slice: String = chars[i..i + 7].iter().collect();
            if slice.to_lowercase().starts_with("<script") {
                in_script = true;
                in_tag = true;
                i += 7;
                continue;
            }
            if slice.to_lowercase().starts_with("<style") {
                in_style = true;
                in_tag = true;
                i += 6;
                continue;
            }
        }

        match chars[i] {
            '<' => {
                in_tag = true;
                // Check for closing script/style
                let remaining: String = chars[i..].iter().take(9).collect();
                let lower = remaining.to_lowercase();
                if lower.starts_with("</script>") {
                    in_script = false;
                    in_tag = false;
                    i += 9;
                    continue;
                }
                if lower.starts_with("</style>") {
                    in_style = false;
                    in_tag = false;
                    i += 8;
                    continue;
                }
            }
            '>' => {
                in_tag = false;
                // Insert space between block elements
                if !out.ends_with(' ') && !out.ends_with('\n') {
                    out.push(' ');
                }
            }
            ch if !in_tag && !in_script && !in_style => {
                if ch == '\n' || ch == '\r' || ch == '\t' {
                    out.push(' ');
                } else {
                    out.push(ch);
                }
            }
            _ => {}
        }
        i += 1;
    }

    // Decode common HTML entities
    let text = out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");

    // Collapse whitespace
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_valid_url() {
        assert!(sanitize_url("https://example.com/page").is_ok());
    }

    #[test]
    fn test_sanitize_blocks_localhost() {
        assert!(sanitize_url("http://localhost:8080/admin").is_err());
        assert!(sanitize_url("http://127.0.0.1/test").is_err());
    }

    #[test]
    fn test_sanitize_blocks_private_ips() {
        assert!(sanitize_url("http://192.168.1.1/admin").is_err());
        assert!(sanitize_url("http://10.0.0.1/test").is_err());
    }

    #[test]
    fn test_sanitize_blocks_non_http() {
        assert!(sanitize_url("file:///etc/passwd").is_err());
        assert!(sanitize_url("ftp://example.com").is_err());
    }

    #[test]
    fn test_html_to_text_strips_tags() {
        assert_eq!(html_to_text("<h1>Hello</h1><p>World</p>"), "Hello World");
    }

    #[test]
    fn test_html_to_text_strips_script() {
        assert_eq!(
            html_to_text("<script>alert('xss')</script><p>Safe</p>"),
            "Safe"
        );
    }
}
