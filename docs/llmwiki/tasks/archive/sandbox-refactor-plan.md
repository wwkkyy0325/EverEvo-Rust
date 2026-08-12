# everevo-sandbox — Audit & Refactor Plan
> **状态**:✅ 已完成(归档)— 沙箱重构计划,已落地

---


> 目标：条理清晰、模块解耦、稳定健全

---

## 当前架构评估

### 模块结构

```
everevo-sandbox/src/           2148 行
├── lib.rs                      60 行 — 清晰
├── provider.rs                468 行 — 过胖
├── session.rs                 184 行 — 清晰
├── resolved.rs                 76 行 — 清晰
├── permission/
│   ├── mod.rs                  82 行 — 清晰
│   ├── rules.rs               442 行 — 过胖
│   ├── patterns.rs            153 行 — 清晰
│   ├── paths.rs               164 行 — 清晰
│   └── level.rs                32 行 — 清晰
├── audit.rs                    63 行 — 清晰
├── config.rs                   54 行 — 清晰
├── error.rs                    31 行 — 清晰
├── limits.rs                   50 行 — 清晰
├── job_object.rs              129 行 — Windows 专用
└── unix_limits.rs               9 行 — 空桩
```

### 评分

| 维度 | 评分 | 说明 |
|------|------|------|
| 模块划分 | ⭐⭐⭐⭐ | permission 拆分合理，provider 过胖 |
| 解耦度 | ⭐⭐⭐ | `SandboxProvider` trait 在 core 中，正确 |
| 异步安全 | ⭐⭐⭐ | `std::sync::Mutex` 持锁极短，但脆弱 |
| 错误处理 | ⭐⭐ | `SandboxError → EverEvoError(Sandbox(String))` 丢信息 |
| 平台隔离 | ⭐⭐⭐⭐ | `#[cfg(windows)]` / `#[cfg(not)]` 干净 |
| 测试覆盖 | ⭐⭐ | 无独立测试文件 |

---

## 问题清单

### HIGH — 影响稳定性

#### H1. provider.rs — 468 行 God 函数

`execute()` 承担全部：权限检查→Shell构建→PATH注入→JobObject→超时→审计→输出捕获。

**方案**: 提取 5 个独立函数：
1. `check_permission_gate()` — 权限判断
2. `build_shell_command()` — 命令 + PATH + env
3. `apply_os_limits()` — JobObject / rlimit
4. `spawn_and_wait()` — 启动 + 超时 + 输出捕获
5. `build_audit_record()` — 审计记录构建

#### H2. `std::sync::Mutex` 在异步上下文

`TieredSandbox.rules` 和 `audit_log` 用 `std::sync::Mutex`。不跨 `.await` 时安全，但脆弱。

**方案**: 统一为 `tokio::sync::Mutex`。

#### H3. `unix_limits.rs` — 空桩

Linux/macOS 上不限制内存/CPU/进程数。9 行 `Ok(())`。

**方案**: Phase 3 实现 cgroups v2 + rlimit。

### MEDIUM — 影响可维护性

#### M1. `permission/rules.rs` — 442 行

权限规则+决策逻辑+默认规则+路径检查都在一个文件中。

**方案**: 拆分为：
- `rules.rs` — 规则数据结构 (80 行)
- `check.rs` — 权限检查决策逻辑 (200 行)
- `defaults.rs` — 默认 deny/allow 模式 (160 行)

#### M2. `SandboxError → EverEvoError(Sandbox(String))` 丢失结构化信息

**方案**: `EverEvoError::Sandbox` 改为结构化变体 `Sandbox { kind, context }`

#### M3. `provider.rs` 顶部 `#![allow(clippy::disallowed_methods)]`

整个模块禁用了 clippy 检查，而非针对具体行。

**方案**: 移除模块级 allow，改为行级 `#[allow(...)]`。

#### M4. `resolved.rs` 顶部同样的 `#![allow(clippy::disallowed_methods)]`

**方案**: 同 M3。

### LOW — 不影响功能

#### L1. `job_object.rs` — Windows 专用，无 Linux 对应实现

Windows Job Object 已经实现；Linux cgroups 在 `unix_limits.rs` 中是空桩。

#### L2. 无测试文件

2148 行代码，零测试。

#### L3. `AuditRecord` 结构体定义在 `provider.rs` 中

审计记录应该是独立模块。

**方案**: 移到 `audit.rs`。

---

## 重构执行计划

### Phase 1 — 安全加固 (2h)

| # | 行动 | 文件 |
|---|------|------|
| 1 | `std::sync::Mutex` → `tokio::sync::Mutex` | provider.rs |
| 2 | 移除模块级 `#[allow(clippy)]` → 行级 | provider.rs, resolved.rs |
| 3 | `AuditRecord` 移到 `audit.rs` | provider.rs → audit.rs |

### Phase 2 — 模块拆分 (3h)

| # | 行动 | 文件 |
|---|------|------|
| 4 | `execute()` 拆 5 函数 | provider.rs → provider/ |
| 5 | `permission/rules.rs` 拆分 | rules.rs → rules + check + defaults |
| 6 | `SandboxError → EverEvoError` 结构化 | error.rs + everevo-core |

### Phase 3 — 平台补齐 + 测试 (4h)

| # | 行动 | 文件 |
|---|------|------|
| 7 | Linux cgroups v2 + rlimit | unix_limits.rs (重写) |
| 8 | 沙箱集成测试 | tests/sandbox_test.rs |
| 9 | 权限规则单元测试 | permission/__tests__/ |

---

## 解耦原则（重构后保证）

```
everevo-core::sandbox::SandboxProvider  ← trait 定义
        ↑
everevo-sandbox::TieredSandbox          ← 实现
        ↑
everevo-sandbox::SessionSandbox         ← session 封装
        ↑
everevo-server::chat.rs                 ← 调用方（通过 SessionSandbox）

依赖方向: server → sandbox → core (全部正确)
```