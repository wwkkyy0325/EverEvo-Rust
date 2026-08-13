//! End-of-run execution summary: turns, tool calls, and failures classified
//! as transient (environment) vs structural (implementation defect).

/// Cap a failure message for the retrospective (keep it compact).
pub(crate) fn truncate_for_retro(msg: &str) -> String {
    const MAX: usize = 160;
    if msg.len() <= MAX {
        msg.to_string()
    } else {
        // Byte-slicing `&msg[..MAX]` panics ("byte index is not a char
        // boundary") when a multi-byte UTF-8 char (Chinese, emoji) straddles
        // the 160-byte boundary — a real crash that killed the full GAIA run
        // mid-flight (server panic → every later question 502). Take chars
        // instead, which is always boundary-safe.
        let truncated: String = msg.chars().take(MAX).collect();
        format!("{truncated}…")
    }
}

/// Classify a failure message as transient (environmental, retryable) or
/// structural (needs a code fix). Mirrors `HttpClient::is_retryable` semantics
/// for tool/LLM failures surfaced in the loop.
pub(crate) fn classify_failure(msg: &str) -> &'static str {
    let lower = msg.to_ascii_lowercase();
    const TRANSIENT: &[&str] = &[
        "timed out",
        "timeout",
        "stalled",
        "network",
        "connection reset",
        "connection refused",
        "rate limit",
        "temporarily unavailable",
        "429",
        "502",
        "503",
        "504",
        "retry",
    ];
    if TRANSIENT.iter().any(|k| lower.contains(k)) {
        "transient"
    } else {
        "structural"
    }
}

/// Build the end-of-run retrospective markdown block.
pub(crate) fn build_retrospective(
    turns: i32,
    total_tool_calls: i32,
    total_tool_success: i32,
    failures: &[String],
) -> String {
    let failed = total_tool_calls - total_tool_success;
    let transient = failures
        .iter()
        .filter(|f| classify_failure(f) == "transient")
        .count();
    let structural = failures.len() - transient;

    let mut out = format!(
        "## 执行复盘\n\n- 轮次：{turns}\n- 工具调用：{total_tool_calls} 次（成功 {total_tool_success}，失败 {failed}）"
    );
    if failures.is_empty() {
        out.push_str("\n- 故障：无");
    } else {
        out.push_str(&format!(
            "\n- 故障：{} 处（临时性 {}，结构性 {}）",
            failures.len(),
            transient,
            structural
        ));
        for f in failures.iter().take(3) {
            out.push_str(&format!("\n  - {f}"));
        }
        if failures.len() > 3 {
            out.push_str(&format!("\n  - … 另有 {} 处", failures.len() - 3));
        }
    }
    if structural > 0 {
        out.push_str("\n- 优化点：结构性故障需修复底层逻辑；临时性故障可在后续轮次重试。");
    } else {
        out.push_str("\n- 优化点：本轮无结构性故障。");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Regression: `&msg[..160]` byte-slicing panicked ("byte index is not a
    // char boundary") on multi-byte UTF-8, crashing the server and killing the
    // whole GAIA run mid-flight. Char-safe truncation must not panic.
    #[test]
    fn truncate_for_retro_handles_multibyte_utf8() {
        let msg = "错误消息".repeat(60); // 180+ chars, 6 bytes each
        let out = truncate_for_retro(&msg);
        assert!(out.ends_with('…'), "should be truncated with ellipsis");
        assert!(out.chars().count() <= 161, "at most 160 chars + ellipsis");
    }

    #[test]
    fn truncate_for_retro_short_msg_unchanged() {
        let out = truncate_for_retro("short");
        assert_eq!(out, "short");
    }
}

/// Emit the per-turn `TurnComplete` event + telemetry record. Extracted from
/// driver.rs during the 2026-08-13 physical restructure.
pub(crate) async fn emit_turn_complete(
    tx: &tokio::sync::mpsc::Sender<super::event::AgentEvent>,
    telemetry: Option<&std::sync::Arc<everevo_core::TelemetryPipeline>>,
    trace_id: Option<uuid::Uuid>,
    turn: usize,
    tool_calls_len: usize,
    tool_calls_success: i32,
    turn_start: std::time::Instant,
) {
    use everevo_core::TelemetryEmitContext;

    let _ = tx.send(super::event::AgentEvent::TurnComplete).await;

    if let Some(telemetry) = telemetry {
        let turn_error = (tool_calls_success as usize) < tool_calls_len;
        let (error_type, error_message) = if turn_error {
            let failed = tool_calls_len as i32 - tool_calls_success;
            (
                Some("tool_error".to_string()),
                Some(format!("{failed} of {tool_calls_len} tool calls failed",)),
            )
        } else {
            (None, None)
        };
        telemetry.emit(&TelemetryEmitContext {
            trace_id,
            turn_number: Some(turn as i32),
            tool_calls_total: Some(tool_calls_len as i32),
            tool_calls_success: Some(tool_calls_success),
            task_completed: Some(false),
            turn_latency_ms: Some(turn_start.elapsed().as_millis() as i64),
            error_type,
            error_message,
            ..Default::default()
        });
    }
}
