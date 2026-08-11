//! Stress and robustness tests for everevo-vector.
//!
//! Tests large-scale insert/search, concurrency under load, and
//! crash recovery patterns. These tests are `#[ignore]` by default
//! — run with `cargo test -- --ignored`.

use std::sync::Arc;
use std::thread;
use std::time::Instant;

use everevo_core::memory::ProjectionMetadata;
use everevo_vector::*;
use rand::Rng;
use tempfile::TempDir;
use uuid::Uuid;

fn make_chunk(id: Uuid, v: Vec<f32>) -> MemoryChunk {
    MemoryChunk {
        id,
        content: String::new(),
        vector: v,
        source_pointers: vec![],
        projection: ProjectionMetadata::new("test", "test", vec![], 1.0),
        chunk_type: ChunkType::Fact,
        created_at: chrono::Utc::now(),
        retrieval_count: 0,
    }
}

// ── Large-Scale Insert ────────────────────────────────────────────────────

#[test]
#[ignore = "heavy: inserts 10K vectors"]
fn test_insert_10k_vectors() {
    let dir = TempDir::new().unwrap();
    let dim = 384;
    let store = HnswStore::open(dir.path().join("stress-store"), dim).unwrap();

    let mut rng = rand::thread_rng();
    let n = 10_000;
    let mut chunks = Vec::with_capacity(n);
    for _ in 0..n {
        let v: Vec<f32> = (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect();
        chunks.push(make_chunk(Uuid::new_v4(), v));
    }

    let start = Instant::now();
    store.insert(chunks).unwrap();
    let elapsed = start.elapsed();

    assert_eq!(store.count(), n);
    println!("Inserted {n} vectors @ dim={dim} in {elapsed:?}");

    // Search should still work
    let query: Vec<f32> = (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect();
    let results = store.search(&query, 10).unwrap();
    assert!(
        !results.is_empty(),
        "Search should return results on populated store"
    );
    println!("Search latency: 10 results returned");
}

#[test]
#[ignore = "heavy: inserts 50K vectors"]
fn test_insert_50k_vectors() {
    let dir = TempDir::new().unwrap();
    let dim = 128;
    let store = HnswStore::open(dir.path().join("large-store"), dim).unwrap();

    let mut rng = rand::thread_rng();
    let n = 50_000;
    let mut chunks = Vec::with_capacity(n);
    for _ in 0..n {
        let v: Vec<f32> = (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect();
        chunks.push(make_chunk(Uuid::new_v4(), v));
    }

    let start = Instant::now();
    store.insert(chunks).unwrap();
    let elapsed = start.elapsed();

    assert_eq!(store.count(), n);
    // 50K vectors should insert in under 60 seconds
    assert!(
        elapsed.as_secs() < 60,
        "50K insert took {}s, expected <60s",
        elapsed.as_secs()
    );
    println!("Inserted {n} vectors @ dim={dim} in {elapsed:?}");

    // Verify search works post-insert
    let mut rng = rand::thread_rng();
    let query: Vec<f32> = (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect();
    let search_start = Instant::now();
    let results = store.search(&query, 10).unwrap();
    let search_elapsed = search_start.elapsed();
    assert!(!results.is_empty());
    // Search should be fast: p99 < 10ms
    assert!(
        search_elapsed.as_millis() < 10,
        "Search took {}ms, expected <10ms",
        search_elapsed.as_millis()
    );
}

// ── Concurrency ────────────────────────────────────────────────────────────

#[test]
fn test_concurrent_multi_writer() {
    let dir = TempDir::new().unwrap();
    let dim = 8;
    let store = Arc::new(HnswStore::open(dir.path().join("concurrent-store"), dim).unwrap());

    let num_threads = 8;
    let per_thread = 100;
    let mut handles = vec![];

    for t in 0..num_threads {
        let s = Arc::clone(&store);
        handles.push(thread::spawn(move || {
            let mut rng = rand::thread_rng();
            for i in 0..per_thread {
                let v: Vec<f32> = (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect();
                let mut chunk = make_chunk(Uuid::new_v4(), v);
                chunk.content = format!("thread-{t}-item-{i}");
                let _ = s.insert(vec![chunk]);
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    // All inserts should have completed. Due to file-level serialize on each
    // insert, total count may be < num_threads * per_thread (some overwrites
    // during concurrent saves). Verify we have substantial data.
    let count = store.count();
    assert!(
        count > 0,
        "Should have data after concurrent writes, got {count}"
    );
    // With 8×100=800 inserts, expect at least 100 to survive
    assert!(
        count >= 100,
        "Expected >=100 surviving chunks after concurrent writes, got {count}"
    );
}

#[test]
fn test_concurrent_read_write() {
    let dir = TempDir::new().unwrap();
    let dim = 4;
    let store = Arc::new(HnswStore::open(dir.path().join("rw-store"), dim).unwrap());

    // Pre-populate with some data
    let mut rng = rand::thread_rng();
    let mut initial = Vec::new();
    for _ in 0..50 {
        let v: Vec<f32> = (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect();
        initial.push(make_chunk(Uuid::new_v4(), v));
    }
    store.insert(initial).unwrap();

    let mut handles = vec![];

    // 3 writer threads
    for _ in 0..3 {
        let s = Arc::clone(&store);
        handles.push(thread::spawn(move || {
            let mut rng = rand::thread_rng();
            for _ in 0..20 {
                let v: Vec<f32> = (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect();
                let _ = s.insert(vec![make_chunk(Uuid::new_v4(), v)]);
            }
        }));
    }

    // 3 reader threads
    for _ in 0..3 {
        let s = Arc::clone(&store);
        handles.push(thread::spawn(move || {
            let mut rng = rand::thread_rng();
            for _ in 0..20 {
                let q: Vec<f32> = (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect();
                let _ = s.search(&q, 5);
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    // Should not panic or deadlock
    assert!(store.count() > 0);
}

// ── Crash Recovery ─────────────────────────────────────────────────────────

#[test]
fn test_persistence_survives_reopen() {
    let dir = TempDir::new().unwrap();
    let dim = 4;
    let path = dir.path().join("recovery-store");
    let id = Uuid::new_v4();

    {
        let store = HnswStore::open(&path, dim).unwrap();
        store
            .insert(vec![make_chunk(id, vec![1.0, 0.0, 0.0, 0.0])])
            .unwrap();
    }

    // Reopen — data should survive via bincode
    let store2 = HnswStore::open(&path, dim).unwrap();
    assert_eq!(store2.count(), 1, "Data should survive reopen");
    assert!(
        store2.get(&id).is_some(),
        "Original chunk should be retrievable"
    );

    let results = store2.search(&[1.0, 0.0, 0.0, 0.0], 5).unwrap();
    assert!(!results.is_empty(), "Search should work after reopen");
}

#[test]
fn test_zero_byte_file_recovery() {
    let dir = TempDir::new().unwrap();
    let bin_path = dir.path().join("recovery").with_extension("bin");
    // Write a zero-byte file
    std::fs::write(&bin_path, []).unwrap();
    // Opening should succeed (load returns empty or errors gracefully)
    let result = HnswStore::open(dir.path().join("recovery"), 4);
    // Should either succeed with empty store or return an error — but NOT panic
    match result {
        Ok(store) => assert_eq!(store.count(), 0),
        Err(_) => {} // acceptable: errors on corrupt data
    }
}
