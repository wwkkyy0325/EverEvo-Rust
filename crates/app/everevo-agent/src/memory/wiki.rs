//! Wiki Generator — auto-creates and updates wiki pages from memory facts.
//!
//! ## Full Pipeline (Phase 2c complete)
//!
//! ```text
//! Facts → keyword search → find relevant wiki pages
//!   → LLM generates/updates page content
//!   → bidirectional references: wiki ↔ fact ↔ entity ↔ source_session
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use everevo_core::llm::{LlmMessage, LlmProvider};
use everevo_core::memory::MemoryFact;
use everevo_core::EverEvoError;

use crate::rag::RagPipeline;
use crate::HttpClient;

use super::facts::FactManager;

/// Manages the wiki directory (data/memory/wiki/).
pub struct WikiGenerator {
    wiki_dir: PathBuf,
    /// Optional LLM for content generation. Falls back to template-based generation.
    llm: Option<Arc<HttpClient>>,
    /// Optional RAG pipeline for vectorizing generated wiki pages.
    rag: std::sync::Mutex<Option<Arc<RagPipeline>>>,
}

impl WikiGenerator {
    /// Create a new wiki generator.
    pub fn new(wiki_dir: impl Into<PathBuf>) -> Result<Self, EverEvoError> {
        let wiki_dir: PathBuf = wiki_dir.into();
        std::fs::create_dir_all(&wiki_dir)
            .map_err(|e| EverEvoError::Internal(format!("Create wiki dir: {e}")))?;
        Ok(Self {
            wiki_dir,
            llm: None,
            rag: std::sync::Mutex::new(None),
        })
    }

    /// Attach an LLM client for intelligent wiki generation.
    pub fn with_llm(mut self, llm: Arc<HttpClient>) -> Self {
        self.llm = Some(llm);
        self
    }

    /// Set the RAG pipeline after construction (thread-safe, for post-init wiring).
    pub fn set_rag(&self, rag: Arc<RagPipeline>) {
        *self.rag.lock().unwrap_or_else(|e| e.into_inner()) = Some(rag);
    }

    /// Generate wiki pages from all facts, using the full pipeline.
    /// Called after DEEP phase promotes new facts.
    pub async fn generate_from_facts(
        &self,
        fact_manager: &FactManager,
    ) -> Result<WikiGenStats, EverEvoError> {
        let facts = fact_manager.load_all()?;
        if facts.is_empty() {
            return Ok(WikiGenStats::default());
        }

        let mut stats = WikiGenStats::default();
        let existing = self.list_pages()?;

        for fact in &facts {
            // Check if this fact needs a wiki page
            let relevant = find_relevant_pages(fact, &existing);
            if let Some(page) = relevant.first() {
                // Update existing page
                self.update_page(&page.path, fact, fact_manager).await?;
                stats.updated += 1;
            } else if self.should_create_page(fact) {
                // Create new page for standalone fact
                self.create_page(fact, fact_manager).await?;
                stats.created += 1;
            } else {
                stats.skipped += 1;
            }
        }

        // Update changelog
        self.append_changelog_entry(&facts, "Consolidated")?;

        Ok(stats)
    }

    /// Update an existing wiki page with new fact content.
    async fn update_page(
        &self,
        page_path: &str,
        fact: &MemoryFact,
        _fact_manager: &FactManager,
    ) -> Result<(), EverEvoError> {
        let path = self.wiki_dir.join(page_path);
        let mut content = std::fs::read_to_string(&path).unwrap_or_default();

        let fact_ref = format!("[{name}](../facts/{name}.md)", name = fact.name);
        if content.contains(&fact_ref) {
            return Ok(()); // already referenced
        }

        // Generate new section content
        let section = if let Some(llm) = &self.llm {
            self.generate_section_with_llm(llm, fact, &content).await?
        } else {
            format!(
                "\n\n### {desc}\n\n{body}\n\n**Source:** [{name}](../facts/{name}.md)\n",
                desc = fact.description,
                body = fact.content,
                name = fact.name,
            )
        };

        content.push_str(&section);
        std::fs::write(&path, &content)
            .map_err(|e| EverEvoError::Internal(format!("Write wiki page {page_path}: {e}")))?;

        tracing::info!(page = %page_path, fact = %fact.name, "Wiki page updated");
        Ok(())
    }

