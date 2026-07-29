//! llmwiki manager — project knowledge base (docs/llmwiki/).
//!
//! ## Structure
//!
//! ```text
//! docs/llmwiki/
//!   ├── design.md           ← living architecture summary
//!   ├── changelog.md         ← append-only modification log
//!   └── tasks/
//!       └── <task>.md        ← per-task breakdown with checkbox steps
//! ```
//!
//! ## Usage
//!
//! ```rust,ignore
//! let wiki = LlmwikiManager::open(project_root)?;
//! wiki.ensure_directories()?;
//! let tasks = wiki.list_tasks()?;
//! let design = wiki.read_design()?;
//! wiki.append_changelog("Added feature X")?;
//! ```
//!
//! This module also provides an indexer that feeds documents into the RAG
//! pipeline for semantic search.

use std::path::{Path, PathBuf};

use everevo_core::EverEvoError;
use serde::{Deserialize, Serialize};

/// Frontmatter parsed from a markdown file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentMeta {
    pub path: PathBuf,
    pub title: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub size_bytes: u64,
    pub modified: String,
}

/// Manages the project knowledge base (docs/llmwiki/ directory).
pub struct LlmwikiManager {
    root: PathBuf,
}

impl LlmwikiManager {
    /// Open a llmwiki manager for the given project root directory.
    pub fn open(root: &Path) -> Result<Self, EverEvoError> {
        Ok(Self {
            root: root.join("docs").join("llmwiki"),
        })
    }

    /// Ensure all expected directories exist.
    pub fn ensure_directories(&self) -> Result<(), EverEvoError> {
        std::fs::create_dir_all(self.root.join("tasks"))
            .map_err(|e| EverEvoError::Internal(format!("Create llmwiki dirs: {e}")))?;
        Ok(())
    }

    /// Path to the design document.
    pub fn design_path(&self) -> PathBuf {
        self.root.join("design.md")
    }

    /// Read the design document if it exists.
    pub fn read_design(&self) -> Result<Option<String>, EverEvoError> {
        let path = self.design_path();
        if path.exists() {
            Ok(Some(std::fs::read_to_string(&path).map_err(|e| {
                EverEvoError::Internal(format!("Read design.md: {e}"))
            })?))
        } else {
            Ok(None)
        }
    }

    /// Write (overwrite) the design document.
    pub fn write_design(&self, content: &str) -> Result<(), EverEvoError> {
        self.ensure_directories()?;
        std::fs::write(self.design_path(), content)
            .map_err(|e| EverEvoError::Internal(format!("Write design.md: {e}")))?;
        Ok(())
    }

    /// Path to the changelog.
    pub fn changelog_path(&self) -> PathBuf {
        self.root.join("changelog.md")
    }

    /// Append a dated entry to the changelog.
    pub fn append_changelog(&self, entry: &str) -> Result<(), EverEvoError> {
        self.ensure_directories()?;
        let path = self.changelog_path();
        let date = chrono::Utc::now().format("%Y-%m-%d");
        let line = format!("- **{date}**: {entry}\n");

        let existing = if path.exists() {
            std::fs::read_to_string(&path).unwrap_or_default()
        } else {
            String::from("# Changelog\n\n")
        };

        std::fs::write(&path, format!("{existing}{line}"))
            .map_err(|e| EverEvoError::Internal(format!("Write changelog.md: {e}")))?;
        Ok(())
    }

    /// Read the changelog.
    pub fn read_changelog(&self) -> Result<Option<String>, EverEvoError> {
        let path = self.changelog_path();
        if path.exists() {
            Ok(Some(std::fs::read_to_string(&path).map_err(|e| {
                EverEvoError::Internal(format!("Read changelog.md: {e}"))
            })?))
        } else {
            Ok(None)
        }
    }

