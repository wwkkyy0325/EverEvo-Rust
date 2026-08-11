# Task: Semantic Split of Large Source Files

> **Status: RECORDED / DEFERRED.** Execute only after (a) the GAIA benchmark
> questions are resolved and (b) a full benchmark re-run confirms no problems
> (approaching launch readiness). Do NOT split now — splitting mid-benchmark
> risks regressions and invalidates the current baseline.

User directive (2026-08-11): after the current failing-question subset run,
**record** a plan to semantically split source files over 800 lines. After the
benchmark questions are solved, run a full benchmark again. Once confirmed there
are no problems and the project is approaching launch, split files **over 900
lines**. Splitting must be **semantic/logical, never forced** (模块内聚边界,
不是按行数硬切). Make a careful plan and verify once.

## Inventory (as of 2026-08-11)

### >900 lines (split at launch-readiness)

| Lines | File | Notes |
|------:|------|-------|
| 1986 | `crates/app/everevo-agent/src/loop_/mod.rs` | Agent main loop — highest priority |
| 1222 | `crates/kernel/everevo-core/src/context.rs` | Context pipeline stages |
| 1156 | `crates/app/everevo-server/src/app_state.rs` | Server state, provider resolution |
| 1112 | `crates/infra/everevo-bootstrap/src/pipeline.rs` | First-run provisioning pipeline |
| 997 | `crates/app/everevo-agent/src/llm/http.rs` | HTTP LLM client |
| 993 | `crates/infra/everevo-knowledge/src/graph/graph.rs` | Knowledge graph |
| 924 | `crates/app/everevo-server/src/routes/chat/handler.rs` | Chat SSE handler |
| 907 | `crates/app/everevo-agent/src/memory/engine.rs` | Memory engine |
| 902 | `crates/infra/everevo-bootstrap/src/lib.rs` | Bootstrap crate root |

Also: `plugins/tools/web_search/src/main.rs` (2383 lines, MCP plugin — not under
`crates/`; include if the plugin boundary matters for launch).

### 800–900 lines (recorded; split when they cross 900 or when the enclosing
module is being refactored)

| Lines | File |
|------:|------|
| 872 | `crates/app/everevo-server/src/startup_check.rs` |
| 866 | `crates/infra/everevo-downloader/src/worker.rs` |
| 859 | `crates/app/everevo-agent/src/skill.rs` |

## Constraints (from user, binding)

1. **Semantic/logical splitting only** — split along module cohesion boundaries
   (state, types, helpers, submodules), NOT by forcing a line count.
2. **Make a careful plan first** (this doc, then per-file plans) and **verify
   once** — the workspace must build + test green after each split
   (`cargo fmt --check && cargo clippy --workspace -- -D warnings &&
   cargo test --workspace && cd frontend && npx tsc --noEmit`).
3. Splitting is **deferred** until benchmark questions are resolved and a full
   re-run is clean. Splitting before that invalidates the benchmark baseline.

## Execution checklist (to be done at launch-readiness)

- [ ] Re-inventory files >900 lines (may have shifted).
- [ ] For each file, write a per-file split plan: proposed submodule layout,
      public API surface, what moves where, and the verify check.
- [ ] Split one file at a time, smallest first (skill.rs → engine.rs → … →
      loop_/mod.rs). Verify after each.
- [ ] Full verification once after all splits: fmt, clippy -D warnings,
      workspace tests, frontend tsc + vite build, harness `--self-test`.
- [ ] Update `docs/llmwiki/design.md` + `changelog.md` + `api-registry.md` if
      any public item moved.
