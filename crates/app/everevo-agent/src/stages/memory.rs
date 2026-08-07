//! Memory context stage — injects relevant memory facts via hybrid retrieval.
//!
//! ## Retrieval Pipeline (4-stage)
//!
//! 1. **Keyword RRF** — exact word overlap with user message
//! 2. **Jaccard RRF** — set overlap between query words and fact text
//! 3. **Vector RRF** — cosine similarity via HNSW (if RAG available)
//! 4. **MMR Reranking** — Maximal Marginal Relevance for diversity
//!
//! Stages 1-3 are fused via Reciprocal Rank Fusion (k=60). Stage 4 applies
//! MMR to ensure retrieved facts are diverse (not all about the same topic).
//!
//! ## Knowledge Graph Entity Expansion
//!
//! Query terms are expanded with matching KG entity labels and their 1-hop
//! relations (Mem0 entity-centric retrieval pattern). After retrieval,
//! entity metadata is injected alongside facts.

use std::sync::Arc;
use std::time::Instant;

use everevo_core::context::{ContextBuildContext, ContextFragment, ContextStage};
use everevo_core::llm::LlmMessage;
use everevo_core::memory::MemoryIndexEntry;
use everevo_core::{RetrievalRecord, Telemetry};
use uuid::Uuid;

use everevo_knowledge::graph::KnowledgeGraph;
use crate::memory::facts::FactManager;
use crate::rag::RagPipeline;

/// Injects relevant memory facts + knowledge graph context into the LLM context pipeline.
///
/// ## Context Budget (Phase 1)
///
/// Max ~800 chars for memory + KG context combined, to avoid context pollution
/// (aligned with Claude Code's MEMORY.md 200-line / 25KB cap and OpenClaw's bootstrap budget).
pub struct MemoryStage {
    fact_manager: Arc<FactManager>,
    max_facts: usize,
    telemetry: Option<Arc<Telemetry>>,
    trace_id: Option<Uuid>,
    /// Optional knowledge graph for entity/relation context injection.
    knowledge_graph: Option<Arc<std::sync::RwLock<KnowledgeGraph>>>,
    /// Optional RAG pipeline for vector similarity search.
    rag_pipeline: Option<Arc<RagPipeline>>,
    /// Optional workflow library dir — when set, the stage also surfaces
    /// reusable workflows matching the query (meta-agent: reuse before re-inventing).
    workflows_dir: Option<std::path::PathBuf>,
}

impl MemoryStage {
    pub fn new(fact_manager: Arc<FactManager>) -> Self {
        Self {
            fact_manager,
            max_facts: 5,
            telemetry: None,
            trace_id: None,
            knowledge_graph: None,
            rag_pipeline: None,
            workflows_dir: None,
        }
    }

