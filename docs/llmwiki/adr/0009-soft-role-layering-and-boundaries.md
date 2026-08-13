# ADR 0009 — Soft Role Layering + Epistemic Boundaries

- **Status:** Accepted
- **Date:** 2026-08-13
- **Decision Makers:** user + Claude Code

## Context

User directive: build the three-tier agent on the **prior archived design**
(`agent-orchestration.md`: Supervisor / SubAgent / Skill), do **NOT** hard-cut
the existing system into three rigid tiers, and make the layering **soft and
extensible** so future pipeline adjustments are cheap. The unifying purpose of
this work is **boundary drawing**: give the agent clear boundaries so it knows
what it knows and what it doesn't, and prevent context pollution from causing
unreliable generation.

The existing system already implements the two tiers (Supervisor = main agent
loop; SubAgent = task/team/cluster sub-agents; Skill = `SkillStage`). The prior
planning proposed a hard `Orchestrator/Executor/Verifier` cut; this ADR replaces
that with soft metadata.

## Decision

1. **`AgentRole` = shared role vocabulary** (`subagent_roles.rs`). Existing role
   systems (`stype_guidance`, `TeamRole`) keep their own prompts and behavior;
   the enum gives one set of names + an optional canonical prompt provider new
   roles opt into. Legacy aliases preserved (`code-explorer`→Researcher is the
   historical default). Extensible: add a variant.
2. **`AgentTier` = soft layering metadata** (`Supervisor / SubAgent / Verifier`).
   Annotation, not structure — nothing is cut or moved. The prior
   `Orchestrator/Executor/Verifier` vocabulary is retained as the tier names.
3. **Sub-agent context budget inherited** (`SubAgentContext.max_context_tokens`,
   default 80000): the parent model window threads into sub-agent assembly and
   the sub-agent `AgentLoop` context ceiling (`×4` chars/token, matching the main
   loop). Fallback preserves the legacy value.
4. **Context-injection boundaries (P2)**:
   - Drift-bomb elimination: "我做了X"/"继续" live only in `TaskStateStage`;
     "fix tests / verify / match style" only in `BestPracticesStage`; trimmed
     from `SYSTEM_PROMPT` (one convention per layer).
   - **Epistemic boundary rule** added to `AnswerDiscipline` (Hard): the agent
     must keep `[VERIFIED] / [UNVERIFIED] / [UNKNOWN]` explicit and commit only
     `[VERIFIED]` — the direct implementation of "know what you don't know".
   - Hard verification fragments capped by `clamp_verify_fragment` (generous
     bound derived from the memory allocation; no-op today, bounds worst case).

## Consequences

- No behavior regression from the role refactor: `TeamRole` untouched,
  `stype_guidance` behavior-preserving (verified by existing + new tests).
- Sub-agents now have a real context budget instead of a hardcoded cap, enabling
  deeper research/verification on the parent's window.
- Reduced prompt redundancy (fewer tokens, fewer contradiction surfaces).
- The epistemic-boundary rule directly addresses the user's core concern:
  confident-but-wrong generations from context pollution.

## Not done (future)

- Cross-model verifier (ADR 0007).
- Hard structural hierarchy; the layering remains annotation + vocabulary.
