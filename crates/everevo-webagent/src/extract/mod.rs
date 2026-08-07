//! Content extraction — HTML→Markdown/plain text + structured data (JSON-LD, microdata, RDFa).

/// HTML → Markdown / plain text conversion.
/// Reuses `protect::sanitize::html_to_text` for the text path.
#[allow(dead_code)]
pub mod html;

/// Structured data extraction from JSON-LD, microdata, RDFa.
#[allow(dead_code)]
pub mod structured;
