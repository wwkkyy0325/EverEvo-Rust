//! BEIR-format retrieval benchmark harness.
//!
//! Loads BEIR datasets (corpus.jsonl, queries.jsonl, qrels.tsv),
//! indexes them with the HNSW vector store, and evaluates retrieval
//! quality using NDCG@k, Recall@k, MRR, and Precision@k.
//!
//! ## Usage
//!
//! Download a BEIR dataset (e.g., NFCorpus) and place it under `data/bench/`:
//! ```bash
//! pip install beir
//! python -c "
//! from beir.datasets.data_loader import GenericDataLoader
//! from beir import util
//! import json, os
//! url = 'https://public.ukp.informatik.tu-darmstadt.de/thakur/BEIR/datasets/nfcorpus.zip'
//! data_path = util.download_and_unzip(url, 'data/bench/nfcorpus')
//! "
//! ```
//!
//! Then run with:
//! ```bash
//! cargo test -p everevo-knowledge --test beir_benchmark -- --ignored --nocapture
//! ```

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

use everevo_core::memory::ProjectionMetadata;
use everevo_knowledge::metrics::{ndcg_at_k, precision_at_k, recall_at_k, reciprocal_rank};
use everevo_vector::{
    ChunkType, DummyEmbedder, EmbeddingModel, HnswStore, MemoryChunk, VectorStore,
};
use uuid::Uuid;

// ── BEIR Data Types ──────────────────────────────────────────────────────

/// A single document in the BEIR corpus.
#[derive(Debug, Clone)]
pub struct BeirDocument {
    pub id: String,
    pub title: Option<String>,
    pub text: String,
}

/// A search query.
#[derive(Debug, Clone)]
pub struct BeirQuery {
    pub id: String,
    pub text: String,
}

/// Relevance judgments: query_id → { doc_id → relevance_score }
pub type Qrels = HashMap<String, HashMap<String, u32>>;

/// A loaded BEIR dataset.
#[derive(Debug, Clone)]
pub struct BeirDataset {
    pub name: String,
    pub corpus: Vec<BeirDocument>,
    pub queries: Vec<BeirQuery>,
    pub qrels: Qrels,
}

// ── BEIR Loader ──────────────────────────────────────────────────────────

impl BeirDataset {
    /// Load a BEIR dataset from a directory containing corpus.jsonl, queries.jsonl, qrels.tsv.
    pub fn load(path: impl AsRef<Path>, name: &str) -> Result<Self, String> {
        let path = path.as_ref();
        let corpus = Self::load_corpus(&path.join("corpus.jsonl"))?;
        let queries = Self::load_queries(&path.join("queries.jsonl"))?;
        let qrels = Self::load_qrels(&path.join("qrels").join("test.tsv"))?;
        Ok(Self {
            name: name.to_string(),
            corpus,
            queries,
            qrels,
        })
    }

    fn load_corpus(path: &Path) -> Result<Vec<BeirDocument>, String> {
        let file = std::fs::File::open(path).map_err(|e| format!("Open corpus: {e}"))?;
        let reader = BufReader::new(file);
        let mut docs = Vec::new();
        for line in reader.lines() {
            let line = line.map_err(|e| format!("Read line: {e}"))?;
            if line.trim().is_empty() {
                continue;
            }
            let v: serde_json::Value =
                serde_json::from_str(&line).map_err(|e| format!("Parse JSON: {e}"))?;
            docs.push(BeirDocument {
                id: v["_id"].as_str().unwrap_or("").to_string(),
                title: v["title"].as_str().map(|s| s.to_string()),
                text: v["text"].as_str().unwrap_or("").to_string(),
            });
        }
        Ok(docs)
    }

