# Microkernel + Plugin Architecture — Implementation Record

## Design Principle

**Kernel never breaks. Agent can always self-repair.**

The kernel contains bootstrap tools (shell, read_file, write_file, plugin management)
that are compiled into the kernel binary and CANNOT be removed. Even if the agent
breaks all 22 plugins, these 5 bootstrap tools remain available — the agent can
always git checkout, recompile, and roll back any broken plugin.

## Architecture Overview

```
┌──────────────────────────────────────────────────────────────────┐
│                     everevo-kernel (不可变)                       │
│                                                                   │
│  ┌─ 核心循环 ─────────────────────────────────────────────────┐  │
│  │ AgentLoop, ContextPipeline, ProactivityState, MetaAgentState│  │
│  └────────────────────────────────────────────────────────────┘  │
│                                                                   │
│  ┌─ 插件管理 ─────────────────────────────────────────────────┐  │
│  │ PluginRegistry, VersionStore, ProcessPool, CanaryRouter     │  │
│  └────────────────────────────────────────────────────────────┘  │
│                                                                   │
│  ┌─ 自修复工具 (编译进内核, 不可移除) ─────────────────────────┐  │
│  │ BootstrapShell, BootstrapReadFile, BootstrapWriteFile,       │  │
│  │ PluginStatus, PluginRollback                                │  │
│  └────────────────────────────────────────────────────────────┘  │
│                                                                   │
│  ┌─ 基础设施 ─────────────────────────────────────────────────┐  │
│  │ ApiError, Telemetry, McpClient, Tool trait, ContextStage     │  │
│  └────────────────────────────────────────────────────────────┘  │
│                                                                   │
│  测试覆盖率: ≥95%  |  发布周期: 月级  |  变更需要 ADR             │
└──────────────────────────────────────────────────────────────────┘
         │
         │  MCP stdio (子进程) 或 MCP HTTP (远程)
         ▼
┌──────────────────────────────────────────────────────────────────┐
│                     Plugin Fleet (Agent 可修改)                    │
│                                                                    │
│  tools/                  stages/                  hooks/          │
│  ├── web_search/         ├── best_practices/      ├── audit/     │
│  ├── web_fetch/          ├── persona/             ├── review/    │
│  ├── memory/             ├── skill/               └── reflect/   │
│  ├── task/ (sub-agent)   ├── memory_stage/                       │
│  ├── code_search/        └── domain/                             │
│  ├── list_dir/                                                   │
│  ├── ... (其余 16 个)                                             │
│  └── registry.toml                                               │
│                                                                    │
│  外部 MCP Server (同协议):                                        │
│  └── github-mcp, postgres-mcp, ...                               │
└──────────────────────────────────────────────────────────────────┘
```

## Bootstrap Tools (Kernel-Built, Never Removable)

These 5 tools are compiled into the kernel binary. They cannot be modified or
removed by the agent. They provide the minimum capability set for self-repair.

| Tool | Purpose | Why in kernel |
|------|---------|---------------|
| `shell` | Execute commands (git, cargo) | Needed to compile plugins |
| `read_file` | Read plugin source code | Needed to understand what broke |
| `write_file` | Write fixed plugin source code | Needed to apply fixes |
| `plugin_status` | Check plugin health/metrics | Needed to diagnose failures |
| `plugin_rollback` | Emergency rollback any plugin | Needed to recover from bad upgrades |

## Directory Layout After Migration

