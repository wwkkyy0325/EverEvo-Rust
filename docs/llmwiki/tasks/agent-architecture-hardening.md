# Task: Agent Architecture Hardening

**Started:** 2026-08-10 · **Status:** in progress · **Owner:** wwkkyy0325

User-specified architecture improvements for the EverEvo agent, each with a
stated normative basis. Current-state facts from `crates/` + `plugins/` audit
(2026-08-10, Explore map) and direct code reads.

---

## 1. Todo 任务体系 — ✅ DONE

Support dynamic append + item modification; status: pending / in_progress /
completed / failed / skipped / deferred; think before each step, authoritative
web verification, no blind execution.

- [x] Extend `TodoItem.status` to 6 values (`todo_write.rs` schema + description + summary)
- [x] Frontend `TodoPanel` renders new statuses (❌/⏭️/⏸️, failed in red)
- [x] Tests: schema coverage, per-status counting, append+modify (3 tests pass)
- [x] `api-registry.md` + `changelog.md` updated
- [ ] Prompt nudge "verify each step against authoritative sources before executing" → folded into item 4 (autonomy) / best-practices stage
  - verify: `cargo test -p everevo-agent --lib todo_write` green; `npx tsc --noEmit` clean

---

## 2. 容错 + 故障复盘 — ✅ PARTIAL (stall guard + retrospective done)

Blockers → multi-path cross-verification; cap tool-call rounds (no deadloop);
distinguish transient environment errors vs architecture defects; end-of-run
summary of execution / root cause / optimization points.

**Current state (verified):**
- `max_turns` default **0 = unlimited** in production (`loop_/mod.rs:286,701`); only
  CLI(30)/sub-agent set caps. Main `run_loop` reads the stream with a bare `recv()`,
  **no stall timeout** (`mod.rs:793`).
- LLM HTTP errors: `HttpClient` retries 3× exp backoff, classifies retryable
  (429/5xx/timeout) vs not (4xx) (`llm/http.rs:442-558`).
- Tool errors feed back to the LLM, never abort (`mod.rs:968-1052`).
- No in-loop retrospective — loop ends with `AgentEvent::Done { final_text }`.
- Proactivity L0–L3 escalation exists (web-research at L2, divergence checklist at L3).

**Plan:**
- [x] Main-stream stall timeout: stream `recv()` wrapped in a 120s per-event stall
      guard (mirrors sub-agent guard, `mod.rs` main stream) → emits `Error` and
      returns `EverEvoError::Agent` instead of hanging forever. **DONE 2026-08-10**
- [x] End-of-loop retrospective: after `Done`, emit `AgentEvent::Retrospective`
      (turns, tool calls, failures classified transient vs structural via
      `classify_failure`, optimization notes). Built from already-tracked loop
      stats — no extra LLM call. SSE maps to `event("retrospective")`.
      **DONE 2026-08-10** — verified `cargo check` + `cargo test -p everevo-agent --lib` (210 passed)
- [ ] Production loop cap: user chose **不设上限** (no hard cap) — skip `EVEREVO_MAX_TURNS`
      default; stall timeout is the deadlock guard. (User decision 2026-08-10)
- [ ] Surface HTTP retry classification on final failure: when the loop hits an
      LLM error after retries, tag it transient vs permanent in the `AgentEvent::Error`.
      (`classify_failure` helper now exists in `loop_/mod.rs` — reuse for the Error path.)
- verify: `cargo test -p everevo-agent --lib` green (210 passed); SSE retrospective
  event observed in a live chat stream

---

## 3. 工具问题修复 — ✅ DONE

Fix code_map tool warnings; open codesearch to full-scope read-only source search,
remove access interception.

**Current state (verified):**
- Production: `code_search` = MCP plugin (`plugins/tools/code_search`, stateless rg,
  no interception); `code_map` = in-process `CodeMapTool` (`tools.rs:424-427`) because
  the plugin exposes no code_map.
- In-process `CodeSearchTool` (fallback only) emits `tracing::warn!` on every failed
  background index / auto-reindex (`code_search.rs:83,89,147,481`) — a persistent
  index failure would spam every 10s poll.
- `code_map` execute returns `is_error` only on `read_dir` failure.

**Plan:**
- [x] Inspect `plugins/tools/code_search/src/main.rs`: exposes ONLY `code_search`
      (stateless rg/grep/findstr/walk, `path` arg free → already full-scope
      read-only, no interception). `code_map` has no plugin — always in-process.