    fn load_queries(path: &Path) -> Result<Vec<BeirQuery>, String> {
        let file = std::fs::File::open(path).map_err(|e| format!("Open queries: {e}"))?;
        let reader = BufReader::new(file);
        let mut queries = Vec::new();
        for line in reader.lines() {
            let line = line.map_err(|e| format!("Read line: {e}"))?;
            if line.trim().is_empty() {
                continue;
            }
            let v: serde_json::Value =
                serde_json::from_str(&line).map_err(|e| format!("Parse JSON: {e}"))?;
            queries.push(BeirQuery {
                id: v["_id"].as_str().unwrap_or("").to_string(),
                text: v["text"].as_str().unwrap_or("").to_string(),
            });
        }
        Ok(queries)
    }

    fn load_qrels(path: &Path) -> Result<Qrels, String> {
        let file = std::fs::File::open(path).map_err(|e| format!("Open qrels: {e}"))?;
        let reader = BufReader::new(file);
        let mut qrels: Qrels = HashMap::new();
        for line in reader.lines() {
            let line = line.map_err(|e| format!("Read line: {e}"))?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() < 3 {
                continue;
            }
            let qid = parts[0].to_string();
            let doc_id = parts[1].to_string();
            let score: u32 = parts[2].parse().unwrap_or(1);
            qrels.entry(qid).or_default().insert(doc_id, score);
        }
        Ok(qrels)
    }
}

// ── Benchmark Runner ─────────────────────────────────────────────────────

/// Run a retrieval benchmark using a dummy embedder (zero vectors).
///
/// This tests the evaluation pipeline correctness with deterministic results.
/// Real embedding benchmarks should use ONNX embedder.
pub fn run_dummy_benchmark(dataset: &BeirDataset, dim: usize) -> BenchmarkResult {
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let store = HnswStore::open(dir.path().join("bench-store"), dim).unwrap();
    let embedder = DummyEmbedder::new(dim);
    let num_docs = dataset.corpus.len();

    // Index all documents
    let mut doc_id_to_uuid: HashMap<String, Uuid> = HashMap::new();
    let mut chunks = Vec::new();
    for doc in &dataset.corpus {
        let uuid = Uuid::new_v4();
        doc_id_to_uuid.insert(doc.id.clone(), uuid);
        // Use deterministic "embedding" from document metadata
        let vector = embedder.encode(&doc.text).unwrap();
        chunks.push(MemoryChunk {
            id: uuid,
            content: doc.text.clone(),
            vector,
            source_pointers: vec![],
            projection: ProjectionMetadata::new("beir-bench", "dummy", vec![], 1.0),
            chunk_type: ChunkType::Fact,
            created_at: chrono::Utc::now(),
            retrieval_count: 0,
        });
    }
    if !chunks.is_empty() {
        store.insert(chunks).unwrap();
    }

    // Evaluate
    let mut ndcg_scores = Vec::new();
    let mut recall_scores = Vec::new();
    let mut rr_scores = Vec::new();
    let mut precision_scores = Vec::new();
    let top_k = 10;

    for query in &dataset.queries {
        let qvec = embedder.encode(&query.text).unwrap();
        let results = store.search(&qvec, top_k).unwrap();

        // Map UUIDs back to BEIR doc IDs
        let result_ids: Vec<String> = results
            .iter()
            .map(|r| {
                // Reverse lookup: find doc_id by UUID (inefficient but fine for benchmarks)
                doc_id_to_uuid
                    .iter()
                    .find(|(_, &u)| u == r.chunk.id)
                    .map(|(doc_id, _)| doc_id.clone())
                    .unwrap_or_default()
            })
            .collect();

        if let Some(qrel) = dataset.qrels.get(&query.id) {
            ndcg_scores.push(ndcg_at_k(&result_ids, qrel, top_k));
            recall_scores.push(recall_at_k(&result_ids, qrel, top_k));
            precision_scores.push(precision_at_k(&result_ids, qrel, top_k));
            rr_scores.push(reciprocal_rank(&result_ids, qrel));
        }
    }

    BenchmarkResult {
        dataset_name: dataset.name.clone(),
        num_docs,
        num_queries: dataset.queries.len(),
        ndcg_at_10: mean(&ndcg_scores),
        recall_at_10: mean(&recall_scores),
        mrr: mean(&rr_scores),
        precision_at_10: mean(&precision_scores),
    }
}

