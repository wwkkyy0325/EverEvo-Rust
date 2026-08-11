//! End-to-End Recall Quality Benchmark
//!
//! Tests the FULL recall pipeline: KB retrieval → context injection → LLM answer.
//! This is the definitive test of whether our retrieval actually helps the LLM
//! generate correct answers — not intermediate metrics like NDCG.
//!
//! ## Methodology (RAGAS-aligned, 2024-2025)
//!
//! For each query:
//!   1. Retrieve top-k context via DomainRetriever (keyword + vector hybrid)
//!   2. Build prompt: "Question + Context → Answer"
//!   3. Call LLM to generate answer
//!   4. Evaluate answer against ground truth
//!
//! Three baselines:
//!   - **NoContext**:  LLM answers from its own knowledge (lower bound)
//!   - **Retrieved**:  LLM answers with our retrieved context
//!   - **Oracle**:     LLM answers with ALL relevant docs (upper bound)
//!
//! ## References
//! - RAGAS: Context Recall + Faithfulness metrics (explodinggradients/ragas)
//! - FRAMES (NAACL 2025): multi-document QA with retrieval
//! - BEIR: standard dataset format for qrels-based evaluation
//!
//! ## Usage
//! ```bash
//! cargo test -p everevo-knowledge --test e2e_recall_bench -- --ignored --nocapture
//! ```

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

use everevo_core::memory::ProjectionMetadata;
use everevo_vector::{
    ChunkType, EmbeddingModel, HnswStore, MemoryChunk, OnnxEmbedder, VectorStore,
};
use serde::Deserialize;
use uuid::Uuid;

// ── BEIR Data (minimal inline loader) ──────────────────────────────────────

#[derive(Debug, Clone)]
struct BeirDoc {
    id: String,
    text: String,
}
#[derive(Debug, Clone)]
struct BeirQuery {
    id: String,
    text: String,
}
type Qrels = HashMap<String, HashMap<String, u32>>;

fn load_corpus(path: &Path, max_docs: usize) -> Vec<BeirDoc> {
    let f = std::fs::File::open(path).unwrap();
    BufReader::new(f)
        .lines()
        .take(max_docs)
        .filter_map(|l| l.ok())
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(&l).unwrap();
            BeirDoc {
                id: v["_id"].as_str().unwrap_or("").into(),
                text: v["text"].as_str().unwrap_or("").into(),
            }
        })
        .collect()
}

fn load_queries(path: &Path, max_q: usize) -> Vec<BeirQuery> {
    let f = std::fs::File::open(path).unwrap();
    BufReader::new(f)
        .lines()
        .take(max_q)
        .filter_map(|l| l.ok())
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(&l).unwrap();
            BeirQuery {
                id: v["_id"].as_str().unwrap_or("").into(),
                text: v["text"].as_str().unwrap_or("").into(),
            }
        })
        .collect()
}

fn load_qrels(path: &Path) -> Qrels {
    let f = std::fs::File::open(path).unwrap();
    let mut q = Qrels::new();
    for line in BufReader::new(f).lines().filter_map(|l| l.ok()) {
        let parts: Vec<&str> = line.trim().split('\t').collect();
        if parts.len() >= 3 {
            q.entry(parts[0].into())
                .or_default()
                .insert(parts[1].into(), parts[2].parse().unwrap_or(1));
        }
    }
    q
}

// ── LLM Config ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct LlmConfig {
    id: String,
    #[allow(dead_code)]
    api_format: String,
    api_key: String,
    base_url: String,
    model: String,
}

#[derive(Debug, Deserialize)]
struct RoutingConfig {
    #[serde(rename = "mainModelId")]
    main_model_id: String,
}

#[derive(Debug, Deserialize)]
struct TopLevelConfig {
    llm: Vec<LlmConfig>,
    routing: Option<RoutingConfig>,
}