- [x] In-process `CodeSearchTool`: added exponential reindex-failure backoff
      (1min → 10min cap, `auto_reindex_backoff_secs`) so a broken index stops
      re-attempting/logging every 10s poll. `tracing::warn!` now fires once per
      backoff window with `backoff_secs`. Reset on success. Unit test added.
- [x] Codesearch access interception closed: in-process `CodeSearchTool` AND
      `CodeMapTool` were scoped to `session_work_dir` (isolated `data/sandbox/{id}/work`),
      so project-path queries failed with read_dir errors. Now scoped to
      `project_root` (full read-only source tree) in `orchestration/tools.rs`.
      Plugin code_search confirmed read-only + full-scope already.
- verify: `cargo test -p everevo-agent --lib code_search` (18 passed); clippy clean

---

## 4. 自主能力优化 — ✅ DONE

Reduce hard prompt constraints; let the agent self-schedule tools/retrieval/retry;
keep only safety + fact-verification bottom lines.

**Current state (verified):**
- SYSTEM_PROMPT "Tool Rules (MUST FOLLOW)": shell = LAST RESORT, read via read_file
  etc. (`context.rs:709-723`); "2-failure limit: stop after 2 failures" (`:758-760`).
- `stages/best_practices.rs` (active stage, p2): verification + planning mandates.
- The elaborate `src/best_practices.rs` (Anti-Fixation, plan-mode workflow) is
  **orphaned** — not in lib.rs, not registered (dead code).

**Plan:**
- [x] Soften "Tool Rules (MUST FOLLOW)" → "Tool Preferences" (guidance: prefer
      specialized tools, judgment to fall back to shell). Table column reframed
      ✅→Prefer, ❌→Shell fallback. (`context.rs` SYSTEM_PROMPT)