```
EverEvo-Rust/
├── crates/
│   ├── everevo-core/              ← 共享类型 (不变)
│   ├── everevo-kernel/            ← [NEW] 微内核 (从 everevo-agent + server 提取)
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── agent/             ← AgentLoop + 循环
│   │   │   ├── context/           ← ContextPipeline + ContextStage trait
│   │   │   ├── tool/              ← Tool trait + ToolRegistry + ToolHook
│   │   │   ├── plugin/            ← PluginRegistry + VersionStore + Pool + Canary
│   │   │   ├── bootstrap/         ← 自修复工具 (shell/read/write/status/rollback)
│   │   │   ├── error.rs           ← ApiError + ErrorCode
│   │   │   ├── telemetry.rs       ← Telemetry
│   │   │   └── session.rs         ← SessionCoordinator
│   │   └── tests/
│   │
│   ├── everevo-agent/             ← 精简: 只剩 LLM client + subagent + 非内核 stage
│   ├── everevo-server/            ← 精简: 只剩 HTTP routes + SSE + orchestration
│   ├── everevo-mcp/               ← MCP client (已有, 不变)
│   ├── everevo-mcp-protocol/      ← [NEW] MCP 协议类型 (从 everevo-mcp 提取)
│   ├── everevo-db/                ← 不变
│   ├── everevo-sandbox/           ← 不变
│   ├── everevo-vector/            ← 不变
│   ├── everevo-knowledge/         ← 不变
│   ├── everevo-workflow/          ← 不变
│   └── ... (其余 crate 不变)
│
├── plugins/                       ← [NEW] 外挂舰队
│   ├── Cargo.toml                 ← workspace members
│   ├── tools/
│   │   ├── web_search/            ← 从 tools/builtins/web_search/ 迁移
│   │   │   ├── Cargo.toml
│   │   │   ├── src/main.rs        ← MCP server (~50行)
│   │   │   └── src/engine.rs      ← 原 search engine 逻辑
│   │   ├── web_fetch/
│   │   ├── memory/
│   │   ├── task/
│   │   ├── code_search/
│   │   ├── ... (所有 17 个非自修复 tool)
│   │   └── registry.toml
│   ├── stages/
│   │   ├── best_practices/
│   │   ├── persona/
│   │   ├── skill/
│   │   ├── memory_stage/
│   │   ├── domain/
│   │   └── registry.toml
│   └── hooks/
│       ├── audit/
│       ├── review_gate/
│       ├── reflect_gate/
│       └── registry.toml
│
├── data/plugins/                  ← 运行时版本存储 (自动管理)
│   ├── web_search/
│   │   ├── versions/v1.0.0/plugin.exe + checksum.sha256
│   │   ├── stable → versions/v1.0.0
│   │   └── canary → versions/v1.0.1 (可选)
│   └── ...
│
└── frontend/                      ← 不变
```

## Step-by-Step Implementation

### Step 0: Create mcp-protocol crate (extract shared types)

**Goal**: Extract MCP protocol types to a lightweight crate that both kernel
and plugins can depend on without pulling in tokio/reqwest.

**Why**: Currently `everevo-mcp` depends on tokio + reqwest. Plugins only need
the JSON-RPC types (ToolDef, CallToolResult, etc.) — not the async client.

**Actions**:
1. Create `crates/everevo-mcp-protocol/Cargo.toml`:
   ```toml
   [package]
   name = "everevo-mcp-protocol"
   version.workspace = true
   edition.workspace = true

   [dependencies]
   serde.workspace = true
   serde_json.workspace = true
   ```

2. Move types from `everevo-mcp/src/protocol.rs` to `everevo-mcp-protocol/src/lib.rs`:
   - `ToolDef`, `CallToolParams`, `CallToolResult`, `ContentBlock`
   - `InitializeParams`, `InitializeResult`, `ServerInfo`, `ClientCapabilities`
   - `ListToolsResult`, `Request`, `Response`, `Notification`
   - `ResourceDef`, `PromptDef`, `ReadResourceResult`, `GetPromptResult`

3. Update `everevo-mcp/Cargo.toml`:
   ```toml
   [dependencies]
   everevo-mcp-protocol = { path = "../everevo-mcp-protocol" }
   ```

4. Update `everevo-mcp/src/protocol.rs` to `pub use everevo_mcp_protocol::*;`

5. Plugins depend ONLY on `everevo-mcp-protocol` (zero heavy deps).

**Verify**: `cargo build -p everevo-mcp-protocol -p everevo-mcp`

**Lines**: ~50 moved, ~20 new

---

### Step 1: Create everevo-kernel crate

**Goal**: Create the kernel crate skeleton. Move kernel-level files from
everevo-agent and everevo-core. Kernel depends only on everevo-core + everevo-mcp
(not on everevo-server).

**Actions**:
1. Create `crates/everevo-kernel/Cargo.toml`:
   ```toml
   [package]
   name = "everevo-kernel"
   version.workspace = true
   edition.workspace = true

   [dependencies]
   everevo-core = { path = "../everevo-core" }
   everevo-mcp = { path = "../everevo-mcp" }
   everevo-db = { path = "../everevo-db" }
   everevo-knowledge = { path = "../everevo-knowledge" }
   tokio.workspace = true
   serde.workspace = true
   serde_json.workspace = true
   uuid.workspace = true
   chrono.workspace = true
   tracing.workspace = true
   ```

