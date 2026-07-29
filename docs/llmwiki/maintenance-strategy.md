# Project Maintenance & Testing Strategy

> 项目增长后的维护策略：API 管理、测试分层、增量测试、CI 门禁

---

## 1. 测试金字塔 — 改一个地方测多少

```
          ╱  E2E  ╲          cargo test --workspace -- --ignored
         ╱  集成   ╲         cargo test -p <crate>
        ╱   单元   ╲        cargo test -p <crate> --lib
       ╱  编译检查  ╲       cargo check --workspace
      ╱  fmt + lint ╲      cargo fmt --check && cargo clippy
```

### 逐层策略

| 层级 | 命令 | 改什么跑什么 | 频率 |
|------|------|-------------|------|
| **fmt + lint** | `cargo fmt --check && cargo clippy` | 每次保存 | 秒级 |
| **编译检查** | `cargo check --workspace` | 每次改动 | 1-2 秒 |
| **单元测试** | `cargo test -p <changed_crate> --lib` | 改哪个 crate 跑哪个 | 开发中 |
| **集成测试** | `cargo test --workspace` | PR 前 | 1-2 分钟 |
| **E2E 测试** | `cargo test --workspace -- --ignored` | 发布前 | 30 秒+ |

### 增量测试（避免全量）

```bash
# 只改 everevo-agent → 只跑它和依赖它的 crate
cargo test -p everevo-agent -p everevo-server

# 只改前端 → 只跑前端
cd frontend && npx tsc --noEmit && npx vite build

# 只改 everevo-sandbox → 只跑沙箱
cargo test -p everevo-sandbox
```

### 快速验证命令（最常用）

```bash
# 30 秒全量验证
cargo check --workspace && cargo test -p everevo-agent --lib && cd frontend && npx tsc --noEmit

# 预提交检查
cargo fmt --check && cargo clippy --workspace -- -D warnings && cargo test --workspace
```

---

## 2. API 接口维护

### 原则

- **Trait 在 `everevo-core`** — 所有公共接口通过 trait 定义，实现隔离在各 crate
- **`pub(crate)` 优先** — 不暴露内部实现
- **破坏性变更** — 先标记 `#[deprecated]`，给一个版本过渡期

### 需要向后兼容的关键接口

| 接口 | 位置 | 影响面 |
|------|------|--------|
| `Tool` trait | `everevo-core/src/tool.rs` | 所有工具实现 |
| `ContextStage` trait | `everevo-core/src/context.rs` | 所有 pipeline 阶段 |
| `SandboxProvider` trait | `everevo-core/src/sandbox.rs` | 沙箱实现 |
| `LlmMessage` | `everevo-core/src/llm.rs` | LLM 客户端 |
| `AgentEvent` | `everevo-agent/src/loop_.rs` | SSE 流 |
| `MessageItem` | `frontend/src/store.ts` | 前端消息渲染 |

### 变更检查清单

- [ ] 改了 trait → 检查所有 `impl` 实现者
- [ ] 改了 `EverEvoError` → 检查所有 `.into()` 和 `?` 传播路径
- [ ] 改了 `ChatState` → 检查前端所有 `useStore` hook
- [ ] 改了 `AgentEvent` → 检查前端 SSE 解析器

---

## 3. CI Pipeline（建议配置）

```yaml
# .github/workflows/ci.yml
jobs:
  check:
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions-rust-lang/setup-rust-toolchain@v1
      
      # Stage 1: Format (fail-fast, ~2s)
      - run: cargo fmt --check
      
      # Stage 2: Lint (~30s)
      - run: cargo clippy --workspace -- -D warnings
      
      # Stage 3: Build check (~60s)
      - run: cargo check --workspace
      
      # Stage 4: Unit + Integration tests (~2min)
      - run: cargo test --workspace
      
      # Stage 5: Frontend
      - uses: actions/setup-node@v4
      - run: cd frontend && npm ci && npx tsc --noEmit && npx vite build
```

---

## 4. Claude Code 自动验证

在 `CLAUDE.md` 中编码验证规则，Agent 每次改动后自动执行：

```markdown
## Verification
After every change, run:
1. `cargo check --workspace` (must pass, 0 errors)
2. `cargo test -p <changed-crate> --lib` (must pass)
3. Report failures with exact file:line
Never claim completion without fresh verification output.
```

---

## 5. 工具链

| 工具 | 用途 |
|------|------|
| `cargo fmt` | 格式化（必须通过） |
| `cargo clippy` | 代码风格检查 |
| `cargo check` | 编译检查（比 build 快） |
| `cargo test -p <crate>` | 增量单元测试 |
| `cargo test --workspace` | 全量测试 |
| `cargo audit` | 依赖漏洞扫描（建议配） |
| `npx tsc --noEmit` | 前端类型检查 |
| `npx vite build` | 前端构建验证 |
