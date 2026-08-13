# 底层大调整方案(Architecture Restructure Plan)

Status: **PROPOSAL** — 待用户审批。问题记录见 `architecture-audit.md`。
Date: 2026-08-13. User directive: "我需要我的agent全部接入状态机 / 系统的做底层大调整 / 先定方案".

## 目标架构一句话

**一个统一入口(`AgentRun`)+ 一个循环引擎(共用 `run_loop`)+ 一张工具注册表 + 特性单一 owner + 写入收敛。**

## 核心设计

### 目标:`AgentRun` — "run one agent on a request" 单一抽象

```rust
// everevo-agent/src/run/mod.rs (新)
pub struct AgentRun {
    ctx: AgentRunContext,        // 上下文组装(主/子 agent 共用)
    registry: ToolRegistry,      // 一张注册表(见 P3)
    engine: LoopEngine,          // 一个循环引擎(见 P1)
    cancel: CancellationToken,
    max_turns: usize,
    sink: EventSink,             // Stream(SSE) | Collect(子 agent) | Cli
}

impl AgentRun {
    pub async fn start(request: AgentRequest, config: RunConfig) -> EventStream;
}
```

11 个入口全部收敛为 `AgentRun::start(request, config)`:主会话 / auto-continue / CLI / TaskTool / SubAgentPool / TeamTool / WorkflowTool / WorkflowRunner agent 步 / WebSearchDelegate / A2A。

---

## 分阶段实施(每阶段验证:cargo check/test/fmt/clippy 全绿)

### P0 — 统一循环引擎 + 统一入口(核心,先做)

**P0.1 统一循环引擎**
- `run_subagent` 改成 `run_loop` 的薄包装:给 `run_loop` 加 `RunMode { Stream, Collect }`(或让 run_subagent 消费 Done 事件取 final_text)。
- 删掉 run_subagent 里重写的流解析/id-less arg 去重/截断续跑/pending 门控(近逐字副本)。
- 行为保真:子 agent 的工具子集注册、无 SSE、id-less 去重——都要在 Collect 模式里等价保留。
- 风险:行为等价性;逐调用点核对。

**P0.2 统一入口 AgentRun**
- 抽 `AgentRun` 封装:上下文组装 + 注册表 + 引擎 + cancel + 轮次 + 事件汇。
- 消除 handler.rs:658 vs 916 的重复接线;三套子 agent 派发机制(TaskTool 通道 / TeamTool 通道+backlog / SubAgentPool 阻塞)统一为 AgentRun::start。
- system prompt 拼装统一走 SubAgentContext(删 team/workflow/a2a/web_search 的硬编码 inline)。
- cancel 传播统一(子 agent 继承父 cancel,删 fresh CancellationToken)。

**验证:** P0 后 `cargo test --workspace` 全绿;子 agent 行为(团队/cluster/workflow/a2a/web_search)逐路径冒烟。

### P1 — 工具归属归一 + 特性单一 owner

**P1.1 工具回 agent,注册表合一**
- server 的 5 个 agent 域工具迁回 agent:`problem_model` / `pipeline` / `ask_user` / `web_search_delegate` / `sandbox`。需要会话状态的(ask_user/problem_model)用注入接口(server 提供 session-scoped state holder)。
- `build_registry` 成为**唯一**注册表;server 只加会话状态注入 + HTTP glue。删 `tools.rs:7-9` 的 "stay in sync" 注释。
- 子 agent 基础注册表(5 份手抄)改为从唯一注册表按需派生。

**P1.2 特性单一 owner(去字符串耦合)**
- **问题建模**:ProblemModel 纯 serde 类型 → core;工具 + stage + driver 门引用同一模块;`model_drafted` 判定从字符串 `'problem_model'` 改为类型化 `ToolKind::ProblemModel`。
- **验证门**:`is_verification_call` 从字符串匹配改为类型化 `ToolKind::Verifier`;verify_candidate 脚本路径提为共享常量。
- **plan mode**:server 强制 + agent 工具并成一套 `PlanModeState` owner(删 backward-compat 双轨)。
- **stage 目录**:`stage_catalog()` 从 `tool_visible()` 元数据**派生**(core 的 tool_visible 不再是死方法),删手维护 4/16 清单;`run_stage` 返回真实 fragment 而非罐头串。

**验证:** P1 后工具可见性测试(主循环 vs 子 agent)逐工具断言;problem_model/plan mode/verify 门单测改类型化。

### P2 — 写入/读取收敛(最侵入,增量)

- 单一 session-content coordinator:协调 DB / dreaming / memory(FactManager)/ workflows / problem-model / tool-cache 六个写者。
- 读源单一化:DB 历史 vs RollingSummary vs autocompact 摘要定一个权威。
- 最侵入、可拆多个子任务,必要时推迟到 P2.5。

### P3 — 审计遗留修复(见边界审计 26 项)

plan_mode session_id 注入、CJK 难度误判、agent_character token cap、提示优先级倒置(EvidenceChecklist p2 vs VerifyCandidate/ProblemModeling p3)、cluster items 上限、shell confirmed 死路、错误表对账。

---

## 优先级与依赖

| 阶段 | 内容 | 依赖 | 预估风险 |
|---|---|---|---|
| **P0** | 统一循环 + 统一入口 | 无 | 高(核心重构,需逐调用点保真) |
| **P1** | 工具归属 + 特性 owner | P0(注册表/入口定型后) | 中 |
| **P2** | 写入收敛 | P0+P1 | 高(6 存储协调) |
| **P3** | 审计修复 | 独立,可并行 | 低-中 |

## 不做的事(明确排除)

- 不改 FSM 语义(T1-T26 保持不变,只动入口/引擎/归属)。
- 不碰评分层、不改 difficulty 分类逻辑(只加 CJK 修复)、不删现有 stage 内容。
- 不引入外部状态机库/依赖。
- **调整期间不触发 GAIA benchmark**(绑定约束);每次跑前执行污染隔离协议。

## 验证策略

1. 每阶段 `cargo check --workspace && cargo test --workspace && cargo fmt --check && cargo clippy -- -D warnings`。
2. P0 子 agent 行为逐路径冒烟(团队/cluster/workflow/a2a/web_search)。
3. P1 工具可见性逐工具断言。
4. 前端未动则跳过 tsc。

## 决策点(已定,2026-08-13 用户审批)

1. **P0 方案**:✅ `RunMode` 共用 run_loop —— run_subagent 改为薄包装(run_loop 已发 AgentEvent::Done,Collect 只是建通道→drain→取 final_text),删全部重复。
2. **P1 工具迁移范围**:✅ **全部 5 个迁回 agent**(problem_model/pipeline/ask_user/web_search_delegate/sandbox),会话状态用注入接口。
3. **P2 写入收敛**:✅ **本次全含 P2**。
4. **P3 审计修复**:✅ **并入本次**(依赖入口/归属定型)。

**实施顺序:** P0.1(循环统一,RunConfig 打包 + run_subagent 委派)→ P0.2(AgentRun 统一入口,11 调用点收敛)→ P1(工具归属 + 特性 owner)→ P2(写入收敛)→ P3(审计 26 项)。每阶段验证全绿。
