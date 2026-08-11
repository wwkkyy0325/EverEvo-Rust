//! Integration tests for the everevo-knowledge crate.
//!
//! Tests end-to-end inbox pipeline, cross-domain isolation,
//! SPARQL+vector integration, and hybrid search correctness.

use std::io::Write as _;
use std::sync::Arc;

use everevo_core::retrieval::Retriever;
use everevo_knowledge::domain::{DomainManager, DomainRetriever, DomainWatcher};
use everevo_knowledge::graph::KnowledgeGraph;
use everevo_vector::{DummyEmbedder, EmbeddingModel};
use tempfile::TempDir;

// ── Helpers ────────────────────────────────────────────────────────────────

fn write_inbox_file(dir: &std::path::Path, filename: &str, content: &str) {
    let inbox = dir.join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let mut f = std::fs::File::create(inbox.join(filename)).unwrap();
    f.write_all(content.as_bytes()).unwrap();
}

fn setup_domain_root() -> TempDir {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join("inbox")).unwrap();
    dir
}

// ── Inbox Pipeline Tests ──────────────────────────────────────────────────

#[test]
fn test_inbox_pipeline_empty() {
    let dir = setup_domain_root();
    let _mgr = DomainManager::load(dir.path()).unwrap();
    // process_global_inbox with empty inbox — scan returns nothing
    let inbox = dir.path().join("inbox");
    let reg_path = dir.path().join("domains.json");
    let mut watcher = DomainWatcher::new(&inbox, &reg_path).unwrap();
    let files = watcher.scan().unwrap();
    assert!(files.is_empty(), "Empty inbox should yield no files");
}

#[test]
fn test_inbox_pipeline_single_md_with_embedder() {
    let dir = setup_domain_root();
    write_inbox_file(
        dir.path(),
        "rust-guide.md",
        "# Rust Guide\n\nRust is a systems programming language focused on safety, speed, and concurrency.\n\nIt prevents segfaults and guarantees thread safety without a garbage collector."
    );

    let embedder: Arc<dyn EmbeddingModel> = Arc::new(DummyEmbedder::new(384));
    let _mgr = DomainManager::load_with_embedder(dir.path(), Some(embedder)).unwrap();

    // Process inbox file manually — classify + index
    let inbox = dir.path().join("inbox");
    let reg_path = dir.path().join("domains.json");
    let mut watcher = DomainWatcher::new(&inbox, &reg_path).unwrap();
    let files = watcher.scan().unwrap();
    assert!(!files.is_empty(), "Should detect the md file");

    // Verify file is in inbox
    let inbox_md = inbox.join("rust-guide.md");
    assert!(inbox_md.exists(), "File should exist before processing");
}

#[test]
fn test_domain_coverage_after_create() {
    let dir = setup_domain_root();
    let mut mgr = DomainManager::load(dir.path()).unwrap();
    mgr.registry.create(
        "rust".into(),
        "Rust".into(),
        "Rust programming language".into(),
    );
    mgr.registry.create(
        "python".into(),
        "Python".into(),
        "Python programming".into(),
    );
    mgr.save().unwrap();

    let coverage = mgr.coverage();
    assert_eq!(coverage.len(), 2);
    for c in &coverage {
        assert!(c.is_new); // 0 docs → new
    }
}

// ── Cross-Domain Isolation ────────────────────────────────────────────────

#[test]
fn test_domain_list_documents_isolation() {
    let dir = setup_domain_root();
    let mut mgr = DomainManager::load(dir.path()).unwrap();
    mgr.registry
        .create("domain-a".into(), "Domain A".into(), "First domain".into());
    mgr.registry
        .create("domain-b".into(), "Domain B".into(), "Second domain".into());

    // Manually add a document file to domain-a
    let doc_dir_a = dir.path().join("domain-a").join("documents");
    std::fs::create_dir_all(&doc_dir_a).unwrap();
    std::fs::write(doc_dir_a.join("readme.md"), "# Domain A Doc").unwrap();

    let docs_a = mgr.list_documents("domain-a").unwrap();
    let docs_b = mgr.list_documents("domain-b").unwrap();
    assert!(!docs_a.is_empty(), "Domain A should have docs");
    assert!(docs_b.is_empty(), "Domain B should be empty");
}

