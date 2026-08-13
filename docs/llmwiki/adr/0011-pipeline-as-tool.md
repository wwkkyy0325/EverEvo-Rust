# ADR 0011 — Pipeline-as-Tool (Selective Reuse + Self-Assembly)

- **Status:** Accepted
- **Date:** 2026-08-13
- **Decision Makers:** user + Claude Code

## Context

User directive: tune all pipelines — pipelines should have DEFAULT behavior
(auto-injected) while also being CALLABLE AS TOOLS by the agent; support
SELECTIVE reuse of pipeline parts (not the whole) and SELF-ASSEMBLY of a
pipeline. Research authoritative info first.

Research (cross-validated): SELF-DISCOVER (compose reasoning structure from
atomic modules, 10-40x efficiency), semstreams (`decide()` tool triggers stages),
PreAct (stages = registry rows with own prompt/context/tools/model), fleet-rlm
(light→heavy escalation), ReAct Toolbelt (dynamic minimal tool inventory),
LangGraph (runtime routing), MAVEN (structured reasoning interface).

## Decision

1. **`ContextStage` metadata**: `tool_visible()` + `description()` (defaults).
   Marked tool-visible: AnswerDiscipline / EvidenceChecklist / VerifyCandidate /
   ProblemModeling.
2. **`stage_catalog()`** (agent crate): the module library — name, description,
   short canonical prompt for each tool-visible stage.
3. **`pipeline` tool** (main-loop only): `list_stages` (module library view),
   `run_stage {name}` (apply one stage on demand — selective reuse),
   `run_pipeline {stages}` (apply a selected subset), `compose {task}`
   (recommended stage sequence — keyword-based SELF-DISCOVER-lite).
4. **Default behavior unchanged**: stages still auto-inject by priority; the
   tool is an on-demand supplement. Simple questions still skip the heavy stages.

## Consequences

- The agent can re-apply / selectively reuse reasoning stages without the whole
  pipeline, and compose a task-specific sequence.
- No behavior change to the default pipeline (soft, additive).
- Tool inventory stays small (catalog of 4, plus the existing registry).