2. Create `crates/everevo-kernel/src/lib.rs`:
   ```rust
   pub mod agent;
   pub mod bootstrap;
   pub mod context;
   pub mod error;
   pub mod plugin;
   pub mod session;
   pub mod telemetry;
   pub mod tool;

   // Re-export for server access
   pub use agent::AgentLoop;
   pub use agent::ProactivityState;
   pub use agent::MetaAgentState;
   pub use context::{ContextPipeline, ContextStage, ContextBuildContext};
   pub use plugin::{PluginRegistry, VersionStore, ProcessPool, CanaryRouter};
   pub use session::SessionCoordinator;
   pub use tool::{Tool, ToolRegistry, ToolHook, ToolOutput};
   ```

3. Move files from everevo-agent to kernel (文件移动, 不改代码):
   - `everevo-agent/src/loop_/mod.rs` → `everevo-kernel/src/agent/mod.rs`
   - `everevo-agent/src/loop_/event.rs` → `everevo-kernel/src/agent/event.rs`
   - `everevo-agent/src/loop_/hooks.rs` → `everevo-kernel/src/agent/hooks.rs`
   - `everevo-agent/src/loop_/trim.rs` → `everevo-kernel/src/agent/trim.rs`
   - `everevo-agent/src/memory/meta_agent.rs` → `everevo-kernel/src/agent/meta.rs`

4. Move files from everevo-core to kernel (文件移动, 不改代码):
   - `everevo-core/src/context.rs` → `everevo-kernel/src/context/mod.rs`
   - `everevo-core/src/tool.rs` → `everevo-kernel/src/tool/mod.rs`
   - `everevo-core/src/error.rs` → `everevo-kernel/src/error.rs`
   - `everevo-core/src/telemetry.rs` → `everevo-kernel/src/telemetry.rs`
   - `everevo-core/src/memory.rs` → keep in core (shared types), add `pub use` in kernel

5. Update everevo-agent/Cargo.toml to depend on everevo-kernel:
   ```toml
   [dependencies]
   everevo-kernel = { path = "../everevo-kernel" }
   ```
   And `pub use everevo_kernel::*;` in agent's lib.rs to maintain backward compat.

6. Update everevo-server/Cargo.toml similarly.

7. For everevo-core: keep `memory.rs`, `llm.rs`, `types.rs`, `config.rs` —
   these are shared types needed by all crates. Kernel re-exports what it uses.

**Verify**: `cargo check --workspace` — all existing code still compiles

**Lines**: ~200 (Cargo.toml + lib.rs), all existing code moved (not changed)

---

### Step 2: Bootstrap Tools (kernel-built, never removable)

**Goal**: Implement the 5 self-repair tools that are compiled into the kernel.
These guarantee the agent can always recover from broken plugins.

**Actions**:

1. Create `crates/everevo-kernel/src/bootstrap/mod.rs`:
   ```rust
   //! Bootstrap tools — compiled into kernel, never removable.
   //! These guarantee self-repair capability even if all plugins are broken.

   pub mod shell;
   pub mod read_file;
   pub mod write_file;
   pub mod plugin_status;
   pub mod plugin_rollback;

   use everevo_core::tool::ToolRegistry;
   use std::sync::Arc;

   /// Register all bootstrap tools into a ToolRegistry.
   /// Called once at kernel init — these cannot be removed.
   pub fn register_all(registry: &mut ToolRegistry) {
       registry.register(Arc::new(shell::BootstrapShell::default()));
       registry.register(Arc::new(read_file::BootstrapReadFile::default()));
       registry.register(Arc::new(write_file::BootstrapWriteFile::default()));
       registry.register(Arc::new(plugin_status::PluginStatus::default()));
       registry.register(Arc::new(plugin_rollback::PluginRollback::default()));
   }
   ```

2. Create `bootstrap/shell.rs` — copy from `everevo-agent/src/tools/builtins/shell.rs`
   but simplified to only what's needed for self-repair:
   - Execute git commands (checkout, status, log)
   - Execute cargo commands (build, check)
   - Execute filesystem commands (cp, mv, ls)
   - No sandbox — uses real filesystem (kernel privilege)
   - Marked as RiskLevel::High, always requires confirmation

3. Create `bootstrap/read_file.rs` — copy from existing `builtins/read_file.rs`

4. Create `bootstrap/write_file.rs` — copy from existing `builtins/write_file.rs`

5. Create `bootstrap/plugin_status.rs`:
   ```rust
   /// Check plugin health, metrics, version info.
   /// Parameters: { "plugin_id"?: string } — omit for all plugins
   /// Returns: JSON with status, metrics, active version for each plugin
   pub struct PluginStatus { registry: Arc<PluginRegistry> }
   ```