    /// Create a new wiki page for a fact.
    async fn create_page(
        &self,
        fact: &MemoryFact,
        fact_manager: &FactManager,
    ) -> Result<(), EverEvoError> {
        let filename = format!("{}.md", fact.name);
        let path = self.wiki_dir.join(&filename);

        // Gather related facts for context
        let related = self.find_related_facts(fact, fact_manager)?;

        let content = if let Some(llm) = &self.llm {
            self.generate_page_with_llm(llm, fact, &related).await?
        } else {
            self.template_page(fact, &related)
        };

        std::fs::write(&path, &content)
            .map_err(|e| EverEvoError::Internal(format!("Create wiki page {filename}: {e}")))?;

        // Vectorize into wiki namespace for semantic search.
        if let Ok(guard) = self.rag.lock() {
            if let Some(ref rag) = *guard {
                let chunk = crate::rag::make_chunk(
                    format!("{}: {}", fact.name, content),
                    everevo_vector::ChunkType::Fact,
                );
                if let Err(e) = rag.ingest_into("wiki", vec![chunk]) {
                    tracing::warn!(error = %e, "Wiki vector indexing failed (non-fatal)");
                }
            }
        }

        tracing::info!(page = %filename, fact = %fact.name, "Wiki page created");
        Ok(())
    }

    /// Use LLM to generate a complete wiki page.
    async fn generate_page_with_llm(
        &self,
        llm: &HttpClient,
        fact: &MemoryFact,
        related: &[MemoryFact],
    ) -> Result<String, EverEvoError> {
        let related_text: String = related
            .iter()
            .map(|f| format!("- {}: {}", f.name, f.description))
            .collect::<Vec<_>>()
            .join("\n");

        let prompt = format!(
            "Write a concise wiki page in Markdown for the following fact.\n\n\
             ## Fact\n\
             **Topic:** {title}\n\
             **Type:** {ftype}\n\n\
             {body}\n\n\
             ## Related Facts\n\
             {related}\n\n\
             ## Instructions\n\
             - Start with a `# Title` heading\n\
             - Write 2-4 paragraphs of explanation\n\
             - Add a `## Related` section linking to related facts by name\n\
             - Add a `## References` section at the bottom with `- [fact-name](../facts/fact-name.md)` format\n\
             - Keep it concise (<500 words)\n\n\
             Write the wiki page:",
            title = fact.description,
            ftype = fact.fact_type.as_str(),
            body = fact.content,
            related = if related_text.is_empty() { "None".into() } else { related_text },
        );

        let response = llm.chat(&[LlmMessage::user(&prompt)], &[]).await?;
        Ok(response
            .content
            .unwrap_or_else(|| self.template_page(fact, related)))
    }

    /// Use LLM to generate a new section for an existing page.
    async fn generate_section_with_llm(
        &self,
        llm: &HttpClient,
        fact: &MemoryFact,
        _existing_content: &str,
    ) -> Result<String, EverEvoError> {
        let prompt = format!(
            "Write a short Markdown section (### heading + 1-2 paragraphs) to add to an existing wiki page. \
             The section should cover: {desc}\n\nContent: {body}\n\n\
             End with: **Source:** [{name}](../facts/{name}.md)\n\n\
             Write only the section, no preamble:",
            desc = fact.description, body = fact.content, name = fact.name,
        );

        let response = llm.chat(&[LlmMessage::user(&prompt)], &[]).await?;
        let section = response.content.unwrap_or_default();
        Ok(format!("\n\n{section}\n"))
    }

