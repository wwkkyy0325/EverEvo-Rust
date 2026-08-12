// ── Cross-Platform Asset Definitions ───────────────────────────────────
//
// Models are platform-independent (ONNX). Runtimes are per-platform.
// SystemProvided assets (git on macOS/Linux) are NOT downloaded — they
// are expected to exist on the system PATH and are checked at startup.

use crate::{Asset, AssetFile, AssetKind};
use std::collections::HashMap;
use std::sync::LazyLock;

const PYTHON_VERSION: &str = "3.12.8";
/// python-build-standalone release tag (date, not Python version).
const PBS_RELEASE: &str = "20241215";
const NODE_VERSION: &str = "22.12.0";
const GIT_VERSION: &str = "2.47.1";
const ONNX_VERSION: &str = "1.24.2";

// ── Models (platform-independent) ──────────────────────────────────────

/// Shared across ALL platforms — ONNX models work everywhere.
fn shared_models() -> Vec<Asset> {
    vec![
        Asset {
            key: "bge-small-zh".into(),
            kind: AssetKind::Model,
            version: "v1.5".into(),
            primary_url: "https://hf-mirror.com/Xenova/bge-small-zh-v1.5/resolve/main/onnx/model_quantized.onnx".into(),
            mirror_urls: vec![],
            extra_files: vec![
                AssetFile { filename: "tokenizer.json".into(), url: "https://hf-mirror.com/Xenova/bge-small-zh-v1.5/resolve/main/tokenizer.json".into(), mirror_url: None },
                AssetFile { filename: "config.json".into(), url: "https://hf-mirror.com/Xenova/bge-small-zh-v1.5/resolve/main/config.json".into(), mirror_url: None },
                AssetFile { filename: "special_tokens_map.json".into(), url: "https://hf-mirror.com/Xenova/bge-small-zh-v1.5/resolve/main/special_tokens_map.json".into(), mirror_url: None },
                AssetFile { filename: "tokenizer_config.json".into(), url: "https://hf-mirror.com/Xenova/bge-small-zh-v1.5/resolve/main/tokenizer_config.json".into(), mirror_url: None },
            ],
            sha256: None,
            size_bytes: 35_500_000,
            description: "BGE-small-zh — Chinese sentence embedding, 384 dims".into(),
        },
        Asset {
            key: "all-MiniLM-L6-v2".into(),
            kind: AssetKind::Model,
            version: "v1".into(),
            primary_url: "https://hf-mirror.com/Xenova/all-MiniLM-L6-v2/resolve/main/onnx/model_quantized.onnx".into(),
            mirror_urls: vec![],
            extra_files: vec![
                AssetFile { filename: "tokenizer.json".into(), url: "https://hf-mirror.com/Xenova/all-MiniLM-L6-v2/resolve/main/tokenizer.json".into(), mirror_url: None },
                AssetFile { filename: "config.json".into(), url: "https://hf-mirror.com/Xenova/all-MiniLM-L6-v2/resolve/main/config.json".into(), mirror_url: None },
                AssetFile { filename: "special_tokens_map.json".into(), url: "https://hf-mirror.com/Xenova/all-MiniLM-L6-v2/resolve/main/special_tokens_map.json".into(), mirror_url: None },
                AssetFile { filename: "tokenizer_config.json".into(), url: "https://hf-mirror.com/Xenova/all-MiniLM-L6-v2/resolve/main/tokenizer_config.json".into(), mirror_url: None },
            ],
            sha256: None,
            size_bytes: 22_500_000,
            description: "all-MiniLM-L6-v2 — English sentence embedding, 384 dims".into(),
        },
        Asset {
            key: "reranker-en".into(),
            kind: AssetKind::Model,
            version: "v1".into(),
            primary_url: "https://hf-mirror.com/Xenova/ms-marco-MiniLM-L-6-v2/resolve/main/onnx/model_quantized.onnx".into(),
            mirror_urls: vec![],
            extra_files: vec![
                AssetFile { filename: "tokenizer.json".into(), url: "https://hf-mirror.com/Xenova/ms-marco-MiniLM-L-6-v2/resolve/main/tokenizer.json".into(), mirror_url: None },
                AssetFile { filename: "config.json".into(), url: "https://hf-mirror.com/Xenova/ms-marco-MiniLM-L-6-v2/resolve/main/config.json".into(), mirror_url: None },
                AssetFile { filename: "special_tokens_map.json".into(), url: "https://hf-mirror.com/Xenova/ms-marco-MiniLM-L-6-v2/resolve/main/special_tokens_map.json".into(), mirror_url: None },
                AssetFile { filename: "tokenizer_config.json".into(), url: "https://hf-mirror.com/Xenova/ms-marco-MiniLM-L-6-v2/resolve/main/tokenizer_config.json".into(), mirror_url: None },
            ],
            sha256: None,
            size_bytes: 90_000_000,
            description: "EN cross-encoder reranker — re-rank retrieved docs".into(),
        },
        Asset {
            key: "reranker-cn".into(),
            kind: AssetKind::Model,
            version: "v1".into(),
            primary_url: "https://hf-mirror.com/Xenova/bge-reranker-base/resolve/main/onnx/model_quantized.onnx".into(),
            mirror_urls: vec![],
            extra_files: vec![
                AssetFile { filename: "tokenizer.json".into(), url: "https://hf-mirror.com/Xenova/bge-reranker-base/resolve/main/tokenizer.json".into(), mirror_url: None },
                AssetFile { filename: "config.json".into(), url: "https://hf-mirror.com/Xenova/bge-reranker-base/resolve/main/config.json".into(), mirror_url: None },
                AssetFile { filename: "special_tokens_map.json".into(), url: "https://hf-mirror.com/Xenova/bge-reranker-base/resolve/main/special_tokens_map.json".into(), mirror_url: None },
                AssetFile { filename: "tokenizer_config.json".into(), url: "https://hf-mirror.com/Xenova/bge-reranker-base/resolve/main/tokenizer_config.json".into(), mirror_url: None },
            ],
            sha256: None,
            size_bytes: 280_000_000,
            description: "BGE cross-encoder reranker — bilingual CN+EN re-ranking".into(),
        },
    ]
}

