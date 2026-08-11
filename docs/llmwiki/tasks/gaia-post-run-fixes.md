# GAIA L1 Post-Run Fixes — Official scorer + attachments + search-empty + answer discipline + sandbox pattern

## Goal

Land the post-run fixes identified by the 2026-08-11 full 53q audit, with authoritative grounding (GAIA official type-aware quasi-exact scoring). Scope confirmed by user: P0 + P2 + P3 + sandbox pattern fix; P1 (vision/OCR, headless browser) deferred.

- [x] Phase 1 — Official GAIA scorer + final-answer extraction + `--scoring`/`--self-test`/`--regrade` in `scripts/gaia_bench.py`
  - verify: `--self-test` green; `--regrade <20260811 results> --scoring official` prints corrected count; `--scoring legacy` reproduces 41/53
  - **DONE 2026-08-11:** self-test 20/20; official regrade 27/53 (50.9%); legacy reproduces 41/53. Artifact `official_regrade_20260811_152735.json`.
- [x] Phase 2 — Attachment whitelist `'.pptx','.ppt','.doc','.odt'` + `python-pptx` in sandbox venv
  - verify: whitelist literal contains the four exts; venv has python-pptx
  - **DONE 2026-08-11:** whitelist extended; python-pptx installed into `data/bench/venv`.
- [x] Phase 3 — `web_search_local` empty-vs-failure distinction in `plugins/tools/web_search/src/main.rs`
  - verify: plugin builds; `any_responded` tail returns `Ok("No results found…")` instead of `Err`
  - **DONE 2026-08-11:** `any_responded`/`tried` tracking; release build + clippy clean. Note: running server caches plugins — fresh server for future runs.
- [x] Phase 4 — `AnswerDisciplineStage` (q30/q37/q16) registered in pipeline at priority 2
  - verify: `cargo check --workspace && cargo test -p everevo-agent --lib`
  - **DONE 2026-08-11:** stage added, pipeline-registered; 213 agent lib tests pass.
- [x] Phase 5 — `"at "` → `"^at "` anchor fix in sandbox `patterns.rs:123` + `command_matches_any` anchor support + 3 regression tests
  - verify: `cargo test -p everevo-sandbox`
  - **DONE 2026-08-11:** anchor + `^` support landed; also fixed a real pre-existing SemiAuto ordering bug (external-path Confirm now precedes safe-Allow); 31 lib + 10 integration tests pass.
- [x] Phase 6 — Update `GAIA_L1_REPORT_20260811.md` (official figure + q1/q6/q18 delta + had_marker), `changelog.md`, memory
  - verify: docs consistent with regrade output
  - **DONE 2026-08-11:** report UPDATE block + §8 added; changelog entry appended; memory `gaia-benchmark-state.md` + `MEMORY.md` updated.

**Binding constraint:** do NOT run any benchmark (smoke or full) without notifying the user + explicit confirmation.
