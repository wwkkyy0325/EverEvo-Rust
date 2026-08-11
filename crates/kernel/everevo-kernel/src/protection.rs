//! Kernel self-protection — defines what the kernel considers immutable.
//!
//! ## Design Rationale
//!
//! The kernel is the **only code that an agent must never modify**. If the
//! agent can edit kernel source, recompile, and restart, there's no guarantee
//! the self-repair bootstrap tools still work on the next startup.
//!
//! ## Protection Layers
//!
//! | Layer | Mechanism | What it blocks |
//! |-------|-----------|----------------|
//! | **Path** | Glob-based deny list for all writes | Kernel source, binary, config |
//! | **Compile** | Package name prefix check | `cargo build -p everevo-kernel` |
//! | **Integrity** | SHA256 self-check on startup | Tampered kernel binary |
//!
//! ## What is protected
//!
//! - `crates/kernel/` — all kernel source code (everevo-core, everevo-mcp, etc.)
//! - `Cargo.toml`, `Cargo.lock` — workspace-level build config
//! - `src-tauri/` — Tauri desktop shell (if present)
//! - `target/release/everevo-server*` — compiled kernel binary
//! - `target/release/everevo.exe`, `everevo-kernel*` — kernel binaries
//!
//! ## What is NOT protected (by design)
//!
//! - `plugins/` — MCP plugin source and binaries (agent-safe to modify)
//! - `crates/app/` — application-layer crates (changeable, non-kernel)
//! - `crates/infra/` — infrastructure crates (changeable, non-kernel)
//! - `frontend/` — web UI
//! - `data/` — runtime data

use std::path::Path;

// ── Protected Paths ────────────────────────────────────────────────────────

/// Glob patterns for directories/files the kernel will NEVER write to.
///
/// These patterns are checked by `write_file`, `shell`, and `plugin_dev edit`.
/// Patterns listed here are DIRECTORIES (containing `**`) — individual file
/// names like `Cargo.toml` are handled by `kernel_protected_files()` +
/// `is_workspace_level()` which correctly distinguishes workspace-level from
/// plugin-level files.
pub(crate) fn kernel_protected_globs() -> Vec<&'static str> {
    vec![
        // ── Kernel source ────────────────────────────────────────────
        "**/crates/kernel/**",
        // ── Tauri desktop shell ──────────────────────────────────────
        "**/src-tauri/**",
        // ── Compiled kernel binaries ─────────────────────────────────
        "**/target/release/everevo-server*",
        "**/target/release/everevo.exe",
        "**/target/release/everevo-kernel*",
        "**/target/release/everevo_core*",
        "**/target/release/everevo_mcp*",
        // ── Migration files (DB schema) ──────────────────────────────
        "**/migrations/**",
        // ── Git internals ────────────────────────────────────────────
        "**/.git/**",
    ]
}

/// Protected directory names — if a path CONTAINS any of these as a
/// directory component, it's blocked regardless of glob matching.
/// This catches edge cases like unusual path separators or symlinks.
pub(crate) fn kernel_protected_dirs() -> Vec<&'static str> {
    vec!["crates/kernel", "src-tauri", "migrations"]
}

/// Protected file names (exact match on filename, any directory).
pub(crate) fn kernel_protected_files() -> Vec<&'static str> {
    vec![
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        "everevo-server.exe",
        "everevo-server",
        "everevo.exe",
    ]
}

/// Kernel binary names that must never be built by plugin_dev.
pub(crate) fn kernel_package_names() -> Vec<&'static str> {
    vec![
        "everevo-kernel",
        "everevo-core",
        "everevo-mcp",
        "everevo-mcp-protocol",
    ]
}

// ── Path Validation ────────────────────────────────────────────────────────

/// Check whether a filesystem path is inside a kernel-protected area.
///
/// Returns `true` if the path is PROTECTED (writes should be blocked).
/// Returns `false` if the path is safe to write to.
pub fn is_kernel_protected(path: &str) -> bool {
    let normalized = normalize_path(path);

    // 1. Check directory components
    for protected_dir in kernel_protected_dirs() {
        let dir_normalized = normalize_path(protected_dir);
        if normalized.contains(&format!("/{dir_normalized}/"))
            || normalized.starts_with(&format!("{dir_normalized}/"))
            || normalized == dir_normalized
        {
            return true;
        }
    }

    // 2. Check exact file name matches (for workspace-level files)
    if let Some(filename) = normalized.rsplit('/').next() {
        for protected_file in kernel_protected_files() {
            if filename.eq_ignore_ascii_case(protected_file) {
                // Only block workspace-level config files, not plugin-level
                // e.g. block "/Cargo.toml" but allow "plugins/tools/search/Cargo.toml"
                if is_workspace_level(&normalized, filename) {
                    return true;
                }
            }
        }
    }

    // 3. Check glob patterns
    for pattern in kernel_protected_globs() {
        if glob_match_kernel(pattern, &normalized) {
            return true;
        }
    }

    false
}

