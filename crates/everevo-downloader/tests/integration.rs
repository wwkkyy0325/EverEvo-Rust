//! Integration tests for the downloader — mirror resolution, config, task building.

use everevo_downloader::config::DownloaderConfig;
use everevo_downloader::mirror::MirrorRegistry;
use everevo_downloader::task::{DownloadTask, Priority, Region};

/// Test: mirror registry resolves GitHub release URLs to domestic mirrors.
#[test]
fn test_github_release_domestic_mirrors() {
    let reg = MirrorRegistry::with_defaults();
    let candidates = reg.resolve(
        "https://github.com/BurntSushi/ripgrep/releases/download/14.0.0/ripgrep-14.0.0-x86_64-pc-windows-msvc.zip",
        Region::Domestic,
    );
    // Should find at least Tsinghua TUNA and Aliyun
    assert!(!candidates.is_empty(), "Expected domestic mirror candidates");
    for (url, name) in &candidates {
        println!("  {name}: {url}");
        // Original URL should not be returned as-is (ghproxy URLs contain github.com in path, that's OK)
        assert_ne!(url, "https://github.com/BurntSushi/ripgrep/releases/download/14.0.0/ripgrep-14.0.0-x86_64-pc-windows-msvc.zip");
    }
}

/// Test: mirror registry for raw GitHub files routes to jsdelivr.
#[test]
fn test_raw_github_to_jsdelivr() {
    let reg = MirrorRegistry::with_defaults();
    let candidates = reg.resolve(
        "https://raw.githubusercontent.com/rust-lang/rust/master/README.md",
        Region::Auto,
    );
    assert!(!candidates.is_empty());
    // jsDelivr should be one of the candidates
    let has_jsdelivr = candidates.iter().any(|(url, _)| url.contains("jsdelivr.net"));
    assert!(has_jsdelivr, "Expected jsdelivr mirror, got: {candidates:?}");
}

/// Test: task builder creates correct task.
#[test]
fn test_task_builder_pattern() {
    let task = DownloadTask::new("https://example.com/file.zip", "./downloads/file.zip")
        .with_priority(Priority::High)
        .with_region(Region::Domestic)
        .with_retries(5)
        .with_timeout(60)
        .with_sha256("abcdef1234567890")
        .with_metadata(serde_json::json!({"source": "agent-download-tool"}));

    assert_eq!(task.priority, Priority::High);
    assert!(matches!(task.region, Region::Domestic));
    assert_eq!(task.max_retries, 5);
    assert_eq!(task.timeout_secs, 60);
    assert_eq!(task.expected_sha256.as_deref(), Some("abcdef1234567890"));
    assert_eq!(task.metadata["source"], "agent-download-tool");
}

/// Test: config defaults are reasonable.
#[test]
fn test_config_defaults_sane() {
    let config = DownloaderConfig::default();
    assert_eq!(config.max_concurrent_tasks, 4);
    assert_eq!(config.default_chunk_size, 4 * 1024 * 1024);
    assert!(config.chunk_threshold >= 5 * 1024 * 1024);
    assert_eq!(config.max_retries, 3);
    assert!(config.mirror_enabled);
}

/// Test: effective_* methods respect task overrides.
#[test]
fn test_effective_overrides() {
    let config = DownloaderConfig::default();

    // Task with custom retries
    assert_eq!(config.effective_retries(0), 3); // default
    assert_eq!(config.effective_retries(10), 10); // task override

    // Task with custom chunks
    assert_eq!(config.effective_chunk_size(0), 4 * 1024 * 1024);
    assert_eq!(config.effective_chunk_size(8 * 1024 * 1024), 8 * 1024 * 1024);

    // Task with custom timeout
    assert_eq!(config.effective_timeout_secs(0), 30);
    assert_eq!(config.effective_timeout_secs(120), 120);
}

/// Test: chunk_threshold based auto-chunking decision.
#[test]
fn test_should_chunk_decision() {
    let config = DownloaderConfig::default();

    // Small file
    assert!(!config.should_chunk(1 * 1024 * 1024, 0));
    // Large file
    assert!(config.should_chunk(20 * 1024 * 1024, 0));
    // Explicit chunks
    assert!(config.should_chunk(1 * 1024 * 1024, 4));
    // Threshold disabled
    let config2 = DownloaderConfig {
        chunk_threshold: 0,
        ..DownloaderConfig::default()
    };
    assert!(!config2.should_chunk(100 * 1024 * 1024, 0));
}