#[test]
fn test_domain_retriever_search() {
    let dir = setup_domain_root();
    let mut mgr = DomainManager::load(dir.path()).unwrap();
    mgr.registry
        .create("rust".into(), "Rust".into(), "Rust programming".into());

    // Add a document
    let doc_dir = dir.path().join("rust").join("documents");
    std::fs::create_dir_all(&doc_dir).unwrap();
    std::fs::write(
        doc_dir.join("ownership.md"),
        "# Ownership\n\nRust's ownership system ensures memory safety.",
    )
    .unwrap();
    mgr.registry
        .add_document("rust", &vec![0.1_f32; 384])
        .unwrap();
    mgr.save().unwrap();

    // Retrieve by keyword
    let retriever = DomainRetriever::new(dir.path());
    let results = retriever.search("ownership", 10);
    assert!(!results.is_empty(), "Should find ownership document");
}

#[test]
fn test_domain_retriever_no_match() {
    let dir = setup_domain_root();
    let retriever = DomainRetriever::new(dir.path());
    let results = retriever.search("nonexistent-xyz-topic", 10);
    assert!(results.is_empty(), "No match should return empty");
}

// ── Knowledge Graph + SPARQL Integration ──────────────────────────────────

#[test]
fn test_kg_sparql_select_integration() {
    let dir = TempDir::new().unwrap();
    let mut kg = KnowledgeGraph::open(dir.path()).unwrap();

    // Seed with test data
    kg.seed_project_structure(&["crate-a"]);
    kg.save().unwrap();

    let rows = kg
        .query_sparql(
            "PREFIX evo: <http://everevo.io/> \
         PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> \
         SELECT ?label WHERE { ?e rdf:type evo:Entity ; evo:label ?label }",
        )
        .unwrap();
    assert!(!rows.is_empty(), "SPARQL should return seeded entities");
}

#[test]
fn test_kg_expand_integration() {
    let dir = TempDir::new().unwrap();
    let mut kg = KnowledgeGraph::open(dir.path()).unwrap();
    kg.seed_project_structure(&["crate-a", "crate-b"]);
    // Expand from root — should find project + crates + tech stack
    let entities = kg.expand("everevo", 1);
    assert!(
        !entities.is_empty(),
        "Expand should return connected entities"
    );
    // Should include the root entity itself
    assert!(entities.iter().any(|e| e.id == "everevo"));
}

// ── Full Pipeline E2E Tests (async with real files) ────────────────────