/// Returns true only for workspace-root-level files (not plugin-level).
fn is_workspace_level(normalized_path: &str, filename: &str) -> bool {
    // Count path depth. Workspace root files are depth 1 (just "/Cargo.toml")
    // Plugin Cargo.toml would be at least depth 3: "/plugins/tools/search/Cargo.toml"
    let depth = normalized_path.split('/').filter(|s| !s.is_empty()).count();
    if depth <= 1 {
        return true;
    }

    // Also protect files directly under crates/ or src-tauri/
    if normalized_path.contains("/crates/") || normalized_path.contains("/src-tauri/") {
        return true;
    }

    // Also protect files directly under target/release/ that match kernel binary names
    if normalized_path.contains("/target/release/") {
        let kernel_names = [
            "everevo-server",
            "everevo-server.exe",
            "everevo.exe",
            "everevo-kernel",
            "everevo-kernel.exe",
            "everevo_core",
            "everevo_mcp",
        ];
        for kn in &kernel_names {
            if filename.eq_ignore_ascii_case(kn) {
                return true;
            }
        }
    }

    false
}

/// Normalize a path for comparison: backslash → forward slash, lowercase,
/// strip `./` and `.\` prefixes, collapse multiple slashes.
pub fn normalize_path(path: &str) -> String {
    let mut s = path.replace('\\', "/").to_lowercase();
    // Strip leading ./ (relative path indicator)
    while s.starts_with("./") {
        s = s[2..].to_string();
    }
    // Collapse multiple consecutive slashes
    while s.contains("//") {
        s = s.replace("//", "/");
    }
    s
}

/// Simple glob matching for kernel protection patterns.
/// Supports `**` (any depth, including zero) and `*` (within segment).
///
/// Handles both absolute paths (`f:/workspace/crates/kernel/src/lib.rs`)
/// and relative paths (`crates/kernel/src/lib.rs`).
fn glob_match_kernel(pattern: &str, path: &str) -> bool {
    let pattern = normalize_path(pattern);
    let path = normalize_path(path);

    // Convert glob to regex:
    // - `**/` → `(.*/)?` (optional directory prefix, zero or more segments)
    // - Trailing `**` → `.*` (match everything to end)
    // - `*` → `[^/]*` (match within a single segment)
    // First, temporarily replace **/ patterns to avoid double-processing
    let escaped = pattern
        .replace('.', "\\.")
        .replace("**/", "___DS_DIR___")
        .replace("**", "___DS_ANY___")
        .replace('*', "[^/]*")
        .replace("___DS_DIR___", "(.*/)?")
        .replace("___DS_ANY___", ".*");

    // Ensure leading optional-directory also matches empty (no leading /)
    // e.g. `.git/config` should match `**/.git/**`
    let anchored = format!("^{escaped}$");

    regex_lite::Regex::new(&anchored)
        .map(|re| re.is_match(&path))
        .unwrap_or(false)
}

/// Check whether a Cargo package name is a kernel crate.
/// Used by `plugin_dev build` to prevent building kernel packages.
pub fn is_kernel_package(pkg_name: &str) -> bool {
    let normalized = pkg_name.to_lowercase();
    kernel_package_names()
        .iter()
        .any(|k| normalized == k.to_lowercase())
}

/// Validate that a plugin package name follows the `plugin-*` convention.
/// Returns `true` if the name looks like a plugin package.
pub fn is_plugin_package(pkg_name: &str) -> bool {
    let normalized = pkg_name.to_lowercase();
    normalized.starts_with("plugin-")
}