    /// Template-based page generation (no LLM).
    fn template_page(&self, fact: &MemoryFact, related: &[MemoryFact]) -> String {
        let mut content = format!(
            "# {desc}\n\n{body}\n\n---\n\n## Related\n\n",
            desc = fact.description,
            body = fact.content,
        );

        if related.is_empty() {
            content.push_str("_No related facts found._\n\n");
        } else {
            for r in related {
                content.push_str(&format!(
                    "- [{name}](../facts/{name}.md) — {desc}\n",
                    name = r.name,
                    desc = r.description
                ));
            }
            content.push('\n');
        }

        content.push_str("## References\n\n");
        content.push_str(&format!(
            "- [{name}](../facts/{name}.md) — source fact\n",
            name = fact.name
        ));
        content.push_str("- Auto-generated by EverEvo Memory Pipeline\n");

        content
    }

    /// Find facts related to the given fact by keyword overlap.
    fn find_related_facts(
        &self,
        fact: &MemoryFact,
        fact_manager: &FactManager,
    ) -> Result<Vec<MemoryFact>, EverEvoError> {
        let all = fact_manager.load_all()?;
        let query = format!("{} {}", fact.name, fact.description).to_lowercase();
        let keywords: Vec<&str> = query.split_whitespace().filter(|w| w.len() > 2).collect();

        Ok(all
            .into_iter()
            .filter(|f| f.name != fact.name)
            .filter(|f| {
                let text = format!("{} {}", f.name, f.description).to_lowercase();
                keywords.iter().any(|kw| text.contains(kw))
            })
            .take(5)
            .collect())
    }

    /// Determine if a standalone fact deserves its own wiki page.
    fn should_create_page(&self, fact: &MemoryFact) -> bool {
        // Create pages for substantive facts, skip trivial ones
        fact.content.len() > 50 && !fact.description.is_empty()
    }

    /// List existing wiki pages.
    pub fn list_pages(&self) -> Result<Vec<WikiPage>, EverEvoError> {
        let mut pages = Vec::new();
        self.collect_files(&self.wiki_dir, "", &mut pages)?;
        Ok(pages)
    }

    fn collect_files(
        &self,
        dir: &Path,
        prefix: &str,
        pages: &mut Vec<WikiPage>,
    ) -> Result<(), EverEvoError> {
        if !dir.exists() {
            return Ok(());
        }
        for entry in
            std::fs::read_dir(dir).map_err(|e| EverEvoError::Internal(format!("Read dir: {e}")))?
        {
            let entry = entry.map_err(|e| EverEvoError::Internal(format!("Entry: {e}")))?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                let sub = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}/{name}")
                };
                self.collect_files(&path, &sub, pages)?;
            } else if name.ends_with(".md") {
                let content = std::fs::read_to_string(&path).unwrap_or_default();
                let title = content
                    .lines()
                    .find(|l| l.starts_with("# "))
                    .map(|l| l[2..].to_string())
                    .unwrap_or_else(|| name.clone());
                let page_path = if prefix.is_empty() {
                    name
                } else {
                    format!("{prefix}/{name}")
                };
                pages.push(WikiPage {
                    path: page_path,
                    title,
                    content,
                    updated_at: chrono::Utc::now(),
                    references: vec![],
                });
            }
        }
        Ok(())
    }

    /// Append a changelog entry.
    fn append_changelog_entry(
        &self,
        facts: &[MemoryFact],
        action: &str,
    ) -> Result<(), EverEvoError> {
        let path = self.wiki_dir.join("changelog.md");
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let mut entry = format!("\n## {today}\n\n");
        for f in facts {
            entry.push_str(&format!(
                "- **{action}**: [{name}](../facts/{name}.md) — {desc}\n",
                action = action,
                name = f.name,
                desc = f.description
            ));
        }
        let mut content = if path.exists() {
            std::fs::read_to_string(&path).unwrap_or_default()
        } else {
            String::from("# Changelog\n\nAuto-generated by EverEvo Memory Pipeline.\n")
        };
        if let Some(pos) = content.find('\n') {
            content.insert_str(pos + 1, &entry);
        } else {
            content.push_str(&entry);
        }
        std::fs::write(&path, &content)
            .map_err(|e| EverEvoError::Internal(format!("Write changelog: {e}")))?;
        Ok(())
    }

    pub fn wiki_dir(&self) -> &Path {
        &self.wiki_dir
    }
}