// ── Platform runtimes ──────────────────────────────────────────────────

fn win_runtimes() -> Vec<Asset> {
    vec![
        Asset {
            key: "python".into(),
            kind: AssetKind::Runtime,
            version: PYTHON_VERSION.into(),
            primary_url: format!(
                "https://cdn.npmmirror.com/binaries/python/{0}/python-{0}-embed-amd64.zip",
                PYTHON_VERSION
            ),
            mirror_urls: vec![format!(
                "https://registry.npmmirror.com/-/binary/python/{0}/python-{0}-embed-amd64.zip",
                PYTHON_VERSION
            )],
            extra_files: vec![],
            sha256: None,
            size_bytes: 10_000_000,
            description: "Python 3.12 embeddable runtime (portable, no install)".into(),
        },
        Asset {
            key: "node".into(),
            kind: AssetKind::Runtime,
            version: NODE_VERSION.into(),
            primary_url: format!(
                "https://cdn.npmmirror.com/binaries/node/v{0}/node-v{0}-win-x64.zip",
                NODE_VERSION
            ),
            mirror_urls: vec![format!(
                "https://npmmirror.com/mirrors/node/v{0}/node-v{0}-win-x64.zip",
                NODE_VERSION
            )],
            extra_files: vec![],
            sha256: None,
            size_bytes: 30_000_000,
            description: "Node.js portable runtime".into(),
        },
        Asset {
            key: "git".into(),
            kind: AssetKind::Runtime,
            version: GIT_VERSION.into(),
            primary_url: format!(
                "https://cdn.npmmirror.com/binaries/git-for-windows/v{0}.windows.1/MinGit-{0}-64-bit.zip",
                GIT_VERSION
            ),
            mirror_urls: vec![format!(
                "https://npmmirror.com/mirrors/git-for-windows/v{0}.windows.1/MinGit-{0}-64-bit.zip",
                GIT_VERSION
            )],
            extra_files: vec![],
            sha256: None,
            size_bytes: 50_000_000,
            description: "MinGit portable (minimal Git for Windows)".into(),
        },
        Asset {
            key: "onnxruntime".into(),
            kind: AssetKind::Runtime,
            version: ONNX_VERSION.into(),
            primary_url: format!(
                "https://github.com/microsoft/onnxruntime/releases/download/v{0}/onnxruntime-win-x64-{0}.zip",
                ONNX_VERSION
            ),
            mirror_urls: vec![
                format!("https://cdn.npmmirror.com/binaries/onnxruntime/v{0}/onnxruntime-win-x64-{0}.zip", ONNX_VERSION),
                format!("https://registry.npmmirror.com/-/binary/onnxruntime/v{0}/onnxruntime-win-x64-{0}.zip", ONNX_VERSION),
            ],
            extra_files: vec![],
            sha256: None,
            size_bytes: 71_000_000,
            description: "ONNX Runtime for model inference".into(),
        },
    ]
}

