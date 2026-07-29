//! Domain knowledge context stage — injects relevant domain chunks into the agent's prompt.
//!
//! ## Architecture
//!
//! Implements `everevo_core::context::ContextStage` to sit between memory (priority 3)
//! and conversation history (priority 80) in the context pipeline. When the user asks
//! about a topic that matches domain documents, relevant chunks are injected.
//!
//! ## Design
//!
//! Uses the same RRF fusion approach as `MemoryStage`: keyword matching + content overlap
//! to find the most relevant domain documents and chunks for the current conversation.

use std::path::PathBuf;

use crate::knowledge::domain::DomainManager;
use everevo_core::context::{ContextBuildContext, ContextFragment, ContextStage};
use everevo_core::llm::LlmMessage;

/// Injects relevant domain knowledge chunks into the agent's context.
///
/// Priority 4 — after memory (3), before session metadata (5).
pub struct DomainKnowledgeStage {
    domain_root: PathBuf,
    /// Maximum number of relevant documents to inject.
    max_docs: usize,
    /// Maximum number of chunks per document.
    max_chunks_per_doc: usize,
}

impl DomainKnowledgeStage {
    pub fn new(domain_root: impl Into<PathBuf>) -> Self {
        Self {
            domain_root: domain_root.into(),
            max_docs: 3,
            max_chunks_per_doc: 2,
        }
    }

    pub fn with_max_docs(mut self, n: usize) -> Self {
        self.max_docs = n;
        self
    }

    /// Search domain documents relevant to the query.
    fn find_relevant(&self, query: &str) -> Vec<DomainMatch> {
        let mut matches = Vec::new();
        let query_lower = query.to_lowercase();
        let query_words: Vec<&str> = query_lower
            .split_whitespace()
            .filter(|w| w.len() > 2)
            .collect();

        let Ok(mgr) = DomainManager::load(&self.domain_root) else {
            return matches;
        };

        for (id, domain) in &mgr.registry.domains {
            if domain.merged_into.is_some() {
                continue;
            }

            // Domain name match
            let name_score = if domain.name.to_lowercase().contains(&query_lower)
                || query_lower.contains(&domain.name.to_lowercase())
            {
                0.9
            } else if query_words
                .iter()
                .any(|w| domain.name.to_lowercase().contains(*w))
            {
                0.5
            } else {
                0.0
            };

            // Description match
            let desc_score = if domain.description.to_lowercase().contains(&query_lower) {
                0.3
            } else {
                (query_words
                    .iter()
                    .filter(|w| domain.description.to_lowercase().contains(*w))
                    .count() as f32)
                    / (query_words.len().max(1) as f32)
                    * 0.3
            };

            let score: f32 = f32::min(name_score + desc_score, 1.0_f32);
            if score > 0.0 {
                // Get document content snippets
                let chunks = self.load_domain_chunks(&mgr, id);
                if !chunks.is_empty() {
                    matches.push(DomainMatch {
                        domain_name: domain.name.clone(),
                        domain_id: id.clone().to_string(),
                        description: domain.description.clone(),
                        score,
                        chunks,
                    });
                }
            }
        }

        matches.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        matches.truncate(self.max_docs);
        matches
    }

    /// Load content chunks from a domain's documents.
    fn load_domain_chunks(&self, mgr: &DomainManager, domain_id: &str) -> Vec<String> {
        let mut chunks = Vec::new();
        let Ok(docs) = mgr.list_documents(domain_id) else {
            return chunks;
        };

        for doc in docs.iter().take(self.max_chunks_per_doc) {
            let doc_path = self
                .domain_root
                .join(domain_id)
                .join("documents")
                .join(&doc.filename);
            if doc_path.exists() {
                if let Ok(content) = std::fs::read_to_string(&doc_path) {
                    let preview: String = content.lines().take(20).collect::<Vec<_>>().join("\n");
                    chunks.push(preview);
                }
            }
        }
        chunks
    }
}

impl ContextStage for DomainKnowledgeStage {
    fn priority(&self) -> i32 {
        4 // after memory (3), before session metadata (5)
    }

    fn name(&self) -> &str {
        "domain_knowledge"
    }

    fn build(&self, ctx: &ContextBuildContext) -> Option<ContextFragment> {
        if ctx.user_message.is_empty() {
            return None;
        }

        let matches = self.find_relevant(&ctx.user_message);
        if matches.is_empty() {
            return None;
        }

        let mut lines = Vec::new();
        lines.push("## Domain Knowledge".into());
        lines.push(
            "The following knowledge-base documents are relevant to the current conversation:\n"
                .into(),
        );

        for m in &matches {
            lines.push(format!(
                "### [{name}](domain/{id}) — {desc}",
                name = m.domain_name,
                id = m.domain_id,
                desc = m.description
            ));
            for (i, chunk) in m.chunks.iter().enumerate() {
                lines.push(format!(
                    "--- Document excerpt {} (domain: {name}) ---\n{chunk}\n",
                    i + 1,
                    name = m.domain_name,
                ));
            }
        }

        lines.push("Use the domain search tool or POST /api/domains/search to find more specific documents.".into());

        Some(ContextFragment {
            label: "Domain Knowledge".into(),
            messages: vec![LlmMessage::user(lines.join("\n"))],
        })
    }
}

struct DomainMatch {
    domain_name: String,
    domain_id: String,
    description: String,
    score: f32,
    chunks: Vec<String>,
}