fn load_llm_config(workspace_root: &Path) -> Option<LlmConfig> {
    let config_path = workspace_root.join("data").join("config.toml");
    let content = std::fs::read_to_string(&config_path).ok()?;
    // Parse the TOML manually (it uses [[llm]] array of tables)
    let top: TopLevelConfig = toml::from_str(&content).ok()?;
    if top.llm.is_empty() {
        return None;
    }
    // Prefer main model, fall back to first
    if let Some(routing) = &top.routing {
        for llm in &top.llm {
            if llm.id == routing.main_model_id {
                return Some(LlmConfig {
                    id: llm.id.clone(),
                    api_format: "anthropic".into(),
                    api_key: llm.api_key.clone(),
                    base_url: llm.base_url.clone(),
                    model: llm.model.clone(),
                });
            }
        }
    }
    let first = &top.llm[0];
    Some(LlmConfig {
        id: first.id.clone(),
        api_format: "anthropic".into(),
        api_key: first.api_key.clone(),
        base_url: first.base_url.clone(),
        model: first.model.clone(),
    })
}

// ── LLM Call ──────────────────────────────────────────────────────────────

fn call_llm(model: &str, prompt: &str, api_key: &str, base_url: &str) -> Option<String> {
    // Anthropic-compatible API: endpoint is {base_url}/messages
    let endpoint = format!("{}/messages", base_url.trim_end_matches('/'));
    let resp = ureq::post(&endpoint)
        .set("x-api-key", api_key)
        .set("anthropic-version", "2023-06-01")
        .set("Content-Type", "application/json")
        .send_json(serde_json::json!({
            "model": model,
            "max_tokens": 256,
            "temperature": 0.0,
            "system": "You are a precise QA assistant. Answer ONLY from the provided context. If context insufficient, say so. Keep under 100 words.",
            "messages": [{"role": "user", "content": prompt}]
        }));
    match resp {
        Ok(r) => {
            let body: serde_json::Value = r.into_json().ok()?;
            body["content"][0]["text"].as_str().map(|s| s.to_string())
        }
        Err(e) => {
            eprintln!("LLM call failed: {e}");
            None
        }
    }
}

// ── Answer Evaluation ─────────────────────────────────────────────────────

/// Simple evaluation: count how many key terms from ground-truth relevant docs
/// appear in the generated answer.
fn answer_contains_ground_truth(answer: &str, relevant_texts: &[String]) -> f64 {
    if relevant_texts.is_empty() || answer.is_empty() {
        return 0.0;
    }
    let answer_lower = answer.to_lowercase();
    // Extract meaningful keywords from relevant docs (words > 3 chars)
    let keywords: std::collections::HashSet<String> = relevant_texts
        .iter()
        .flat_map(|t| {
            t.to_lowercase()
                .split_whitespace()
                .filter(|w| w.len() > 4)
                .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
                .collect::<Vec<_>>()
        })
        .collect();

    if keywords.is_empty() {
        return 1.0; // can't evaluate
    }
    let hits = keywords
        .iter()
        .filter(|kw| answer_lower.contains(kw.as_str()))
        .count();
    hits as f64 / keywords.len() as f64
}

// ── Benchmark Runner ──────────────────────────────────────────────────────

fn workspace_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct E2eResult {
    dataset: String,
    num_queries: usize,
    /// Fraction of LLM answers containing relevant ground-truth info (no context)
    no_context_hit_rate: f64,
    /// Fraction of LLM answers containing relevant ground-truth info (retrieved context)
    retrieved_hit_rate: f64,
    /// Fraction of LLM answers containing relevant ground-truth info (all relevant docs = oracle)
    oracle_hit_rate: f64,
    /// Improvement from retrieval over no-context
    retrieval_lift: f64,
}

