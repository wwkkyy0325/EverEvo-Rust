# 2026-07-19 Codebase Improvements

Tasks derived from the comprehensive codebase analysis.

## 1. File Size Reduction (Split Large Files)

**Context:** Many files exceed 800 lines, making them difficult for LLMs to modify efficiently. Each file should target <500 lines, maximum 800.

**Strategy:** Split by logical concern — each file should have a single clear responsibility. Tests stay with their tested code. `pub(crate)` for internal visibility, `pub` for public API.

### Priority A — Files over 800 lines

- [x] 1.1 Split `crates/everevo-sandbox/src/permission.rs` (1089→~320 avg)
  → `permission/{mod,level,paths,patterns,rules}.rs`
  Verify: ✅ `cargo check` passes

- [x] 1.2 Split `crates/everevo-vector/src/lib.rs` (1084→~150 avg)
  → `vector/{types,embedding,store_trait,memory_store,lancedb_store,persistent,engine}.rs`
  Verify: ✅ `cargo check` passes

- [x] 1.3 Split `crates/everevo-domain/src/lib.rs` (901→~110 avg)
  → `domain/{registry,document,classifier,parser,chunker,retriever,watcher,manager,helpers}.rs`
  Verify: ✅ `cargo check` passes

- [x] 1.4 Split `crates/everevo-telemetry/src/lib.rs` (869→~130 avg)
  → `telemetry/{config,records,trace,writer}.rs`
  Verify: ✅ `cargo check` passes

- [x] 1.5 Split `crates/everevo-kg/src/lib.rs` (771→~180 avg)
  → `kg/{types,resolver,graph,extraction}.rs`
  Verify: ✅ `cargo check` passes

### Priority B — Files 600-800 lines

- [ ] 1.6 Split `crates/everevo-agent/src/memory/engine.rs` (713 lines)
  → `memory/engine/mod.rs`, `memory/engine/phases.rs`
  Verify: `cargo check -p everevo-agent` passes

- [ ] 1.7 Split `crates/everevo-agent/src/orchestration.rs` (703 lines)
  → `orchestration/mod.rs`, `orchestration/task.rs`, `orchestration/subagent.rs`, `orchestration/pool.rs`, `orchestration/supervisor.rs`
  Verify: `cargo check -p everevo-agent` passes

## 2. Architecture Improvements

- [x] 2.1 Fix `everevo-domain` reverse dependency on `everevo-agent`
  **Fix:** Removed `everevo-agent` from domain's Cargo.toml dependencies. Grep confirmed zero usages of `everevo_agent` in domain source — it was a stale dependency.
  Verify: ✅ `cargo check` passes

- [ ] 2.2 Reduce `everevo-agent` "god crate" size
  **Status:** Agent now 11 sub-modules after file splits. Full extraction to separate crate deferred — evaluate after orchestration/engine splits.

## 3. Security Hardening

- [x] 3.1 Add zip path traversal protection in `extract_zip()`
  **Fix:** Added canonicalize-check for each zip entry in `server/src/main.rs:extract_zip()`. Bootstrap runtime.rs already had ZIP Slip defense (verified).
  Verify: ✅ `cargo check` passes

- [x] 3.2 Tighten CORS policy for production
  **Fix:** Replaced `Any` origin with `EVEREVO_CORS_ORIGINS` env var (comma-separated) defaulting to `http://localhost:3000`.
  Verify: ✅ `cargo check` passes

## 4. Technical Debt

- [ ] 4.1 Replace `DummyEmbedder` with `fastembed-rs` integration
  **File:** `crates/everevo-vector/src/lib.rs` — `DummyEmbedder` returns zero vectors, making all semantic search degenerate.
  **Priority:** Phase 2b. Depends on ONNX Runtime bootstrap being complete.

- [ ] 4.2 Implement LLM-based task decomposition in `TaskDecomposer`
  **File:** `crates/everevo-agent/src/orchestration.rs:92-167`
  **Current:** Simple heuristic on Chinese connectors. LLM prompt builder exists but is unused.

- [ ] 4.3 Split `crates/everevo-server/src/main.rs` (512 lines)
  **Issue:** Bootstrap provisioning logic, LLM config loading, and subcommands are mixed in the entry point.
  **How:** Extract `cmd_dev`, `cmd_serve`, `cmd_bootstrap`, `cmd_chat` into `server/commands/` sub-modules.

- [ ] 4.4 Extract LLM provider env-var logic from `everevo-core::config`
  **File:** `crates/everevo-core/src/config.rs:214-253`
  **Issue:** `AppConfig` shouldn't know about specific LLM providers.
  **How:** Move `from_env_anthropic/openai/ollama` to a free function in `everevo-agent::llm` or a separate config helper.

## 5. Phase 2/3 Improvements (2026-07-19)

- [x] 5.1 Add `Agent` trait to `everevo-core`
  **File:** `crates/everevo-core/src/agent.rs` — `Agent` trait + `AgentContext` + `AgentOutput`
  Verify: ✅ `cargo check` passes

- [x] 5.2 Replace `std::sync::Mutex` with `tokio::sync::Mutex` in `MockLlmProvider`
  **File:** `crates/everevo-agent/src/llm.rs` — uses `.lock().await` / `.blocking_lock()`
  Verify: ✅ `cargo check` passes

- [x] 5.3 Add proper error variants and `From` impls
  - Added `Bootstrap` and `Download` variants to `EverEvoError`
  - Fixed `From<BootstrapError>` to use `Bootstrap` variant (was incorrectly using `Config`)
  - Added `From<DownloadError>` to `EverEvoError`
  Verify: ✅ `cargo check` passes

