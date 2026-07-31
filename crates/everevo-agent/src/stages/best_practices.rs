//! Best Practices Stage — injects verification, planning, and architecture
//! guidelines as a pluggable context stage. Decoupled from system prompt so
//! it can be toggled, extended, or replaced independently.

use everevo_core::context::{ContextBuildContext, ContextFragment, ContextStage};
use everevo_core::llm::LlmMessage;

/// Injects Claude Code-style best practices for agent behavior.
///
/// Priority: 2 (after system prompt at 0, after persona at 1,
/// before skills and domain knowledge).
pub struct BestPracticesStage;

impl ContextStage for BestPracticesStage {
    fn priority(&self) -> i32 {
        2
    }
    fn name(&self) -> &str {
        "best_practices"
    }

    fn build(&self, ctx: &ContextBuildContext) -> Option<ContextFragment> {
        let tool_count = ctx.tool_count;
        let shell = ctx.shell_name.as_deref().unwrap_or("unknown");
        let perm = ctx.permission_level.as_deref().unwrap_or("semi_auto");

        let content = format!(
            "\
## Best Practices

### Verification
Run project verification after changes. Report `file:line` for failures. \
Fix code — never weaken tests. Don't claim success without fresh evidence. \
Spawn sub-agent to independently verify when uncertain.

### Planning & Code
Explore codebase before large changes. Use TodoWrite (one in_progress at a time). \
Match existing style — don't \"improve\" adjacent code. Remove only imports/vars \
YOUR changes made unused. Write test → reproduce bug → fix. Prefer simple solutions.

### Shell & Permissions ({shell}, {perm})
Shell is for build/test/git/packages, NOT for read/write/list/search/fetch. \
Use relative paths (./file.txt). {tool_count} tools available. \
Explain when elevated permissions are needed."
        );

        Some(ContextFragment {
            label: "Best Practices".into(),
            messages: vec![LlmMessage::user(&content)],
        })
    }
}
