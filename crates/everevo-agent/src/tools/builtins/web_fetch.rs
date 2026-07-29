//! WebFetch built-in tool — fetches URL content and returns it as text.
//!
//! Claude Code equivalent: `WebFetch` tool. Fetches a URL, converts HTML
//! to readable text, and returns the content truncated to a safe limit.
//!
//! For web SEARCH (multi-source lookup), use an MCP search server
//! (Brave Search, Tavily, etc.) — the MCP infrastructure already
//! auto-registers those tools.

use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use tokio_util::sync::CancellationToken;

/// Maximum response body size (characters) before truncation.
const MAX_CONTENT_LEN: usize = 16_000;

/// HTTP request timeout.
const REQUEST_TIMEOUT_SECS: u64 = 30;

/// Fetches content from a URL. Strips HTML tags for readability.
/// For authenticated/private URLs, this will fail — use shell + curl for those.
pub struct WebFetchTool;

impl WebFetchTool {
    /// Strip HTML tags to produce readable plain text.
    fn strip_html(html: &str) -> String {
        let mut out = String::with_capacity(html.len());
        let mut in_tag = false;
        for ch in html.chars() {
            match ch {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => out.push(ch),
                _ => {}
            }
        }
        // Collapse whitespace
        out.split_whitespace().collect::<Vec<_>>().join(" ")
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch content from a URL. Returns the page text (HTML tags stripped). \
         Use for reading documentation, API responses, or any public webpage. \
         For authenticated endpoints, use the shell tool with curl instead. \
         Parameters: url (the URL to fetch, http/https only)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch content from (must start with http:// or https://)"
                }
            },
            "required": ["url"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low // read-only, only fetches public URLs
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        cancel: Option<&CancellationToken>,
    ) -> Result<ToolOutput, EverEvoError> {
        // Check cancellation early
        if cancel.is_some_and(|c| c.is_cancelled()) {
            return Ok(ToolOutput {
                content: "cancelled".into(),
                is_error: true,
            });
        }

        let url = params["url"]
            .as_str()
            .ok_or_else(|| EverEvoError::InvalidInput("url is required".into()))?;

        // Only allow http/https
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Ok(ToolOutput {
                content: format!(
                    "Error: Only http/https URLs are supported. Got: {url}"
                ),
                is_error: true,
            });
        }

        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .user_agent("EverEvo/0.1 (desktop AI agent)")
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|e| EverEvoError::LlmProvider(e.to_string()))?;

        let resp = client
            .get(url)
            .send()
            .await
            .map_err(|e| EverEvoError::LlmProvider(format!("Fetch failed: {e}")))?;

        let status = resp.status();
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_string();

        let body = resp
            .text()
            .await
            .map_err(|e| EverEvoError::LlmProvider(format!("Read body: {e}")))?;

        // Convert HTML to plain text; leave JSON/XML/text as-is
        let text = if content_type.contains("html") {
            Self::strip_html(&body)
        } else {
            body
        };

        // Truncate and add summary header
        let truncated = if text.chars().count() > MAX_CONTENT_LEN {
            let head: String = text.chars().take(MAX_CONTENT_LEN).collect();
            format!(
                "[HTTP {status}] {url}\nContent-Type: {content_type}\nBody ({total} chars, showing first {shown}):\n\n{head}\n\n... truncated ...",
                total = text.chars().count(),
                shown = MAX_CONTENT_LEN,
            )
        } else {
            format!(
                "[HTTP {status}] {url}\nContent-Type: {content_type}\nBody ({len} chars):\n\n{text}",
                len = text.chars().count(),
            )
        };

        Ok(ToolOutput {
            content: truncated,
            is_error: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_html_basic() {
        let html = "<html><body><p>Hello <b>world</b></p></body></html>";
        assert_eq!(WebFetchTool::strip_html(html), "Hello world");
    }

    #[test]
    fn test_strip_html_plain_text() {
        let text = "just plain text\nno tags here";
        assert_eq!(WebFetchTool::strip_html(text), "just plain text no tags here");
    }

    #[test]
    fn test_strip_html_nested() {
        // adjacent tags with no whitespace between them — no space inserted
        assert_eq!(
            WebFetchTool::strip_html("<div><span>a</span><span>b</span></div>"),
            "ab"
        );
    }

    #[test]
    fn test_strip_html_with_attributes() {
        assert_eq!(
            WebFetchTool::strip_html(r#"<a href="/foo" class="bar">click here</a>"#),
            "click here"
        );
    }

    #[test]
    fn test_name_and_schema() {
        let tool = WebFetchTool;
        assert_eq!(tool.name(), "web_fetch");
        let schema = tool.parameters_schema();
        assert_eq!(schema["required"][0], "url");
        assert_eq!(tool.risk_level(), RiskLevel::Low);
    }
}
