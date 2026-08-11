//! Incremental, durable rolling summary (spec rule 1 + D1 budget-aware chunking).
//!
//! ## Guarantees
//!
//! - **No recursive re-summarization** (rule 1): the old summary is never fed to
//!   the LLM. It is kept verbatim as the prefix of the next summary; only the
//!   messages newer than the watermark are summarized.
//! - **Budget-aware chunking** (D1): when the compaction model's window is
//!   small, messages are packed into chunks of ≤ `ctx_window - 1536` tokens and
//!   summarized one chunk at a time.
//! - **Deterministic extractive fallback**: if the model is unavailable or the
//!   window is too small, keep head + tail + high-value lines, marked
//!   `[extractive]`. This never blocks the main loop.
//! - **Summary cap**: merged summary is capped at `SUMMARY_CAP_TOKENS`,
//!   dropping the oldest *new* sentences first while the old-facts prefix is
//!   preserved (old facts are the durable layer).

use everevo_core::llm::{LlmMessage, LlmProvider};
use everevo_db::models::MessageRow;

/// Cap on the merged rolling summary (tokens). Old summary prefix is preserved;
/// only the newly appended content is trimmed when the cap binds.
pub const SUMMARY_CAP_TOKENS: usize = 2048;

/// Assumed window when a provider has no `context_window` configured. 32K is a
/// safe floor for both the main model and a local 6GB vision/compact model.
const DEFAULT_CTX_WINDOW: u32 = 32_768;

/// Tokens reserved per chunk for output + instruction overhead.
const CHUNK_RESERVE_TOKENS: usize = 1536;

/// Minimum usable chunk budget (tokens) before we give up on the LLM path.
const MIN_CHUNK_BUDGET_TOKENS: usize = 512;

/// Instruction prepended to every summarization prompt.
const SUMMARIZE_INSTRUCTION: &str =
    "Summarize this conversation excerpt into concise bullet points capturing key \
     facts, decisions, numbers, names, URLs, and file paths. Preserve exact names, \
     numbers, and identifiers. Do not invent or infer anything not present.";

/// Result of one maintenance pass.
#[derive(Debug, Clone)]
pub struct RollingSummaryResult {
    /// Merged summary: old prefix (verbatim) + new chunk summaries (or extractive).
    pub summary: String,
    /// Newest message id covered. None when there was nothing new to summarize.
    pub watermark_message_id: Option<String>,
}

/// Incrementally summarize `new_messages` (already filtered to the post-watermark
/// tail) on top of `old_summary`. See module docs for the guarantees.
pub async fn maintain_rolling_summary(
    llm: &dyn LlmProvider,
    new_messages: &[MessageRow],
    old_summary: Option<&str>,
    ctx_window: Option<u32>,
) -> RollingSummaryResult {
    let old = old_summary.unwrap_or("");

    // Nothing new → return the existing summary unchanged, no watermark advance.
    let Some(last) = new_messages.last() else {
        return RollingSummaryResult {
            summary: old.to_string(),
            watermark_message_id: None,
        };
    };

    let window = ctx_window.unwrap_or(DEFAULT_CTX_WINDOW) as usize;
    let chunk_budget_tokens = window.saturating_sub(CHUNK_RESERVE_TOKENS);
    let chunk_budget_chars = chunk_budget_tokens * 4;

    // Too small to summarize via LLM → deterministic extractive fallback.
    let mut new_summary = if chunk_budget_tokens < MIN_CHUNK_BUDGET_TOKENS {
        extractive_fallback(new_messages, chunk_budget_chars.max(1024))
    } else {
        match summarize_via_llm(llm, new_messages, chunk_budget_chars).await {
            Some(chunk_summaries) => {
                if chunk_summaries.iter().all(|c| c.trim().is_empty()) {
                    extractive_fallback(new_messages, chunk_budget_chars.max(1024))
                } else {
                    chunk_summaries.join("\n")
                }
            }
            None => extractive_fallback(new_messages, chunk_budget_chars.max(1024)),
        }
    };

    // Merge: old prefix verbatim + new content, capped. Oldest new sentences are
    // dropped first; the old-facts prefix is preserved (test (d)).
    new_summary = merge_with_cap(old, &new_summary);

    RollingSummaryResult {
        summary: new_summary,
        watermark_message_id: Some(last.id.to_string()),
    }
}

/// Summarize `new_messages` chunk-by-chunk via the LLM. Returns `None` if any
/// chunk call fails (caller falls back to extractive). Each chunk is a single
/// user message; `images` are not carried (summary is text-only).
async fn summarize_via_llm(
    llm: &dyn LlmProvider,
    messages: &[MessageRow],
    budget_chars: usize,
) -> Option<Vec<String>> {
    let chunks = chunk_messages(messages, budget_chars);
    let mut summaries = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        let mut body = String::from(SUMMARIZE_INSTRUCTION);
        body.push_str("\n\nEXCERPT:\n");
        for m in &chunk {
            let role = match m.role.as_str() {
                "assistant" => "assistant",
                "tool" | "tool_result" => "tool",
                _ => "user",
            };
            body.push_str(&format!("[{role}] {}\n", m.content));
        }
        match llm.chat(&[LlmMessage::user(&body)], &[]).await {
            Ok(resp) => {
                let text = resp.content.unwrap_or_default().trim().to_string();
                if text.is_empty() {
                    return None;
                }
                summaries.push(text);
            }
            Err(e) => {
                tracing::warn!(error = %e, "rolling summary chunk summarization failed");
                return None;
            }
        }
    }
    Some(summaries)
}