/// Compute SHA256 of a file. Used for kernel binary integrity check.
pub fn sha256_file(path: &Path) -> Result<String, std::io::Error> {
    use sha2::Digest;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = std::io::Read::read(&mut file, &mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Verify the current running binary against a known-good checksum.
///
/// On startup, the kernel computes SHA256 of its own binary and compares
/// against a checksum file. If they don't match, the kernel has been tampered
/// with — it logs a critical warning and returns `false`.
///
/// The checksum file is written once at build time (or first run) and is
/// itself in a protected path (`data/` is not protected but the checksum
/// comparison detects changes).
pub fn verify_kernel_integrity(expected_checksum: &str) -> Result<bool, String> {
    let exe_path =
        std::env::current_exe().map_err(|e| format!("Cannot get current exe path: {e}"))?;

    let actual = sha256_file(&exe_path).map_err(|e| format!("SHA256 of kernel binary: {e}"))?;

    Ok(actual == expected_checksum)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_kernel_protected ────────────────────────────────────────────

    #[test]
    fn test_kernel_source_is_protected() {
        assert!(is_kernel_protected(
            "crates/kernel/everevo-kernel/src/lib.rs"
        ));
        assert!(is_kernel_protected(
            "crates\\kernel\\everevo-core\\src\\lib.rs"
        ));
        assert!(is_kernel_protected(
            "crates/kernel/everevo-mcp/src/client.rs"
        ));
    }

    #[test]
    fn test_cargo_toml_at_workspace_root_is_protected() {
        assert!(is_kernel_protected("Cargo.toml"));
        assert!(is_kernel_protected("./Cargo.toml"));
    }

    #[test]
    fn test_plugin_cargo_toml_is_not_protected() {
        assert!(!is_kernel_protected("plugins/tools/search/Cargo.toml"));
        assert!(!is_kernel_protected("plugins/hooks/audit/Cargo.toml"));
    }

    #[test]
    fn test_cargo_lock_is_protected() {
        assert!(is_kernel_protected("Cargo.lock"));
    }

    #[test]
    fn test_kernel_binary_is_protected() {
        assert!(is_kernel_protected("target/release/everevo-server.exe"));
        assert!(is_kernel_protected("target/release/everevo-server"));
        assert!(is_kernel_protected("target/release/everevo.exe"));
    }

    #[test]
    fn test_plugin_binary_is_not_protected() {
        assert!(!is_kernel_protected("target/release/plugin-search.exe"));
        assert!(!is_kernel_protected("target/release/plugin_web_search.exe"));
    }

    #[test]
    fn test_migrations_are_protected() {
        assert!(is_kernel_protected("migrations/001_init.sql"));
        assert!(is_kernel_protected("migrations/002_add_users.sql"));
    }

    #[test]
    fn test_src_tauri_is_protected() {
        assert!(is_kernel_protected("src-tauri/Cargo.toml"));
        assert!(is_kernel_protected("src-tauri/src/main.rs"));
    }

    #[test]
    fn test_git_internals_are_protected() {
        assert!(is_kernel_protected(".git/config"));
        assert!(is_kernel_protected(".git/HEAD"));
    }

    #[test]
    fn test_plugin_source_is_not_protected() {
        assert!(!is_kernel_protected("plugins/tools/search/src/main.rs"));
        assert!(!is_kernel_protected(
            "plugins/stages/best_practices/src/main.rs"
        ));
        assert!(!is_kernel_protected("plugins/hooks/audit/src/main.rs"));
    }

    #[test]
    fn test_app_crates_are_not_protected() {
        assert!(!is_kernel_protected(
            "crates/app/everevo-server/src/main.rs"
        ));
        assert!(!is_kernel_protected("crates/app/everevo-agent/src/lib.rs"));
    }

    #[test]
    fn test_infra_crates_are_not_protected() {
        assert!(!is_kernel_protected(
            "crates/infra/everevo-sandbox/src/provider.rs"
        ));
        assert!(!is_kernel_protected(
            "crates/infra/everevo-db/src/models.rs"
        ));
    }

    #[test]
    fn test_data_dir_is_not_protected() {
        assert!(!is_kernel_protected("data/sandbox/test/output.txt"));
        assert!(!is_kernel_protected("data/db/everevo.db"));
    }

    #[test]
    fn test_frontend_is_not_protected() {
        assert!(!is_kernel_protected("frontend/src/App.tsx"));
        assert!(!is_kernel_protected("frontend/package.json"));
    }

    // ── is_kernel_package ──────────────────────────────────────────────

    #[test]
    fn test_kernel_package_names_are_detected() {
        assert!(is_kernel_package("everevo-kernel"));
        assert!(is_kernel_package("everevo-core"));
        assert!(is_kernel_package("everevo-mcp"));
        assert!(is_kernel_package("everevo-mcp-protocol"));
    }

    #[test]
    fn test_plugin_package_names_are_not_kernel() {
        assert!(!is_kernel_package("plugin-search"));
        assert!(!is_kernel_package("plugin-web-search"));
        assert!(!is_kernel_package("plugin-hooks-audit"));
    }

    // ── is_plugin_package ──────────────────────────────────────────────

    #[test]
    fn test_plugin_package_detection() {
        assert!(is_plugin_package("plugin-search"));
        assert!(is_plugin_package("plugin-web_search"));
        assert!(is_plugin_package("plugin-hooks-audit"));
        assert!(is_plugin_package("plugin-stages-best_practices"));
    }

    #[test]
    fn test_non_plugin_packages() {
        assert!(!is_plugin_package("everevo-kernel"));
        assert!(!is_plugin_package("everevo-server"));
        assert!(!is_plugin_package("serde"));
    }

    // ── normalize_path ─────────────────────────────────────────────────

    #[test]
    fn test_normalize_backslashes() {
        assert_eq!(
            normalize_path(r"crates\kernel\everevo-kernel\src\lib.rs"),
            "crates/kernel/everevo-kernel/src/lib.rs"
        );
    }

    #[test]
    fn test_normalize_lowercase() {
        assert_eq!(normalize_path("Cargo.TOML"), "cargo.toml");
    }

    // ── glob_match_kernel ──────────────────────────────────────────────

    #[test]
    fn test_glob_match_deep_paths() {
        assert!(glob_match_kernel(
            "**/crates/kernel/**",
            "f:/workspace/everevo/crates/kernel/everevo-kernel/src/lib.rs"
        ));
        assert!(glob_match_kernel(
            "**/target/release/everevo-server*",
            "/home/user/everevo/target/release/everevo-server.exe"
        ));
    }
}
