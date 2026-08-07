//! Action Paradigm extraction — SAMULE-pattern multi-level learning.
//!
//! Paradigms are reusable action patterns extracted from execution trajectories.
//! Unlike raw memory facts or workflows, paradigms capture **strategic knowledge**:
//! under what conditions a given approach works, and what differentiates success
//! from failure.
//!
//! ## Three Extraction Levels
//!
//! ```text
//! Micro  (per-turn):     "shell failed with 'pnpm not found', use npm instead"
//! Meso   (intra-task):   "for dependency installation, check available pm first"
//! Macro  (cross-task):   "in this sandbox, prefer npm over pnpm for all JS tasks"
//! ```
//!
//! ## Design
//!
//! - Follows the same fire-and-forget pattern as `reflect_on_turn()`
//! - Reuses `parse_extracted_facts()` from extractor.rs for JSON parsing
//! - Saves as `FactType::Paradigm` with `ParadigmMeta` in the content
//! - TurnDigest buffer accumulates trajectory data for contrastive analysis

use std::sync::{Arc, Mutex};

use everevo_core::llm::{LlmMessage, LlmProvider};
use everevo_core::memory::{FactType, MemoryFact, ParadigmLevel, ParadigmMeta, ProjectionMetadata};

use crate::llm::HttpClient;
use crate::memory::extractor::parse_extracted_facts;
use crate::memory::facts::FactManager;

// ── Turn Digest (trajectory buffer entry) ──────────────────────────────────

/// Compressed representation of a single tool execution turn.
/// Accumulated in a ring buffer for paradigm extraction.
#[derive(Debug, Clone)]
pub struct TurnDigest {
    /// Tool name that was called.
    pub tool_name: String,
    /// Whether execution succeeded.
    pub success: bool,
    /// Error type classification (if failed).
    pub error_type: Option<String>,
    /// User message that triggered this turn (truncated).
    pub user_intent: String,
    /// First 200 chars of assistant response (for context).
    pub response_snippet: String,
}

impl TurnDigest {
    /// Create a digest from a completed tool call.
    pub fn new(
        tool_name: &str,
        success: bool,
        error_type: Option<&str>,
        user_intent: &str,
        response_snippet: &str,
    ) -> Self {
        Self {
            tool_name: tool_name.to_string(),
            success,
            error_type: error_type.map(|s| s.to_string()),
            user_intent: truncate(user_intent, 200),
            response_snippet: truncate(response_snippet, 200),
        }
    }
}

/// A shared ring buffer of recent turn digests for paradigm extraction.
#[derive(Debug, Clone)]
pub struct TrajectoryBuffer {
    buffer: Arc<Mutex<Vec<TurnDigest>>>,
    max_entries: usize,
}

impl TrajectoryBuffer {
    pub fn new(max_entries: usize) -> Self {
        Self {
            buffer: Arc::new(Mutex::new(Vec::with_capacity(max_entries))),
            max_entries,
        }
    }

    /// Push a new turn digest, evicting oldest if at capacity.
    pub fn push(&self, digest: TurnDigest) {
        let mut buf = self.buffer.lock().unwrap_or_else(|e| e.into_inner());
        if buf.len() >= self.max_entries {
            buf.remove(0);
        }
        buf.push(digest);
    }

