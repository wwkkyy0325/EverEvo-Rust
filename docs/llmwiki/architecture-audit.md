# Architecture Audit — 层次/散乱/流向/入口(2026-08-13)

Status: **RECORD** — 4-dim workflow audit (17 agents, adversarial-verified: 12/12 structural claims REAL).
Date: 2026-08-13. Trigger: user critique "层次不够明确,有点散乱,内容流向不清晰,没有统一入口".

## 结论:四个症状全部确认

| 症状 | 判定 | 一句话 |
|---|---|---|
| 没有统一入口 | ✅ CONFIRMED | **两套独立循环引擎**,11 个入口散在 8 文件,各拼 setup |
| 散乱 | ✅ CONFIRMED | 工具跨 4 crate,**两套工具注册表**互要"手动同步",3 套子 agent 派发机制并存 |
| 层次不清 | ✅ CONFIRMED | `run_subagent` 近逐字复制 `run_loop`;特性跨 crate 无单一 owner;两套"stage"概念 |
| 内容流向不清 | ✅ CONFIRMED(主路径例外) | 主路径线性;但一条消息扇出 6+ 重叠存储,读源竞争;跨层靠字符串名耦合 |

**例外(骨架是干净的):** 依赖方向 `kernel → agent → server` 无环(Cargo.toml 验证),agent 不依赖 server。问题在**特性归属**,不在依赖方向。

---

## 1. 没有统一入口 — 两套循环引擎 + 11 个入口

### 引擎 A:`AgentLoop::run()` → `driver::run_loop`(流式 ReAct)
- 定义 `loop_/mod.rs:260`,唯一生产调用 `run_loop`(mod.rs:325)
- **3 个调用点**:主会话(handler.rs:715,经 main_session 工厂 658 全量接线)、auto-continue 恢复(handler.rs:937,手工重建 AgentLoop::new 于 916 —— **与 main_session 接线重复**)、CLI(main.rs:582,max_turns=30,无子 agent 通道/cancel)

### 引擎 B:`AgentLoop::run_subagent()`(阻塞,返回 String)
- 定义 `loop_/mod.rs:381`。**独立的内联循环**,自带流解析(443-509)、id-less arg 增量去重(473-486,与 driver.rs:334-347 **近逐字复制**)、工具派发、截断续跑、pending 门控。**不复用 run_loop**。默认 max_turns=3(mod.rs:392,与 sub_agent 工厂的 0=无限**不一致**)
- **8 个调用点**:TaskTool(delegate/spawn.rs:62,复制 base 注册表)、SubAgentPool×2(subagent_pool.rs:137/211,cluster 用)、TeamTool(team.rs:354,**内联派发,不走 SubAgentPool,尽管 pool 文档自称"替代了 TeamTool"**,fresh CancellationToken)、WorkflowTool(workflow.rs:364,硬编码 inline system prompt)、WebSearchDelegateTool(web_search_delegate.rs:84,**空 ToolRegistry**,零工具)、WorkflowRunnerTool agent 步(orchestration/tools.rs:432)、A2A executor(a2a/executor.rs:128,硬编码 inline prompt)

### 每处手工重拼的 setup(不一致)
- **system prompt**:SubAgentContext.build_system_prompt(3 路)vs 硬编码 inline(team/workflow/a2a/web_search)
- **工具注册表**:复制 base(spawn)、预置 cluster_base(tools.rs:504-528)、空注册表(web_search:80)、传入 base(其余)
- **cancel token**:会话 token vs `CancellationToken::new()`(team:354/workflow:212,不传播父 cancel)
- **轮次上限**:主无限 / CLI 30 / run_subagent 默认 3 / 各工具覆盖

**注意:** HTTP seam 是统一的(POST /api/chat 都进 handler.rs),"没有统一入口"指**没有"run one agent on a request"抽象**——两套引擎、重复 setup 才是病根。

---

## 2. 散乱 — 工具跨 4 crate + 两套注册表

### 两套独立注册表
- **R1** agent CLI:`tools.rs:26-62 build_registry()`(11 工具),`tools.rs:7-9` 注释原话 **"For the full server-mode registry... The two registries should stay in sync"**
- **R2** server per-session:`orchestration/tools.rs:72-811 assemble()`(811 行,权威),内含 **5 个子 agent 基础注册表按名手抄**:base_for_task(:543)/base_for_workflow(:580)/cluster_base(:506)/team_base(:717)/wf_tools(:342)

### 工具定义分布
| crate | 工具 | 说明 |
|---|---|---|
| everevo-agent | 22(builtins) | Shell/Download/Bootstrap/PlanMode/Compact/ToolCacheRead/CodeSearch/Skill/Memory/TodoWrite/PromoteSkill/Task/CancelTask/Team/Workflow/WorkflowRunner/List+SaveWorkflow/Cluster/… |
| everevo-server | 5 | ask_user、**problem_model、pipeline**、sandbox、web_search_delegate |
| everevo-kernel | 6 bootstrap | kernel 层引导工具 |
| everevo-mcp | 1 通用适配 | MCP 桥 |

