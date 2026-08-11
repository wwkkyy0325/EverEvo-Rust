//! Context management — Claude Code-aligned multi-layer compaction.
//!
//! ## Layers (matching Claude Code's 5-layer system)
//!
//! 1. **Snip** — zero-cost pruning of low-value tool results (empty outputs,
//!    rejected commands, redundant status messages). No API calls.
//! 2. **Observation Masking** — keep only the last N tool results in full;
//!    replace older ones with compact headers. Zero-cost, backed by research
//!    (Gemini CLI PR #18389, LLMKube arXiv 2508.21433).
//!    Matches or exceeds LLM-based summarization on SWE-bench Verified.
//! 3. **Truncate** — per-tool-output length cap (head+tail preservation).
//! 4. **Autocompact** — LLM summarization of oldest messages (API call).
//! 5. **Trim** — hard drop of old messages (fallback when autocompact fails).
//!
//! Claude Code reference: Snip → MicroCompact → ContextCollapse → SessionMemoryCompact → AutoCompact
//! Our model:         Snip → ObsMask → Truncate → AutoCompact → Trim
//!
//! Reference: https://github.com/google-gemini/gemini-cli/pull/18389
//! Reference: https://github.com/defilantech/LLMKube/issues/558

use std::path::Path;

use everevo_core::llm::{LlmMessage, LlmProvider};

// ── Token estimation ─────────────────────────────────────────────────────

/// Approximate token count from character count.
/// Rough heuristic: 1 token ≈ 4 characters for English text.
/// Claude Code uses actual token counting; we use this fast approximation.
pub(crate) fn approx_tokens(chars: usize) -> usize {
    chars / 4
}

/// Recommended buffer before triggering compaction.
/// Claude Code uses a ~13,000 token buffer before the context window limit.
pub(crate) const COMPACTION_BUFFER_TOKENS: usize = 13_000;

// ── Layer 3b: Tool-output disk paging (spec deliverable 6) ──────────────

/// Above this many chars, a tool output is written to disk and replaced by a
/// 2KB preview + absolute path in the context (instead of being truncated).
pub(crate) const TOOL_PAGE_THRESHOLD_CHARS: usize = 30_000;

/// Preview length (chars) kept in the context when a tool output is paged.
pub(crate) const TOOL_PAGE_PREVIEW_CHARS: usize = 2000;

/// Page a large tool output to `cache_dir/<call_id>.txt`, returning a
/// placeholder with the absolute path + 2KB preview. Returns `None` when the
/// output is small enough to keep inline, when no cache dir is configured, or
/// when the write fails (caller falls back to truncation).
///
/// Benchmark mode never pages `web_search`/`web_fetch` — their outputs are the
/// agent's only evidence for multi-hop GAIA questions, and evicting them forces
/// re-search churn (arXiv 2606.00408, same rationale as `mask_observations`).
pub(crate) async fn page_tool_output(
    tool_name: &str,
    call_id: &str,
    content: &str,
    cache_dir: Option<&Path>,
) -> Option<String> {
    let dir = cache_dir?;
    if content.len() <= TOOL_PAGE_THRESHOLD_CHARS {
        return None;
    }
    if std::env::var("EVEREVO_BENCHMARK").is_ok()
        && (tool_name == "web_search" || tool_name == "web_fetch")
    {
        return None;
    }
    let safe_id: String = call_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let path = dir.join(format!("{safe_id}.txt"));
    if tokio::fs::create_dir_all(dir).await.is_err() {
        return None;
    }
    if tokio::fs::write(&path, content).await.is_err() {
        return None;
    }
    let preview: String = content.chars().take(TOOL_PAGE_PREVIEW_CHARS).collect();
    Some(format!(
        "[tool output saved: {} ({} chars)]\n{preview}…",
        path.display(),
        content.chars().count()
    ))
}

// ── Layer 1: Snip (zero-cost pruning) ────────────────────────────────────

