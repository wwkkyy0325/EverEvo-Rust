# 2026-08-13 — Benchmark feedback learning: agent convergence (anti-verification-spiral)

## Context

Batches 4-7 (24 questions, 5 exact / 19 fail). Failure classification:
- **13/19 timeouts** — agent found a verified candidate then kept re-verifying / re-searching until the 400s wall-clock hard-stop (empty pred).
- **4/19 committed-wrong** — Box Office (3 vs 6), crocodiles (7 vs 6), Newton (0.00022 vs 0.00033), World Bank (5 vs 4).
- **1/19 vendor error** (DeepSeek HTTP 500 mid-question), **1/19 GT typo** (Polybius/Ploybius — agent correct).

Root cause: the agent's epistemic framework (VERIFIED/UNVERIFIED/UNKNOWN, ADR 0009) has no **stopping criterion**.
A `verified` candidate with no contradiction should terminate research, but nothing requires it to. Convergence threshold
tweaks were the wrong lever (wall-clock pressure the model doesn't internalize). The fix is a reasoning-level satisficing
criterion + runtime enforcement symmetric with the existing under-verification commit gate.

## Research grounding

- **CGDP (arXiv 2605.07042)**: programmatic exhaustion gate — halts unproductive search without premature stopping; saved up to 39% tokens with no degradation.
- **SAAS (arXiv 2605.29796)**: over-search mitigation — agents "fail to terminate search even when adequate evidence has been collected".
- **Satisficing / bounded rationality**: explicit aspiration threshold; stop at first option that clears it; "an answer now usually beats a marginally better answer later".
- **Groundedness / faithfulness verification**: a claim is supported only if it traces to a retrieved source span; distinguish supported vs contradicted vs insufficient.

## Tasks

- [x] Analyze wrong answers + classify failure modes (done 2026-08-13)
- [x] Research authoritative grounding (done 2026-08-13)
- [x] Static rules in ContextStages:
  - [x] AnswerDiscipline — satisficing/STOP criterion (candidate + ≥1 direct source + no contradiction = SUFFICIENT → commit)
  - [x] VerifyCandidate — one-pass checkpoint, then commit; circular-verifier warning = re-derive independently
  - [x] EvidenceChecklist — a verified item stays done; when all done, commit; do not extend
  - [x] ProblemModeling — ≤3 problem_model calls; cluster map_reduce for N-item collection; transient failure → UNKNOWN
- [x] SYSTEM_PROMPT Critical Rules — vision/tool-failure protocol (describe_image fails twice → no pixel forensics)
- [x] Runtime gate in driver.rs — post_verify_turns counter + verified-aware convergence nudge
- [x] Verify: cargo fmt/clippy/test; update changelog + llmwiki docs

## Verify

- cargo check -p everevo-agent && cargo test -p everevo-agent --lib
- cargo fmt --check && cargo clippy --workspace -- -D warnings
- No benchmark run (binding constraint until next HF_TOKEN run)
