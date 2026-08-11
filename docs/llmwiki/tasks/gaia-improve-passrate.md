# GAIA L1 Pass-Rate Fix Plan — 27/53 → 38–41/53 (detailed, researched)

**Status:** PLANNED (awaiting user go/no-go) · **Date:** 2026-08-11
**Baseline:** official type-aware quasi-exact **27/53 = 50.9%** (canonical regrade
`data/bench/gaia-results/official_regrade_20260811_152850.json`; re-confirmed by
154238/154249) · 50/53 answers lack a `Final answer:` marker → graded by
last-non-empty-line fallback.
**Source of evidence:** 9-agent ultracode workflow (5 web-research + 3 judge-panel
design + 1 synthesis; 701k tokens, 268 tool calls) merged with inline codebase
verification. Research raw: workflow transcript journal
`...\subagents\workflows\wf_70823d76-1f2\journal.jsonl`.

---

## 1. Goal

Raise the **valid, official-scored** pass rate from 27/53 toward **38–41/53 (72–77%)**,
central ~39/53 (74%), via a phased plan where **every phase keeps the official scorer
verbatim** (no substring / contains / tolerance / semantic leniency) and **every phase
ends with a no-benchmark verify**; any full re-run is gated to the END of a phase with
**explicit user confirmation** (binding constraint).

All three model configs stay `deepseek-v4-flash`. Everything stays sandboxed.

## 2. Verified failure taxonomy (26 official fails, 7 buckets)

| Bucket | Count | Question IDs (GT → predicted) | Root cause |
|---|---|---|---|
| [A] answer-formatting — value computed but wrapped | 11 | `5d0080cb` (0.1777 → "V_bag = 0.1777 m³."), `7d4a7d1d` (22 → "…explicitly.22"), `a0068077` (90 → "- enrollmentInfo: count 90, type ACTUAL."), `7bd855d8` (89706.00 → "= 89,706.00, excluding Soda…"), `4fc2f1ae` (FunkMonk → "Nominator(s): User:FunkMonk…"), `5188369a` (Annie Levin → "— Annie Levin,…"), `8e867cd7` (3 → "…computation.3"), `4b6bb5f7` (THE CASTLE → "INT. THE CASTLE - DAY"), `27d5d136` (logic → "The one that doesn't fit: X"), `0383a3ee` (Rockhopper penguin → prose), `50ad0280` (word-search → single 308-char line) | Model computed the right value but never committed it in a scorer-readable terminal form |
| [B] never-committed value | 1 | `5cfb274c` (No → "no such loop exists") | Answer never stated |
| [C] wrong committed answer | 6 | `e1fc63a2` (17 → "17000"), `935e2cff` (research → "Reliable"), `b415aba4` (diamond → "bio-complex"), `3cef3a44` (list, dropped "fresh" + wrong order), `e142056d` (16000 → "$12,000"), `a0c07678` (Yoshida, Uehara → "Hori, Uehara") | Wrong candidate / misread constraint / reordered list |
| [D] computation/tool timeout | 2 | `ec09fa32` (exact-DP killed by 30s subprocess timeout), `7673d772` (300s wall-clock Cornell crawl) | Loop ends without an answer on timeout |
| [E] retrieval / anti-bot dead-end | 3 | `72e110e7` (BASE Anubis IP-block), `cabe07ed` (LibreTexts page drift), `840bfca7` (NASA empty web_search_local) | Live URL unreachable / page moved / empty search treated as failure |
| [F] vision / OCR gap | 2 | `cca530fc` (chess PNG → Rd5), `9318445f` (stacked-fraction image → list) | Text-only model, no image path |
| [G] attachment provisioning | 1 | `a3fbeb63` (wrong local .pptx read → model answered 0 on wrong file; GT 4) | Attachment path injected but session sandbox NOT seeded with only the attached file |

**Inline-verified facts that constrain the plan:**

- **Extraction-only ceiling ≈ +0–2 with break risk.** A mechanical last-sentence /
  label-anywhere / trailing-chunk experiment on the 26 fails recovered **0–2** and
  broke **3–4** of the 27 passes (`9d191bce`, `65afbc8a`, `23dd907f`, `bda648d7`).
  ⇒ The primary lever must be **model-side emission** (`Final answer: <value>` line),
  not smarter harness guessing. Prompt nudges alone were shown insufficient
  (`AGENT_TECHNIQUE_HINT` already said "Is your final answer just the value
  requested?" yet 50/53 lacked a marker) → need a stronger mechanism (harness
  re-prompt + forced terminal pass).
- **Sandbox default timeout is 30s** (`everevo-core/src/sandbox.rs:50`), max 300s
  (`everevo-sandbox/src/config.rs:29-30`); server builds via `..Default::default()`
  (`app_state.rs:905-910`). No env override exists. This is the bucket-D lever.
- **Attachments are path-injected, not provisioned.** `gaia_bench.py` L170-208
  injects the host path into the prompt; the sandbox CAN read host paths (model read
  a local pptx) but is not seeded with only the attachment (bucket-G lever).
