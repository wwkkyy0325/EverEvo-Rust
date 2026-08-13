# Agent 状态机全接入设计(FSM Full-Integration Design)

Status: **IMPLEMENTED (方案 A)** — 2026-08-13 用户确认"先做a等级"。见 changelog。

Date: 2026-08-13
Author: Claude (per user directive "我需要我的agent全部接入状态机", scope decision: 先出设计文档再定 → "先做a等级")

---

## 1. 背景与目标

用户需求:**agent 全部接入状态机**。当前主循环骨架已走 FSM(`transition()`, T1-T20, 测试断言每一行),但**多个关键行为是内联 `if/else`,不在状态机里**——收敛升级、验证螺旋、提交门建议、观察期混合逻辑。

目标:让 **每个行为分支都是文档化 + 测试断言的状态转移**,状态机成为 agent 行为的单一真相源,`agent-states.md` 与代码不可漂移。

非目标:不把每个工具/stage 变成状态(过度建模,违背"软性"原则);不重写运行架构。

---

## 2. 现状架构(诚实描述)

### 2.1 当前是"线性 turn 骨架 + 转移标记",不是 dispatch

`run_loop` 是单个 `while !is_terminal(state)` 循环,每个 turn 线性走:

```
Observe ──(T2)──▶ Solve ──┬─(T5)──▶ Act ──(T10/11)──▶ Observe (re-loop)
                          ├─(T8)──▶ Verify ──(T12)──▶ Observe (re-loop)
                          ├─(T7)──▶ Converge ──(T14)──▶ Done
                          ├─(T6)──▶ WaitSubAgents (yield)
                          ├─(T9)──▶ Done
                          ├─(T3/T4)──▶ Error
                          └─(T19)──▶ Solve (self-loop)
边界: T16 Cancel / T17 TurnsExhausted→Error / T18 WallClockLow→TerminalCommit
```

- `state` 变量 + `transition()` 是**进度/意图标记 + 边界守卫 + 可观测性**,不是 `match state` 分发。
- 每轮顶部 `state = LoopState::Observe;`(driver.rs:169)手动重置。
- **已有 16 处 `transition()` 调用**(driver.rs:150/159/239/272/285/308/374/412/425/445/473/496/519/533/993/1009/1015/1018)。

### 2.2 已接入 FSM 的行为

| 行为 | 转移 | 位置 |
|---|---|---|
| 取消 | T16 any→Cancelled | 159 |
| 观察期 trim/mask/compact | Observe 状态内动作 | 169-238 |
| LLM 调用 + 溢出/流错 | T2→Solve, T3/T4→Error | 239-380 |
| 原生搜索截断 | T19 Solve self-loop | 412 |
| 子 agent 待处理 | T6→WaitSubAgents | 425 |
| 纯思考无值 | T7→Converge→T14 Done | 445/473 |
| 未验证提交门 | T8→Verify→T12/13 | 496/519 |
| 工具调用 | T5→Act | 533 |
| 墙钟/轮次耗尽 | T17/T18 | 993-1018 |

### 2.3 未接入 FSM 的行为(内联,不经过 transition())

| 行为 | 现状 | 位置 |
|---|---|---|
| **收敛升级**(wall-clock Converge/Commit 提示) | 内联 `match convergence_stage(...)` 直接 push 消息 | driver.rs ~919-937 |
| **验证螺旋**(post_verify_turns ≥ 6 → verified-aware 提示) | 内联计数器 + 内联 push | driver.rs ~912-918 |
| **预算行**(budget_line 注入) | 内联 push | driver.rs ~945 |
| **model_drafted 建模建议** | T8 转移内的附加消息(可视为 Verify 状态动作) | driver.rs ~503-511 |
| **观察期内部**(trim/mask/compact/难度判定) | Observe 状态内动作(可保持现状) | driver.rs 169-238 |

**结论:** 收敛升级和验证螺旋是"行为决定状态"却完全游离在状态机外——这是"全部接入"的落点。