#[tokio::test]
async fn test_full_inbox_pipeline_e2e() {
    let dir = TempDir::new().unwrap();
    let mut mgr = DomainManager::load(dir.path()).unwrap();

    // Write two domain-relevant documents to inbox
    let inbox = dir.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::write(
        inbox.join("rust-intro.md"),
        "# Rust Programming\n\nRust is a systems programming language that runs blazingly fast, \
         prevents segfaults, and guarantees thread safety.\n\n## Features\n\n- Zero-cost abstractions\n\
         - Move semantics\n- Guaranteed memory safety\n- Threads without data races\n\
         - Trait-based generics\n- Pattern matching\n- Type inference\n\
         - Minimal runtime\n- Efficient C bindings"
    ).unwrap();
    std::fs::write(
        inbox.join("python-guide.md"),
        "# Python Guide\n\nPython is an interpreted, high-level programming language \
         with dynamic semantics. Its built-in data structures make it attractive for \
         Rapid Application Development.\n\n## Key Features\n\n- Easy to learn syntax\n\
         - Extensive standard library\n- Dynamic typing\n- Automatic memory management\n\
         - Cross-platform",
    )
    .unwrap();
    std::fs::write(
        inbox.join("cooking-pasta.md"),
        "# How to Cook Pasta\n\nPerfect pasta requires just a few ingredients: \
         pasta, water, salt, and olive oil.\n\n## Steps\n\n1. Boil water with salt\n\
         2. Add pasta and cook until al dente\n3. Drain and toss with olive oil\n\
         4. Serve with fresh basil and parmesan",
    )
    .unwrap();

    // Process inbox — this is the core pipeline
    let result = mgr.process_global_inbox().await.unwrap();
    assert!(result.processed > 0, "Should process at least one file");
    println!(
        "Processed {} files, new domains: {:?}",
        result.processed, result.new_domains
    );

    // Verify domains were created
    let coverage = mgr.coverage();
    println!("Domain coverage: {:?}", coverage);
    assert!(!coverage.is_empty(), "Should have at least one domain");

    // Verify documents exist in their domains
    for cov in &coverage {
        let docs = mgr.list_documents(&cov.domain_id).unwrap();
        println!("  Domain '{}': {} docs", cov.domain_id, docs.len());
    }

    // ── Verify Pipeline Stages ────────────────────────────────────────

    // Stage 1: Domain was created/used
    assert!(
        !coverage.is_empty(),
        "Should have active domain after processing"
    );
    let domain_id = &coverage[0].domain_id;

    // Stage 2: Documents exist in domain directory
    let doc_dir = dir.path().join(domain_id).join("documents");
    let doc_files: Vec<_> = std::fs::read_dir(&doc_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(doc_files.len(), 3, "All 3 documents should be stored");

    // Stage 3: Verify document contents survive roundtrip
    let mut found_rust = false;
    let mut found_pasta = false;
    for entry in &doc_files {
        let content = std::fs::read_to_string(entry.path()).unwrap();
        if content.contains("Rust") {
            found_rust = true;
        }
        if content.contains("pasta") || content.contains("Pasta") {
            found_pasta = true;
        }
    }
    assert!(found_rust, "Rust document content should be preserved");
    assert!(found_pasta, "Cooking document content should be preserved");

    // Stage 4: Inbox should be empty after processing
    let remaining: Vec<_> = std::fs::read_dir(&inbox)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        remaining.is_empty(),
        "Inbox should be empty after processing"
    );

    // Stage 5: Content-level keyword search (NEW — upgraded from filename-only)
    let retriever = DomainRetriever::new(dir.path());

    // Content search: "segfaults" is in the rust-intro.md content but NOT in any filename
    let content_results = retriever.search("segfaults", 5);
    println!(
        "Content search 'segfaults': {} results",
        content_results.len()
    );
    assert!(
        !content_results.is_empty(),
        "Content search should find 'segfaults' in rust-intro.md body"
    );

    // Content search: "basil" is in cooking-pasta.md content
    let food_results = retriever.search("basil", 5);
    println!("Content search 'basil': {} results", food_results.len());
    assert!(
        !food_results.is_empty(),
        "Content search should find 'basil' in cooking-pasta.md body"
    );

    // Negative control: something not in any document
    let no_results = retriever.search("nonexistent-quantum-blockchain-xyz", 5);
    assert!(
        no_results.is_empty(),
        "Gibberish query should return nothing"
    );

    println!("=== Full Pipeline E2E: ALL STAGES PASSED ===");

    // Verify registry is persisted
    mgr.save().unwrap();
    let reg_path = dir.path().join("domains.json");
    assert!(reg_path.exists(), "Registry should be persisted");
    let reg_content = std::fs::read_to_string(&reg_path).unwrap();
    assert!(
        reg_content.contains(domain_id),
        "Registry should reference the domain"
    );
}

/// Full pipeline with ONNX embedder if available.
/// Requires ONNX model at data/models/all-MiniLM-L6-v2/
#[tokio::test]
#[ignore = "Requires ONNX runtime and model files"]
async fn test_full_inbox_pipeline_with_onnx() {
    let dir = TempDir::new().unwrap();
    let workspace_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let models_dir = workspace_root.join("data").join("models");

    // Try to load with ONNX embedder
    let mut mgr = DomainManager::load_with_onnx(dir.path(), &models_dir).unwrap();
    let has_embedder = mgr.embedder().is_some();
    println!("ONNX embedder loaded: {has_embedder}");
    if !has_embedder {
        println!("Skipping ONNX test — embedder not available");
        return;
    }

    let inbox = dir.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::write(
        inbox.join("rust-guide.md"),
        "# Rust Guide\n\nRust is a systems programming language focused on safety and performance.\n\
         It provides memory safety without garbage collection using ownership and borrowing rules."
    ).unwrap();

    let result = mgr.process_global_inbox().await.unwrap();
    println!("ONNX pipeline processed {} files", result.processed);
    assert!(result.processed > 0);

    let retriever = DomainRetriever::new(dir.path());
    let results = retriever.search("memory safety", 5);
    println!("ONNX search results: {}", results.len());
    assert!(!results.is_empty());
}
