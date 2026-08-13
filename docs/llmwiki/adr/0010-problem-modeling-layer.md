# ADR 0010 — Problem-Modeling Layer (Causal Draft)

- **Status:** Accepted
- **Date:** 2026-08-13
- **Decision Makers:** user + Claude Code

## Context

User directive: complete the cross-validation logic. On HARD questions, the
agent should PROACTIVELY problem-model — build a causal draft, research
authoritative sources, deliberate (multi-party), route collected data through a
modeling pipeline, then answer systematically (especially compound questions).

User decisions: temporary KB (session-scoped) + solution-approach distillation
to workflows; structural depth; Hard questions only.

Research (cross-validated): CausalAgent (hierarchical causal graphs, 87.3%),
Separable Pathways (context graphs → 94% of reasoning gain), SAKE (-90% tokens),
CausalRAG2 (causal gates), working-vs-persistent memory ("pick the shallowest
layer").

## Decision

1. **`ProblemModelStore`** (session-scoped, volatile): nodes
   (SubQuestion/Fact/Claim/Candidate/Constraint) tagged with epistemic status
   (Verified/Unverified/Unknown) + causal/dependency/evidence/contradicts edges.
2. **`problem_model` tool** (main-loop only): init / add_node / add_edge /
   update_status / list / finalize.
3. **`ProblemModelingStage`** (Hard-only, priority 3, before the verification
   ensemble): guides the causal-draft modeling → research → multi-party
   deliberation → systematic answer.
4. **Driver marker** (`is_problem_model_finalize` + `model_drafted`): the Hard
   commit gate suggests modeling when the agent committed unverified without a
   model (informational, not enforced — avoids over-orchestration).
5. **Solution distillation**: post-turn, a finalized model triggers saving a
   generic `causal-draft-problem-modeling` workflow (the process structure, not
   specific question content — anti-contamination). Benchmark-gated.

## Consequences

- Hard/compound questions get a structural modeling discipline → systematic,
  evidence-traced answers; simple questions pay nothing.
- Temporary KB avoids cross-session pollution; workflows persist the reusable
  process.
- All behavior is additive to the existing verification ensemble (ADR 0007).
