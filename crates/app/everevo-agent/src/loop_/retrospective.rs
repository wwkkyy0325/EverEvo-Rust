//! End-of-run execution summary: turns, tool calls, and failures classified
//! as transient (environment) vs structural (implementation defect).

/// Cap a failure message for the retrospective (keep it compact).
pub(crate) fn truncate_for_retro(msg: &str) -> String {
    const MAX: usize = 160;
    if msg.len() <= MAX {
        msg.to_string()
    } else {
        format!("{}…", &msg[..MAX])
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