fn mean(scores: &[f64]) -> f64 {
    if scores.is_empty() {
        0.0
    } else {
        scores.iter().sum::<f64>() / scores.len() as f64
    }
}

#[derive(Debug, Clone)]
pub struct BenchmarkResult {
    pub dataset_name: String,
    pub num_docs: usize,
    pub num_queries: usize,
    pub ndcg_at_10: f64,
    pub recall_at_10: f64,
    pub mrr: f64,
    pub precision_at_10: f64,
}

impl std::fmt::Display for BenchmarkResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "=== {} ===", self.dataset_name)?;
        writeln!(f, "  Documents: {}", self.num_docs)?;
        writeln!(f, "  Queries:   {}", self.num_queries)?;
        writeln!(f, "  NDCG@10:   {:.4}", self.ndcg_at_10)?;
        writeln!(f, "  Recall@10: {:.4}", self.recall_at_10)?;
        writeln!(f, "  MRR:       {:.4}", self.mrr)?;
        writeln!(f, "  P@10:      {:.4}", self.precision_at_10)?;
        Ok(())
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    /// Create a minimal synthetic BEIR dataset for pipeline validation.
    fn synthetic_dataset(dir: &TempDir) -> BeirDataset {
        // corpus.jsonl
        let corpus = dir.path().join("corpus.jsonl");
        let mut f = std::fs::File::create(&corpus).unwrap();
        writeln!(f, r#"{{"_id":"doc1","title":"Rust","text":"Rust is a systems programming language focused on safety and concurrency."}}"#).unwrap();
        writeln!(f, r#"{{"_id":"doc2","title":"Python","text":"Python is an interpreted high-level programming language for general-purpose programming."}}"#).unwrap();
        writeln!(f, r#"{{"_id":"doc3","title":"Cooking","text":"Learn how to cook pasta with fresh tomatoes basil and olive oil."}}"#).unwrap();

        // queries.jsonl
        let queries = dir.path().join("queries.jsonl");
        let mut f = std::fs::File::create(&queries).unwrap();
        writeln!(f, r#"{{"_id":"q1","text":"systems programming language"}}"#).unwrap();
        writeln!(f, r#"{{"_id":"q2","text":"how to make pasta"}}"#).unwrap();

        // qrels/test.tsv
        let qrels_dir = dir.path().join("qrels");
        std::fs::create_dir_all(&qrels_dir).unwrap();
        let qrels = qrels_dir.join("test.tsv");
        let mut f = std::fs::File::create(&qrels).unwrap();
        writeln!(f, "q1\tdoc1\t1").unwrap();
        writeln!(f, "q2\tdoc3\t1").unwrap();

        BeirDataset::load(dir.path(), "synthetic").unwrap()
    }

    #[test]
    fn test_beir_loader_synthetic() {
        let dir = TempDir::new().unwrap();
        let dataset = synthetic_dataset(&dir);
        assert_eq!(dataset.corpus.len(), 3);
        assert_eq!(dataset.queries.len(), 2);
        assert_eq!(dataset.qrels.len(), 2);
        assert_eq!(dataset.qrels["q1"]["doc1"], 1);
    }

    #[test]
    fn test_dummy_benchmark_pipeline() {
        let dir = TempDir::new().unwrap();
        let dataset = synthetic_dataset(&dir);
        let result = run_dummy_benchmark(&dataset, 16);
        assert_eq!(result.num_docs, 3);
        assert_eq!(result.num_queries, 2);
        println!("{result}");
    }

    /// Resolve workspace root from the crate's manifest directory.
    fn workspace_root() -> std::path::PathBuf {
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        // crate is at crates/infra/everevo-knowledge → go up 3 levels
        manifest_dir
            .parent() // everevo-knowledge
            .unwrap()
            .parent() // infra
            .unwrap()
            .parent() // crates
            .unwrap()
            .to_path_buf()
    }

    /// Load NFCorpus from `data/bench/nfcorpus/` if available, and run the
    /// full retrieval quality benchmark with real metrics.
    #[test]
    #[ignore = "Requires NFCorpus dataset at data/bench/nfcorpus/"]
    fn benchmark_nfcorpus_real() {
        let path = workspace_root().join("data").join("bench").join("nfcorpus");
        if !path.exists() {
            println!("Skipping: NFCorpus not found at {}", path.display());
            println!("Download with: pip install beir && python download_beir.py nfcorpus");
            return;
        }
        let dataset = BeirDataset::load(path, "NFCorpus").expect("Failed to load NFCorpus");
        println!(
            "Loaded NFCorpus: {} docs, {} queries",
            dataset.corpus.len(),
            dataset.queries.len()
        );
        let result = run_dummy_benchmark(&dataset, 384);
        println!("{result}");
    }

    /// Run NFCorpus with real ONNX embeddings (all-MiniLM-L6-v2).
    /// Requires ONNX runtime DLL + model files + NFCorpus dataset.
    #[test]
    #[ignore = "Requires ONNX runtime + NFCorpus dataset"]
    fn benchmark_nfcorpus_onnx() {
        let ws = workspace_root();
        let path = ws.join("data").join("bench").join("nfcorpus");
        if !path.exists() {
            println!("NFCorpus not found at {}", path.display());
            return;
        }
        let models_dir = ws.join("data").join("models");
        everevo_vector::configure_ort_dylib(&ws.join("data"));

        let dataset = BeirDataset::load(&path, "NFCorpus-ONNX").expect("Failed to load NFCorpus");
        println!(
            "Loaded NFCorpus: {} docs, {} queries",
            dataset.corpus.len(),
            dataset.queries.len()
        );

        // Try to load the ONNX embedder
        match everevo_vector::OnnxEmbedder::new("all-MiniLM-L6-v2", &models_dir) {
            Ok(e) if e.is_loaded() => {
                println!(
                    "ONNX embedder loaded: all-MiniLM-L6-v2 (dim={})",
                    e.dimension()
                );
                let result = run_onnx_benchmark(&dataset, &e);
                println!("{result}");
            }
            _ => {
                println!("ONNX embedder not available — falling back to DummyEmbedder");
                let result = run_dummy_benchmark(&dataset, 384);
                println!("{result}");
            }
        }
    }

    /// Benchmark with a real ONNX embedder.
    fn run_onnx_benchmark(dataset: &BeirDataset, embedder: &dyn EmbeddingModel) -> BenchmarkResult {
        use tempfile::TempDir;

        let dim = embedder.dimension();
        let dir = TempDir::new().unwrap();
        let store = HnswStore::open(dir.path().join("onnx-bench"), dim).unwrap();
        let num_docs = dataset.corpus.len();

        // Batch embed all documents
        let texts: Vec<String> = dataset.corpus.iter().map(|d| d.text.clone()).collect();
        println!("Embedding {num_docs} documents with ONNX...");
        let vectors = embedder.encode_batch(&texts).unwrap_or_else(|e| {
            println!("Warning: batch embed failed ({e}), using zeros");
            texts.iter().map(|_| vec![0.0_f32; dim]).collect()
        });

        let mut doc_id_to_uuid: HashMap<String, Uuid> = HashMap::new();
        let mut chunks = Vec::new();
        for (doc, vector) in dataset.corpus.iter().zip(vectors) {
            let uuid = Uuid::new_v4();
            doc_id_to_uuid.insert(doc.id.clone(), uuid);
            chunks.push(MemoryChunk {
                id: uuid,
                content: doc.text.clone(),
                vector,
                source_pointers: vec![],
                projection: ProjectionMetadata::new("beir-bench", "all-MiniLM-L6-v2", vec![], 1.0),
                chunk_type: ChunkType::Fact,
                created_at: chrono::Utc::now(),
                retrieval_count: 0,
            });
        }
        if !chunks.is_empty() {
            store.insert(chunks).unwrap();
        }
        println!(
            "Indexed {num_docs} documents. Running {} queries...",
            dataset.queries.len()
        );

        let mut ndcg_scores = Vec::new();
        let mut recall_scores = Vec::new();
        let mut rr_scores = Vec::new();
        let mut precision_scores = Vec::new();
        let top_k = 10;
        let mut evaluated = 0usize;

        for query in &dataset.queries {
            let qvec = match embedder.encode(&query.text) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let results = match store.search(&qvec, top_k) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let result_ids: Vec<String> = results
                .iter()
                .map(|r| {
                    doc_id_to_uuid
                        .iter()
                        .find(|(_, &u)| u == r.chunk.id)
                        .map(|(doc_id, _)| doc_id.clone())
                        .unwrap_or_default()
                })
                .collect();
            if let Some(qrel) = dataset.qrels.get(&query.id) {
                ndcg_scores.push(ndcg_at_k(&result_ids, qrel, top_k));
                recall_scores.push(recall_at_k(&result_ids, qrel, top_k));
                precision_scores.push(precision_at_k(&result_ids, qrel, top_k));
                rr_scores.push(reciprocal_rank(&result_ids, qrel));
                evaluated += 1;
            }
        }

        BenchmarkResult {
            dataset_name: dataset.name.clone(),
            num_docs,
            num_queries: evaluated,
            ndcg_at_10: mean(&ndcg_scores),
            recall_at_10: mean(&recall_scores),
            mrr: mean(&rr_scores),
            precision_at_10: mean(&precision_scores),
        }
    }

    /// Quick BEIR benchmark: NFCorpus + SciFact + FiQA-5K (subset).
    /// Runs FiQA capped at 5,000 docs for speed.
    #[test]
    #[ignore = "Requires ONNX runtime + BEIR datasets"]
    fn benchmark_beir_quick() {
        let ws = workspace_root();
        let models_dir = ws.join("data").join("models");
        everevo_vector::configure_ort_dylib(&ws.join("data"));

        let embedder = match everevo_vector::OnnxEmbedder::new("all-MiniLM-L6-v2", &models_dir) {
            Ok(e) if e.is_loaded() => {
                println!("ONNX embedder: all-MiniLM-L6-v2 (dim={})\n", e.dimension());
                e
            }
            _ => {
                println!("ONNX not available");
                return;
            }
        };

        let runs: Vec<(&str, &str, Option<usize>)> = vec![
            ("nfcorpus", "NFCorpus", None),
            ("scifact", "SciFact", None),
            ("fiqa", "FiQA-5K", Some(5000)),
        ];

        println!("╔══════════════════════════════════════════════════╗");
        println!("║   BEIR Quick Benchmark (ONNX)                   ║");
        println!("╠══════════════════════════════════════════════════╣");
        println!("║ Model: all-MiniLM-L6-v2 (384-dim)              ║");
        println!("╚══════════════════════════════════════════════════╝\n");

        for (dir_name, display_name, max_docs) in &runs {
            let path = ws.join("data").join("bench").join(dir_name);
            if !path.join("corpus.jsonl").exists() {
                println!("  [{display_name}] SKIP — not found");
                continue;
            }
            let mut dataset = BeirDataset::load(&path, display_name)
                .unwrap_or_else(|e| panic!("Load {display_name}: {e}"));
            if let Some(limit) = max_docs {
                dataset.corpus.truncate(*limit);
            }
            println!(
                "  [{display_name}] {docs} docs, {queries} queries — running...",
                docs = dataset.corpus.len(),
                queries = dataset.queries.len()
            );
            let result = run_onnx_benchmark(&dataset, &embedder);
            println!(
                "    NDCG@10={:.4}  Recall@10={:.4}  MRR={:.4}  P@10={:.4}\n",
                result.ndcg_at_10, result.recall_at_10, result.mrr, result.precision_at_10
            );
        }

        println!("Published baselines (NFCorpus NDCG@10):");
        println!("  BM25: 0.325 | BGE-small: 0.367 | BGE-small+RRF: 0.395-0.432");
    }

    /// Run ALL available BEIR datasets with ONNX embeddings.
    /// This is the authoritative end-to-end retrieval quality benchmark.
    #[test]
    #[ignore = "Requires ONNX runtime + all BEIR datasets downloaded"]
    fn benchmark_beir_all_onnx() {
        let ws = workspace_root();
        let models_dir = ws.join("data").join("models");
        everevo_vector::configure_ort_dylib(&ws.join("data"));

        let embedder = match everevo_vector::OnnxEmbedder::new("all-MiniLM-L6-v2", &models_dir) {
            Ok(e) if e.is_loaded() => {
                println!("ONNX embedder: all-MiniLM-L6-v2 (dim={})", e.dimension());
                e
            }
            _ => {
                println!("ONNX embedder not available. Aborting.");
                return;
            }
        };

        let datasets = [
            ("nfcorpus", "NFCorpus"),
            ("scifact", "SciFact"),
            ("fiqa", "FiQA"),
        ];

        println!("\n╔══════════════════════════════════════════════════╗");
        println!("║   BEIR Retrieval Quality Benchmark (ONNX)       ║");
        println!("╠══════════════════════════════════════════════════╣");
        println!("║ Model: all-MiniLM-L6-v2 (384-dim)              ║");
        println!("║ Method: pure vector search (HNSW)              ║");
        println!("╚══════════════════════════════════════════════════╝\n");

        let mut all_results = Vec::new();

        for (dir_name, display_name) in &datasets {
            let path = ws.join("data").join("bench").join(dir_name);
            if !path.join("corpus.jsonl").exists() {
                println!("  [{display_name}] SKIP — not found at {}", path.display());
                continue;
            }

            let dataset = match BeirDataset::load(&path, display_name) {
                Ok(d) => d,
                Err(e) => {
                    println!("  [{display_name}] ERROR loading: {e}");
                    continue;
                }
            };
            println!(
                "  [{display_name}] {docs} docs, {queries} queries — running...",
                docs = dataset.corpus.len(),
                queries = dataset.queries.len()
            );

            let result = run_onnx_benchmark(&dataset, &embedder);
            all_results.push(result.clone());
            println!(
                "    NDCG@10={:.4}  Recall@10={:.4}  MRR={:.4}  P@10={:.4}",
                result.ndcg_at_10, result.recall_at_10, result.mrr, result.precision_at_10
            );
        }

        // Summary
        println!("\n┌──────────────────────────────────────────────────┐");
        println!("│              BEIR Benchmark Summary              │");
        println!("├──────────┬──────────┬──────────┬────────┬────────┤");
        println!("│ Dataset  │ NDCG@10  │ Recall@10│ MRR    │ P@10   │");
        println!("├──────────┼──────────┼──────────┼────────┼────────┤");
        for r in &all_results {
            println!(
                "│ {:<8} │ {:>8.4} │ {:>8.4} │ {:>6.4} │ {:>6.4} │",
                r.dataset_name, r.ndcg_at_10, r.recall_at_10, r.mrr, r.precision_at_10
            );
        }
        println!("└──────────┴──────────┴──────────┴────────┴────────┘");

        // Compare against published baselines
        println!("\nPublished baselines (NFCorpus NDCG@10):");
        println!("  BM25 (sparse):              0.325");
        println!("  BGE-small (33M):            0.367");
        println!("  BGE-small + RRF (hybrid):   0.395–0.432");
        println!("  Ours (all-MiniLM-L6-v2, pure vector): see above");
    }
}
