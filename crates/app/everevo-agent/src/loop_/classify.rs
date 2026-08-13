//! Typed tool-call classification — replaces fragile string-name coupling
//! between the loop and the tools (architecture-restructure-plan.md P1.2).
//! Extracted from driver.rs during the 2026-08-13 physical restructure.

/// Typed classification of a tool call's KIND — replaces fragile string-name
/// coupling between the loop and the tools (architecture-restructure-plan.md
/// P1.2). Each variant maps to the loop behavior that cares about that tool,
/// so the loop works with a typed enum instead of ad-hoc name/args matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolKind {
    /// A verification step: the deterministic sandbox verifier
    /// (verify_candidate.py via shell) or the adversarial `cluster verify`
    /// sub-agent review. Satisfies the hard-question commit gate.
    Verifier,
    /// Finalizing a structural problem model (`problem_model` + finalize).
    ProblemModelFinalize,
    /// Any other tool.
    Other,
}

/// Classify a tool call by KIND. The string matching is centralized HERE (one
/// place, documented); the loop consumes the typed [`ToolKind`].
pub(crate) fn classify_tool(name: &str, args: &serde_json::Value) -> ToolKind {
    match name {
        "cluster"
            if args
                .get("action")
                .and_then(|v| v.as_str())
                .is_some_and(|a| a == "verify") =>
        {
            ToolKind::Verifier
        }
        "shell"
            if args
                .get("command")
                .and_then(|v| v.as_str())
                .is_some_and(|c| c.contains("verify_candidate")) =>
        {
            ToolKind::Verifier
        }
        "problem_model"
            if args
                .get("action")
                .and_then(|v| v.as_str())
                .is_some_and(|a| a == "finalize") =>
        {
            ToolKind::ProblemModelFinalize
        }
        _ => ToolKind::Other,
    }
}