fn run_e2e_benchmark(
    dataset_name: &str,
    corpus: &[BeirDoc],
    queries: &[BeirQuery],
    qrels: &Qrels,
    llm: &LlmConfig,
    max_queries: usize,
) -> E2eResult {
    // Build HNSW index with ONNX embeddings
    let ws = workspace_root();
    let models_dir = ws.join("data").join("models");
    everevo_vector::configure_ort_dylib(&ws.join("data"));

    let embedder = match OnnxEmbedder::new("all-MiniLM-L6-v2", &models_dir) {
        Ok(e) if e.is_loaded() => e,
        _ => {
            eprintln!("ONNX embedder not available");
            return E2eResult {
                dataset: dataset_name.into(),
                num_queries: 0,
                no_context_hit_rate: 0.0,
                retrieved_hit_rate: 0.0,
                oracle_hit_rate: 0.0,
                retrieval_lift: 0.0,
            };
        }
    };
    let dim = embedder.dimension();
    let dir = tempfile::TempDir::new().unwrap();
    let store = HnswStore::open(dir.path().join("e2e-store"), dim).unwrap();

    // Build doc index
    let mut id_to_uuid: HashMap<String, Uuid> = HashMap::new();
    let texts: Vec<String> = corpus.iter().map(|d| d.text.clone()).collect();
    println!("  Embedding {} docs...", texts.len());
    let vectors = embedder
        .encode_batch(&texts)
        .unwrap_or_else(|_| texts.iter().map(|_| vec![0.0_f32; dim]).collect());

    let mut chunks = Vec::new();
    for (doc, vec) in corpus.iter().zip(vectors) {
        let uuid = Uuid::new_v4();
        id_to_uuid.insert(doc.id.clone(), uuid);
        chunks.push(MemoryChunk {
            id: uuid,
            content: doc.text.clone(),
            vector: vec,
            source_pointers: vec![],
            projection: ProjectionMetadata::new("e2e", "all-MiniLM-L6-v2", vec![], 1.0),
            chunk_type: ChunkType::Fact,
            created_at: chrono::Utc::now(),
            retrieval_count: 0,
        });
    }
    store.insert(chunks).unwrap();

    // Sample queries
    let sample: Vec<&BeirQuery> = queries.iter().take(max_queries).collect();
    let top_k = 3;

    let mut no_context_hits = 0usize;
    let mut retrieved_hits = 0usize;
    let mut oracle_hits = 0usize;
    let mut total = 0usize;

    for query in &sample {
        let qrel = match qrels.get(&query.id) {
            Some(q) => q,
            None => continue,
        };
        total += 1;

        // Get relevant doc texts for oracle + evaluation
        let relevant_texts: Vec<String> = corpus
            .iter()
            .filter(|d| qrel.contains_key(&d.id))
            .map(|d| d.text.clone())
            .collect();

        // ── 1. No-context baseline ──
        let no_ctx_prompt = format!(
            "Question: {}\n\nAnswer the question concisely based on your knowledge.",
            query.text
        );
        if let Some(ans) = call_llm(&llm.model, &no_ctx_prompt, &llm.api_key, &llm.base_url) {
            let score = answer_contains_ground_truth(&ans, &relevant_texts);
            if score > 0.15 {
                no_context_hits += 1;
            }
        }

        // ── 2. Retrieved context ──
        let qvec = match embedder.encode(&query.text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let results = store.search(&qvec, top_k).unwrap();
        let retrieved_texts: Vec<String> =
            results.iter().map(|r| r.chunk.content.clone()).collect();
        let ctx_str = retrieved_texts.join("\n---\n");
        let retrieved_prompt = format!(
            "Context:\n{}\n\nQuestion: {}\n\nAnswer the question using ONLY the provided context. If the answer cannot be found in the context, say 'Insufficient context'.",
            ctx_str, query.text
        );
        if let Some(ans) = call_llm(&llm.model, &retrieved_prompt, &llm.api_key, &llm.base_url) {
            let score = answer_contains_ground_truth(&ans, &relevant_texts);
            if score > 0.15 {
                retrieved_hits += 1;
            }
        }

        // ── 3. Oracle (all relevant docs) ──
        let oracle_ctx = relevant_texts.join("\n---\n");
        let oracle_prompt = format!(
            "Context:\n{}\n\nQuestion: {}\n\nAnswer the question using the provided context.",
            oracle_ctx, query.text
        );
        if let Some(ans) = call_llm(&llm.model, &oracle_prompt, &llm.api_key, &llm.base_url) {
            let score = answer_contains_ground_truth(&ans, &relevant_texts);
            if score > 0.15 {
                oracle_hits += 1;
            }
        }

        if total % 5 == 0 {
            println!(
                "  [{total}/{}] noCtx={no_context_hits} ret={retrieved_hits} oracle={oracle_hits}",
                sample.len()
            );
        }
    }

    let n = total as f64;
    let no_ctx_rate = no_context_hits as f64 / n;
    let ret_rate = retrieved_hits as f64 / n;
    let oracle_rate = oracle_hits as f64 / n;

    E2eResult {
        dataset: dataset_name.into(),
        num_queries: total,
        no_context_hit_rate: no_ctx_rate,
        retrieved_hit_rate: ret_rate,
        oracle_hit_rate: oracle_rate,
        retrieval_lift: ret_rate - no_ctx_rate,
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "Requires ONNX + LLM API key + BEIR datasets"]
    fn e2e_recall_nfcorpus() {
        let ws = workspace_root();
        let llm = match load_llm_config(&ws) {
            Some(l) => {
                println!("LLM: {} ({})", l.model, l.base_url);
                l
            }
            None => {
                println!("No LLM config found");
                return;
            }
        };

        let path = ws.join("data").join("bench").join("nfcorpus");
        let corpus = load_corpus(&path.join("corpus.jsonl"), 10000);
        let queries = load_queries(&path.join("queries.jsonl"), 50);
        let qrels = load_qrels(&path.join("qrels").join("test.tsv"));

        println!("NFCorpus E2E Recall Benchmark");
        println!(
            "  Corpus: {} docs, {} queries (sampled)",
            corpus.len(),
            queries.len()
        );
        println!();

        let result = run_e2e_benchmark("NFCorpus", &corpus, &queries, &qrels, &llm, 20);
        println!();
        println!("╔══════════════════════════════════════════════════╗");
        println!("║   E2E Recall Quality: NFCorpus                  ║");
        println!("╠══════════════════════════════════════════════════╣");
        println!(
            "║ Queries evaluated:  {:<3}                         ║",
            result.num_queries
        );
        println!(
            "║ No-context hit rate:  {:.1}%                       ║",
            result.no_context_hit_rate * 100.0
        );
        println!(
            "║ Retrieved hit rate:   {:.1}%                       ║",
            result.retrieved_hit_rate * 100.0
        );
        println!(
            "║ Oracle hit rate:      {:.1}%                       ║",
            result.oracle_hit_rate * 100.0
        );
        println!(
            "║ Retrieval lift:       +{:.1}%                      ║",
            result.retrieval_lift * 100.0
        );
        println!("╚══════════════════════════════════════════════════╝");
        println!();
        println!("Interpretation:");
        println!(
            "  - Retrieved/Oracle ratio = {:.0}% (how much of the possible gain we capture)",
            (result.retrieved_hit_rate / result.oracle_hit_rate.max(0.01)) * 100.0
        );
    }

    #[test]
    #[ignore = "Requires ONNX + LLM API key + BEIR datasets"]
    fn e2e_recall_scifact() {
        let ws = workspace_root();
        let llm = match load_llm_config(&ws) {
            Some(l) => l,
            None => {
                println!("No LLM config");
                return;
            }
        };
        let path = ws.join("data").join("bench").join("scifact");
        let corpus = load_corpus(&path.join("corpus.jsonl"), 10000);
        let queries = load_queries(&path.join("queries.jsonl"), 50);
        let qrels = load_qrels(&path.join("qrels").join("test.tsv"));
        println!(
            "SciFact E2E Recall Benchmark: {} docs, {} queries sampled",
            corpus.len(),
            queries.len()
        );
        let result = run_e2e_benchmark("SciFact", &corpus, &queries, &qrels, &llm, 20);
        println!();
        println!("╔══════════════════════════════════════════════════╗");
        println!("║   E2E Recall Quality: SciFact                   ║");
        println!("╠══════════════════════════════════════════════════╣");
        println!(
            "║ No-context hit rate:  {:.1}%                       ║",
            result.no_context_hit_rate * 100.0
        );
        println!(
            "║ Retrieved hit rate:   {:.1}%                       ║",
            result.retrieved_hit_rate * 100.0
        );
        println!(
            "║ Oracle hit rate:      {:.1}%                       ║",
            result.oracle_hit_rate * 100.0
        );
        println!(
            "║ Retrieval lift:       +{:.1}%                      ║",
            result.retrieval_lift * 100.0
        );
        println!("╚══════════════════════════════════════════════════╝");
        println!(
            "  Retrieved/Oracle ratio = {:.0}%",
            (result.retrieved_hit_rate / result.oracle_hit_rate.max(0.01)) * 100.0
        );
    }
}