6. Create `bootstrap/plugin_rollback.rs`:
   ```rust
   /// Emergency rollback any plugin to its last stable version.
   /// Parameters: { "plugin_id": string }
   /// Kills canary processes, resets canary_pct to 0, keeps stable unchanged.
   pub struct PluginRollback { registry: Arc<PluginRegistry> }
   ```

**Verify**: `cargo test -p everevo-kernel --lib bootstrap`

**Lines**: ~500 (mostly copied from existing tools + 2 new tools)

---

### Step 3: PluginRegistry + VersionStore

**Goal**: Filesystem-based plugin version management. No database needed.

**Actions**:

1. Create `crates/everevo-kernel/src/plugin/mod.rs`:
   ```rust
   pub mod registry;
   pub mod version;
   pub mod pool;
   pub mod canary;

   pub use registry::PluginRegistry;
   pub use version::VersionStore;
   pub use pool::ProcessPool;
   pub use canary::CanaryRouter;
   ```

2. Create `plugin/version.rs` — VersionStore (~120 lines):
   - `open(dir)` — initialize from filesystem
   - `exe_path(plugin_id, version)` — get binary path
   - `resolve(plugin_id, session_id)` — deterministic version routing
   - `stage(plugin_id, version, exe_path)` — register new version
   - `set_canary(plugin_id, version, pct)` — start canary
   - `promote(plugin_id)` — canary → stable
   - `rollback(plugin_id)` — remove canary, keep stable
   - `record_call(plugin_id, version, success, latency)` — update metrics
   - `save()` / `load()` — registry.toml persistence
   - `cleanup(keep_stable)` — remove old unused versions

3. Create `plugin/pool.rs` — ProcessPool (~80 lines):
   - `acquire(plugin_id, version, exe_path)` → `Arc<Mutex<McpClient>>`
   - `release(plugin_id, version, client)` → return to idle queue
   - `health_check()` → ping idle clients, kill stale ones
   - Uses existing `McpClient::connect_stdio()` for spawn + handshake
   - Uses existing `McpClient::ping()` for liveness check

4. Create `plugin/canary.rs` — CanaryRouter (~100 lines):
   - `evaluate(plugin_id)` → `PromoteDecision`
   - Decision logic:
     - success_rate drop > 5% OR crash_count > 3/10min → ROLLBACK
     - success_rate >= stable AND p50 latency <= 1.1x stable → PROMOTE
     - otherwise → KEEP_OBSERVING
   - Requires minimum 100 samples + 30 minute observation window

5. Create `plugin/registry.rs` — PluginRegistry (~80 lines):
   - Coordinates VersionStore + ProcessPool + CanaryRouter
   - `register_plugin(plugin_id, exe_path)` → discover tools via MCP → ToolRegistry
   - `get_tools(plugin_id, session_id)` → resolve version → get or spawn client → McpTool::from_defs()
   - `evaluate_all()` → run CanaryRouter on all plugins with active canaries
   - Background task: every 60s run evaluate_all + health_check

**Verify**: `cargo test -p everevo-kernel --lib plugin`

**Lines**: ~380 new

---

### Step 4: Migrate first plugin (web_search as MCP server)

**Goal**: Prove the architecture by migrating one existing tool to the plugin model.
Choose web_search because it has no filesystem dependencies (self-contained).

**Actions**:

1. Create `plugins/Cargo.toml`:
   ```toml
   [workspace]
   members = ["tools/*"]
   resolver = "2"

   [workspace.dependencies]
   serde = "1"
   serde_json = "1"
   everevo-mcp-protocol = { path = "../crates/everevo-mcp-protocol" }
   ```

2. Create `plugins/tools/web_search/Cargo.toml`:
   ```toml
   [package]
   name = "plugin-web-search"
   version = "1.0.0"
   edition.workspace = true

   [[bin]]
   name = "plugin-web-search"

   [dependencies]
   serde.workspace = true
   serde_json.workspace = true
   everevo-mcp-protocol.workspace = true
   reqwest = { version = "0.12", default-features = false, features = ["rustls-tls"] }
   ```

3. Create `plugins/tools/web_search/src/main.rs` (~50行 MCP server 框架)

4. Create `plugins/tools/web_search/src/engine.rs`:
   - 复制 `everevo-agent/src/tools/builtins/web_search/engine.rs` 的核心搜索逻辑
   - 去掉 `Tool` trait impl，改为普通函数
   - 输入: `serde_json::Value` (params), 输出: `Result<String, String>`