fn mac_runtimes() -> Vec<Asset> {
    let mut rt = unix_runtimes("x86_64-apple-darwin");
    rt.push(system_git());
    rt
}

fn linux_runtimes() -> Vec<Asset> {
    let mut rt = unix_runtimes("x86_64-unknown-linux-gnu");
    rt.push(system_git());
    rt
}

fn system_git() -> Asset {
    Asset {
        key: "git".into(),
        kind: AssetKind::SystemProvided,
        version: "system".into(),
        primary_url: String::new(),
        mirror_urls: vec![],
        extra_files: vec![],
        sha256: None,
        size_bytes: 0,
        description: "Git (system-provided)".into(),
    }
}

/// Build runtime assets for a unix target triple.
/// `pbs_target` is the python-build-standalone target suffix
/// (e.g. "x86_64-apple-darwin", "x86_64-unknown-linux-gnu").
fn unix_runtimes(pbs_target: &str) -> Vec<Asset> {
    let (node_os, node_ext) = if pbs_target.contains("apple") {
        ("darwin", "tar.gz")
    } else {
        ("linux", "tar.xz")
    };
    let node_arch = if pbs_target.contains("aarch64") {
        "arm64"
    } else {
        "x64"
    };

    let (ort_os, ort_ext) = if pbs_target.contains("apple") {
        ("osx-universal2", "tgz")
    } else if pbs_target.contains("aarch64") {
        ("linux-aarch64", "tgz")
    } else {
        ("linux-x64", "tgz")
    };

    vec![
        Asset {
            key: "python".into(),
            kind: AssetKind::Runtime,
            version: PYTHON_VERSION.into(),
            primary_url: format!(
                "https://github.com/astral-sh/python-build-standalone/releases/download/{0}/cpython-{1}+{0}-{2}-install_only.tar.gz",
                PBS_RELEASE, PYTHON_VERSION, pbs_target
            ),
            mirror_urls: vec![],
            extra_files: vec![],
            sha256: None,
            size_bytes: 40_000_000,
            description: format!("Python {PYTHON_VERSION} standalone ({pbs_target})"),
        },
        Asset {
            key: "node".into(),
            kind: AssetKind::Runtime,
            version: NODE_VERSION.into(),
            primary_url: format!(
                "https://cdn.npmmirror.com/binaries/node/v{0}/node-v{0}-{1}-{2}.{3}",
                NODE_VERSION, node_os, node_arch, node_ext
            ),
            mirror_urls: vec![format!(
                "https://npmmirror.com/mirrors/node/v{0}/node-v{0}-{1}-{2}.{3}",
                NODE_VERSION, node_os, node_arch, node_ext
            )],
            extra_files: vec![],
            sha256: None,
            size_bytes: 40_000_000,
            description: format!("Node.js {NODE_VERSION} ({node_os}-{node_arch})"),
        },
        Asset {
            key: "onnxruntime".into(),
            kind: AssetKind::Runtime,
            version: ONNX_VERSION.into(),
            primary_url: format!(
                "https://github.com/microsoft/onnxruntime/releases/download/v{0}/onnxruntime-{1}-{0}.{2}",
                ONNX_VERSION, ort_os, ort_ext
            ),
            mirror_urls: vec![
                format!("https://cdn.npmmirror.com/binaries/onnxruntime/v{0}/onnxruntime-{1}-{0}.{2}", ONNX_VERSION, ort_os, ort_ext),
                format!("https://registry.npmmirror.com/-/binary/onnxruntime/v{0}/onnxruntime-{1}-{0}.{2}", ONNX_VERSION, ort_os, ort_ext),
            ],
            extra_files: vec![],
            sha256: None,
            size_bytes: 80_000_000,
            description: format!("ONNX Runtime {ONNX_VERSION} ({ort_os})"),
        },
    ]
}

