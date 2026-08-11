//! EvidenceChecklist ContextStage — verifier-gated commit discipline.
//!
//! ECLoop-style (arXiv 2607.28815): pre-declare the evidence conditions an
//! answer must satisfy, then gate the commit step on each condition having a
//! source and a deterministic numeric check. Added at priority 2 (right after
//! AnswerDiscipline, before skills) and covers the [C] "wrong committed
//! answer" family in the GAIA L1 benchmark:
//!
//! - e1fc63a2: order-of-magnitude misread (claimed 17000, expected order 17)
//! - 3cef3a44: verbatim list dropped "fresh" / reordered
//! - e142056d: constraint misread ($12,000 vs 16000)
//!
//! The gate itself is prompt-level: the model runs the deterministic sandbox
//! verifier (`verify_candidate.py`) and only commits a `Final answer:` that
//! passes. The verifier loop is wall-clock capped so it can never consume the
//! question budget — when time is short the model commits its best candidate
//! rather than looping (never "no answer").

use everevo_core::context::{ContextBuildContext, ContextFragment, ContextStage};
use everevo_core::llm::LlmMessage;

/// Injects the pre-commit evidence checklist: enumerate the constraints the
/// answer must honor, verify each one deterministically, then commit.
///
/// Priority: 2 (stable-sorts right after AnswerDiscipline, before skills).
pub struct EvidenceChecklistStage;

impl ContextStage for EvidenceChecklistStage {
    fn priority(&self) -> i32 {
        2
    }
    fn name(&self) -> &str {
        "evidence_checklist"
    }

    fn build(&self, _ctx: &ContextBuildContext) -> Option<ContextFragment> {
        let content = "\
## Evidence Checklist (commit gate)

### 1. Enumerate before you start
In your FIRST response, write an explicit checklist of every NUMBER, UNIT, \
NAMED ENTITY, and OPERATION your final answer must honor. Examples:
\"order of magnitude ~10^2\", \"unit: hours\", \"entities: Y. Uehara\", \
\"operation: 16000 × 0.75\". Keep it visible so you can check each item \
against evidence later.

### 2. Verify before you commit
Before emitting `Final answer:`, every checklist item MUST have:
- a SOURCE (attached file, web result, or tool output that states it), and
- a DETERMINISTIC CHECK (a numeric computation, not a guess).
Run the sandbox verifier for this:
  `python verify_candidate.py verify --answer <your answer> \\
      --expected <expected value> [--unit <unit>] [--compute <expr>] \\
      [--expect-list <verbatim items>] [--entity <name>]`
Use `--expected` with the value you derived and `--unit` with its dimension, \
so order-of-magnitude and unit errors are caught. If it reports violations, \
repair the candidate and re-verify — at most 2 attempts.

### 3. Cap the verify loop
Do not let re-verification consume the remaining time budget. If the verifier \
still disagrees after 2 repairs, commit the BEST verified candidate anyway. \
Never respond \"no answer\" — a best-effort value beats an empty answer.

### 4. Order of magnitude first
If the question is numeric, sanity-check the magnitude before anything else: \
a 17000 where the quantity is ~17 is a wrong answer regardless of how the \
computation was \"derived\".";
        Some(ContextFragment {
            label: "Evidence Checklist".into(),
            messages: vec![LlmMessage::user(content)],
        })
    }
}