    /// List all task files in `docs/llmwiki/tasks/`.
    pub fn list_tasks(&self) -> Result<Vec<DocumentMeta>, EverEvoError> {
        let tasks_dir = self.root.join("tasks");
        if !tasks_dir.exists() {
            return Ok(vec![]);
        }
        let mut tasks = Vec::new();
        for entry in std::fs::read_dir(&tasks_dir)
            .map_err(|e| EverEvoError::Internal(format!("Read tasks dir: {e}")))?
        {
            let entry = entry.map_err(|e| EverEvoError::Internal(format!("Entry: {e}")))?;
            let path = entry.path();
            if !path.is_file() || path.extension().map_or(true, |e| e != "md") {
                continue;
            }
            let meta = entry
                .metadata()
                .map_err(|e| EverEvoError::Internal(format!("Metadata: {e}")))?;
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| {
                    chrono::DateTime::from_timestamp(
                        t.duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs() as i64,
                        0,
                    )
                })
                .map(|dt| dt.format("%Y-%m-%d").to_string())
                .unwrap_or_default();

            // Try to parse frontmatter for title/description
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            let (title, description, tags) = parse_simple_frontmatter(&content);

            tasks.push(DocumentMeta {
                path,
                title,
                description,
                tags,
                size_bytes: meta.len(),
                modified,
            });
        }
        Ok(tasks)
    }

    /// Read a specific task file.
    pub fn read_task(&self, name: &str) -> Result<Option<String>, EverEvoError> {
        let path = self.root.join("tasks").join(name);
        if path.exists() {
            Ok(Some(std::fs::read_to_string(&path).map_err(|e| {
                EverEvoError::Internal(format!("Read task {name}: {e}"))
            })?))
        } else {
            Ok(None)
        }
    }

    /// Index all documents into a RAG pipeline for semantic search.
    ///
    /// Call this after significant changes to the knowledge base.
    pub fn index_into_rag(&self, rag: &crate::rag::RagPipeline) -> Result<usize, EverEvoError> {
        let mut count = 0;
        let paths = self.collect_all_documents()?;

        for path in &paths {
            let content = std::fs::read_to_string(path).ok().unwrap_or_default();
            if content.is_empty() {
                continue;
            }
            rag.ingest(vec![crate::rag::make_chunk(
                content,
                everevo_vector::ChunkType::Fact,
                vec![],
            )])?;
            count += 1;
        }
        Ok(count)
    }

    /// Collect paths of all markdown documents in the llmwiki tree.
    fn collect_all_documents(&self) -> Result<Vec<PathBuf>, EverEvoError> {
        let mut paths = Vec::new();
        if !self.root.exists() {
            return Ok(paths);
        }
        Self::walk_dir(&self.root, &mut paths)?;
        Ok(paths)
    }

    fn walk_dir(dir: &Path, paths: &mut Vec<PathBuf>) -> Result<(), EverEvoError> {
        for entry in
            std::fs::read_dir(dir).map_err(|e| EverEvoError::Internal(format!("Read dir: {e}")))?
        {
            let entry = entry.map_err(|e| EverEvoError::Internal(format!("Entry: {e}")))?;
            let path = entry.path();
            if path.is_dir() {
                Self::walk_dir(&path, paths)?;
            } else if path.extension().is_some_and(|e| e == "md") {
                paths.push(path);
            }
        }
        Ok(())
    }
}

/// Parse minimal frontmatter (YAML-style `---` delimited block) from markdown.
///
/// Returns `(title, description, tags)`.
fn parse_simple_frontmatter(content: &str) -> (Option<String>, Option<String>, Vec<String>) {
    let mut title = None;
    let mut description = None;
    let mut tags = Vec::new();

    if !content.starts_with("---") {
        return (title, description, tags);
    }

    let rest = &content[3..]; // skip opening `---`
    let end = rest.find("\n---").unwrap_or(rest.len());
    let fm = &rest[..end];

    for line in fm.lines() {
        let line = line.trim();
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim().trim_matches('"').trim_matches('\'');
            match key.trim().to_lowercase().as_str() {
                "title" => title = Some(value.to_string()),
                "description" => description = Some(value.to_string()),
                "tags" => {
                    tags = value
                        .trim_start_matches('[')
                        .trim_end_matches(']')
                        .split(',')
                        .map(|t| t.trim().trim_matches('"').to_string())
                        .filter(|t| !t.is_empty())
                        .collect();
                }
                _ => {}
            }
        }
    }

    (title, description, tags)
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_create_and_write_design() {
        let dir = TempDir::new().unwrap();
        let wiki = LlmwikiManager::open(dir.path()).unwrap();
        wiki.ensure_directories().unwrap();

        wiki.write_design("# Test Project\n\nDesign document.")
            .unwrap();
        let content = wiki.read_design().unwrap();
        assert!(content.is_some());
        assert!(content.unwrap().contains("Test Project"));
    }

    #[test]
    fn test_append_changelog() {
        let dir = TempDir::new().unwrap();
        let wiki = LlmwikiManager::open(dir.path()).unwrap();
        wiki.ensure_directories().unwrap();

        wiki.append_changelog("Initial release").unwrap();
        wiki.append_changelog("Added feature X").unwrap();

        let log = wiki.read_changelog().unwrap().unwrap();
        assert!(log.contains("Initial release"));
        assert!(log.contains("Added feature X"));
    }

    #[test]
    fn test_list_tasks_empty() {
        let dir = TempDir::new().unwrap();
        let wiki = LlmwikiManager::open(dir.path()).unwrap();
        wiki.ensure_directories().unwrap();

        let tasks = wiki.list_tasks().unwrap();
        assert!(tasks.is_empty());
    }

    #[test]
    fn test_list_tasks_with_files() {
        let dir = TempDir::new().unwrap();
        let wiki = LlmwikiManager::open(dir.path()).unwrap();
        wiki.ensure_directories().unwrap();

        std::fs::write(
            wiki.root.join("tasks").join("setup.md"),
            "---\ntitle: Setup\ndescription: Initial setup\n---\n\n# Setup",
        )
        .unwrap();

        let tasks = wiki.list_tasks().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title.as_deref(), Some("Setup"));
    }

    #[test]
    fn test_parse_frontmatter() {
        let content =
            "---\ntitle: My Title\ndescription: A description\ntags: [rust, agent]\n---\n\n# Body";
        let (title, desc, tags) = parse_simple_frontmatter(content);
        assert_eq!(title.unwrap(), "My Title");
        assert_eq!(desc.unwrap(), "A description");
        assert_eq!(tags, vec!["rust", "agent"]);
    }

    #[test]
    fn test_parse_frontmatter_no_frontmatter() {
        let content = "# Just a heading\n\nSome content.";
        let (title, desc, tags) = parse_simple_frontmatter(content);
        assert!(title.is_none());
        assert!(desc.is_none());
        assert!(tags.is_empty());
    }
}