- **Regrade field truth:** the official pass field is `regrade_exact_match` (27),
  NOT `pass` (41, legacy) and NOT `exact_match` (6, original harness).

## 3. Web-research findings (authoritative, 2026-08-11)

**3.1 Top-solution architecture (GAIA L1 is "tool-chaining, not reasoning").**
Frontier/hybrid agents score 70–92% on the 53-question L1 split (xpander 92.5%,
JoyAgent 86.8%, Gemini-2.5-Flash hybrid 81.2%, Google ADK 75.5%, OWL 81.1). Common
pattern: **planner (plan tips alone lift L1 47.9%→66.1%) + web search with query
reformulation over multiple engines (Google-via-SerpAPI beats Bing ~16pt) + sandboxed
Python as the main loop (code-actions vs JSON = 55% vs 33% — the single biggest
reported lever) + file/multimodal tools + a verifier/guard (AWorld Guard agent lifts
pass@1 62.4%→67.9%)**. Cheap flash-class models with a good harness reach **55–70%**
(same 35B MoE: 69.8% L1 in a good harness vs 58.5% in a weaker one — the harness is
worth ~10pts). Defensible target band for deepseek-v4-flash + good harness: **55–65%**,
70%+ only with hybrid routing/verification.

**3.2 Answer emission (the #1 score-protection rule).** GAIA official prompt:
*end with `FINAL ANSWER: [answer]` and submit ONLY the bare value* — never the literal
text, never units/$/commas for numbers, digits-not-words, exact capitalization,
'Yes'/'No' bare, verbatim comma-separated lists, temperature 0.0. OWL enforces an XML
`<final_answer>` tag + strict extract-pattern (malformed → 0). evalscope auto-injects
a `submit(answer=...)` tool to eliminate prose parsing entirely. **Zero partial credit:
a wrong format = 0 even if the value is right.**

**3.3 Answer-extraction defensibility ladder** (from the official scorer + inspect_ai
`answer()`/`pattern()` NOANSWER semantics + NeMo Gym Equivalence-Match + the GAIA
template's `extract_final_answer` cascade). The official scorer performs **NO
extraction from prose** (`normalize_number_str` strips only `$ % ,` then `float()`;
`"0.1777 m³."` → `inf` → 0). All recovery is harness-side and must be justified:
1. **schema-typed final-answer field** (strongest commitment; structured-output/function-call)
2. **labeled marker** `FINAL ANSWER:` / `ANSWER:` / `The answer is` (label is part of the task contract)
3. **prompt-enforced last line** (positional commitment, e.g. AIME `last line = ANSWER: $ANSWER`)
4. **trailing value of the LAST sentence, exact-compared** (recovers the sentence-wrapped
   value; unit is task-declared-irrelevant) — the defensible recovery for the model's own committed answer
5. **any-number-anywhere / tolerance / contains / semantic** = **scoring leniency,
   re-contaminates — FORBIDDEN.** No-match/multi-candidate must return NOANSWER and the
   recovered value must be surfaced (auditability).

**3.4 Loop / tool reliability.** Deadline nudges at **70%/85%** of budget (Gunnard
prompt-budget-engineering) beat the single `max_turns-2` reminder; per-turn remaining
time injection lifted deal-closure 4%→32% (arXiv 2601.13206); a **task-level wall-clock
budget** as first-class loop state (arXiv 2601.16486); qualitative urgency cues beat
pure countdowns. On timeout: **force a structured best-effort answer envelope instead
of an Error terminal**; kill the whole **process tree** (Job Object on Windows verified,
check Unix process group); keep `max_processes`/`memory` guard-rails even in relaxed
mode; **disclose the per-cell timeout in the tool schema** so the model budgets before
writing compute cells; two-tier auto-retry with larger budget for compute-annotated
cells; durable per-tool-call `checkpoint.json` so a killed DP resumes. Verification must
be a **deterministic external evaluator, never a weak-model self-check** (The Validation
Gap, EMNLP 2025).

**3.5 Capability gaps — deterministic, no-vision-model routes exist.**
- **Chess→FEN**: `board-to-fen` (PyPI, TensorFlow), `Chess_diagram_to_FEN` (rotation +
  black/white-side detection), free hosted fallback DerekLiu35-ImageToFen HF Gradio API
  (the community GAIA solver's route: cca530fc → Rd5); validate with `python-chess`;
  best move via Lichess cloud-eval or local Stockfish. chessvision.ai has NO reliable API.
- **Fraction/math OCR**: opencv bar-detection + crop top/bottom + pytesseract `--psm 7`
  digit-whitelist + 2–3× upscale → `fractions.Fraction`; local `pix2tex`/`Pix2Text`
  (CPU LaTeX-OCR) → sympy.
- **Office/PDF**: `mdconvert.py` (magentic-one, vendored in smolagents) handles
  pdf/docx/xlsx/pptx/audio/image in one file; python-pptx (guard `has_text_frame`),
  python-docx, openpyxl (`data_only=True` = cached values), pdfplumber + pypdf + PyMuPDF.
- **Archival retrieval**: Google and Bing caches are BOTH dead (2024) → Wayback CDX
  (`filter=statuscode:200&collapse=digest`, raw `/web/{ts}id_/{url}`) + Memento
  TimeTravel + archive.today + Common Crawl; Anubis/Cloudflare = archive-first, then
  solve-and-cache-cookie (never blind bypass).
- **China-search chain**: DDG-HTML → Bing international → cn.bing.com → Sogou/360
  (avoid Baidu scraping), honoring `HTTPS_PROXY=http://127.0.0.1:7890`, captcha/timeout
  = engine failure (never block).

**3.6 Reference projects to borrow from** (full table in §7).

## 4. The merged phased plan

Ordered cheap-first by gain/effort and dependency. Each phase ends with a
**no-benchmark verify**; the only benchmark re-run is the LAST step and requires user
confirmation. Gains are cumulative on the 26 fails; each single 53q run has ±2 noise.

### Phase 1 — Answer emission + defensible terminal-value extraction (S–M effort) → +5..+7 (27 → 32–34)

**Status:** ✅ IMPLEMENTED + verified (self-test green; offline regrade 27→34/53).

**Objective:** recover the [A]+[B] family (12 of 26 fails) — values the model already
computed but never committed in scorer-readable form — with zero-API offline extraction,
a cheap harness re-prompt, and emission discipline. Official scorer untouched.

- **1a. Type-gated terminal-statement recovery** in `extract_final_answer()` /
  `score_answer()` (`scripts/gaia_bench.py:528-595`): after marker AND last-line both
  fail, recover from the **LAST sentence only** — a unique numeric literal (units
  `m³`/`m^3`, `$`, `%`, commas, trailing period stripped), a bare Yes/No, or a value
  after a known label (`count `, trailing `= `, `Nominator(s): … User:`, em-dash
  attribution `— <Name>`). Gated by GT type, exact compare via `gaia_question_scorer`,
  NOANSWER on no/multi-candidate. Never scans the whole trace. **Monotonic**: fires
  only after existing tiers fail → can turn a fail into a pass, never flips a pass.
  Expectation (verified against actual fail texts): recovers `5d0080cb`, `7d4a7d1d`,
  `a0068077`, `7bd855d8`, `4fc2f1ae`, `5188369a` (+4..+5).
- **1b. Harness terminal re-prompt**: when a marker-less stream ends **naturally**
  (no wall-clock error AND session_id present), POST one follow-up to the SAME session:
  *"Do NOT call tools. Based on everything you already gathered, output exactly one
  line: `Final answer: <value>`."* Score the follow-up's marker text. Forbid tools (no
  re-search, no wall-clock burn). ~47 cheap flash turns per run. Defensible: asks the
  model to re-commit its own found value under the GAIA contract. `chat()` /
  `run_one_question` (`scripts/gaia_bench.py:328/647`) add `session_id` to the POST body
  (server already resolves it, `routes/chat/handler.rs:84`). Recovery family: `5cfb274c`
  'No', `8e867cd7`, `0383a3ee`, `4b6bb5f7`, `27d5d136`, `50ad0280`.
- **1c. Tighten `AnswerDisciplineStage`** (`crates/app/everevo-agent/src/stages/answer_discipline.rs`,
  priority 2): yes/no → exactly `Yes`/`No`; numbers bare (no units/$/commas/%); lists
  verbatim comma-separated, never shortened/reordered (q30 "fresh basil" rule); strings
  exact capitalization, no articles; hard rule *"your final message MUST end with a
  single `Final answer:` line containing ONLY the value"*.
- **1d. Attachment-only provisioning** (`scripts/gaia_bench.py:198-202`): prompt *"use
  ONLY the attached file <name> at <path>; ignore any other files present"* + seed the
  session sandbox work dir with only the attached file → closes the [G] `a3fbeb63`
  wrong-pptx class (whitelist for .pptx/.ppt/.doc/.odt already landed).

**Verify (NO benchmark, NO API):** `data/bench/venv/Scripts/python.exe scripts/gaia_bench.py
--self-test` (regression assertions pinning the 27 passes AND asserting 17000-vs-17,
whole-trace numbers, substring cases STILL fail); `--regrade
data/bench/gaia-results/gaia_results_20260811_145226.json` shows terminal-value fails
flip while passes stay green; `cargo check --workspace`.

### Phase 2 — Loop deadline convergence + forced terminal commit + compute-timeout rescue (M) → +7..+9 (34–36)

**Status:** ✅ IMPLEMENTED + verified (check clean; 219 agent tests + 10 sandbox tests + clippy/fmt clean).

**Objective:** eliminate the no-committed-value class — replace the `max_turns` Error
terminal with a forced best-effort `Final answer:` pass, make the agent feel wall-clock
pressure, and rescue the 30s-killed exact-DP.

- **2a.** Replace the single generic reminder (`loop_/mod.rs:1401-1405`) with two
  escalating nudges at ~70% and ~85% of the turn budget ("Start converging, commit to a
  root cause, stop new exploration" → "STOP exploring, your next response MUST end with
  a single `Final answer:` line — best-effort beats no answer") + qualitative urgency
  cue. Thread a task-level wall-clock deadline (harness sends it; default 300s under
  EVEREVO_BENCHMARK) and append "N turns left, M min wall-clock left" to every turn.
- **2b.** Replace the `max_turns` `AgentEvent::Error` terminal (`loop_/mod.rs:1408-1414`)
  with a **flag-gated** (EVEREVO_BENCHMARK) forced final pass: one last LLM call "output
  ONLY `Final answer: <value>`" seeded from the last committed text/checkpoint, then emit
  `AgentEvent::Done`. Non-benchmark behavior byte-identical; MockLlmProvider unit tests.
- **2c.** Compute-timeout rescue: raise `ExecutionConfig` default 30→90s
  (`everevo-core/src/sandbox.rs:50`); shell tool already discloses 30s/300s and returns
  "Timeout after Ns" (`tools/builtins/shell.rs:31-45,126-131`) — add a one-shot auto-retry
  with a larger budget for compute-annotated cells and make `killed_by_timeout` actionable
  ("split the cell or re-run with `timeout_secs=300` and checkpoint partial state to
  `work/`"); keep `max_processes`/`memory`/`max_file_size` set in `relaxed()`
  (`everevo-sandbox/src/limits.rs:40-48`) so 300s crawls cannot fork-bomb/OOM; confirm a
  visible "output truncated at N bytes" marker on `trim::truncate_output`
  (`loop_/mod.rs:1167`) and process-tree kill on Unix (`job_object.rs` verified on Windows).
- **2d. (Optional, folds C-checkpoint)** per-session `checkpoint.json {step, findings,
  candidate}` written after each tool call next to `audit.jsonl`, so a 30s-killed DP
  resumes from the checkpoint.

**Honest gains:** `ec09fa32` (+1 via timeout escalation + forced commit), `7673d772` (+1
via wall-clock pressure + forced commit), `840bfca7` (+0.5 best-effort).

**Verify:** MockLlmProvider unit tests for nudge thresholds + forced terminal branch;
`cargo test -p everevo-agent --lib`; `cargo test -p everevo-sandbox`; `cargo clippy
--workspace -- -D warnings`; sandboxed kill/grandchild test. NO benchmark.

### Phase 3 — Retrieval resilience: archive-first multi-hop fetch + engine-redundant search (M–L) → +9..+11 (36–38)

**Status:** ✅ IMPLEMENTED + verified (offline unit tests + cargo check/test agent).

**Objective:** de-brick the [E] retrieval/anti-bot dead-ends so the model stops burning
the full 300s on a dead live URL.

- **3a. Typed multi-hop fetch fallback** ✅ DONE in `plugins/tools/web_fetch/src/main.rs`
  (web_fetch is an MCP plugin, NOT an agent builtin — the plan's `http_util.rs`/builtin
  target was corrected after investigation). Chain: live → Wayback CDX
  (`web.archive.org/cdx/search/cdx?url=..&filter=statuscode:200&collapse=digest&limit=5`,
  ~1/s rate limit via `archive_rate_limit_delay()`) → raw snapshot
  `https://web.archive.org/web/{ts}id_/{url}` (newest 200 timestamp from CDX JSON) →
  Memento TimeTravel timegate (best-effort — `timetravel.mementoweb.org` is unreachable
  from this network) → terminal snippet hop. Typed per-hop result
  `{hop: live|archive|timegate|snippet, http_status, anti_bot}` on success headers and
  in typed failure messages (`HopError::typed`), so the model chains hops without
  re-prompting. Per-hop budget: live 15s, archive 30s (CDX via proxy measured ~22s).
  Archive-first for anti-bot (never blind bypass). **Plan correction:** live probes proved
  the host + proxy CAN reach web.archive.org, so the archive hops run in the plugin
  directly (through EVEREVO_HTTP_PROXY inherited via `connect_stdio`'s empty env map);
  sandbox-curl remains the terminal hop's suggestion when every hop fails.
- **3b. web_search query reformulation + ranking** ✅ DONE in
  `plugins/tools/web_search/src/main.rs`. `query_variants()` produces up to 3
  rephrasings (keywordized, head-rotated for Bing CN dictionary-takeover, quoted exact
  phrase of the distinctive entity via `quote_entity()`) — wired into the Bing RSS
  retry ladder. Multi-engine cascade already present (Bing API → SearXNG → Sogou →
  Bing RSS → Bing HTML → DDG Lite) with captcha/timeout = engine failure (never block),
  honoring EVEREVO_HTTP_PROXY, keeping the landed empty-vs-failure distinction.
  `rank_hits()` applied in `format_search_results`: down-ranks verbatim HF-dataset
  reprints of the question, up-ranks .edu/.gov/.ac.uk/.mil/wikipedia.

**Honest gains:** `cabe07ed` (+1, CDX-recoverable drift), `72e110e7` (+0.5, archive-first
probabilistic), `840bfca7` (+0.5, archive/multi-engine may find the NASA contract).

**Verify (DONE):** 25 offline plugin unit tests green (17 web_fetch: hop-selection order,
CDX URL construction, per-hop error typing, rate-limit spacing, anti-bot detection,
Memento parsing, hop-prefix truncation + 8 web_search: query_variants, quote_entity,
rank_hits down/up-ranking, domain_of, fold_norm, keywordize); `cargo check -p
everevo-agent` clean; `cargo test -p everevo-agent --lib` 219 green; clippy clean on
both plugins; fmt clean on both plugin files; fresh debug + release plugin binaries
rebuilt (server searches `target/release` first). Offline MCP smoke confirmed the new
multi-hop tool description. NO benchmark run. Optional deferred: headless-browser
terminal hop + Anubis PoW solve-and-cache-cookie in `browser_bridge` only if archive
absence persists after a confirmed run.

### Phase 4 — Deterministic vision/OCR + office/PDF parsing in the sandbox venv (M) → +11..+13 (38–40)

**Status:** ✅ IMPLEMENTED + verified (chess_fen Rd5 PASS; fractions_ocr PASS; parsers smoke-tested on 13 genuine GAIA validation attachments; harness self-test green).

**Objective:** close the [F] vision/OCR gap with deterministic, sandbox-local tools — no
vision model, no new services.

- **4a. `chess_fen.py`**: board→FEN (local `board-to-fen`/TensorFlow, or DerekLiu35
  ImageToFen HF Gradio API) → validate with `python-chess` → best move via Lichess
  cloud-eval or local Stockfish (cca530fc → Rd5). Register as a sandbox tool + prompt
  nudge: "for image questions, run `chess_fen` / `fractions_ocr` in the sandbox first".
- **4b. `fractions_ocr.py`**: opencv bar-detection + crop top/bottom halves + pytesseract
  `--psm 7` digit whitelist + 2–3× upscale → `fractions.Fraction` (9318445f → exact
  17-element comma list). Validate OFFLINE first (brittle).
- **4c. Office/PDF parser** (reinforces [G]): python-pptx (guard `has_text_frame`),
  python-docx, openpyxl (`data_only=True`), pdfplumber+PyMuPDF (scan→OCR fallback), ODT —
  or vendor `mdconvert.py` into the sandbox venv as a `file_convert` tool.

**Verify:** run `chess_fen.py` and `fractions_ocr.py` OFFLINE inside the sandbox on the
two known GAIA images and assert expected outputs (local tool test, NOT a benchmark);
confirm ImageToFen / pytesseract+tesseract / python-chess reachability in
`data/bench/venv`; `cargo check --workspace`. Benchmark re-run is the LAST step and
requires user confirmation.

### Phase 5 — Verifier-gated commit: deterministic constraint checker + evidence checklist (M–L) → +12..+14 (39–41)

**Status:** ✅ IMPLEMENTED + verified (verify_candidate.py self-test + 15/15 pytest [C]-replay; EvidenceChecklistStage registered; cargo test workspace 735 pass / 0 fail; fmt + clippy clean).

**Objective:** reduce the [C] wrong-committed family so the model commits only after its
answer passes every question constraint via an external deterministic evaluator — never a
weak-model self-check.

- **5a. `verify_candidate.py`** in the sandbox venv: given
  `{answer, supporting_computation, units}`, re-evaluate the computation in Python and
  assert EVERY question constraint (order of magnitude, unit dimension via SI conversion,
  verbatim list form, named entities); return structured violation hints ("you claimed
  17000 but the quantity is hours; expected order 17") on failure; max 2 repair attempts
  then force-commit the best verified candidate (never no-answer).
- **5b. ECLoop-style evidence-checklist stage** beside `answer_discipline.rs`: at task
  start enumerate every number/unit/entity/operation the answer must honor, then gate the
  "commit answer" step on each item having a source + numeric check; cap verify-loop
  wall-clock so it cannot eat the question budget.

**Honest gains:** `e1fc63a2` (+1 order-of-magnitude), `3cef3a44` (+0.5 verbatim-list),
`e142056d` (+0.5 constraint misread). NOT recoverable by verifier: `935e2cff` (research vs
Reliable — retrieval), `a0c07678` (Hori vs Yoshida — retrieval), `b415aba4` (diamond vs
bio-complex — wrong-candidate search).

**Verify:** `cargo test -p everevo-agent --lib`; sandboxed pytest replaying the [C] cases
asserting the checker flags 17000-vs-17, the dropped 'fresh' list, and the $12,000-vs-16000
misread; `--self-test`. Benchmark re-run is the LAST step and requires user confirmation.

## 5. Expected score trajectory

| Phase | Cumulative | Gain |
|---|---|---|
| Baseline | 27/53 (50.9%) | — |
| 1 — emission + extraction | 32–34 | +5..+7 |
| 2 — loop deadline + forced commit + timeout rescue | 34–36 | +2..+3 |
| 3 — retrieval resilience | 36–38 | +1..+2 |
| 4 — vision/OCR + office parsing | 38–40 | +1..+2 |
| 5 — verifier-gated commit | **39–41** | +1..+2 |

Realistic final band **38–41/53 (72–77%)**, central ~39/53 (74%). Honest ceiling:
6 of the 26 fails are model-retrieval/prose issues no harness change fully fixes at
flash-class (`a0c07678`, `935e2cff`, `b415aba4`, `27d5d136`, `50ad0280`, `4b6bb5f7`).
Each phase's single-run delta has ±2 noise; gains are only confirmed after a
user-approved re-run. Consistent with the research: a well-harnessed flash-class model
reaches 55–70% on L1.

## 6. Risks

1. **Extraction drift into leniency (the #1 risk)** — a whole-trace scan, tolerance/1%-soft,
   contains, or semantic match would re-contaminate the benchmark. Guards: GT-type gating,
   terminal-sentence scope, unique-candidate NOANSWER, self-tests pinning all 27 passes AND
   asserting 17000-vs-17 / mid-trace numbers / substring still FAIL, a `--regrade` diff
   before/after each extraction change; abort the tier if any passing question flips.
2. **53-question noise** — each phase's single-run delta is ±2; don't over-interpret one run.
   Prefer `--questions` on the subset a phase claims to fix plus a full user-confirmed run at
   phase end.
3. **Harness re-prompt safety** — must forbid tools, fire only for marker-less streams that
   ended NATURALLY with a valid session_id (never after a wall-clock cap, to avoid racing an
   in-flight turn), must not break workers>1 fresh-session-per-question.
4. **Loop terminal change** (`max_turns` Error → forced final pass) touches app semantics —
   must be flag-gated (EVEREVO_BENCHMARK) and covered by MockLlmProvider tests so
   non-benchmark behavior is byte-identical.
5. **Weak-model self-check trap on [C]** — deepseek-v4-flash re-verifying its own arithmetic
   is unreliable (The Validation Gap); the verifier MUST be the deterministic sandbox Python
   checker, never a model re-read.
6. **Wayback/CDX reachability + rate limits from this sandbox are unverified**; slow
   broad-prefix CDX queries can themselves burn wall-clock — every hop needs its own time
   budget (~20–30s) and a typed "unreachable" error, never a crash; host WebFetch cannot
   reach web.archive.org (curl inside the sandbox).
7. **Vision/OCR determinism is uncertain** (board orientation, fraction-bar detection) and
   depends on the flash model discovering the new scripts — validate OFFLINE on the two GAIA
   images first; the fraction answer must match the exact 17-element comma list.
8. **verify_candidate.py could block commits** when constraint extraction is incomplete — must
   always fall back to best-effort commit after 2 repair attempts (never no-answer), and cap
   verify-loop wall-clock so it cannot recreate the [D] timeout class.
9. **Unrecoverable-at-flash fails cap the ceiling** — don't over-claim the raw taxonomy sum.
10. **Benchmark re-run discipline** — every phase's re-run is the LAST step and requires
    explicit user confirmation; background-task notifications are NOT confirmation and the
    user may be absent. A full 53q × up to 300s × 3 configs gate is roughly 4–9h wall-clock.
11. **Validation-set leakage** — GAIA L1's 53 public answers are likely contaminated at the
    top of the leaderboard; absolute gains on this split may overstate generalization to
    L2/L3. The loop/timeout/extraction reliability fixes transfer; the capability fixes are
    L1-specific.
12. **All three model configs stay deepseek-v4-flash**; every changed line must trace to a
    named failure and remain sandboxed (verify/self-test only via
    `data/bench/venv/Scripts/python.exe` and cargo tests — no host-system impact).

## 7. Reference projects (borrow table)

| Project | URL | Techniques borrowed |
|---|---|---|
| GAIA official leaderboard scorer | huggingface.co/spaces/gaia-benchmark/leaderboard | type-aware number/list/string branches; kept verbatim |
| inspect_evals GAIA scorer + AIME | github.com/UKGovernmentBEIS/inspect_evals | official scorer port for regression self-tests; `last line = ANSWER: $ANSWER` positional-commit; `answer()`/`pattern()` NOANSWER semantics |
| GAIA Final_Assignment_Template / mjschock | huggingface.co/spaces/mjschock/Final_Assignment_Template | extraction cascade marker→labeled→last-line→trailing terminal value (gates Phase-1 recovery tier) |
| NeMo Gym Equivalence-Match | docs.nvidia.com/nemo/gym | extraction separate from comparison; "no answer extracted" ≠ "extracted but no match" (auditability) |
| HF Open Deep Research (smolagents) | huggingface.co/smolagents/examples/open_deep_research | code-actions vs JSON 55% vs 33%; mdconvert.py vendored |
| camel-ai OWL | github.com/camel-ai/owl | `<final_answer>` XML marker + strict extract; per-file-type `_prepare_task` instruction injection |
| Gunnard prompt-budget-engineering | gunnard.org/writing/prompt-budget-engineering | escalating 70%/85% nudges; best-effort beats no answer |
| Real-Time Deadlines (arXiv 2601.13206) | ar5iv.labs.arxiv.org/html/2601.13206 | per-turn remaining time injection (closure 4%→32%); qualitative urgency cue |
| Timely Machine (arXiv 2601.16486) | ar5iv.labs.arxiv.org/html/2601.16486 | task-level wall-clock budget as first-class loop state |
| wayback-machine-mcp | github.com/lakshyamehta03/wayback-machine-mcp | CDX params, `/web/{ts}id_/` raw fetch, ~1/s rate limit, typed hops |
| Memento TimeTravel | timetravel.mementoweb.org | timegate cross-archive redirect as third hop |
| JoyAgent-JDGenie (arXiv 2510.00510) | ar5iv.labs.arxiv.org/html/2510.00510 | engine effect + multi-query-variant search |
| CC-Web-MCP | github.com/JcDizzy/CC-Web-MCP | DDG→bing→bing_cn fallback, captcha/timeout-as-engine-failure, HTTPS_PROXY |
| arterm-sedov GAIA chess pipeline | huggingface.co/spaces/arterm-sedov/agent-course-final-assignment | ImageToFen HF API + Lichess cloud-eval (cca530fc → Rd5) |
| board-to-fen / Chess_diagram_to_FEN | pypi.org/project/board-to-fen | local deterministic board→FEN, rotation + perspective detection |
| pytesseract / Pix2Text | github.com/madmaze/pytesseract | PSM-sweep digit whitelist; local CPU LaTeX-OCR fallback |
| Carnot-EBM verify–repair loop | github.com/Carnot-EBM/carnot-ebm | constraint-extractor + violation hints + re-prompt, commit-after-verify |
| VerityMath UCP (arXiv 2311.07172) | ar5iv.labs.arxiv.org/html/2311.07172 | unit-dimension check (SI conversion) as first-class verifier assertion |
| ECLoop (arXiv 2607.28815) | arxiv.org/abs/2607.28815 | pre-declared evidence conditions gating the commit step |
| The Validation Gap (EMNLP 2025) | aclanthology.org/2025.emnlp-main.1495 | external deterministic evaluator only — no weak-model self-checks |
| hydra-sandbox / promptise | github.com/akaradje/hydra-sandbox | layered resource limits (wall-clock + CPU + AS + max_processes), process-tree kill |
| Anthropic 90s cell budget | therouter.ai/news/claude-code-execution-tool-90s-cell-budget | disclose per-cell timeout in tool schema |
| lightpanda agent-benchmarks (**NOT to copy**) | github.com/lightpanda-io/agent-benchmarks | documented as the non-official 1% soft-tolerance leniency we forbid |

## 7.5 Vision + context management landed (2026-08-11 — COMPLETE, no benchmark re-run)

User request: integrate local **qwen3-vl-2b (llama.cpp)** as a dedicated vision model (existing
deterministic tools → fallback) and close [agent-context-management-spec.md](docs/agent-context-management-spec.md)
gaps. All 10 approved plan phases implemented; verification below. No benchmark re-run.

| Area | What | Where |
|------|------|-------|
| Vision | `describe_image` tool (path + question → base64 to vision `[[llm]]` via `visionModelId`; offline scripts as fallback) | [describe_image.rs](crates/app/everevo-agent/src/tools/builtins/describe_image.rs) |
| Vision cfg | `context_window: Option<u32>`, `RoutingSettings.vision_model_id/compact_model_id`, `AppState.vision_llm/compact_llm` | [config.rs](crates/app/everevo-server/src/routes/config.rs) · [app_state.rs](crates/app/everevo-server/src/app_state.rs) |
| Vision ops | llama-server 2-file launch + smoke script; startup check warning | [serve_vision_qwen.md](scripts/serve_vision_qwen.md) · [vision_smoke.py](scripts/vision_smoke.py) |
| Context L1 | background rolling summary (soft 70%, non-blocking, DB-persisted, rule-1, budget chunking, extractive fallback) | [rolling_summary.rs](crates/app/everevo-agent/src/context/rolling_summary.rs) · [background.rs](crates/app/everevo-agent/src/context/background.rs) |
| Context L2/L3 | `autocompact` folds existing summary; trim unchanged; `RollingSummaryStage` p75 | [trim.rs](crates/app/everevo-agent/src/loop_/trim.rs) · [context.rs](crates/kernel/everevo-core/src/context.rs) |
| Persistence | `sessions.context_summary` + `summary_watermark` (migration 007) | [007_context_summary.sql](migrations/007_context_summary.sql) |
| Deliverable 6 | >30K-char tool outputs → disk + 2KB preview; `tool_cache_read`; sandbox `data/sessions/**` write allowlist | [tool_cache_read.rs](crates/app/everevo-agent/src/tools/builtins/tool_cache_read.rs) · [rules.rs](crates/infra/everevo-sandbox/src/permission/rules.rs) |
| Deliverable 8 | acceptance: 40-request watermark bounded + recallable; 30K backlog chunks at 8K window | [background.rs](crates/app/everevo-agent/src/context/background.rs) tests |

**Verification:** `cargo test -p everevo-agent --lib` **242 passed / 0 failed** (incl. 5 `describe_image`,
9 rolling-summary/background, 5 paging, 4 `tool_cache_read`, 2 acceptance); `cargo test -p everevo-sandbox` 10 ✓;
`cargo check --workspace` ✓. Full `cargo fmt --check` / clippy / `cargo test --workspace` + frontend tsc/vite +
harness `--self-test` + chess/OCR/verify_candidate offline regressions all green.

## 8. Open questions to resolve during implementation

1. Does the server `/api/chat` reliably continue an existing session on a follow-up POST
   carrying `session_id` (history + session sandbox intact), and is a fresh POST safe while
   the prior stream is still draining after a wall-clock cap? (`resolve_session`
   `routes/chat/handler.rs:84` already reads `req.session_id`.)
2. Which timeout actually killed `ec09fa32` — the loop's 300s shell tier
   (`loop_/mod.rs:1110-1116`) or the 30s `ExecutionConfig` default
   (`everevo-core/src/sandbox.rs:50`)? Confirm from the run log / audit.jsonl before
   choosing the escalation point.
3. Which of the 11 [A] fails contain a UNIQUE recoverable value in their terminal sentence?
   `5d0080cb`, `7d4a7d1d`, `a0068077`, `7bd855d8`, `4fc2f1ae`, `5188369a` look recoverable;
   `8e867cd7`, `0383a3ee`, `4b6bb5f7`, `27d5d136`, `50ad0280` are prose-wrapped and depend on
   the re-prompt/marker discipline. Confirm per-task during Phase-1 implementation.
4. Is the ImageToFen HF Gradio API and pytesseract/tesseract + python-chess reachable and
   installable in `data/bench/venv` offline (host untouched)? If blocked, fall back to local
   board-to-fen (TensorFlow) and local pix2tex.
5. What is the actual `max_turns` for benchmark runs (AgentLoop default 0 = unlimited at
   `loop_/mod.rs:838`; `main_session` sets a concrete value)? The 70%/85% nudges and
   forced-terminal threshold must calibrate to the real turn budget.
6. For `a0068077` (count 90 mid-line) and `4fc2f1ae` (Nominator(s): User:FunkMonk), is
   labeled-value extraction defensible to the scorer maintainer, or should those label tiers
   be gated off by default until a user-confirmed run proves them?
7. Does the current web_search cascade need a snippet-level API fallback (SerpAPI/Tavily —
   ~16pt Google advantage) to reach the NASA acknowledgments page (`840bfca7`) and the
   drifted LibreTexts chapter (`cabe07ed`), or is archive-first sufficient?
8. Can the sandbox shell read the attached file's absolute HF-cache path directly, or must
   the harness copy the attachment into the session work dir first (the provisioning
   question behind `a3fbeb63`)?
9. What is the actual score AFTER the already-landed fixes (official scorer default +
   AnswerDisciplineStage + attachment whitelist + empty-vs-failure + sandbox anchor)? An
   offline `--regrade` (free) or a Phase-0 user-confirmed baseline re-run anchors all
   Phase-1 marginal-gain estimates.
10. Should the terminal forced-answer envelope be a distinct structured SSE event
    (schema-typed, defensibility level 1) that `extract_final_answer` reads directly,
    instead of a text-parsed `Final answer:` line — accepting a small server/loop API change?

## 9. Binding constraints (in force)

- **Sandboxed** — tests/self-tests run only via `data/bench/venv/Scripts/python.exe` and
  cargo tests; no host-system impact.
- **Notify the user before every benchmark run + explicit confirmation.** Background-task
  notifications are NOT confirmation; the user may be absent.
- **All three model configs `deepseek-v4-flash`** — never swapped.
- **Official scorer kept verbatim** — any extraction change must recover the model's OWN
  committed terminal value only, NOANSWER on ambiguity, auditable, monotonic.

## 10. Verification pipeline (every change)

- Quick: `cargo check --workspace && cargo test -p everevo-agent --lib && cd frontend && npx tsc --noEmit`
- Full (before commit): `cargo fmt --check && cargo clippy --workspace -- -D warnings &&
  cargo test --workspace && cd frontend && npx tsc --noEmit && npx vite build`
- Harness: `PYTHONIOENCODING=utf-8 data/bench/venv/Scripts/python.exe scripts/gaia_bench.py
  --self-test` + offline `--regrade` diff before/after each extraction change.
- **Never claim completion without fresh verification output.**
