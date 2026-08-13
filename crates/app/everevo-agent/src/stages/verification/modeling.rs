//! ProblemModeling ContextStage — causal-draft problem modeling for hard
//! questions (Phase A of the problem-modeling layer).
//!
//! Injected on Hard questions (right before the verification ensemble, priority
//! 3). Instructs the agent to build a session-scoped PROBLEM MODEL via the
//! `problem_model` tool: decompose into sub-questions, tag every node with an
//! epistemic status (VERIFIED / UNVERIFIED / UNKNOWN), link causal/dependency/
//! evidence edges, research + deliberate (multi-party verification), then
//! answer systematically — each sub-question with its [VERIFIED] evidence.
//!
//! Research-grounded: CausalAgent (hierarchical causal graphs), Separable
//! Pathways (context graphs drive reasoning quality), 4D-ARE (four-dimensional
//! decomposition). Simple questions are skipped entirely (zero overhead).

use super::gate::clamp_verify_fragment;
use super::gate::{classify, Difficulty};
use everevo_core::context::{ContextBuildContext, ContextFragment, ContextStage};
use everevo_core::llm::LlmMessage;

/// Injects the causal-draft problem-modeling discipline.
///
/// Priority: 3 (stable-sorts right before VerifyCandidate — the modeling
/// precedes the verification ensemble).
pub struct ProblemModelingStage;

impl ContextStage for ProblemModelingStage {
    fn priority(&self) -> i32 {
        3
    }
    fn name(&self) -> &str {
        "problem_modeling"
    }
    fn tool_visible(&self) -> bool {
        true
    }
    fn description(&self) -> &str {
        "Build a structural problem model (causal draft): decompose into sub-questions, tag VERIFIED / UNVERIFIED / UNKNOWN, link causal/evidence edges, then answer systematically."
    }

    fn build(&self, ctx: &ContextBuildContext) -> Option<ContextFragment> {
        if classify(&ctx.user_message) == Difficulty::Simple {
            return None;
        }
        let content = clamp_verify_fragment(
            &ctx.budget,
            "\
## Problem Modeling (HARD questions — causal draft)

For a COMPLEX or COMPOUND question, do NOT answer in a single pass. Build a
structural PROBLEM MODEL first using the `problem_model` tool, then answer
systematically from it.

### 1. Model the problem (causal draft) — COMPACT
Use `problem_model` to decompose the question. Build a COMPACT model — AT MOST
5 nodes. Add only the KEY sub-questions; do NOT create a node for every
quantity/entity/unit (over-modeling wastes the budget). Prefer one `add_nodes`
batch call over many `add_node` calls:
- `init` to start fresh.
- `add_nodes` with the 2-5 KEY sub-questions / claims in ONE call
  (`{id, kind, content, status, source}` per node).
- Link them with `add_edge` (`causal` / `dependency` / `evidence` /
  `contradicts`) so the reasoning structure is explicit.
**Model discipline: 1 `init` + 1 `add_nodes` total, then only the
`update_status` / `add_edge` calls needed to record findings. After 3
`problem_model` calls, STOP editing the model and work from it — a sub-question
node is DONE once it is `verified` with a source; never re-research a done
node.

### 2. Tag the epistemic boundary
Every node carries a status (ADR 0009):
- `verified` — the value appeared in a retrieved tool result (record `source`).
- `unverified` — derived/recalled, no retrieved source.
- `unknown` — no source retrieved yet.
Use `update_status` as research progresses. NEVER answer a sub-question from an
`unknown` node — keep researching (web_search / web_fetch / download) until it
is `verified`, or say you cannot determine it.

### 3. Research + deliberate
- Search authoritative sources for each sub-question's solution approach.
- **For a compound question with INDEPENDENT sub-parts, collect IN PARALLEL**:
  use `cluster map_reduce` with one item per sub-question so independent
  research runs concurrently (much faster than serial), then fold each returned
  value + source into the model as a `[VERIFIED]` node.
- **NEVER collect N independent items serially** — a serial fetch loop over many
  items is the top timeout cause. Always use `cluster map_reduce`.
- **Transient fetch failure**: retry at most ONCE, then record that sub-question
  as `[UNKNOWN]` and move on. Do not re-loop over failed items.
- Run the multi-party verification (deterministic `verify_candidate.py`, then
  `cluster verify` on disagreement) on each CANDIDATE node.
- Resolve `contradicts` edges with the source that survives verification.
- **Time discipline**: research + model within the budget; never let modeling
  or research consume the whole question time. A sub-question resolved as
  `[VERIFIED]` is FINAL — do not re-fetch it.

### 4. Finalize + answer systematically
- `finalize` once every sub-question node is `verified` or explicitly
  unreachable.
- Answer in the structure of the model — each sub-question gets its value with
  its [VERIFIED] source. For compound questions this structured answer is what
  makes it accurate, not a single-pass guess.",
        );
        Some(ContextFragment {
            label: "Problem Modeling".into(),
            messages: vec![LlmMessage::user(content)],
        })
    }
}
