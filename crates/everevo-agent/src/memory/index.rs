//! MEMORY.md index — auto-generated from facts/ directory.

use std::collections::HashMap;
use std::path::Path;

use everevo_core::memory::{FactType, MemoryFact, MemoryIndexEntry};
use everevo_core::EverEvoError;

use super::frontmatter::parse_fact_file;

/// Load all facts from a facts directory.
pub fn load_all_facts(facts_dir: &Path) -> Result<Vec<MemoryFact>, EverEvoError> {
    let mut facts = Vec::new();
    if !facts_dir.exists() {
        return Ok(facts);
    }
    let entries = std::fs::read_dir(facts_dir)
        .map_err(|e| EverEvoError::Internal(format!("Failed to read facts dir: {e}")))?;
    for entry in entries {
        let entry = entry.map_err(|e| EverEvoError::Internal(format!("Dir entry: {e}")))?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "md") {
            if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Some(fact) = parse_fact_file(name, &content) {
                        facts.push(fact);
                    }
                }
            }
        }
    }
    Ok(facts)
}

/// Regenerate MEMORY.md from all facts in the facts directory.
pub fn regenerate_index(facts_dir: &Path, index_path: &Path) -> Result<(), EverEvoError> {
    let facts = load_all_facts(facts_dir)?;
    let mut index = String::from("# EverEvo Memory Index\n\n");

    let mut by_type: HashMap<FactType, Vec<&MemoryFact>> = HashMap::new();
    for fact in &facts {
        by_type
            .entry(fact.fact_type.clone())
            .or_default()
            .push(fact);
    }

    let type_order = [
        FactType::User,
        FactType::Project,
        FactType::Feedback,
        FactType::Reference,
    ];
    let labels: HashMap<FactType, &str> = [
        (FactType::User, "## User Preferences"),
        (FactType::Project, "## Project Knowledge"),
        (FactType::Feedback, "## Feedback & Corrections"),
        (FactType::Reference, "## References"),
    ]
    .into_iter()
    .collect();

    for ty in &type_order {
        if let Some(items) = by_type.get(ty) {
            if let Some(label) = labels.get(ty) {
                index.push_str(&format!("\n{label}\n\n"));
            }
            for fact in items {
                index.push_str(&format!(
                    "- [{name}](facts/{name}.md) \u{2014} {desc}\n",
                    name = fact.name,
                    desc = fact.description,
                ));
            }
        }
    }

    std::fs::write(index_path, &index)
        .map_err(|e| EverEvoError::Internal(format!("Failed to write index: {e}")))?;

    tracing::info!(count = facts.len(), "MEMORY.md regenerated");
    Ok(())
}

/// Parse MEMORY.md into structured entries.
pub fn parse_index(content: &str) -> Vec<MemoryIndexEntry> {
    let mut entries = Vec::new();
    let mut current_type = FactType::Project;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("## User") {
            current_type = FactType::User;
            continue;
        }
        if trimmed.starts_with("## Project") {
            current_type = FactType::Project;
            continue;
        }
        if trimmed.starts_with("## Feedback") {
            current_type = FactType::Feedback;
            continue;
        }
        if trimmed.starts_with("## Reference") {
            current_type = FactType::Reference;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("- [") {
            if let Some(name_end) = rest.find(']') {
                let name = rest[..name_end].to_string();
                let after_link = &rest[name_end + 1..];
                let description = if let Some(pos) = after_link.find('\u{2014}') {
                    after_link[pos + '\u{2014}'.len_utf8()..].trim().to_string()
                } else if let Some(pos) = after_link.find(" - ") {
                    after_link[pos + 3..].trim().to_string()
                } else {
                    String::new()
                };
                if !name.is_empty() {
                    entries.push(MemoryIndexEntry {
                        name,
                        description,
                        fact_type: current_type.clone(),
                    });
                }
            }
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_parse_entries() {
        let content = "## User Preferences\n- [pref](facts/pref.md) \u{2014} A preference";
        let entries = parse_index(content);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "pref");
        assert_eq!(entries[0].fact_type, FactType::User);
    }

    #[test]
    fn test_regenerate_empty() {
        let dir = TempDir::new().unwrap();
        let idx = dir.path().join("MEMORY.md");
        regenerate_index(dir.path(), &idx).unwrap();
        let content = std::fs::read_to_string(&idx).unwrap();
        assert!(content.contains("EverEvo Memory Index"));
    }
}