5. Register in kernel: during AgentLoop init, call
   `registry.register_plugin("web_search", "plugins/tools/web_search/...")` which
   spawns the binary, does MCP handshake, discovers tools, and wraps them via
   `McpTool::from_defs()`.

**Verify**:
- `cargo build -p plugin-web-search` → produces `plugin-web-search.exe`
- Kernel spawns it → MCP initialize → tools/list → tools/call
- Existing web_search integration tests pass

**Lines**: ~80 new (main.rs + engine.rs adaptation)

---

### Step 5: Migrate remaining plugins (batch)

**Goal**: Migrate all non-bootstrap tools, stages, and hooks to plugins.

**Pattern for each plugin**:
1. Create `plugins/{type}/{name}/Cargo.toml` (独立 crate)
2. Create `plugins/{type}/{name}/src/main.rs` (MCP server ~50行)
3. Copy core logic to `plugins/{type}/{name}/src/logic.rs` (原 tool execute 函数)
4. Remove original file from everevo-agent
5. Register in kernel PluginRegistry

**Plugins to migrate**:

| # | Type | Name | Source | Risk |
|---|------|------|--------|------|
| 1 | tool | web_fetch | builtins/web_fetch | Low |
| 2 | tool | memory | builtins/memory_tool | Low |
| 3 | tool | task | builtins/delegate/spawn | Medium (depends on AgentLoop) |
| 4 | tool | code_search | builtins/code_search | Low |
| 5 | tool | code_map | builtins/code_search | Low |
| 6 | tool | list_dir | builtins/list_dir | Low |
| 7 | tool | write_file | builtins/write_file | Low (kernel has bootstrap version) |
| 8 | tool | web_search | builtins/web_search | (done in Step 4) |
| 9 | tool | todo_write | builtins | Low |
| 10 | tool | plan_mode | builtins/plan_mode | Low |
| 11 | tool | verify | builtins/verify | Low |
| 12 | tool | compact | builtins/compact | Low |
| 13 | tool | skill | builtins/skill | Low |
| 14 | tool | cluster | builtins/cluster | Medium |
| 15 | tool | team | builtins/team | Medium |
| 16 | tool | workflow | builtins/workflow | Medium |
| 17 | tool | workflow_runner | builtins/workflow_runner | Low |
| 18 | stage | best_practices | stages/best_practices | Low |
| 19 | stage | persona | stages/persona | Low |
| 20 | stage | skill | stages/skill | Low |
| 21 | stage | memory_stage | stages/memory | Low |
| 22 | stage | domain | stages/domain_stage | Low |
| 23 | hook | audit | tools/audit_hook | Low |
| 24 | hook | review_gate | tools/review_gate | Low |
| 25 | hook | reflect_gate | tools/reflect_gate | Low |

**Verify after each batch**: `cargo test --workspace`

**Lines**: ~50 × 25 = ~1250 new (mostly MCP server boilerplate)

---

### Step 6: Agent self-modification safety pipeline

**Goal**: Enable the agent to modify plugins, compile them, and deploy them
with automatic safety checks and rollback.

**Actions**:

1. Add `BootstrapShell` sandbox mode for plugin compilation:
   - Compilation runs in a separate process with timeout (120s)
   - Network access blocked during compilation (env: `CARGO_NET_OFFLINE=1`)
   - Only allowed to write to `target/` and `plugins/`
   - stdout/stderr captured for error reporting

2. Create compile-and-stage pipeline (kernel function, called by agent):
   ```rust
   // everevo-kernel/src/plugin/build.rs
   pub async fn compile_and_stage(
       plugin_id: &str,
       new_version: &str,
       source_dir: &Path,
   ) -> Result<StageResult, BuildError> {
       // 1. Verify source changed (diff against last compiled version)
       // 2. cargo build -p plugin-{id} --release (in sandbox, 120s timeout)
       // 3. On failure: capture stderr → return BuildError with details
       // 4. Verify binary: SHA256 checksum
       // 5. Stage: copy to versions/{version}/plugin.exe + checksum.sha256
       // 6. git tag the source version for audit trail
       // 7. Return success with binary path
   }
   ```