/// Linux ARM64 runtimes: same as x86_64 but ARM64 URLs (deprecated — use unix_runtimes).
#[allow(dead_code)]
fn arm_linux_runtimes() -> Vec<Asset> {
    let mut rt = vec![
        Asset {
            key: "python".into(),
            kind: AssetKind::Runtime,
            version: PYTHON_VERSION.into(),
            primary_url: format!(
                "https://github.com/astral-sh/python-build-standalone/releases/download/{0}/cpython-{1}+{0}-aarch64-unknown-linux-gnu-install_only.tar.gz",
                PBS_RELEASE, PYTHON_VERSION
            ),
            mirror_urls: vec![],
            extra_files: vec![],
            sha256: None,
            size_bytes: 40_000_000,
            description: "Python 3.12 standalone (ARM64 Linux)".into(),
        },
        Asset {
            key: "node".into(),
            kind: AssetKind::Runtime,
            version: NODE_VERSION.into(),
            primary_url: format!(
                "https://cdn.npmmirror.com/binaries/node/v{0}/node-v{0}-linux-arm64.tar.xz",
                NODE_VERSION
            ),
            mirror_urls: vec![format!(
                "https://npmmirror.com/mirrors/node/v{0}/node-v{0}-linux-arm64.tar.xz",
                NODE_VERSION
            )],
            extra_files: vec![],
            sha256: None,
            size_bytes: 40_000_000,
            description: "Node.js portable runtime (ARM64 Linux)".into(),
        },
        Asset {
            key: "onnxruntime".into(),
            kind: AssetKind::Runtime,
            version: ONNX_VERSION.into(),
            primary_url: format!(
                "https://github.com/microsoft/onnxruntime/releases/download/v{0}/onnxruntime-linux-aarch64-{0}.tgz",
                ONNX_VERSION
            ),
            mirror_urls: vec![
                format!("https://cdn.npmmirror.com/binaries/onnxruntime/v{0}/onnxruntime-linux-aarch64-{0}.tgz", ONNX_VERSION),
            ],
            extra_files: vec![],
            sha256: None,
            size_bytes: 80_000_000,
            description: "ONNX Runtime for model inference (ARM64 Linux)".into(),
        },
    ];
    // Git is SystemProvided on all Linux variants
    rt.push(Asset {
        key: "git".into(),
        kind: AssetKind::SystemProvided,
        version: "system".into(),
        primary_url: String::new(),
        mirror_urls: vec![],
        extra_files: vec![],
        sha256: None,
        size_bytes: 0,
        description: "Git (system-provided via apt/dnf)".into(),
    });
    rt
}

// ── Target → assets mapping ────────────────────────────────────────────

/// Maps Rust target triples to their complete asset list (runtimes + models).
static PLATFORM_ASSETS: LazyLock<HashMap<&str, Vec<Asset>>> = LazyLock::new(|| {
    let mut map = HashMap::new();

    // ── Windows x64 ────────────────────────────────────────────────
    let mut win = win_runtimes();
    win.extend(shared_models());
    map.insert("x86_64-pc-windows-msvc", win);

    // ── macOS ARM64 ────────────────────────────────────────────────
    let mut mac_arm = unix_runtimes("aarch64-apple-darwin");
    mac_arm.push(system_git());
    mac_arm.extend(shared_models());
    map.insert("aarch64-apple-darwin", mac_arm);

    // ── macOS x64 ──────────────────────────────────────────────────
    let mut mac_x64 = mac_runtimes();
    mac_x64.extend(shared_models());
    map.insert("x86_64-apple-darwin", mac_x64);

    // ── Linux x64 ──────────────────────────────────────────────────
    let mut linux = linux_runtimes();
    linux.extend(shared_models());
    map.insert("x86_64-unknown-linux-gnu", linux);

    // ── Linux ARM64 ────────────────────────────────────────────────
    let mut linux_arm = unix_runtimes("aarch64-unknown-linux-gnu");
    linux_arm.push(system_git());
    linux_arm.extend(shared_models());
    map.insert("aarch64-unknown-linux-gnu", linux_arm);

    map
});

