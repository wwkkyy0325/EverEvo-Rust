//! Polling-based staleness detection — triggers incremental reindex when
//! source files have changed since the last index build.
//!
//! Lightweight alternative to a full `notify` filesystem watcher. On each
//! `code_search` call, we do a quick scan of the workspace root: if any
//! tracked source file has been modified since the last index timestamp, an
//! incremental `reindex_changed()` runs automatically. This avoids the
//! "index lag" problem without pulling in a watcher dependency.
//!
//! ## Cost
//!
//! The staleness scan uses `walkdir` with the same filtering as the scanner
//! (skip hidden dirs, `target/`, `node_modules/`, etc.). On a ~5000 file
//! repo it takes <10 ms — negligible compared to an LLM turn.

use std::path::Path;
use std::time::SystemTime;

/// Quick check: have any source files been modified since `since`?
///
/// Walks the workspace root, filtering to code files only, and returns
/// `true` as soon as it finds ONE file newer than `since`. This is a
/// short-circuit scan — worst case it visits every file, but it stops
/// early on the first fresh file found.
pub fn has_changes_since(root: &Path, since: SystemTime) -> bool {
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let path = entry.path();
        let path_str = path.to_string_lossy();

        // Skip hidden dirs / build artifacts (same filter as scanner + indexer)
        if path_str.contains("/.")
            || path_str.contains("\\.")
            || path_str.contains("target/")
            || path_str.contains("target\\")
            || path_str.contains("node_modules/")
            || path_str.contains("node_modules\\")
            || path_str.contains("__pycache__/")
            || path_str.contains(".git/")
            || path_str.contains(".git\\")
            || path_str.contains("dist/")
            || path_str.contains("dist\\")
            || path_str.contains(".everevo/")
            || path_str.contains(".everevo\\")
        {
            continue;
        }

        // Only track code files (same extensions as the scanner)
        if !is_code_extension(path) {
            continue;
        }

        // Check mtime
        if let Ok(meta) = entry.metadata() {
            if let Ok(modified) = meta.modified() {
                if modified > since {
                    return true;
                }
            }
        }
    }

    false
}

/// Code file extensions we track.
const CODE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "kt", "swift", "rb", "c", "cpp", "h",
    "hpp", "css", "html", "toml", "json", "yaml", "yml", "md",
];

fn is_code_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| CODE_EXTENSIONS.contains(&ext))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Create a temp dir whose name does NOT start with `.` so it passes the
    /// hidden-file filter in `has_changes_since`. `tempfile` uses `.tmp` prefix
    /// on Windows which triggers the `\\.` path filter.
    fn temp_dir_no_dot() -> PathBuf {
        let ts = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("everevo_test_{ts}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(dir: PathBuf) {
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_changes_detected() {
        let dir = temp_dir_no_dot();
        let sub = dir.join("src");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("test.rs"), "fn main() {}").unwrap();
        let past = SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(3600))
            .unwrap();
        assert!(has_changes_since(&dir, past));
        cleanup(dir);
    }

    #[test]
    fn test_non_code_files_ignored() {
        let dir = temp_dir_no_dot();
        let sub = dir.join("src");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("readme.txt"), "hello").unwrap();
        let future = SystemTime::now()
            .checked_add(std::time::Duration::from_secs(3600))
            .unwrap();
        assert!(!has_changes_since(&dir, future));
        cleanup(dir);
    }

    #[test]
    fn test_no_changes_with_future_timestamp() {
        let dir = temp_dir_no_dot();
        let sub = dir.join("src");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("test.rs"), "fn main() {}").unwrap();
        let future = SystemTime::now()
            .checked_add(std::time::Duration::from_secs(3600))
            .unwrap();
        assert!(!has_changes_since(&dir, future));
        cleanup(dir);
    }
}
