//! LLM call with context-overflow recovery — extracted from driver.rs during
//! the 2026-08-13 physical restructure.
//!
//! Claude Code error-recovery waterfall:
//!   1. First call → on overflow, halve the context budget (trim) → retry.
//!   2. Retry fails again → give up with a "too long" error.
//!   3. Non-overflow error → propagate to the caller unchanged.

use everevo_core::llm::{LlmMessage, LlmProvider, StreamEvent, ToolSchema};
use everevo_core::EverEvoError;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::trim;

/// Result of a failed LLM call, tagged with whether it was a context overflow
/// (so the caller can pick the correct FSM transition: T4 Overflow vs T3
/// StreamFailure).
pub(crate) struct LlmCallError {
    pub(crate) error: EverEvoError,
    pub(crate) overflow: bool,
}

/// Call the LLM once; on context overflow, halve the message budget and retry
/// once. Returns the token stream, or a tagged error.
pub(crate) async fn call_llm_with_overflow_recovery(
    llm: &dyn LlmProvider,
    messages: &mut Vec<LlmMessage>,
    tool_schemas: &[ToolSchema],
    cancel: Option<&CancellationToken>,
    max_context_chars: usize,
) -> Result<mpsc::Receiver<StreamEvent>, LlmCallError> {
    match llm
        .stream_chat(messages, tool_schemas, cancel.cloned())
        .await
    {
        Ok(rx) => Ok(rx),
        Err(e) => {
            let err_str = e.to_string();
            let is_overflow = err_str.contains("context_length_exceeded")
                || err_str.contains("prompt too long")
                || err_str.contains("413")
                || err_str.contains("too many tokens")
                || err_str.contains("maximum context length");

            if is_overflow {
                tracing::warn!(
                    error = %err_str,
                    msg_count = messages.len(),
                    "Context overflow detected — attempting emergency compaction"
                );
                // Waterfall step 1: aggressive trim (no API call needed)
                let before = messages.len();
                trim::trim_context(messages, max_context_chars / 2); // halve the budget
                let after = messages.len();
                tracing::info!(before, after, trimmed = before - after, "Emergency trim");

                // Retry
                match llm
                    .stream_chat(messages, tool_schemas, cancel.cloned())
                    .await
                {
                    Ok(rx) => Ok(rx),
                    Err(e2) => {
                        let e2_str = e2.to_string();
                        tracing::error!(
                            error = %e2_str,
                            "Context overflow persists after emergency trim"
                        );
                        Err(LlmCallError {
                            overflow: true,
                            error: EverEvoError::Agent(format!(
                                "Context is too long even after emergency compaction. \
                                 Try using /compact or starting a new session. Detail: {e2_str}"
                            )),
                        })
                    }
                }
            } else {
                Err(LlmCallError {
                    overflow: false,
                    error: e,
                })
            }
        }
    }
}