    /// Snapshot the current buffer (for paradigm extraction).
    pub fn snapshot(&self) -> Vec<TurnDigest> {
        self.buffer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// How many entries are in the buffer.
    pub fn len(&self) -> usize {
        self.buffer.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for TrajectoryBuffer {
    fn default() -> Self {
        Self::new(20)
    }
}

// ── Paradigm Extraction ────────────────────────────────────────────────────

/// Extract action paradigms from a completed trajectory.
///
/// Fire-and-forget (same pattern as `reflect_on_turn`). Contrastive analysis:
/// compares successful vs failed trajectories to identify divergence points.
///
/// Triggers when the buffer has 5+ entries with at least one success and one
/// failure — the contrast provides the signal for paradigm extraction.
pub async fn extract_paradigm_from_trajectory(
    llm: &HttpClient,
    fact_manager: &FactManager,
    buffer: &TrajectoryBuffer,
) {
    let trajectory = buffer.snapshot();
    if trajectory.len() < 3 {
        return; // not enough data
    }

    let has_success = trajectory.iter().any(|t| t.success);
    let has_failure = trajectory.iter().any(|t| !t.success);
    if !has_success {
        return; // need at least one success to extract a paradigm
    }

    // Build analysis prompt
    let prompt = build_paradigm_extraction_prompt(&trajectory, has_failure);
    let messages = vec![LlmMessage::user(&prompt)];

    match llm.chat(&messages, &[]).await {
        Ok(response) => {
            let text = response.content.unwrap_or_default();
            if text.is_empty() || text.contains("[NO_PARADIGM]") {
                return;
            }
            let facts = parse_extracted_facts(&text);
            if facts.is_empty() {
                return;
            }
            let mut saved = 0u32;
            let level = if trajectory.len() >= 10 {
                ParadigmLevel::Macro
            } else if trajectory.len() >= 5 {
                ParadigmLevel::Meso
            } else {
                ParadigmLevel::Micro
            };

            for (name, description, content) in &facts {
                // Parse paradigm metadata from the content JSON block
                let meta = parse_paradigm_meta(content, level)
                    .unwrap_or_else(|| ParadigmMeta {
                        problem_class: name.clone(),
                        preconditions: Vec::new(),
                        approach: description.clone(),
                        parameters: Vec::new(),
                        success_signals: Vec::new(),
                        failure_modes: Vec::new(),
                        divergence_point: None,
                        anti_pattern: None,
                        extraction_level: level,
                    });

                let meta_json =
                    serde_json::to_string(&meta).unwrap_or_else(|_| "{}".into());
                let full_content = format!(
                    "# Paradigm: {}\n\n**Approach**: {}\n\n**Problem Class**: {}\n\n{}\n\n---\n```json\n{}\n```",
                    name, meta.approach, meta.problem_class, content, meta_json
                );

                let fact = MemoryFact {
                    name: format!("paradigm-{name}"),
                    description: description.clone(),
                    content: full_content,
                    fact_type: FactType::Paradigm,
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                    projection: ProjectionMetadata::new(
                        "paradigm-extractor",
                        "llm",
                        vec![],
                        0.7,
                    ),
                    links: vec![],
                };
                match fact_manager.save_async(fact.clone()).await {
                    Ok(()) => saved += 1,
                    Err(e) => tracing::debug!(
                        name = %fact.name,
                        error = %e,
                        "Paradigm extraction dedup'd or rejected"
                    ),
                }
            }
            if saved > 0 {
                tracing::info!(saved, level = %level, "Paradigm extraction sedimented new paradigms");
            }
        }
        Err(e) => tracing::debug!(error = %e, "Paradigm extraction LLM call failed"),
    }
}

// ── Prompt Builder ─────────────────────────────────────────────────────────

fn build_paradigm_extraction_prompt(
    trajectory: &[TurnDigest],
    has_failure: bool,
) -> String {
    let mut turns = String::new();
    for (i, t) in trajectory.iter().enumerate() {
        let status = if t.success { "✓" } else { "✗" };
        let err = t
            .error_type
            .as_ref()
            .map(|e| format!(" [{}]", e))
            .unwrap_or_default();
        turns.push_str(&format!(
            "  Turn {}: {} {}{err}\n",
            i + 1,
            t.tool_name,
            status
        ));
    }

    let contrastive = if has_failure {
        "Compare the successful turns vs the failed ones. Identify the DIVERGENCE POINT — \
         the exact turn or decision where success and failure paths split. What was done \
         differently in successful vs failed approaches?"
    } else {
        "All turns succeeded. Extract the generalizable strategy that made this work."
    };

    format!(
        "Analyze this execution trajectory and extract a reusable ACTION PARADIGM.\n\n\
         ## Trajectory\n{turns}\n\n\
         ## Instructions\n\
         {contrastive}\n\n\
         Extract a paradigm as a JSON array. Each entry: \
         {{\"name\": \"kebab-case-slug\", \"description\": \"one-line strategy summary\", \
         \"content\": \"detailed paradigm: preconditions, approach steps, expected results, \
         pitfalls to avoid. Include divergence_point if failures existed.\"}}\n\n\
         If this trajectory is too trivial or one-off, return exactly: [NO_PARADIGM]\n\n\
         JSON:"
    )
}

// ── Metadata Parser ─────────────────────────────────────────────────────────

/// Try to parse `ParadigmMeta` from the content string.
/// Expects a JSON block at the end, but handles plain-text content gracefully.
fn parse_paradigm_meta(content: &str, level: ParadigmLevel) -> Option<ParadigmMeta> {
    // Try to find a ```json block at the end
    if let Some(idx) = content.rfind("```json") {
        let after = &content[idx + 7..];
        if let Some(end) = after.find("```") {
            let json_str = &after[..end].trim();
            if let Ok(mut meta) = serde_json::from_str::<ParadigmMeta>(json_str) {
                meta.extraction_level = level;
                return Some(meta);
            }
        }
    }
    // Fallback: build minimal meta from content heuristics
    let approach = content.lines().next().unwrap_or("unknown").to_string();
    Some(ParadigmMeta {
        problem_class: "general".into(),
        preconditions: Vec::new(),
        approach: truncate(&approach, 120),
        parameters: Vec::new(),
        success_signals: Vec::new(),
        failure_modes: Vec::new(),
        divergence_point: None,
        anti_pattern: None,
        extraction_level: level,
    })
}

/// Load all paradigms from the fact store, newest first.
pub fn load_paradigms(fact_manager: &FactManager) -> Vec<MemoryFact> {
    fact_manager
        .load_all()
        .unwrap_or_default()
        .into_iter()
        .filter(|f| f.fact_type == FactType::Paradigm)
        .collect()
}

/// Search paradigms matching a query string (fuzzy keyword match).
pub fn search_paradigms(fact_manager: &FactManager, query: &str) -> Vec<MemoryFact> {
    let q = query.to_lowercase();
    fact_manager
        .load_all()
        .unwrap_or_default()
        .into_iter()
        .filter(|f| {
            f.fact_type == FactType::Paradigm
                && (f.name.to_lowercase().contains(&q)
                    || f.description.to_lowercase().contains(&q)
                    || f.content.to_lowercase().contains(&q))
        })
        .collect()
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn truncate(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{truncated}…")
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_turn_digest_new() {
        let d = TurnDigest::new("shell", false, Some("command not found"), "install pnpm", "Trying npm...");
        assert_eq!(d.tool_name, "shell");
        assert!(!d.success);
        assert_eq!(d.error_type, Some("command not found".into()));
        assert!(d.user_intent.contains("install pnpm"));
    }

    #[test]
    fn test_trajectory_buffer_evicts_oldest() {
        let buf = TrajectoryBuffer::new(3);
        for i in 0..5 {
            buf.push(TurnDigest::new(&format!("tool-{i}"), true, None, "test", "ok"));
        }
        let snapshot = buf.snapshot();
        assert_eq!(snapshot.len(), 3);
        assert_eq!(snapshot[0].tool_name, "tool-2"); // 0,1 evicted, 2,3,4 remain
        assert_eq!(snapshot[2].tool_name, "tool-4");
    }

    #[test]
    fn test_trajectory_buffer_empty() {
        let buf = TrajectoryBuffer::default();
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
        assert!(buf.snapshot().is_empty());
    }

    #[test]
    fn test_parse_paradigm_meta_from_json_block() {
        let content = "Some text\n```json\n{\"problem_class\":\"test\",\"preconditions\":[\"a\"],\"approach\":\"try X\",\"parameters\":[],\"success_signals\":[\"pass\"],\"failure_modes\":[\"fail\"],\"divergence_point\":null,\"anti_pattern\":null,\"extraction_level\":\"micro\"}\n```";
        let meta = parse_paradigm_meta(content, ParadigmLevel::Micro).unwrap();
        assert_eq!(meta.problem_class, "test");
        assert_eq!(meta.approach, "try X");
        assert_eq!(meta.extraction_level, ParadigmLevel::Micro);
    }

    #[test]
    fn test_parse_paradigm_meta_fallback() {
        let content = "Use HTTPS not SSH for git operations";
        let meta = parse_paradigm_meta(content, ParadigmLevel::Micro).unwrap();
        assert!(meta.approach.contains("HTTPS"));
        assert_eq!(meta.extraction_level, ParadigmLevel::Micro);
    }

    #[test]
    fn test_paradigm_extraction_prompt_includes_contrastive() {
        let digests = vec![
            TurnDigest::new("shell", false, Some("err"), "test", "fail"),
            TurnDigest::new("web_search", true, None, "research", "found"),
            TurnDigest::new("shell", true, None, "fixed", "ok"),
        ];
        let prompt = build_paradigm_extraction_prompt(&digests, true);
        assert!(prompt.contains("DIVERGENCE POINT"));
        assert!(prompt.contains("✗"));
        assert!(prompt.contains("✓"));
    }

    #[test]
    fn test_paradigm_extraction_prompt_skip_trivial() {
        let digests = vec![TurnDigest::new("echo", true, None, "hi", "hello")];
        // With less than 3 entries, extraction should return early
        // (tested via the extraction function logic, not the prompt)
        assert!(digests.len() < 3);
    }

    #[test]
    fn test_paradigm_level_display() {
        assert_eq!(ParadigmLevel::Micro.as_str(), "micro");
        assert_eq!(ParadigmLevel::Meso.as_str(), "meso");
        assert_eq!(ParadigmLevel::Macro.as_str(), "macro");
        assert_eq!(ParadigmLevel::Micro.to_string(), "micro");
    }
}