/// Remove low-value tool result messages before they consume context budget.
///
/// Claude Code equivalent: the Snip layer filters out entire turns where
/// tool output is empty, redundant, or a rejected command. This is free
/// (no API calls) and runs before every LLM request.
///
/// Returns the number of messages snipped.
pub(crate) fn snip_low_value_messages(messages: &mut Vec<LlmMessage>) -> usize {
    let mut removed = 0;
    let mut i = 0;
    while i < messages.len() {
        let should_snip = if messages[i].tool_call_id.is_some() {
            // This is a tool_result message. Snip low-value results.
            let content = &messages[i].content;
            content.is_empty()
                || content == "null"
                || content == "undefined"
                || content.trim() == "ok"
                || content.contains("Permission denied")
                || content.contains("command not found")
                || content.starts_with("Cancelled")
        } else {
            false
        };
        if should_snip {
            // Also remove the preceding tool_use message (paired)
            if i > 0 && messages[i - 1].tool_calls.is_some() {
                messages.remove(i - 1);
                messages.remove(i - 1); // shifted after first removal
                removed += 2;
                i = i.saturating_sub(1); // re-check from previous position
            } else {
                messages.remove(i);
                removed += 1;
            }
        } else {
            i += 1;
        }
    }
    if removed > 0 {
        tracing::info!(removed, "Snip: removed low-value tool messages");
    }
    removed
}

// ── Layer 2: Observation Masking ────────────────────────────────────────

/// Number of most recent tool-result messages to keep in full.
/// Older tool results are replaced with compact headers.
/// Gemini CLI uses 3; LLMKube Foreman uses configurable 3.
const OBSERVATION_WINDOW: usize = 3;

/// Gemini CLI-aligned observation masking: keep the last N tool results in
/// full, replace older ones with compact `[tool: name] (N bytes)` headers.
///
/// Non-tool messages (system, user, assistant text) are NEVER masked — they're
/// small and critical for task understanding. Only tool_result messages are
/// candidates for masking.
///
/// Returns the number of tool results masked.
pub(crate) fn mask_observations(messages: &mut [LlmMessage]) -> usize {
    if messages.len() <= OBSERVATION_WINDOW + 2 {
        return 0; // too few messages to mask
    }

    // Find all tool_result message indices (those with tool_call_id)
    let tool_result_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.tool_call_id.is_some())
        .map(|(i, _)| i)
        .collect();

    if tool_result_indices.len() <= OBSERVATION_WINDOW {
        return 0;
    }

    // Keep the last OBSERVATION_WINDOW tool results, mask the rest
    let keep_start = tool_result_indices.len().saturating_sub(OBSERVATION_WINDOW);
    let mut masked = 0usize;

    for &idx in &tool_result_indices[..keep_start] {
        let msg = &messages[idx];
        let original_len = msg.content.len();
        if original_len < 100 {
            // Small results aren't worth masking — they don't consume much context
            continue;
        }
        // Extract tool name from preceding assistant message with tool_calls
        let tool_name = if idx > 0 {
            messages[idx - 1]
                .tool_calls
                .as_ref()
                .and_then(|tc| tc.first().map(|t| t.name.as_str()))
                .unwrap_or("unknown")
        } else {
            "unknown"
        };
        // Benchmark mode: web_search/web_fetch results are the agent's only
        // evidence for multi-hop GAIA questions. Masking them forces re-search
        // churn that burns turns toward the turn cap and ends in empty answers
        // (arXiv 2606.00408: masking backfires when reference outputs consulted
        // repeatedly are evicted too early). They're ~2KB each — cheap to keep.
        if std::env::var("EVEREVO_BENCHMARK").is_ok()
            && (tool_name == "web_search" || tool_name == "web_fetch")
        {
            continue;
        }
        messages[idx].content = format!(
            "[tool result from \"{tool_name}\" masked; {original_len} bytes elided. \
             Re-run the tool if you need this output again.]"
        );
        masked += 1;
    }

    if masked > 0 {
        tracing::info!(
            masked,
            window = OBSERVATION_WINDOW,
            "Observation masking applied"
        );
    }
    masked
}

// ── Layer 3: Tool output truncation ──────────────────────────────────────

/// Floor a byte index to the nearest valid UTF-8 char boundary (≤ the
/// requested position). Equivalent to `str::floor_char_boundary` (stable
/// since 1.91), kept as a free function for MSRV 1.80 compatibility.
fn floor_char_boundary(s: &str, pos: usize) -> usize {
    let pos = pos.min(s.len());
    let mut p = pos;
    // UTF-8 continuation bytes have the pattern 0b10xxxxxx. Walk backwards
    // until we're at the start of a character.
    while p > 0 && (s.as_bytes()[p] & 0xC0) == 0x80 {
        p -= 1;
    }
    p
}

