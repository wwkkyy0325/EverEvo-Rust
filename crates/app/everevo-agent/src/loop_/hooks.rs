//! Shared tool execution with hook lifecycle — PreToolUse → execute → PostToolUse.

use everevo_core::tool::ToolOutput;
use everevo_core::EverEvoError;

/// Execute a tool with full hook lifecycle: pre → execute → post.
/// If a pre-hook blocks, the tool is NOT executed and the error is returned.
pub(crate) async fn execute_with_hooks(
    tool: &(dyn everevo_core::tool::Tool + 'static),
    tool_name: &str,
    params: &serde_json::Value,
    cancel: Option<&tokio_util::sync::CancellationToken>,
    hooks: &[std::sync::Arc<dyn everevo_core::tool::ToolHook>],
) -> Result<ToolOutput, EverEvoError> {
    // Pre-hooks — block on first failure
    if !hooks.is_empty() {
        for hook in hooks {
            hook.pre_execute(tool_name, params).await?;
        }
    }

    // Execute (clone params for ownership)
    let result = tool.execute(params.clone(), cancel).await;

    // Post-hooks — always run, even on error
    if !hooks.is_empty() {
        for hook in hooks {
            hook.post_execute(tool_name, params, &result).await;
        }
    }

    result
}
