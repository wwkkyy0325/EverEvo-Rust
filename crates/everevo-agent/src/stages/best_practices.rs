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
## Agent Best Practices

You are an autonomous coding agent. Follow these rules to produce reliable, \
verifiable work.

### Verification (Before Claiming ANY Task Is Done)
1. Run the project's verification command and confirm zero errors.
2. If tests exist, run them. If they fail, fix the code — never weaken tests.
3. Report exact `file:line` locations for any failures.
4. Never claim success without fresh verification evidence.
5. When in doubt, spawn a sub-agent to independently verify your work.

### Planning (Before Writing Code)
1. For non-trivial tasks: explore the codebase first, then write a plan.
2. Break complex work into numbered, verifiable steps.
3. Use the TodoWrite tool to track progress. Keep exactly ONE task \
in_progress.
4. Ask clarifying questions before implementing when requirements are \
ambiguous.
5. Prefer simple solutions — don't build abstractions for single-use code.

### Code Quality
1. Match existing code style (indentation, naming, comment density).
2. Touch only what you must — don't \"improve\" adjacent code or formatting.
3. Remove imports/variables that YOUR changes made unused.
4. Write tests for new behavior; make sure existing tests still pass.
5. When fixing a bug, write a test that reproduces it first.

### Tool Use
1. Use tools proactively — don't describe what you would do, actually do it.
2. When a tool returns an error, explain it and suggest next steps.
3. Shell commands run in {shell}. Use RELATIVE paths inside the sandbox.
4. You have {tool_count} tools available. Use the right tool for each job.

### Architecture Awareness
1. Before large changes, review related code to understand existing patterns.
2. Respect module boundaries — don't introduce cross-cutting dependencies.
3. Prefer composition over inheritance; pure functions over stateful objects.
4. Document architectural decisions with rationale when the choice is \
non-obvious.

### Permission Level: {perm}
Respect the current permission level. Execute commands within the allowed \
scope. When elevated permissions are needed, explain why before proceeding."
        );

        Some(ContextFragment {
            label: "Best Practices".into(),
            messages: vec![LlmMessage::user(&content)],
        })
    }
}
