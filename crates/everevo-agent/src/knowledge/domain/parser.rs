//! Document parser — parses files into plain text based on extension.

use std::path::Path;

use everevo_core::EverEvoError;

/// Parses documents into plain text.
pub struct DocumentParser;

impl DocumentParser {
    /// Parse file content based on extension.
    pub fn parse(filename: &str, raw: &[u8]) -> Result<String, EverEvoError> {
        let ext = Path::new(filename)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        match ext {
            "md" | "markdown" => Self::parse_markdown(raw),
            "txt" => Self::parse_text(raw),
            "rs" | "py" | "ts" | "js" | "go" | "java" | "c" | "cpp" | "h" => {
                Self::parse_code(raw, ext)
            }
            "json" | "toml" | "yaml" | "yml" => Self::parse_text(raw),
            "pdf" => Self::parse_pdf(raw),
            _ => Self::parse_text(raw), // fallback: treat as text
        }
    }

    fn parse_markdown(raw: &[u8]) -> Result<String, EverEvoError> {
        String::from_utf8(raw.to_vec())
            .map_err(|e| EverEvoError::InvalidInput(format!("Invalid UTF-8: {e}")))
    }

    fn parse_text(raw: &[u8]) -> Result<String, EverEvoError> {
        String::from_utf8(raw.to_vec())
            .map_err(|e| EverEvoError::InvalidInput(format!("Invalid UTF-8: {e}")))
    }

    fn parse_code(raw: &[u8], lang: &str) -> Result<String, EverEvoError> {
        let text = String::from_utf8(raw.to_vec())
            .map_err(|e| EverEvoError::InvalidInput(format!("Invalid UTF-8: {e}")))?;
        Ok(format!("```{lang}\n{text}\n```"))
    }

    fn parse_pdf(raw: &[u8]) -> Result<String, EverEvoError> {
        // Basic PDF text extraction: scan for stream objects and text between BT/ET markers.
        // Falls back to extracting readable ASCII spans from the raw bytes.
        let raw_str = String::from_utf8_lossy(raw);
        let mut text = String::new();

        // Try to find text between BT (Begin Text) and ET (End Text) markers
        let mut in_text_block = false;
        for line in raw_str.lines() {
            let trimmed = line.trim();
            if trimmed == "BT" {
                in_text_block = true;
                continue;
            }
            if trimmed == "ET" {
                in_text_block = false;
                continue;
            }
            if in_text_block {
                // Extract text from Tj/TJ operators
                if let Some(tj_start) = trimmed.find("(") {
                    if let Some(tj_end) = trimmed.rfind(")") {
                        let inner = &trimmed[tj_start + 1..tj_end];
                        text.push_str(inner);
                        text.push(' ');
                    }
                }
            }
        }

        if text.trim().is_empty() {
            // Fallback: extract readable ASCII sequences (50+ chars)
            let readable: String = raw_str
                .chars()
                .filter(|c| c.is_ascii_graphic() || c.is_ascii_whitespace())
                .collect();
            for segment in readable.split_whitespace().collect::<Vec<_>>().chunks(20) {
                let line = segment.join(" ");
                if line.len() > 50 {
                    text.push_str(&line);
                    text.push('\n');
                }
            }
        }

        if text.trim().is_empty() {
            Ok("[PDF: binary content, text extraction failed]".into())
        } else {
            Ok(text.trim().to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parser_markdown() {
        let result = DocumentParser::parse("test.md", b"# Hello\n\nWorld").unwrap();
        assert!(result.contains("Hello"));
    }

    #[test]
    fn test_parser_code() {
        let result = DocumentParser::parse("test.rs", b"fn main() {}").unwrap();
        assert!(result.contains("```rs"));
    }
}