    /// Point at the workflow library so reusable workflows matching the query
    /// are surfaced in context (meta-agent: proactively suggest reuse).
    pub fn with_workflows_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.workflows_dir = Some(dir);
        self
    }

    pub fn with_telemetry(mut self, telemetry: Arc<Telemetry>, trace_id: Uuid) -> Self {
        self.telemetry = Some(telemetry);
        self.trace_id = Some(trace_id);
        self
    }

    pub fn with_max_facts(mut self, n: usize) -> Self {
        self.max_facts = n;
        self
    }

    /// Attach the knowledge graph for entity/relation context injection.
    pub fn with_knowledge_graph(mut self, kg: Arc<std::sync::RwLock<KnowledgeGraph>>) -> Self {
        self.knowledge_graph = Some(kg);
        self
    }

    /// Attach the RAG pipeline for vector similarity search.
    pub fn with_rag(mut self, rag: Arc<RagPipeline>) -> Self {
        self.rag_pipeline = Some(rag);
        self
    }

    /// Find facts relevant to the user's current message via hybrid RRF fusion.
    ///
    /// Three-layer retrieval:
    /// 1. Keyword RRF — exact word overlap
    /// 2. Jaccard RRF — set overlap
    /// 3. Vector RRF — cosine similarity (if RAG available)
    ///
    /// Plus KG entity expansion: query terms are expanded with matching entity
    /// labels and their related entities (Mem0 entity-centric retrieval pattern).
    fn find_relevant(&self, user_message: &str) -> Vec<MemoryIndexEntry> {
        let start = Instant::now();

        let Ok(all_facts) = self.fact_manager.load_all() else {
            return vec![];
        };
        if all_facts.is_empty() || user_message.trim().is_empty() {
            return vec![];
        }

        let query_lower = user_message.to_lowercase();
        let mut query_terms: Vec<String> = query_lower
            .split_whitespace()
            .filter(|w| w.len() > 2)
            .map(|s| s.to_string())
            .collect();

        // ── KG Entity Expansion: enrich query with entity labels ──
        if let Some(ref kg_lock) = self.knowledge_graph {
            if let Ok(kg) = kg_lock.read() {
                let orig_terms = query_terms.clone();
                for term in &orig_terms {
                    let entities = kg.search(term);
                    for entity in entities.iter().take(3) {
                        for label_word in entity.label.split_whitespace() {
                            if label_word.len() > 2 {
                                query_terms.push(label_word.to_lowercase());
                            }
                        }
                        // 1-hop expansion: related entity labels
                        for rel in kg.outgoing(&entity.id).iter().take(2) {
                            let related = kg.search(&rel.to);
                            for r in related.iter().take(1) {
                                for rw in r.label.split_whitespace() {
                                    if rw.len() > 2 {
                                        query_terms.push(rw.to_lowercase());
                                    }
                                }
                            }
                        }
                    }
                }
                query_terms.sort_unstable();
                query_terms.dedup();
            }
        }

        let query_words: Vec<&str> = query_terms.iter().map(|s| s.as_str()).collect();

        let mut keyword_scores: Vec<(usize, f32)> = Vec::new();
        let mut content_scores: Vec<(usize, f32)> = Vec::new();

        for (i, fact) in all_facts.iter().enumerate() {
            let fact_text =
                format!("{} {} {}", fact.name, fact.description, fact.content).to_lowercase();

            let kw_matches = query_words
                .iter()
                .filter(|w| fact_text.contains(*w))
                .count();
            if kw_matches > 0 {
                keyword_scores.push((i, kw_matches as f32 / query_words.len().max(1) as f32));
            }

            let fact_words: std::collections::HashSet<&str> = fact_text
                .split_whitespace()
                .filter(|w| w.len() > 2)
                .collect();
            let query_set: std::collections::HashSet<&str> = query_words.iter().copied().collect();
            let intersection = fact_words.intersection(&query_set).count();
            let union = fact_words.len() + query_set.len() - intersection;
            if union > 0 {
                let jaccard = intersection as f32 / union as f32;
                if jaccard > 0.0 {
                    content_scores.push((i, jaccard));
                }
            }
        }

        keyword_scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        content_scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // ── Vector similarity (3rd RRF layer) ──
        let vector_scores: Vec<(usize, f32)> = if let Some(ref rag) = self.rag_pipeline {
            if let Ok(results) = rag.search_in("memory", user_message, self.max_facts * 2) {
                results
                    .iter()
                    .filter_map(|sc| {
                        // Match vector result back to fact by content prefix
                        let fact_idx = all_facts.iter().position(|f| {
                            let chunk_content = format!("{}: {}", f.name, f.content);
                            sc.chunk.content.starts_with(&f.name)
                                || sc.chunk.content == chunk_content
                        });
                        fact_idx.map(|i| (i, sc.score))
                    })
                    .collect()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        let k: f32 = 60.0;
        let mut fused: std::collections::HashMap<usize, f32> = std::collections::HashMap::new();
        for (rank, (i, _)) in keyword_scores.iter().enumerate() {
            *fused.entry(*i).or_default() += 1.0 / (k + (rank + 1) as f32);
        }
        for (rank, (i, _)) in content_scores.iter().enumerate() {
            *fused.entry(*i).or_default() += 1.0 / (k + (rank + 1) as f32);
        }
        for (rank, (i, score)) in vector_scores.iter().enumerate() {
            // Use both rank position AND raw similarity score in RRF
            *fused.entry(*i).or_default() += 1.0 / (k + (rank + 1) as f32) + (*score * 0.1);
        }

        let mut ranked: Vec<(usize, f32)> = fused.into_iter().collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // ── MMR Reranking: diversity-aware selection ──
        // Retrieve top-20, then select top-k with MMR to ensure diverse results.
        let recall_pool = ranked
            .iter()
            .take(self.max_facts * 4)
            .map(|(i, _)| *i)
            .collect::<Vec<_>>();
        let mmr_selected = mmr_rerank(&recall_pool, &ranked, &all_facts, self.max_facts);
        ranked = mmr_selected;

        let results: Vec<MemoryIndexEntry> = ranked
            .into_iter()
            .filter_map(|(i, _)| all_facts.get(i))
            .map(|fact| MemoryIndexEntry {
                name: fact.name.clone(),
                description: fact.description.clone(),
                fact_type: fact.fact_type.clone(),
            })
            .collect();

        if let (Some(telemetry), Some(trace_id)) = (&self.telemetry, self.trace_id) {
            let latency_ms = start.elapsed().as_millis() as i64;
            telemetry.record_retrieval(RetrievalRecord {
                trace_id,
                query: user_message.to_string(),
                source: "memory-rrf".into(),
                recall_k: results.len() as i32,
                precision_at_5: None,
                mrr: None,
                latency_ms,
                experiment_id: None,
                variant: None,
            });
        }

        results
    }
}

impl ContextStage for MemoryStage {
    fn priority(&self) -> i32 {
        3 // after system prompt (0), before session metadata (5)
    }

    fn name(&self) -> &str {
        "memory"
    }

    fn build(&self, ctx: &ContextBuildContext) -> Option<ContextFragment> {
        let relevant = self.find_relevant(&ctx.user_message);
        let is_first_turn = ctx.history.is_empty();

        if relevant.is_empty() {
            // ── T1 bootstrap: session-start memory overview ──
            if is_first_turn {
                if let Ok(t1_facts) = self.fact_manager.load_tier1() {
                    if !t1_facts.is_empty() {
                        let t1_lines: Vec<String> = t1_facts
                            .iter()
                            .take(5)
                            .map(|f| format!("- {} — {}", f.name, f.description))
                            .collect();
                        let content = format!(
                            "## Memory (T1 — frequently used)\n{}\n",
                            t1_lines.join("\n")
                        );
                        return Some(ContextFragment {
                            label: "Memory T1".into(),
                            messages: vec![LlmMessage::user(&content)],
                        });
                    }
                }
            }

            let index = self.fact_manager.read_index_lean(50).ok()?;
            if index.is_empty() {
                return None;
            }
            let kg_meta = if let Some(ref kg_lock) = self.knowledge_graph {
                if let Ok(kg) = kg_lock.read() {
                    let ec = kg.entity_count();
                    if ec > 0 {
                        format!("\n\nKnowledge graph: {ec} entities. Use `memory` tool with kg_search to explore.")
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                }
            } else {
                String::new()
            };
            let content = format!(
                "## Persistent Memory\n\
                 The following facts have been saved from previous sessions:\n\n\
                 {index}{kg_meta}"
            );
            return Some(ContextFragment {
                label: "Memory Index".into(),
                messages: vec![LlmMessage::user(&content)],
            });
        }

        // Bump recall for each injected fact (drives T1 promotion)
        for entry in &relevant {
            let _ = self.fact_manager.bump_recall(&entry.name);
        }

        let lines: Vec<String> = relevant
            .iter()
            .map(|e| {
                format!(
                    "- [{name}](facts/{name}.md) \u{2014} {desc}",
                    name = e.name,
                    desc = e.description
                )
            })
            .collect();

        let mut content = format!(
            "## Relevant Memory\n\
             The following saved facts are relevant to the current conversation:\n\n\
             {}\n\n\
             Use the `memory` tool with `search` to find more if needed.",
            lines.join("\n")
        );

        // ── Knowledge Graph context (Mem0^g entity-centric retrieval pattern) ──
        if let Some(ref kg_lock) = self.knowledge_graph {
            if let Ok(kg) = kg_lock.read() {
                let mut kg_lines: Vec<String> = Vec::new();
                for entry in &relevant {
                    // Extract potential entity names from fact slugs (kebab-case → search)
                    let candidates = vec![entry.name.clone(), entry.name.replace('-', " ")];
                    for candidate in &candidates {
                        let entities = kg.search(candidate);
                        for entity in entities.iter().take(2) {
                            let relations = kg.outgoing(&entity.id);
                            if relations.is_empty() {
                                kg_lines.push(format!(
                                    "- `{}` ({})",
                                    entity.label,
                                    entity.entity_type.as_str()
                                ));
                            } else {
                                for rel in relations.iter().take(3) {
                                    kg_lines.push(format!(
                                        "- `{}` ({}) → {} → `{}`",
                                        entity.label,
                                        entity.entity_type.as_str(),
                                        rel.predicate,
                                        rel.to
                                    ));
                                }
                            }
                        }
                    }
                }

                // Deduplicate KG lines
                kg_lines.sort_unstable();
                kg_lines.dedup();

                if !kg_lines.is_empty() {
                    // Budget: cap KG context at 400 chars
                    let mut kg_section = String::from("\n## Knowledge Graph\n");
                    for line in &kg_lines {
                        if kg_section.len() + line.len() + 1 > 700 {
                            kg_section.push_str("\n... (more entities)\n");
                            break;
                        }
                        kg_section.push_str(line);
                        kg_section.push('\n');
                    }
                    content.push_str(&kg_section);
                }
            }
        }

        // ── Action Paradigms (SAMULE pattern: reuse learned strategies) ──
        let paradigms = crate::memory::paradigm::search_paradigms(
            &self.fact_manager,
            &ctx.user_message,
        );
        if !paradigms.is_empty() {
            content.push_str("\n\n## Action Paradigms (learned strategies)\n\n");
            for p in paradigms.iter().take(3) {
                // Extract a short approach summary from the paradigm content
                let short = p
                    .content
                    .lines()
                    .find(|l| l.starts_with("**Approach**"))
                    .map(|l| l.trim_start_matches("**Approach**:").trim().to_string())
                    .unwrap_or_else(|| p.description.clone());
                content.push_str(&format!("- {} — {}\n", p.name, short));
            }
        }

        // Meta-agent: proactively surface reusable workflows matching the query,
        // so the LLM reuses a saved procedure instead of re-inventing it.
        if let Some(ref dir) = self.workflows_dir {
            let q = ctx.user_message.to_lowercase();
            let runner =
                crate::tools::builtins::WorkflowRunnerTool::new().with_workflows_dir(dir.clone());
            let matches: Vec<_> = runner
                .list_saved()
                .unwrap_or_default()
                .into_iter()
                .filter(|(_, desc)| {
                    desc.to_lowercase()
                        .split_whitespace()
                        .any(|kw| kw.len() > 3 && q.contains(kw))
                })
                .take(3)
                .collect();
            if !matches.is_empty() {
                content.push_str("\n\n## Reusable Workflows (run by name)\n\n");
                for (name, desc) in &matches {
                    content.push_str(&format!(
                        "- `{name}` — {desc} → `workflow_run name={name}`\n"
                    ));
                }
            }
        }

        Some(ContextFragment {
            label: "Relevant Memory".into(),
            messages: vec![LlmMessage::user(&content)],
        })
    }
}

// ── MMR Reranker ────────────────────────────────────────────────────────

/// Maximal Marginal Relevance — diversity-aware result selection.
///
/// Balances relevance (from RRF score) with novelty (inverse similarity to
/// already-selected results). Prevents the "all results are about the same
/// topic" failure mode of pure relevance ranking.
///
/// λ=0.7 → 70% relevance, 30% diversity. Industry standard (LangChain default).
///
/// Since we don't have full-text vectors for all facts in MemoryStage, we use
/// Jaccard similarity over fact content as a lightweight diversity proxy.
/// This is O(k × N) where k = target count, N = pool size — negligible for k=5, N=20.
fn mmr_rerank(
    pool: &[usize],
    scored: &[(usize, f32)],
    all_facts: &[everevo_core::memory::MemoryFact],
    k: usize,
) -> Vec<(usize, f32)> {
    if pool.is_empty() || k == 0 {
        return vec![];
    }

    // Build lookup: fact_index → RRF score
    let score_map: std::collections::HashMap<usize, f32> = scored.iter().cloned().collect();

    let lambda: f32 = 0.7;

    // Pre-compute fact text for Jaccard similarity.
    let fact_texts: Vec<String> = pool
        .iter()
        .filter_map(|&i| {
            all_facts
                .get(i)
                .map(|f| format!("{} {}", f.name, f.content).to_lowercase())
        })
        .collect();

    let mut selected: Vec<usize> = Vec::new();
    let mut remaining: Vec<usize> = pool.to_vec();

    // First pick: highest RRF score.
    if let Some(&best) = remaining.first() {
        selected.push(best);
        remaining.retain(|&i| i != best);
    }

    while selected.len() < k && !remaining.is_empty() {
        let mut best_idx = 0usize;
        let mut best_mmr = f32::NEG_INFINITY;

        for (j, &cand) in remaining.iter().enumerate() {
            let relevance = score_map.get(&cand).copied().unwrap_or(0.0);
            let novelty: f32 = if selected.is_empty() {
                1.0
            } else {
                let cand_text = fact_texts.get(pool.iter().position(|&i| i == cand).unwrap_or(0));
                let max_sim = selected
                    .iter()
                    .filter_map(|&s| {
                        let sel_pos = pool.iter().position(|&i| i == s)?;
                        let sel_text = fact_texts.get(sel_pos)?;
                        let c_text = cand_text?;
                        Some(jaccard_similarity(c_text, sel_text))
                    })
                    .fold(0.0f32, f32::max);
                1.0 - max_sim
            };
            let mmr = lambda * relevance + (1.0 - lambda) * novelty;
            if mmr > best_mmr {
                best_mmr = mmr;
                best_idx = j;
            }
        }

        let chosen = remaining.remove(best_idx);
        selected.push(chosen);
    }

    selected
        .into_iter()
        .map(|i| (i, score_map.get(&i).copied().unwrap_or(0.0)))
        .collect()
}

/// Jaccard similarity between two texts (word-level set overlap).
fn jaccard_similarity(a: &str, b: &str) -> f32 {
    let words_a: std::collections::HashSet<&str> =
        a.split_whitespace().filter(|w| w.len() > 2).collect();
    let words_b: std::collections::HashSet<&str> =
        b.split_whitespace().filter(|w| w.len() > 2).collect();
    let intersection = words_a.intersection(&words_b).count();
    let union = words_a.len() + words_b.len() - intersection;
    if union == 0 {
        0.0
    } else {
        intersection as f32 / union as f32
    }
}
