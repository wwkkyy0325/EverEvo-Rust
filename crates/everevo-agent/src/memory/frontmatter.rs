//! Frontmatter parser for fact files and diary entries.
//!
//! Format:
//! ```markdown
//! ---
//! name: some-slug
//! description: One-line summary
//! type: user|feedback|project|reference
//! created: 2026-07-18T14:30:00Z
//! updated: 2026-07-19T09:00:00Z
//! links: link1, link2
//! ---
//!
//! Markdown body here...
//! ```

use std::collections::HashMap;

use everevo_core::memory::{FactType, MemoryFact, ProjectionMetadata};

/// Parse YAML-like frontmatter from markdown content.
/// Returns `(key_value_map, body_text)` or `None` if no frontmatter found.
pub fn parse_frontmatter(content: &str) -> Option<(HashMap<String, String>, &str)> {
    let content = content.trim();
    if !content.starts_with("---") {
        return None;
    }
    let rest = &content[3..];
    let end = rest.find("\n---")?;
    let fm_text = &rest[..end];
    let body = rest[end + 4..].trim();

    let mut map = HashMap::new();
    for line in fm_text.lines() {
        if let Some((key, value)) = line.split_once(':') {
            map.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    Some((map, body))
}

/// Parse a fact file from markdown content.
pub fn parse_fact_file(name: &str, content: &str) -> Option<MemoryFact> {
    let (fm, body) = parse_frontmatter(content)?;

    let description = fm.get("description").cloned().unwrap_or_default();
    let fact_type = fm
        .get("type")
        .and_then(|t| FactType::from_str(t))
        .unwrap_or(FactType::Project);

    let created_at = fm
        .get("created")
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(chrono::Utc::now);

    let updated_at = fm
        .get("updated")
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or(created_at);

    let links: Vec<String> = if let Some(links_str) = fm.get("links") {
        links_str.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
    } else {
        body.split("[[").skip(1)
            .filter_map(|s| s.split("]]").next())
            .map(|s| s.to_string())
            .collect()
    };

    Some(MemoryFact {
        name: name.to_string(),
        description,
        content: body.to_string(),
        fact_type,
        created_at,
        updated_at,
        projection: ProjectionMetadata::new("2.0.0", "unknown", vec![], 0.85),
        links,
    })
}

/// Serialize a MemoryFact to markdown with frontmatter.
pub fn serialize_fact_file(fact: &MemoryFact) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("name: {}\n", fact.name));
    out.push_str(&format!("description: {}\n", fact.description));
    out.push_str(&format!("type: {}\n", fact.fact_type.as_str()));
    out.push_str(&format!("created: {}\n", fact.created_at.to_rfc3339()));
    out.push_str(&format!("updated: {}\n", fact.updated_at.to_rfc3339()));
    if !fact.links.is_empty() {
        out.push_str(&format!("links: {}\n", fact.links.join(", ")));
    }
    out.push_str("---\n\n");
    out.push_str(&fact.content);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_basic() {
        let content = "---\nname: test\n---\n\nBody";
        let (fm, body) = parse_frontmatter(content).unwrap();
        assert_eq!(fm.get("name").unwrap(), "test");
        assert_eq!(body, "Body");
    }

    #[test]
    fn test_no_frontmatter() {
        assert!(parse_frontmatter("Just text").is_none());
    }

    #[test]
    fn test_roundtrip() {
        let fact = MemoryFact {
            name: "test".into(),
            description: "desc".into(),
            content: "Body with [[a]] and [[b]]".into(),
            fact_type: FactType::User,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            projection: ProjectionMetadata::new("test", "none", vec![], 1.0),
            links: vec!["a".into(), "b".into()],
        };
        let s = serialize_fact_file(&fact);
        let parsed = parse_fact_file("test", &s).unwrap();
        assert_eq!(parsed.links, vec!["a", "b"]);
    }
}