/// Pack messages into chunks of ≤ `budget_chars` (roughly `budget_chars / 4`
/// tokens) along message boundaries. A single oversized message becomes its own
/// chunk (still sent — the caller's trim already caps extreme tool outputs).
fn chunk_messages<'a>(messages: &'a [MessageRow], budget_chars: usize) -> Vec<Vec<&'a MessageRow>> {
    let mut chunks: Vec<Vec<&'a MessageRow>> = Vec::new();
    let mut current: Vec<&'a MessageRow> = Vec::new();
    let mut current_chars = 0usize;
    for m in messages {
        let m_chars = m.content.len() + 16; // + role-label overhead
        if !current.is_empty() && current_chars + m_chars > budget_chars {
            chunks.push(std::mem::take(&mut current));
            current_chars = 0;
        }
        current.push(m);
        current_chars += m_chars;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Deterministic extractive fallback (model unavailable / window too small):
/// head + tail + high-value lines, marked `[extractive]`.
fn extractive_fallback(messages: &[MessageRow], budget_chars: usize) -> String {
    let mut out = String::from(
        "[extractive] — vision/compact model unavailable; \
                                 keeping head + tail + key lines.\n",
    );
    let all = flatten(messages);
    let budget = budget_chars.max(1024);

    let head = take_chars(&all, budget / 3);
    let tail = take_chars_rev(&all, budget / 3);
    out.push_str(&head);
    out.push_str("\n…\n");
    out.push_str(&tail);

    // High-value lines (numbers, URLs, decisions, file paths) appended if room.
    let mut seen = std::collections::HashSet::new();
    for m in messages {
        for line in m.content.lines() {
            let t = line.trim();
            if t.is_empty() || seen.contains(t) {
                continue;
            }
            seen.insert(t);
            if is_high_value(t) && out.len() < budget {
                out.push_str(&format!("\n- {t}"));
            }
        }
    }
    out
}

fn flatten(messages: &[MessageRow]) -> String {
    let mut s = String::new();
    for m in messages {
        s.push_str(&m.content);
        s.push('\n');
    }
    s
}

fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn take_chars_rev(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    let start = chars.len().saturating_sub(n);
    chars[start..].iter().collect()
}

/// A line is high-value if it contains digits, a URL, a path, or decision markers.
fn is_high_value(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    line.chars().any(|c| c.is_ascii_digit())
        || lower.contains("http")
        || lower.contains("decision")
        || lower.contains('\\')
        || lower.contains('/')
        || lower.contains(".py")
        || lower.contains(".rs")
        || lower.contains(".md")
        || lower.contains(".png")
}

/// Merge `old` (verbatim prefix) + `new`, capping total at SUMMARY_CAP_TOKENS.
/// When over cap, drop the oldest *new* sentences first — the old-facts prefix
/// is preserved (test (d)).
fn merge_with_cap(old: &str, new: &str) -> String {
    let cap_chars = SUMMARY_CAP_TOKENS * 4;
    if old.chars().count() >= cap_chars {
        return old.to_string(); // old alone fills the cap; drop all new content
    }
    let new_budget = cap_chars - old.chars().count();
    let mut merged = String::new();
    merged.push_str(old);
    if !new.trim().is_empty() {
        if !old.is_empty() {
            merged.push('\n');
        }
        let new_text = drop_oldest_sentences(new, new_budget);
        merged.push_str(&new_text);
    }
    merged
}

/// Drop leading sentences until the remainder fits `budget_chars`.
fn drop_oldest_sentences(s: &str, budget_chars: usize) -> String {
    if s.chars().count() <= budget_chars {
        return s.to_string();
    }
    // Split on sentence boundaries; keep the newest tail that fits.
    let sentences: Vec<&str> = s.split_inclusive('.').collect();
    let mut kept: Vec<&str> = Vec::new();
    let mut kept_chars = 0usize;
    for sent in sentences.iter().rev() {
        if kept_chars + sent.chars().count() > budget_chars {
            break;
        }
        kept.push(sent);
        kept_chars += sent.chars().count();
    }
    kept.reverse();
    kept.concat()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::llm::mock::MockLlmProvider;
    use everevo_core::llm::{FinishReason, LlmResponse};

    fn msg(role: &str, content: &str) -> MessageRow {
        use uuid::Uuid;
        MessageRow {
            id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            role: role.into(),
            content: content.into(),
            content_hash: String::new(),
            tool_calls: None,
            tool_call_id: None,
            thinking: String::new(),
            blocks_json: None,
            created_at: chrono::Utc::now(),
        }
    }

    /// 30K chars of synthetic messages (forces chunking at window 8K).
    fn big_tail() -> Vec<MessageRow> {
        let filler = "user discusses the Atlas migration and the 42 Mbps limit. ";
        let mut msgs = Vec::new();
        let mut total = 0usize;
        let mut i = 0u32;
        while total < 30_000 {
            let content = format!("msg {i}: {filler}{filler}{filler}");
            total += content.len();
            msgs.push(msg("user", &content));
            i += 1;
        }
        msgs
    }

    #[tokio::test]
    async fn old_summary_not_resent_to_llm() {
        let mock = Arc::new(MockLlmProvider::new().with_text("New facts only."));
        let old = "OLD SUMMARY — the Atlas migration is planned for 2026-09. Do not resend me.";
        let new = vec![msg("user", "We agreed on the release date: 2026-09-15.")];
        let res = maintain_rolling_summary(mock.as_ref(), &new, Some(old), Some(32_768)).await;

        assert!(res.summary.starts_with(old), "old prefix preserved");
        assert!(res.summary.contains("New facts only."));
        // The LLM prompt must NOT contain the old summary text (rule 1).
        let log = mock.call_log();
        assert_eq!(log.len(), 1);
        let prompt = &log[0][0].content;
        assert!(
            !prompt.contains("OLD SUMMARY"),
            "old summary must not be re-summarized: {prompt}"
        );
        assert!(res.watermark_message_id.is_some());
    }

    #[tokio::test]
    async fn chunks_when_window_small() {
        let mut mock = MockLlmProvider::new();
        for _ in 0..8 {
            mock = mock.with_response(LlmResponse {
                content: Some("chunk summary".into()),
                tool_calls: vec![],
                finish_reason: FinishReason::Stop,
            });
        }
        let mock = Arc::new(mock);
        let tail = big_tail(); // ~30K chars
                               // window 8K → chunk budget = (8000-1536)*4 ≈ 25,856 chars → ≥2 chunks.
        let res = maintain_rolling_summary(mock.as_ref(), &tail, None, Some(8_000)).await;
        let log = mock.call_log();
        assert!(
            log.len() >= 2,
            "expected multiple chunks, got {} calls",
            log.len()
        );
        // Each chunk's excerpt text must fit the chunk budget.
        let budget_chars = (8_000usize - CHUNK_RESERVE_TOKENS) * 4;
        for call in &log {
            let excerpt = call[0]
                .content
                .split_once("EXCERPT:")
                .map(|(_, e)| e)
                .unwrap_or("");
            assert!(
                excerpt.chars().count() <= budget_chars,
                "chunk {} > budget {}",
                excerpt.chars().count(),
                budget_chars
            );
        }
        assert!(!res.summary.is_empty());
    }

    #[tokio::test]
    async fn model_failure_extractive_fallback() {
        let mock = Arc::new(MockLlmProvider::new()); // empty → Err("no more responses")
        let new = vec![
            msg(
                "user",
                "Please compute 2 + 2 = 4 and check http://example.com/report.",
            ),
            msg("assistant", "The result is 4; see the report for details."),
        ];
        let res = maintain_rolling_summary(mock.as_ref(), &new, None, Some(32_768)).await;
        assert!(
            res.summary.contains("[extractive]"),
            "expected extractive marker in: {}",
            res.summary
        );
        assert!(res.summary.contains("http://example.com/report"));
        assert!(!res.summary.is_empty());
        assert!(res.watermark_message_id.is_some());
    }

    #[tokio::test]
    async fn too_small_window_extractive_without_llm_call() {
        let mock = Arc::new(MockLlmProvider::new().with_text("should not be used"));
        let new = vec![msg("user", &"x".repeat(200))];
        let res = maintain_rolling_summary(mock.as_ref(), &new, None, Some(1536)).await;
        assert!(res.summary.contains("[extractive]"));
        // Window below MIN → no LLM call at all.
        assert_eq!(mock.call_log().len(), 0);
    }

    #[tokio::test]
    async fn cap_preserves_old_prefix() {
        let mut mock = MockLlmProvider::new();
        for _ in 0..4 {
            mock = mock.with_response(LlmResponse {
                content: Some("Very long new content sentence one. ".repeat(2000)),
                tool_calls: vec![],
                finish_reason: FinishReason::Stop,
            });
        }
        let mock = Arc::new(mock);
        let old = "OLD-FACTS-PREFIX ".repeat(100); // 1700 chars, within cap
        let new = vec![msg("user", "alpha beta gamma delta epsilon.")];
        let res = maintain_rolling_summary(mock.as_ref(), &new, Some(&old), Some(32_768)).await;
        // Old prefix preserved verbatim; new content truncated to fit the cap
        // (oldest new sentences dropped).
        assert!(res.summary.starts_with(old.trim_end()));
        assert!(res.summary.chars().count() <= SUMMARY_CAP_TOKENS * 4 + 8);
        assert!(!res
            .summary
            .contains("Very long new content sentence one. ".repeat(2000).as_str()));
    }
}