同名单工具多 crate 实现 + `HashMap::insert` 覆盖(后注册的替换先注册的),同名单靠注册顺序决定谁生效。

### post-turn 6 个生产者各写各的(无 coordinator)
memory_extraction→FactManager、reflection→FactManager、workflow_compose→workflows 目录、problem_model_distill→workflows 目录、persona_update→profile.json、paradigm_extraction→FactManager;同时 in-loop 还有 BackgroundMaintenance→DB、response.rs 摘要→FactManager、memory 引擎自写。

---

## 3. 层次不清 — 特性归属无单一 owner

### 3.1 `run_subagent` 复制 `run_loop`
流解析、id-less arg 去重、截断续跑、pending 门控全部重写一遍(近逐字)。工具型子 agent 走**不同引擎**。

### 3.2 子 agent 派发三套并行
TaskTool(通道注入主循环)/ SubAgentPool(cluster,工具内阻塞)/ TeamTool(内联)。SubAgentPool 文档自称"替代 TeamTool",但 TeamTool 仍内联跑。

### 3.3 问题建模特性跨 4 文件 2 crate,字符串耦合
- `problem_model.rs`(server,纯 serde 零 I/O——本可在 core)
- `problem_model_tool.rs`(server,注册 tools.rs:256)
- `stages/problem_modeling.rs`(agent,priority 3)
- `loop_/driver.rs:64-70/135-140/500-513/567-569`(agent,`model_drafted` 门**按工具名字符串 "problem_model" 匹配**)

流向:agent stage(提示)→ LLM → server 工具(状态)→ agent driver(门)。依赖方向合法,但**无类型化接口,靠名字符串隐式契约**。

### 3.4 plan mode 两套系统
server `plan_mode_sessions` 强制(handler/tools.rs)+ agent `plan_mode.rs` 工具(PlanModeState 类型 + 谓词)。plan_mode.rs:1-7 文档自称"kept for backward compatibility"——**新旧两套并存**。

### 3.5 两套"stage"概念
真 ContextStage(default_pipeline + build_full_pipeline,16 个)→ 自动注入 fragment;静态 StageCatalogEntry(`stages/mod.rs:67-137`,`stage_catalog()` 只列 **4/16**,`run_stage` 返回**罐头提示串**而非真实 fragment)。core 的 `ContextStage::tool_visible()`(context.rs:66)**定义但零调用点(死元数据)**——目录靠手维护,无派生。

### 3.6 验证门字符串耦合
`is_verification_call`(driver.rs:48-60)按 `cluster`+action=="verify" 或 shell 命令**包含** "verify_candidate" 字符串匹配;bench 脚本 `data/bench/tooltest/verify_candidate.py` 在 crate 树外,靠路径字符串契约。

---

## 4. 内容流向 — 主路径线性,写入扇出

### 主路径(线性可追溯)
POST /api/chat → handler.rs:92 handle_chat → resolve_session → ContextBuildContext(285-426)→ sub_ctx(468-480,子 agent 平行上下文)→ build_full_pipeline + assemble_with_snapshot(518-534)→ SessionCoordinator(571)→ build_registry(595)→ AgentLoop::main_session().run()(658-715)→ SSE 流(728-831)→ 子 agent auto-continue(833-1023)→ finalize_response(1027-1041)→ spawn_post_turn_tasks(1052-1054)。

### 一条消息扇出到 6+ 重叠存储
DB messages、dreaming engine、事实文件/FTS5/vector、workflows 目录、内存 problem-model 图、磁盘 tool-cache。**无单一 authority**。

### 读从竞争源
DB 历史 vs RollingSummaryStage vs autocompact `<conversation_summary>`;子 agent 结果 3 条机制(TaskTool 通道/TeamTool 通道+backlog/SubAgentPool 阻塞)。

---

## 5. 干净的部分(重构需保留)

- **依赖方向 kernel→agent→server 无环**(Cargo.toml 验证,server/Cargo.toml:14-15,agent/Cargo.toml:10,agent 无 server 依赖)
- 主会话路径线性、可追溯
- 验证集合(3 stage + driver 门)在 agent 内相对内聚(除路径字符串契约)
- 记忆/代码搜索/workflow/MCP 的流向大多通过类型化调用

---

## 6. 配套产物

- 重构方案:`docs/llmwiki/architecture-restructure-plan.md`
- 底层大调整任务:`docs/llmwiki/tasks/2026-08-13-architecture-restructure.md`