3. Create automatic rollback trigger (kernel background task):
   ```rust
   // Runs every 60 seconds
   pub async fn plugin_safety_loop(registry: Arc<PluginRegistry>) {
       loop {
           for plugin_id in registry.active_plugins() {
               match registry.evaluate_canary(plugin_id).await {
                   PromoteDecision::Rollback => {
                       tracing::error!(plugin_id, "Auto-rollback triggered");
                       registry.rollback(plugin_id);
                       // Notify all active sessions about the rollback
                   }
                   PromoteDecision::Promote => {
                       tracing::info!(plugin_id, "Auto-promote canary → stable");
                       registry.promote(plugin_id);
                   }
                   _ => {}
               }
           }
           tokio::time::sleep(Duration::from_secs(60)).await;
       }
   }
   ```

4. git integration for audit trail:
   - Each plugin's `src/` is a git repo
   - Agent changes → git diff stored in audit log
   - Compile → git tag `v{version}` at the compiled commit
   - Rollback → git checkout the stable version tag
   - `plugin_status` shows git log for the plugin

**Verify**: Agent modifies web_search plugin → compile fails → auto-revert → verify old version still works

**Lines**: ~200

---

### Step 7: Integration + regression test suite

**Goal**: Ensure existing functionality is preserved after migration.

**Actions**:

1. Move all existing tests from `everevo-agent/src/tools/builtins/` to
   `plugins/tools/{name}/tests/`

2. Add kernel integration tests:
   - `test_bootstrap_tools_always_available` — even with 0 plugins, bootstrap works
   - `test_plugin_spawn_and_call` — spawn plugin, MCP handshake, call tool
   - `test_plugin_pool_reuse` — acquire, release, acquire again (should reuse)
   - `test_canary_routing` — 50% canary, 100 sessions, verify distribution
   - `test_auto_rollback_on_degradation` — inject failures, verify rollback
   - `test_agent_self_repair_scenario` — break plugin → bootstrap shell → git checkout → rebuild → verify fixed

3. Run full test suite:
   ```bash
   cargo check --workspace && cargo clippy --workspace -- -D warnings
   cargo test --workspace  # all 353+ tests must still pass
   ```

**Lines**: ~300 test code

---

### Step 8: Documentation + agent self-evolution guide

**Goal**: Document the plugin system so both humans and the agent can use it.

**Actions**:

1. Create `plugins/README.md` — plugin development guide (for humans)
2. Create `plugins/AGENTS.md` — plugin development guide (for the agent, injected via context)
3. Add `plugin_status` tool output format documentation
4. Add self-repair runbook: "If plugin X is broken, do Y"

---

## Verification Pipeline

After each step:
```bash
cargo check --workspace && cargo test --workspace
```

After all steps:
```bash
cargo check --workspace
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

Final acceptance criteria:
- [ ] All 353+ existing tests pass
- [ ] Bootstrap tools work with 0 plugins loaded
- [ ] Single plugin (web_search) works via MCP stdio
- [ ] Plugin pool reuses idle processes
- [ ] Canary routing distributes traffic correctly
- [ ] Auto-rollback triggers on simulated degradation
- [ ] Agent can modify, compile, and stage a new plugin version
- [ ] Agent can rollback a broken plugin via bootstrap tool
- [ ] Kernel code unchanged from Step 1 baseline (except additions)

---

## Risk Mitigation

| Risk | Mitigation |
|------|-----------|
| Agent breaks all plugins including bootstrap | Bootstrap tools are compiled INTO the kernel binary — cannot be removed |
| Plugin compilation hangs | 120s timeout + process kill in build sandbox |
| Windows binary lock prevents upgrade | drain → kill → replace → restart pattern |
| Pipe deadlock between kernel and plugin | Request-response pattern (no pipelining) + 30s call timeout |
| Too many plugin versions consume disk | Auto-cleanup: keep last 2 stable + all canaries, delete >30 day versions |
| MCP protocol version mismatch | Kernel pins protocol version; plugins declare supported version in plugin.toml |
| Kernel crash leaves orphan plugin processes | stdin EOF → plugins self-terminate; kernel startup scans and kills orphans |

---

## Summary

| Metric | Value |
|--------|-------|
| New crates | 3 (everevo-kernel, everevo-mcp-protocol, plugins workspace) |
| New kernel code | ~1,200 lines (PluginRegistry + Bootstrap + Canary) |
| Modified code | ~500 lines (文件移动 + re-export) |
| Deleted code | ~0 lines (all logic preserved, just moved) |
| Plugin code | ~1,250 lines (25 plugins × 50 line MCP server boilerplate) |
| Bootstrap tools | 5 tools, always available, kernel-compiled |
| External MCP compatibility | 100% (same McpClient code path) |
| Total estimated time | 6.5 days |