- [x] 2-failure hard STOP → anti-fixation guidance ("when a loop forms, stop and
      reconsider"), same intent, softer framing — in `context.rs`, in-process
      `stages/best_practices.rs`, and plugin `stages/best_practices/src/main.rs`.
- [x] Fact-verification bottom lines kept verbatim: "Verify before claiming done.
      Fix code, never weaken tests." + "我做了X = report, verify don't redo."
      (`context.rs:770`, Critical Rules).
- [x] Orphaned `src/best_practices.rs` deleted (user decision: delete) — never
      module-declared, superseded by `stages/best_practices.rs`; its content was
      MORE prescriptive and pulled against autonomy.
- verify: `cargo check -p everevo-core -p everevo-agent` clean; `cargo test -p
      everevo-agent --lib` (211 passed)

---

## 5. 沙箱安全整改 — ✅ DONE (中度: 写需审批，读放行)

Sandbox → independent dedicated isolation dir, not sharing program startup dir;
code/scripts/temp isolated; read/write project source requires approval.

**User decision (2026-08-10):** 中度 — project-source WRITES require approval at
SemiAuto (Confirm), READS auto-allowed; FullyAuto unchanged (GAIA benchmark
unaffected). Baseline state for the medium tier: dedicated isolation already
exists — `sandbox_root = data/sandbox/{session_id}`, default `work_dir =
data/sandbox/{id}/work`; denylist already hard-blocks writes to `crates/kernel/**`,
Cargo.toml/lock, `.git/**`, etc.

**Plan:**
- [x] Close the safe-pattern auto-approve gap: `cp`/`mv`/`mkdir`/`touch`/`echo`/
      `cat` in `safe_patterns` silently passed at SemiAuto even when writing to a
      trusted (workspace/project) path. Added `command_writes_to_any(command,
      paths)` helper — detects shell redirect targets (`>`, `>>`, `&>`) via regex
      and unambiguous mutating first-token commands (`cp mv rm mkdir touch dd tee
      install truncate ln chmod chown chattr shred unlink vi vim nano ed write
      mktemp`) — in `permission/rules.rs`.
- [x] SemiAuto branch of `check_permission`: `writes_trusted_path` → `Confirm`
      ("项目源码写入需审批") evaluated BEFORE the `is_safe && !is_dangerous`
      auto-allow, so workspace writes no longer bypass. Reads stay auto-allowed.
- [x] FullyAuto branch untouched — never evaluates `writes_trusted_path`, so
      unattended GAIA runs keep full auto approval (admin patterns still confirm).
- [x] Unit tests (46 lib + 10 integration pass): cp-to-workspace → Confirm;
      `echo > workspace` redirect → Confirm; `ls` workspace read → Allow;
      FullyAuto write → Allow; no trusted path bound → sandbox-relative writes
      still Allow. Note: `cat` on any path already Confirms at SemiAuto as a
      pre-existing dangerous pattern (unrelated to this gate).
- verify: `cargo test -p everevo-sandbox` (46 lib + 10 integration, green)

**Out of scope (flag to user):** the `write_file` tool has NO approval gate —
it bypasses `check_permission` entirely, so writing project source via write_file
is still ungated at SemiAuto (only kernel-protection blocks `crates/kernel/**`).
Could be a follow-up: route write_file through the same Confirm gate.

---

## 6. 分层记忆架构 — ✅ DONE (two-tier: session working memory + global long-term)

Single-session memory strictly isolated; cross-session long-term memory injected
as on-demand semantic fragments (no full load).

**Design (decided):** two-tier model — every fact carries a `session` tag in
frontmatter: `None` (legacy) or `"global"` = cross-session long-term memory;
`Some(uuid)` = that session's working memory, strictly isolated. Recall sees
only `global` + own-session facts. Promotion to long-term memory is the explicit
`scope: "global"` on `memory add`. Cross-session injection stays on-demand
(top-5 hybrid RRF + KG 1-hop + top-3 paradigms) — never a full corpus load.

**Current state (verified):**
- Cross-session recall was ALREADY on-demand top-5 fragments + KG 1-hop + top-3
  paradigms — never full corpus. ✅ matches MemGPT intent.
- Session history is per-session (DB). ✅
- GAP (now closed): recall/writes were globally shared — `find_relevant` ignored
  `session_id`; writes went to the global FactManager, so session A's facts were
  visible to session B.

**Plan:**
- [x] `MemoryFact.session: Option<String>` added (`everevo-core`, `#[serde(default)]`
      — backward-compatible). Frontmatter serialize/parse round-trips it.
- [x] `fact_visible_to(fact, session_id)` helper in `FactManager` — None/"global"
      visible to all; `Some(uuid)` visible only to its own session.
- [x] `MemoryStage` binds `session_id`; `find_relevant` + T1 bootstrap + persistent-
      index injection all session-filtered (index built from visible facts instead
      of reading the global MEMORY.md). Orphaned `FactManager::read_index_lean`
      removed (its only caller was replaced).
- [x] `memory` tool: `with_session_id`; `add` defaults to session-scoped, with a
      `scope` param (`"session"`/`"global"`) as the explicit promotion bridge.
      `search` (FTS + linear scan) session-filtered too.
- [x] Background writers tiered deliberately: meta-diagnostics, session handoff
      summaries, paradigms, reflection lessons, DEEP themes, workflow recipes,
      domain docs → `"global"` (cross-session reusable). Turn-extractor facts →
      session-scoped (the session's own working memory), session_id threaded
      through `spawn_post_turn_tasks` → `extract_from_turn`.
- [x] Sub-agent T1 injection (handler.rs) session-filtered.
- [x] Tests: `fact_visible_to` scoping, session-tag frontmatter roundtrip.
- verify: `cargo test -p everevo-agent --lib` (213 passed, incl. memory); clippy clean

---

## 7. 全局基线 — ✅ DONE (all four covered)

Time-sensitive/factual info verified against authoritative web sources; full
operation logs retained; loop caps to prevent timeout deadlocks; unified
retrospective at task end.

- [x] **Authoritative verification**: SYSTEM_PROMPT Critical Rules gained an
      explicit bottom line — time-sensitive/factual claims (dates, versions,
      APIs, current events) verified via `web_search`/`web_fetch` against
      authoritative sources before claiming done (`context.rs`).
      verify: `cargo check -p everevo-core`
- [x] **Loop caps** → item 2: production `max_turns` default = unlimited (user
      decision 不设上限); the 120s per-event stall guard is the timeout-deadlock
      guard. ✅
- [x] **Operation logs** → telemetry injection pipeline records agent turns +
      retrieval (`data/telemetry/metrics.db`), wired into both main-session and
      auto-continue loops (`handler.rs:587,755`). Tool-call telemetry stage left
      optional (task doc note).
- [x] **Unified retrospective** → item 2: `AgentEvent::Retrospective` emitted
      before `Done` with turns / tool-call counts / failure classification /
      optimization notes; SSE `event("retrospective")`. ✅