/// Return the full asset list for a given Rust target triple.
///
/// Falls back to Windows assets for unknown triples (the most tested path).
pub fn assets_for_target(target: &str) -> &[Asset] {
    PLATFORM_ASSETS
        .get(target)
        .map(|v| v.as_slice())
        .unwrap_or_else(|| {
            // Default to Windows (primary dev platform)
            PLATFORM_ASSETS
                .get("x86_64-pc-windows-msvc")
                .map(|v| v.as_slice())
                .unwrap_or(&[])
        })
}

/// Detect the current host target triple at runtime.
#[allow(clippy::disallowed_methods)]
pub fn detect_target() -> String {
    // rustc -vV prints "host: x86_64-pc-windows-msvc"
    if let Ok(output) = std::process::Command::new("rustc").arg("-vV").output() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if let Some(triple) = line.strip_prefix("host: ") {
                return triple.trim().to_string();
            }
        }
    }
    // Fallback: cfg-based detection (when rustc is not available)
    if cfg!(target_os = "windows") {
        "x86_64-pc-windows-msvc"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(target_os = "macos") {
        "x86_64-apple-darwin"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(target_arch = "x86_64") {
        "x86_64-unknown-linux-gnu"
    } else {
        // Generic fallback for unknown Linux targets
        "x86_64-unknown-linux-gnu"
    }
    .to_string()
}

/// Legacy Windows-only statics — kept for backward compatibility in tests
/// and for code that hasn't been updated to use `assets_for_target()`.
#[allow(dead_code)]
static RUNTIMES: LazyLock<Vec<Asset>> = LazyLock::new(win_runtimes);
#[allow(dead_code)]
static MODELS: LazyLock<Vec<Asset>> = LazyLock::new(shared_models);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtimes_defined() {
        assert_eq!(RUNTIMES.len(), 4); // python, node, git, onnxruntime
        assert_eq!(MODELS.len(), 4); // 2 embeddings + 2 rerankers
    }

    #[test]
    fn test_asset_urls_valid() {
        for asset in RUNTIMES.iter().chain(MODELS.iter()) {
            assert!(
                asset.primary_url.starts_with("https://"),
                "Invalid URL for {}: {}",
                asset.key,
                asset.primary_url
            );
        }
    }

    #[test]
    fn test_python_is_embeddable() {
        let python = RUNTIMES.iter().find(|a| a.key == "python").unwrap();
        assert!(
            python.primary_url.contains("embed"),
            "Python must be embeddable version"
        );
    }

    #[test]
    fn test_git_is_mingit() {
        let git = RUNTIMES.iter().find(|a| a.key == "git").unwrap();
        assert!(
            git.primary_url.contains("MinGit"),
            "Git must be MinGit portable"
        );
    }

    #[test]
    fn test_models_have_primary_url() {
        for model in MODELS.iter() {
            assert!(
                !model.primary_url.is_empty(),
                "Model {} lacks primary URL",
                model.key
            );
        }
    }

    #[test]
    fn test_total_download_size() {
        let runtime_size: u64 = RUNTIMES.iter().map(|a| a.size_bytes).sum();
        let model_size: u64 = MODELS.iter().map(|a| a.size_bytes).sum();
        // ~90MB runtimes + ~57MB models = ~147MB
        assert!(
            runtime_size > 50_000_000,
            "Runtime estimate too low: {runtime_size}"
        );
        assert!(
            model_size > 30_000_000,
            "Model estimate too low: {model_size}"
        );
    }
}