---

## 3. 方案 A(软性全接入,推荐)

保持线性骨架,把缺失的两个行为 formalize 为状态/事件/转移,driver 全部走 `transition()`,测试断言每一行。

### 3.1 新状态

| 状态 | 语义 | 进入时动作 | 退出 |
|---|---|---|---|
| `Stalled` | **验证螺旋**:已验证候选但仍探索 | 注入 verified-aware wrap-up 提示(`verified_wrapup_prompt`) | Ready → Observe(ReLoop) |
| `Escalating` | **收敛升级**:墙钟进入 Converge/Commit 阶段 | 注入对应收敛提示;Commit 阶段更强 | Ready → Observe(ReLoop);WallClockLow → TerminalCommit |

(复用现有 `Converge` 仅表示"纯思考强制收敛";墙钟收敛用独立 `Escalating`,避免与 T7 语义混淆。)

### 3.2 新事件

| 事件 | 守卫 |
|---|---|
| `VerifiedStalled` | `post_verify_turns >= POST_VERIFY_STALL_TURNS && !post_verify_nudged` |
| `BudgetConverge` | `convergence_stage(...) == Converge`(wall ≤ 0.30) |
| `BudgetCommit` | `convergence_stage(...) == Commit`(wall ≤ 0.15) |

### 3.3 新转移(T21-T26)

| # | From | Event | To | Action |
|---|---|---|---|---|
| T21 | Act | VerifiedStalled | Stalled | 注入 verified-wrapup 提示 |
| T22 | Stalled | Ready | Observe | ReLoop(下轮继续,但已带提示) |
| T23 | Act | BudgetConverge | Escalating | 注入收敛提示 |
| T24 | Escalating | Ready | Observe | ReLoop |
| T25 | Act | BudgetCommit | Escalating | 注入更强提交提示(verified-aware 或 deadline) |
| T26 | Escalating | WallClockLow | TerminalCommit | T18 现有路径复用(≤30s) |

### 3.4 driver.rs 改动点(精确)

在 section 6(收敛区,driver.rs ~912-960)把内联逻辑改为:

```rust
// 1) 验证螺旋 → Stalled 状态
if !post_verify_nudged && post_verify_turns >= POST_VERIFY_STALL_TURNS {
    state = transition(state, LoopEvent::VerifiedStalled).0; // T21
    messages.push(LlmMessage::user(verified_wrapup_prompt(post_verify_turns)));
    post_verify_nudged = true;
}
// 2) 收敛升级 → Escalating 状态(替换内联 match)
match convergence_stage(turn, max_turns, wall_frac) {
    Convergence::Commit => {
        state = transition(state, LoopEvent::BudgetCommit).0; // T25
        push(if verified_aware { verified_deadline_prompt() } else { generic_deadline });
    }
    Convergence::Converge => {
        state = transition(state, LoopEvent::BudgetConverge).0; // T23
        push(if verified_aware { verified_wrapup_prompt(t) } else { generic_converge });
    }
    Convergence::None => {}
}
// 3) 预算行保留(观察性,非状态)
messages.push(LlmMessage::user(budget_line(...)));
// 4) ≤30s → T26 Escalating → TerminalCommit(复用现有 T18 break)
if max_turns > 0 && remaining.as_secs() <= 30 { ... T18 ... break; }
```

**注意:** `state = transition(Act, VerifiedStalled)` 要求状态上下文是 Act;收敛区在 Act 之后、下轮 Observe 之前,上下文一致。若未来想从 Solve 直接螺旋,再加 Solve→Stalled 转移(可扩展,软性)。

### 3.5 测试 + 文档防漂移