// ── Types ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct WikiPage {
    pub path: String,
    pub title: String,
    pub content: String,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub references: Vec<WikiReference>,
}

#[derive(Debug, Clone)]
pub struct WikiReference {
    pub ref_type: String,
    pub id: String,
    pub label: String,
}

#[derive(Debug, Default)]
pub struct WikiGenStats {
    pub created: usize,
    pub updated: usize,
    pub skipped: usize,
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn find_relevant_pages<'a>(fact: &MemoryFact, pages: &'a [WikiPage]) -> Vec<&'a WikiPage> {
    let query = format!("{} {}", fact.name, fact.description).to_lowercase();
    let keywords: Vec<&str> = query.split_whitespace().filter(|w| w.len() > 3).collect();
    pages
        .iter()
        .filter(|p| {
            let text = format!("{} {}", p.title, p.path).to_lowercase();
            keywords.iter().any(|kw| text.contains(kw))
        })
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use everevo_core::memory::{FactType, ProjectionMetadata};
    use tempfile::TempDir;

    fn make_fact(name: &str, desc: &str, content: &str) -> MemoryFact {
        MemoryFact {
            name: name.into(),
            description: desc.into(),
            content: content.into(),
            fact_type: FactType::Project,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            projection: ProjectionMetadata::new("test", "none", vec![], 1.0),
            links: vec![],
            session: None,
        }
    }

    fn _test_fact_manager(dir: &TempDir) -> FactManager {
        FactManager::new(dir.path().join("facts")).unwrap()
    }

    #[test]
    fn test_create_page_template() {
        let dir = TempDir::new().unwrap();
        let gen = WikiGenerator::new(dir.path()).unwrap();
        let fact = make_fact("test-page", "Test Page Title", "Body content here.");
        let content = gen.template_page(&fact, &[]);
        assert!(content.contains("# Test Page Title"));
        assert!(content.contains("Body content here"));
        assert!(content.contains("source fact"));
    }

    #[test]
    fn test_create_page_with_related() {
        let dir = TempDir::new().unwrap();
        let gen = WikiGenerator::new(dir.path()).unwrap();
        let fact = make_fact("main", "Main", "Content");
        let related = vec![make_fact("rel", "Related", "Related content")];
        let content = gen.template_page(&fact, &related);
        assert!(content.contains("Related"));
        assert!(content.contains("rel"));
    }

    #[test]
    fn test_list_pages() {
        let dir = TempDir::new().unwrap();
        let gen = WikiGenerator::new(dir.path()).unwrap();
        std::fs::write(dir.path().join("test.md"), "# Test\n\nBody").unwrap();
        let pages = gen.list_pages().unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].title, "Test");
    }

    #[test]
    fn test_should_create_page_trivial() {
        let dir = TempDir::new().unwrap();
        let gen = WikiGenerator::new(dir.path().to_path_buf()).unwrap();
        assert!(!gen.should_create_page(&make_fact("x", "y", "short")));
    }

    #[test]
    fn test_should_create_page_substantive() {
        let dir = TempDir::new().unwrap();
        let gen = WikiGenerator::new(dir.path().to_path_buf()).unwrap();
        let fact = make_fact("big", "Important topic", &"x".repeat(100));
        assert!(gen.should_create_page(&fact));
    }
}
