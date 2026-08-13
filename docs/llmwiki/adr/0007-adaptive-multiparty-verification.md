# ADR 0007 — Adaptive Multi-Party Verification

- **Status:** Accepted
- **Date:** 2026-08-12
- **Decision Makers:** user + Claude Code

## Context

GAIA benchmark run-7f reached 107/165 (64.8%). Error-set recovery (batches 1+2)
showed 4/13 recovered via prompt-level self-verification, but **self-consistency
voting (`--attempts N`) contributed zero** beyond pass@1 at temperature 0.0 —
each attempt was a fresh session and a deterministic duplicate. The real source
was also paying the full verification prompt tax on *every* question, including
trivial ones where research shows mandatory verification *harms* accuracy.

Authoritative findings (cross-validated via web search, 2026-08-12):

- **VerifiAgent** (Monash/VinUniversity): mandatory tool-use verification
  underperforms plain CoT on simple problems — "unnecessary complexity and more
  opportunities for errors". Solution: two-layer adaptive verification.
- **Leni / arXiv 2607.17044** (GAIA #1, 77.6%): verification's isolated
  contribution (+1.5pp) is *concentrated at the top of the score distribution*
  — it converts otherwise-failing hard tasks. Verifier confusion matrix: catch
  0.20 / fix 0.75 / **no false-alarm regressions**. Ablation: replacing the
  verifier with the generating frontier model eliminates most rescues — the
  verifier must differ from the generator.
- **CalVerT**: the two failure modes are over-verification on easy cases and
  under-grounding on hard ones.
- **DeepVerifier** (ACL 2026 Findings, GAIA +8-11%): rubric-guided adaptive
  verification; break hard verification into small source-checkable questions.

## Decision

1. **Deterministic difficulty gate (free).** New `stages::difficulty` module
   classifies the request (numeric/count/ambiguity/attachment/length signals)
   into `Simple | Hard`. Conservative (errs toward Hard).
2. **Gate the verification stages.** `VerifyCandidateStage` and
   `EvidenceChecklistStage` return `None` (skip) on Simple questions;
   `AnswerDisciplineStage` injects a short format-contract version on Simple.
   Simple questions pay zero verification overhead.
3. **Strengthen multi-party verification on Hard only.** The two stages are
   reframed as independent reviewer personas (not the answer author) to break
   the self-reference trap; `verify_candidate.py` (deterministic) is mandatory,
   escalating to `cluster verify` (adversarial sub-agents) on disagreement.
4. **Loop-level commit gate.** `driver.rs` tracks whether a verification step
   ran; a Hard question that commits unverified is re-prompted (capped at 2
   re-prompts, time/turn-aware), then commits best-effort regardless.
5. **Meta-agent gated by difficulty.** The autonomous meta-diagnosis only
   triggers on Hard sessions (Simple sessions pay no self-diagnosis overhead).
6. **Deprecate `--attempts N` voting** (zero value at temp 0.0; N× cost).

## Consequences

- **Cost:** simple questions go from full verification prompt → near-zero;
  hard questions keep (and strengthen) the ensemble. Deterministic verifier
  runs first; adversarial sub-agents only escalate on disagreement.
- **Accuracy:** gating removes forced-tool overhead on simple questions
  (research: net-positive); hard-question verification is where value
  concentrates (Leni +1.5pp, no false-alarm regressions).
- **Real-runtime:** all changes live in production stages + loop, not the
  benchmark harness; benchmark mode only affects turn/time budgets.
- **Not done (deferred):** cross-model verifier (`verifierModelId` + a second
  LLM client threaded through `SubAgentPool`) — the local 2B is too weak to add
  verification value; revisit when a stronger cheap verifier exists.