- `state.rs` 测试新增: T21-T26 每一行 `assert_eq!(transition(s, e), (to, action))`;终止吸收、Cancel 全局、边界事件扩展。
- `convergence.rs` 测试保留(verified-aware 提示文本 + 阈值)。
- `agent-states.md`:状态表加 `Stalled`/`Escalating`,转移表加 T21-T26。
- `error-transition-table.md`:验证螺旋行加 "检测: T21 转移",循环警告行保留。

### 3.6 方案 A 收益

- 每个行为分支都有文档化、可测的状态转移——状态机成为行为地图。
- 改动集中在 driver.rs section 6 + state.rs + 测试,**风险低**(不动线性骨架)。
- 观察期/提交门保持现状(已是状态内动作或已转移),不重构。

---

## 4. 方案 B(全量 match dispatch 重构,备选)

把 `run_loop` 重构成 `match state` 分发循环,每个状态一个 arm/函数:

```rust
loop {
    match state {
        LoopState::Observe => { /* trim/mask/compact/难度 */ }
        LoopState::Solve => { /* stream_chat */ }
        LoopState::Act => { /* 执行工具 */ }
        LoopState::Stalled => { ... }
        LoopState::Escalating => { ... }
        ...
    }
    state = transition(state, event);
}
```

**优点:** 真正的"状态驱动",每个状态自包含。
**风险:**
- run_loop 是 ~1000 行线性代码,重构为 dispatch 是大手术,易引入回归。
- 每个状态 arm 要重排上下文状态(消息、计数器、标志),中间态易漏。
- 与当前"观察期混在线性流"的实现冲突,需要拆分 Observe 内部逻辑。

**何时值得:** 未来行为继续爆炸、线性流难以维护时再考虑。当前**不推荐**。

---

## 5. 风险与缓解

| 风险 | 缓解 |
|---|---|
| 收敛区状态上下文(Act)与实际流程(工具执行后)不严格一致 | 用 `transition(Act, event)` 校验,测试断言;若未来有 Solve 直连螺旋再加转移 |
| 验证螺旋误伤复合题(合法多部分研究) | `POST_VERIFY_STALL_TURNS=6` 已留余量;提示文案是"2 次调用内收尾"而非强制提交 |
| Escalating 与既有 Converge 语义混淆 | 独立状态 + agent-states.md 注释 |
| driver 改动引入回归 | 不改线性骨架,只在 section 6 换 `transition()` 调用;跑全量测试 + 一次冒烟 |

---

## 6. 不做的事(明确排除)

- 不把每个工具/stage 变成 FSM 状态(工具/stage 是循环调用的功能单元,不是状态;过度建模违背"软性")。
- 不重构观察期内部逻辑为独立状态(trim/mask/compact 是 Observe 的动作)。
- 不引入外部状态机库(自研 transition() 已够,无新依赖)。
- 不在本设计内改收敛阈值(阈值微调方向已按用户指令放弃)。

---

## 7. 决策点(已定,方案 A)

1. **方案 A vs B**:✅ 方案 A(软性全接入)。用户"先做a等级"。
2. **Stalled 进入上下文**:✅ 仅 `Act → Stalled`(T21,收敛区)。未加 Solve→Stalled。
3. **Escalating 独立状态**:✅ 独立(T23/T25),不复用 Converge(避免与 T7 强制收敛混淆)。
4. **预算行/观察期**:✅ 保持现状(非状态)。

## 8. 实施记录(2026-08-13)

- `state.rs`:新增 `Stalled`/`Escalating` 状态,`VerifiedStalled`/`BudgetConverge`/`BudgetCommit` 事件,T21-T25 转移 + T26(全局 WallClockLow 覆盖),新增 3 个测试(14 项全过)。
- `driver.rs` section 6:内联收敛/螺旋逻辑改为 `transition()` 路由;**优先级 Commit > Converge > Stalled**(同一 turn 互斥,升级提示在 verified-aware 时覆盖螺旋提示)。
- `agent-states.md`:状态表 + 转移表更新至 T26,边界注释。
- `error-transition-table.md`:验证螺旋行引用 T21/T23/T25/T26。