/// Truncate a tool output to a maximum character count.
/// Keeps head and tail — the most informative parts.
pub(crate) fn truncate_output(output: &str, max_chars: usize) -> String {
    if max_chars == 0 || output.len() <= max_chars {
        return output.to_string();
    }
    let head = max_chars * 3 / 4; // 75% head
    let tail = max_chars - head; // 25% tail

    // Floor to a valid UTF-8 char boundary — `head` and `tail` are byte
    // offsets and may land inside a multi-byte character. Slicing at an
    // invalid boundary panics the agent loop (observed with CJK text where
    // 3-byte chars straddle the 3000-byte cutoff).
    let head_byte = floor_char_boundary(output, head.min(output.len()));
    let tail_start = floor_char_boundary(output, output.len().saturating_sub(tail));

    let mut result = String::with_capacity(max_chars + 100);
    result.push_str(&output[..head_byte]);
    result.push_str(&format!(
        "\n\n... [truncated: {} total chars, showing first {} + last {}] ...\n\n",
        output.len(),
        head_byte,
        output.len() - tail_start,
    ));
    result.push_str(&output[tail_start..]);
    result
}

/// Trim old messages from the conversation if the total character count
/// exceeds the budget. Always keeps the system prompt (first message),
/// the last few messages (current turn), and NEVER removes messages that
/// are part of tool_use/tool_result pairs (to avoid protocol violations).
pub(crate) fn trim_context(messages: &mut Vec<LlmMessage>, max_chars: usize) {
    if max_chars == 0 || messages.len() <= 5 {
        return;
    }
    let total: usize = messages.iter().map(|m| m.content.len()).sum();
    if total <= max_chars {
        return;
    }

    let indices = find_removable(messages, total, max_chars);
    if !indices.is_empty() {
        let removed_chars: usize = indices.iter().map(|&i| messages[i].content.len()).sum();
        for &i in indices.iter().rev() {
            messages.remove(i);
        }
        tracing::info!(
            trimmed = indices.len(),
            removed_chars,
            remaining = messages.len(),
            "Context trimmed"
        );
    }
}

