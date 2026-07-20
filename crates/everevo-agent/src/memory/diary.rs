//! Diary Manager — LIGHT phase of the dreaming pipeline.
//!
//! Reads raw SQLite messages → LLM trims noise → writes daily diary files.
//!
//! ## Append Behavior
//!
//! Diary files are **append-only per date**:
//! - `diary/2026-07-19.md` exists → new LIGHT entries append to it
//! - Same date, same file — never overwritten, always appended
//! - Different dates get separate files
//!
//! Diary files are pipeline-only artifacts (humans do not edit them).

use std::path::{Path, PathBuf};

use everevo_core::EverEvoError;

/// Manages the diary directory (data/memory/diary/).
///
/// ## Data flow
/// ```text
/// SQLite messages (READ-ONLY)
///   → LLM trim (remove greetings, confirmations, tool noise)
///   → diary/YYYY-MM-DD.md
/// ```
pub struct DiaryManager {
    diary_dir: PathBuf,
}

/// A single entry in a diary file, representing a distilled conversation segment.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DiaryEntry {
    /// Timestamp of the first message in this segment.
    pub timestamp: String,
    /// Session ID this entry came from.
    pub session_id: String,
    /// Distilled content — LLM-trimmed, meaningful conversation only.
    pub content: String,
    /// Source pointers to original SQLite messages.
    pub source_message_ids: Vec<String>,
}

impl DiaryManager {
    /// Create a new diary manager. Creates the diary directory if missing.
    pub fn new(diary_dir: impl Into<PathBuf>) -> Result<Self, EverEvoError> {
        let diary_dir: PathBuf = diary_dir.into();
        std::fs::create_dir_all(&diary_dir).map_err(|e| {
            EverEvoError::Internal(format!("Failed to create diary dir: {e}"))
        })?;
        Ok(Self { diary_dir })
    }

    /// Path to a specific day's diary file.
    pub fn diary_path(&self, date: &str) -> PathBuf {
        self.diary_dir.join(format!("{date}.md"))
    }

    /// Read today's diary entries (raw markdown).
    pub fn read_today(&self) -> Result<String, EverEvoError> {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        self.read_date(&today)
    }

    /// Read a specific date's diary.
    pub fn read_date(&self, date: &str) -> Result<String, EverEvoError> {
        let path = self.diary_path(date);
        if !path.exists() {
            return Ok(String::new());
        }
        std::fs::read_to_string(&path)
            .map_err(|e| EverEvoError::Internal(format!("Read diary: {e}")))
    }

    /// Append entries to today's diary.
    /// Each entry is appended with a timestamp header.
    pub fn append_entries(&self, entries: &[DiaryEntry]) -> Result<(), EverEvoError> {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        self.append_entries_to_date(&today, entries)
    }

    /// Append entries to a specific date's diary.
    pub fn append_entries_to_date(&self, date: &str, entries: &[DiaryEntry]) -> Result<(), EverEvoError> {
        let path = self.diary_path(date);
        let mut content = if path.exists() {
            std::fs::read_to_string(&path).unwrap_or_default()
        } else {
            String::from("# Diary — {date}\n\n")
        };

        for entry in entries {
            content.push_str(&format!(
                "## {ts} (session: {sid})\n\n{body}\n\n",
                ts = entry.timestamp,
                sid = &entry.session_id[..8.min(entry.session_id.len())],
                body = entry.content.trim(),
            ));
        }

        std::fs::write(&path, &content)
            .map_err(|e| EverEvoError::Internal(format!("Write diary: {e}")))?;

        tracing::info!(date, count = entries.len(), "Diary entries appended");
        Ok(())
    }

    /// List all diary files (for REM phase to read recent days).
    pub fn list_files(&self) -> Result<Vec<PathBuf>, EverEvoError> {
        let mut files = Vec::new();
        let entries = std::fs::read_dir(&self.diary_dir).map_err(|e| {
            EverEvoError::Internal(format!("Read diary dir: {e}"))
        })?;
        for entry in entries {
            let entry = entry.map_err(|e| EverEvoError::Internal(format!("Dir entry: {e}")))?;
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "md") {
                files.push(path);
            }
        }
        files.sort();
        Ok(files)
    }

    /// Read the most recent N diary files (for REM phase).
    pub fn read_recent(&self, n: usize) -> Result<Vec<(String, String)>, EverEvoError> {
        let mut files = self.list_files()?;
        files.reverse();
        files.truncate(n);

        let mut result = Vec::new();
        for path in &files {
            let date = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            let content = std::fs::read_to_string(path).unwrap_or_default();
            result.push((date, content));
        }
        Ok(result)
    }

    /// Get the diary directory path.
    pub fn diary_dir(&self) -> &Path {
        &self.diary_dir
    }

    /// Build a prompt for the LLM to trim raw conversation messages.
    /// The LLM should remove greetings, confirmations, tool noise, and
    /// keep only substantive content (decisions, facts, preferences, tasks).
    pub fn build_trim_prompt(messages: &[(String, String, String)]) -> String {
        // messages: Vec<(role, content, message_id)>
        let conversation: String = messages
            .iter()
            .map(|(role, content, _id)| format!("[{role}] {content}"))
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            "You are a conversation distiller. Given the following raw conversation, \
             remove ALL of the following:\n\
             - Greetings (\"你好\", \"Hello\", \"Hi\", etc.)\n\
             - Confirmations (\"好的\", \"OK\", \"Got it\", etc.)\n\
             - Tool output noise (command results, file listings, etc.) unless they contain key facts\n\
             - Repetitive content\n\n\
             KEEP only:\n\
             - Decisions the user or assistant made\n\
             - Facts stated about code, projects, or preferences\n\
             - Tasks discussed or assigned\n\
             - User feedback or corrections to the assistant\n\n\
             Return the distilled conversation as a concise summary. \
             If nothing substantive is found, return \"[NO_SUBSTANCE]\".\n\n\
             === RAW CONVERSATION ===\n\n{conversation}\n\n=== DISTILLED ==="
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_append_and_read() {
        let dir = TempDir::new().unwrap();
        let mgr = DiaryManager::new(dir.path()).unwrap();

        let entries = vec![DiaryEntry {
            timestamp: "2026-07-19T10:00:00Z".into(),
            session_id: "abc12345-1234-1234-1234-123456789abc".into(),
            content: "用户决定使用 async/await 语法".into(),
            source_message_ids: vec!["msg-1".into()],
        }];

        mgr.append_entries_to_date("2026-07-19", &entries).unwrap();
        let content = mgr.read_date("2026-07-19").unwrap();
        assert!(content.contains("async/await"));
    }

    #[test]
    fn test_trim_prompt() {
        let msgs = vec![
            ("user".into(), "你好".into(), "1".into()),
            ("assistant".into(), "你好！有什么可以帮你的？".into(), "2".into()),
            ("user".into(), "帮我写个 Rust 函数".into(), "3".into()),
        ];
        let prompt = DiaryManager::build_trim_prompt(&msgs);
        assert!(prompt.contains("Rust"));
        assert!(prompt.contains("DISTILLED"));
    }
}