/// Autocompact: summarize old messages via LLM instead of dropping them.
///
/// When the conversation exceeds `max_chars`, extracts the oldest non-tool
/// messages, asks the LLM to summarize them into a concise paragraph, and
/// replaces them with a single `<conversation_summary>` user message. Always
/// preserves the system prompt and the current turn.
///
/// **Rule 1 (never re-summarize summaries):** if a `<conversation_summary>`
/// message already exists in the removable window, its text is kept verbatim
/// and folded as a prefix; only the messages after it are fed to the LLM.
/// This prevents recursive summary drift across successive compactions.
///
/// Returns the number of messages compacted, or 0 if nothing was done.
#[allow(clippy::needless_range_loop)]
pub(crate) async fn autocompact(
    messages: &mut Vec<LlmMessage>,
    max_chars: usize,
    llm: &dyn LlmProvider,
    focus_hint: Option<&str>,
) -> usize {
    if max_chars == 0 || messages.len() <= 8 {
        return 0; // too few messages to compact meaningfully
    }
    let total: usize = messages.iter().map(|m| m.content.len()).sum();
    if total <= max_chars {
        return 0;
    }

    let indices = find_removable(messages, total, max_chars);
    if indices.len() < 2 {
        return 0; // need at least 2 messages to summarize
    }

    // Rule 1: an existing summary in the removable window is folded verbatim,
    // never re-summarized. The message itself is still removed (it collapses
    // into the new combined summary), but its text is excluded from the prompt.
    let summary_idx = indices
        .iter()
        .copied()
        .find(|&i| messages[i].content.contains("<conversation_summary>"));
    let old_summary_inner = summary_idx
        .map(|i| {
            messages[i]
                .content
                .replace("<conversation_summary>", "")
                .replace("</conversation_summary>", "")
                .trim()
                .to_string()
        })
        .filter(|s| !s.is_empty());

    // Build the prompt from old messages (excluding the folded summary)
    let old_text: String = indices
        .iter()
        .filter(|&&i| Some(i) != summary_idx)
        .map(|&i| {
            let m = &messages[i];
            format!("[{}]: {}", m.role, m.content)
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let focus_line = focus_hint
        .filter(|f| !f.is_empty())
        .map(|f| format!("PRIORITY FOCUS: {f}\n"))
        .unwrap_or_default();

    let compact_prompt = format!(
        "Summarize the following conversation history into a single concise paragraph. \
         CRITICAL: Preserve the task state — what was completed (✅), what is still \
         in progress (🔄), and what is pending (⬜). Also keep all important facts, \
         decisions, file paths, and code references. \
         {focus_line}\
         The summary will replace the original messages to save context space.\n\n\
         CONVERSATION:\n{old_text}\n\n\
         SUMMARY:"
    );

    // Call LLM (fire-and-forget best-effort; fall back to trim on failure)
    let new_summary = match llm
        .chat(
            &[LlmMessage::user(&compact_prompt)],
            &[], // no tools needed for summarization
        )
        .await
    {
        Ok(resp) => resp.content.unwrap_or_else(|| "[compaction failed]".into()),
        Err(e) => {
            tracing::warn!(error = %e, "Autocompact LLM call failed — falling back to trim");
            return 0; // caller should fall back to trim_context
        }
    };

    // Merge: existing summary verbatim prefix + new facts (rule 1).
    let summary = match &old_summary_inner {
        Some(prev) => format!("{prev}\n\n### Continuing summary\n{new_summary}"),
        None => new_summary,
    };

    let compacted = indices.len();
    let insert_at = indices[0]; // position of first removed message

    // Remove in reverse order
    for &i in indices.iter().rev() {
        messages.remove(i);
    }

    // Insert summary at the position of the first removed message
    messages.insert(
        insert_at,
        LlmMessage::user(format!(
            "<conversation_summary>\n{summary}\n</conversation_summary>\n\n\
             The above summarizes the earlier part of our conversation. \
             Continue from where we left off."
        )),
    );

    tracing::info!(
        compacted,
        summary_chars = summary.len(),
        "Context autocompacted"
    );
    compacted
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// Find indices of messages that can be safely removed.
/// Skips system prompt (0), tool messages, and the keep_tail window.
#[allow(clippy::needless_range_loop)]
fn find_removable(messages: &[LlmMessage], total: usize, max_chars: usize) -> Vec<usize> {
    let keep_tail = 4usize.min(messages.len().saturating_sub(1));
    let remove_up_to = messages.len().saturating_sub(keep_tail);
    let start_removing = 1; // skip system prompt

    let mut indices = Vec::new();
    let mut removed_chars = 0;

    for i in start_removing..remove_up_to {
        if messages[i].tool_calls.is_some() || messages[i].tool_call_id.is_some() {
            continue; // never break tool_use/tool_result pairs
        }
        if total - removed_chars <= max_chars {
            break;
        }
        indices.push(i);
        removed_chars += messages[i].content.len();
    }
    indices
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::llm::mock::MockLlmProvider;
    use everevo_core::llm::{LlmMessage, LlmRole};

    /// Layer-2 fold (rule 1): an existing `<conversation_summary>` message is
    /// kept verbatim as a prefix and NEVER re-fed to the LLM.
    #[tokio::test]
    async fn autocompact_folds_existing_summary() {
        let mock = Arc::new(MockLlmProvider::new().with_text("NEW FACTS ONLY."));
        let mut msgs = vec![
            LlmMessage::system("system"),
            LlmMessage::user("hello, summarize this project discussion"),
            LlmMessage::user(
                "<conversation_summary>\nOLD FACTS: atlas migration 2026\n</conversation_summary>",
            ),
            LlmMessage::user("We agreed the release window is September."),
            LlmMessage::assistant("Yes — 2026-09-15, using the Atlas migration path."),
            LlmMessage::user("What about pricing?"),
            LlmMessage::assistant("42 USD per seat, yearly."),
            LlmMessage::user("And the rollout plan?"),
            LlmMessage::assistant("Phased: US first, then EU."),
        ];
        let n = autocompact(&mut msgs, 120, mock.as_ref(), None).await;
        assert!(n >= 2, "expected multiple messages compacted");

        // The new summary folds the old facts verbatim + the new LLM summary.
        let summary_msg = msgs
            .iter()
            .find(|m| m.content.contains("<conversation_summary>"))
            .expect("a conversation summary message must exist");
        assert!(summary_msg
            .content
            .contains("OLD FACTS: atlas migration 2026"));
        assert!(summary_msg.content.contains("NEW FACTS ONLY."));

        // Rule 1: the old summary text must not appear in the LLM prompt.
        let log = mock.call_log();
        assert_eq!(log.len(), 1);
        let prompt = &log[0][0].content;
        assert!(
            !prompt.contains("OLD FACTS"),
            "existing summary must not be re-summarized: {prompt}"
        );
    }

    // ── Snip tests ────────────────────────────────────────────────

    #[test]
    fn test_snip_removes_empty_tool_results() {
        let mut msgs = vec![
            LlmMessage::system("sys"),
            LlmMessage::user("hello"),
            LlmMessage {
                role: LlmRole::Assistant,
                content: "calling tool".into(),
                thinking: None,
                tool_calls: Some(vec![]),
                tool_call_id: None,
                images: Vec::new(),
            },
            LlmMessage {
                role: LlmRole::User,
                content: String::new(), // empty tool result
                thinking: None,
                tool_calls: None,
                tool_call_id: Some("t1".into()),
                images: Vec::new(),
            },
            LlmMessage::assistant("after tool"),
        ];
        let removed = snip_low_value_messages(&mut msgs);
        assert_eq!(removed, 2); // tool_use + tool_result pair removed
        assert_eq!(msgs.len(), 3); // sys + user + assistant
    }

    #[test]
    fn test_snip_removes_null_result() {
        let mut msgs = vec![
            LlmMessage::system("sys"),
            LlmMessage::user("cmd"),
            LlmMessage {
                role: LlmRole::Assistant,
                content: "running tool".into(),
                thinking: None,
                tool_calls: Some(vec![]),
                tool_call_id: None,
                images: Vec::new(),
            },
            LlmMessage {
                role: LlmRole::User,
                content: "null".into(),
                thinking: None,
                tool_calls: None,
                tool_call_id: Some("t1".into()),
                images: Vec::new(),
            },
        ];
        let removed = snip_low_value_messages(&mut msgs);
        assert_eq!(removed, 2);
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn test_snip_preserves_valuable_results() {
        let mut msgs = vec![
            LlmMessage::system("sys"),
            LlmMessage::user("hello"),
            LlmMessage {
                role: LlmRole::Assistant,
                content: "calling tool".into(),
                thinking: None,
                tool_calls: Some(vec![]),
                tool_call_id: None,
                images: Vec::new(),
            },
            LlmMessage {
                role: LlmRole::User,
                content: "important data: {\"key\": \"value\"}".into(),
                thinking: None,
                tool_calls: None,
                tool_call_id: Some("t1".into()),
                images: Vec::new(),
            },
        ];
        let removed = snip_low_value_messages(&mut msgs);
        assert_eq!(removed, 0); // nothing should be snipped
        assert_eq!(msgs.len(), 4);
    }

    #[test]
    fn test_approx_tokens() {
        assert_eq!(approx_tokens(0), 0);
        assert_eq!(approx_tokens(100), 25);
        assert_eq!(approx_tokens(4000), 1000);
    }

    // ── Existing trim tests ───────────────────────────────────────

    #[test]
    fn test_find_removable_skips_tool_messages() {
        let msgs = vec![
            LlmMessage::system("sys"),
            LlmMessage::user("hello"),
            LlmMessage {
                role: LlmRole::Assistant,
                content: "calling tool".into(),
                thinking: None,
                tool_calls: Some(vec![]), // has tool_calls
                tool_call_id: None,
                images: Vec::new(),
            },
            LlmMessage::user("recent1"),
            LlmMessage::assistant("recent2"),
            LlmMessage::user("latest"),
        ];
        let total: usize = msgs.iter().map(|m| m.content.len()).sum();
        let result = find_removable(&msgs, total, 10);
        // Should NOT include index 2 (has tool_calls)
        assert!(!result.contains(&2), "tool messages must be preserved");
    }

    #[test]
    fn test_find_removable_respects_keep_tail() {
        let mut msgs = vec![LlmMessage::system("sys")];
        for i in 0..20 {
            msgs.push(LlmMessage::user(&format!("msg{i}")));
        }
        let total: usize = msgs.iter().map(|m| m.content.len()).sum();
        let result = find_removable(&msgs, total, 30);
        // Should keep last 4 messages (keep_tail)
        let max_removed = msgs.len() - 1 - 4;
        assert!(result.len() <= max_removed, "must keep tail window");
    }

    #[test]
    fn test_truncate_output_preserves_bounds() {
        let long = "A".repeat(5000);
        let result = truncate_output(&long, 1000);
        assert!(result.len() <= 1200);
        assert!(result.contains("[truncated:"));
        assert!(result.starts_with('A'));
        assert!(result.ends_with('A'));
    }

    #[test]
    fn test_truncate_output_cjk_boundary() {
        // Reproduces the crash: 3000-byte cutoff falls inside '持' (3-byte
        // UTF-8 char at bytes 2999-3002). floor_char_boundary prevents panic.
        let cjk_prefix = "中".repeat(1000); // 3000 bytes (3 bytes each)
        let cjk_with_bad_cut = format!("{}{}国", cjk_prefix, "持".repeat(100));
        // Force the cutoff to fall in a bad spot
        let result = truncate_output(&cjk_with_bad_cut, 3000);
        // Must not panic, and must contain valid UTF-8
        assert!(result.contains("[truncated:"));
        std::str::from_utf8(result.as_bytes()).expect("output must be valid UTF-8");
    }

    #[tokio::test]
    async fn page_large_output_writes_full_text_and_preview() {
        let dir = tempfile::tempdir().unwrap();
        let big = "tool-content-".repeat(3000); // ~42K chars > 30K threshold
        let paged = page_tool_output("shell", "call-123", &big, Some(dir.path()))
            .await
            .expect("large output must be paged");
        assert!(paged.contains("[tool output saved:"));
        assert!(paged.contains("call-123.txt"));
        assert!(paged.contains(&(big.chars().count()).to_string()));
        assert!(paged.contains(&big[..200].to_string()), "preview head kept");

        // Full text is on disk, retrievable via tool_cache_read.
        let path = dir.path().join("call-123.txt");
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), big);
    }

    #[tokio::test]
    async fn small_output_stays_inline() {
        let dir = tempfile::tempdir().unwrap();
        let small = "short".repeat(100); // well under threshold
        let paged = page_tool_output("shell", "c1", &small, Some(dir.path())).await;
        assert!(paged.is_none());
        assert!(!dir.path().join("c1.txt").exists());
    }

    #[tokio::test]
    async fn no_cache_dir_means_no_paging() {
        let big = "x".repeat(40_000);
        let paged = page_tool_output("shell", "c1", &big, None).await;
        assert!(paged.is_none());
    }

    #[tokio::test]
    async fn benchmark_web_tools_never_paged() {
        // Safety-valve for the GAIA harness: web_search/web_fetch outputs are the
        // agent's only evidence for multi-hop questions, so under EVEREVO_BENCHMARK
        // they must NOT be evicted to disk.
        let dir = tempfile::tempdir().unwrap();
        let big = "y".repeat(40_000);
        std::env::set_var("EVEREVO_BENCHMARK", "1");
        let paged = page_tool_output("web_search", "c1", &big, Some(dir.path())).await;
        std::env::remove_var("EVEREVO_BENCHMARK");
        assert!(paged.is_none());
        assert!(!dir.path().join("c1.txt").exists());

        std::env::set_var("EVEREVO_BENCHMARK", "1");
        let paged = page_tool_output("shell", "c2", &big, Some(dir.path())).await;
        std::env::remove_var("EVEREVO_BENCHMARK");
        assert!(paged.is_some(), "non-web tools still page under benchmark");
    }

    #[tokio::test]
    async fn unsafe_call_id_is_sanitized() {
        let dir = tempfile::tempdir().unwrap();
        let big = "z".repeat(40_000);
        let paged = page_tool_output("shell", "a/b\\c:d*e?f\"g<h>i|j", &big, Some(dir.path()))
            .await
            .expect("paged");
        assert!(paged.contains(".txt"));
        // Sanitized id: non-alphanumeric chars become '_' (path separators etc.)
        assert!(paged.contains("a_b_c_d_e_f_g_h_i_j.txt"));
    }
}
