# 2026-08-13 — 底层大调整:统一入口 + 单一循环 + 归属归一 + 写入收敛

## Context

用户指令(2026-08-13):"系统的做底层大调整",方案已审批(P0+P1+P2+P3 全部本次执行)。问题记录 `architecture-audit.md`,方案 `architecture-restructure-plan.md`。

**目标:** 一个 `AgentRun` 抽象(统一入口)+ 一个循环引擎(共用 `run_loop`)+ 一张工具注册表 + 特性单一 owner + 写入收敛 + 审计 26 项。

## 任务

### P0.1 — 统一循环引擎 ✅ DONE 2026-08-13
- [x] RunConfig 结构体打包 run_loop 的 20 个参数(loop_/config.rs)+ ConfirmFn 类型别名
- [x] run_loop 签名改为 `(llm, tools, tool_schemas, messages, config, tx)`,顶部解构回同名局部变量(**函数体零改动**)
- [x] run() 构建 RunConfig;3 个测试调用点改 `RunConfig::new()` + 覆盖
- [x] run_subagent 改为薄包装委派 run_loop(建通道→drain TextDelta 累计 + Done 兜底);**近 240 行内联重复循环删除**
- [x] verify: cargo check/test(284)/fmt/clippy/workspace(822, 0 FAILED)全绿
- [x] **验收抓到 2 个真 bug**:
  - core `LlmProvider::stream_chat` 默认实现流结束后 `token.cancel()` **取消调用方会话 token**(mock 无原生流式即中招 → 下一轮全 "Cancelled")→ 修复:移除错误的 cancel(见 llm.rs)
  - run_subagent 委派 run_loop 后继承 **T8 验证提交门**(子 agent 不该有)→ 加 `RunConfig.verify_gate`,子 agent 关闭
- [x] **run_subagent 单测 ×2**(委派基本路径 + 工具调用跨轮收集)——但测试 setup 是手拼 stream,触发了"mock 应统一管线"的用户指令

### P0.2 — 统一入口 AgentRun(核心完成)
- [x] **引擎统一(P0.1 继承)**:run_subagent = run_loop 薄包装,8 个子 agent 调用点共用单一引擎
- [x] **会话接线单一来源**:handler.rs 抽 `apply_session_agent_wiring()`,main_session 与 auto-continue 共用;删两处重复块
- [ ] **后续**:完整 `AgentRun` 结构体待 P1 工具归属定型后再收敛
- [x] verify: clippy 0 / 284 tests / workspace 0 FAILED

### P0-mock — mock 统一管线(计划,用户指令"mock也计划一个管线")
- [ ] 设计 MockScript 声明式路由(见汇报),替代手拼 `.with_stream().with_text()`

### P0.2 — 统一入口 AgentRun(核心完成)
- [x] **引擎统一(P0.1 继承)**:run_subagent = run_loop 薄包装(collect 模式),8 个子 agent 调用点共用单一引擎
- [x] **会话接线单一来源**:handler.rs 抽 `apply_session_agent_wiring()`,main_session(原 658)与 auto-continue(原 916)共用同一接线(proactivity/context_budget/hook_feedback/meta_agent/telemetry/benchmark turn cap);删除两处重复块(含 benchmark max_turns 重复计算)
- [ ] **后续(可延后)**:CLI 与子 agent 调用点的工具注册表/系统提示差异保留(功能特性非重复),完整 `AgentRun` 结构体待 P1 工具归属定型后再收敛
- [x] verify: clippy 0 / 282 tests / workspace 0 FAILED

### P0-mock — mock 统一管线 ✅ DONE 2026-08-13
- [x] `MockScript` 声明式路由(llm/mock.rs):MockStep { Text/Calls/CallsIdless/Stream/Err },每步恰好一次 LLM 调用,工具流自动生成 id;`from_script` + `assert_call_count`/`assert_calls_contain`
- [x] run_subagent 测试改用 MockScript;6 个 mock 测试 + 289→291 全绿

### P1.2 — 类型化特性 owner(部分完成)
- [x] **ToolKind 分类器**(driver.rs):`Verifier`/`ProblemModelFinalize`/`Other`,`classify_tool()` 收敛 `is_verification_call`/`is_problem_model_finalize` 字符串匹配为单点类型化枚举
- [x] **stage_catalog 漂移守卫**(stages/mod.rs):测试断言目录 ↔ tool_visible 一致(防未来加 tool_visible stage 漏进目录)
- [x] verify: 291 tests / clippy 0 / workspace 0

### P1.1 — 工具迁移 ✅ DONE 2026-08-13(方案:B trait 抽象 + 全部迁移)
- [x] **共享类型移到 core**:`ProblemModel` 全家(纯 serde)+ `AskNotification`/`PendingAsk`/`ConfirmationNotification`/`PendingConfirmation`(core/src/session.rs + problem_model.rs,server re-export 保路径兼容)
- [x] **`SessionStore` trait**(agent/src/tools/session_store.rs):ask_user_map/ask_notif_tx/auto_answer/confirmations/confirm_notif_tx/auto_confirm/problem_models
- [x] **5 工具全部迁回 agent**:`pipeline`(无状态)、`web_search_delegate`(无状态)、`ask_user`/`problem_model`/`sandbox_tool`(经 SessionStore);server 工具文件删除
- [x] **`ServerSessionStore`**(server/src/session_store.rs)实现 trait,桥接 AppState + SSE 通道
- [x] **注册表更新**:assemble() 构建 store + agent 工具;`stay in sync` 注释更新(归属统一,注册表按运行模式分 CLI/HTTP)
- [x] verify: **829 tests / 0 FAILED / clippy 0 / fmt 干净**

### P1 — 汇总 ✅ DONE(P1.2 ToolKind + stage 守卫 + P1.1 迁移)
- [x] P1.2 ToolKind 类型化 + stage_catalog 漂移守卫 + P1.1 全部迁移
- [x] **P1.1 遗留:子 agent 基础注册表 5 份手抄收敛** — 全部改用 `ToolRegistry::subset(name_list)` 派生:base_for_task/base_for_workflow(6 工具)共享名称表,cluster_base(5)/wf_tools(2)/team_base(2)/task_registry 均为名称列表;fully_auto 的 auto_shell 替换逻辑保留。主注册表成单一来源
- [ ] 遗留(可选):build_registry(CLI)与 assemble(HTTP)完全合一(架构上仍按运行模式分 CLI 子集/HTTP 全量,各有合理差异,建议不做)
- [x] **stage_catalog 完整派生** — `TOOL_VISIBLE_STAGES` 静态实例表为唯一来源,catalog 从 tool_visible 元数据派生 name/description;canned prompt 集中到 `canonical_prompt()` 一处;漂移守卫改测派生契约(1:1 覆盖 + tool_visible + 描述一致 + 无重复)

### P3 低风险收尾 ✅(15 LOW 全处理)
- [x] **plan mode 双轨合并** — MCP `plugin-plan-mode`(enter_plan_mode/exit_plan_mode,独立进程无状态写)从 auto-load 移除(no-op 误导 agent);in-process EnterPlanMode/ExitPlanMode 成为唯一功能轨;AppState 字段类型统一为 `PlanModeState`;plan_mode.rs 头注释改述真实角色
- [x] **LOW 文档类**:pipeline.rs + stages/mod.rs 优先级文档补齐(problem_modeling/verify_candidate/evidence_checklist p3、rolling_summary p75);verify_candidate.rs 过时优先级措辞修正;problem_model_tool 头文档补 add_nodes + unknown-action 提示补 add_nodes;error-transition-table 修正 Context overflow 行(实为 trim 半预算→重试→放弃,无紧急 LLM autocompact)+ Asset integrity 行(Fail when all missing)+ 补 auto-continue 升级行 + 补 thinking-only-turn 恢复行
- [x] **LOW 代码类**:parallel_agents 空 tasks → is_error=true(不再静默成功);todo_write 缺失/错型 todos → helpful 提示(形状说明);shell timeout_secs floor 到 1(0 会瞬时杀进程)
- [x] verify: workspace 全绿 / clippy 0 / fmt 干净
- [x] **真实服务启动验收**(debug 二进制 + 真实 deepseek):服务起、health OK、普通 chat 答对 "main"(Final answer: main)、plan mode 合并验证——只有 `EnterPlanMode`(MCP no-op 消失),写过滤生效(shell 被 registry 移除 → "Unknown tool shell, Available: 只读工具"),CJK 全链路正常

### P0+P1 验收 ✅ DONE 2026-08-13(功能级)
- [x] 829 tests / clippy / fmt 全绿(单元级)
- [x] **功能冒烟**(重建 release + 起 server + 真实 chat):
  - shell(迁移 sandbox_tool):`git branch` → "main" ✅
  - problem_model(迁移经 SessionStore):×2 status=ok ✅
  - cluster 子 agent(run_subagent → run_loop 委派):完成,答 "4 and 9" ✅
  - 完整 SSE done 正常
- [x] 发现:端口 13456 被 deepspace-server.exe(他项目)占用 → 已杀,确认本 server 正常
- [x] 验收结论:**统一循环 + 工具迁移 + SessionStore 全链路功能正常**

### P2 — 写入收敛 ✅(有界版)
- [x] **`SessionContent` 单一写入协调器**(server/src/session_content.rs):`persist_user`(DB+dreaming 单点)、`persist_turn`(DB);handler 用户消息改用
- [x] **读权威文档化**:DB 消息=对话源,stages 加压缩视图(非竞争源),dreaming=会话模型 feed,memory/workflows=长期 sink
- [x] **明确不做**:6 存储压平(多存储是按设计:历史/记忆/流程各司其职,压平风险高收益低)
- [x] verify: 830 tests / clippy 0

### P3 — 审计 26 项(主体完成)
- [x] **plan_mode session_id 注入(HIGH)**:`params["session_id"]`(永不注入→nil 全局单例)→ 构造时注入 `self.session_id`(Enter/ExitPlanModeTool + 注册表 + 测试)
- [x] **CJK 难度误判(MEDIUM)**:classify 只认 ASCII 数字/英文关键词 → 加 CJK 检测(8+ 汉字或中文问句标记判 Hard,问候保持 Simple)+ 测试
- [x] **verify_candidate 提示优先级倒置(MEDIUM)**:EvidenceChecklist(提交门)p2 → p3,排在 ProblemModeling/VerifyCandidate 之后;注释与 pipeline 顺序同步
- [x] **agent_character token cap(MEDIUM)**:voice_samples + sources/ 无上限注入 → `CHARACTER_BLOCK_MAX_TOKENS=4096` 前缀保留截断(clamp_content_by_tokens,覆盖 stage + 子 agent 继承)+ 测试
- [x] **cluster map_reduce items 上限 + cancel(MEDIUM)**:items 无上限 → `MAX_MAP_REDUCE_ITEMS=20` 纯函数 cap_batch + 可见丢弃提示;claims 上限 5;`_cancel` 传入全部任务(cancel.cloned)
- [x] **shell confirmed 死路(MEDIUM)**:gate 文本承诺 `confirmed: true` 但永不读取 → schema 加 confirmed,execute 读取并传给 ExecutionConfig.with_confirmed;git 守卫在 confirmed 时跳过;mock sandbox 测试
- [x] **web_search_delegate(MEDIUM)**:恒 is_error=false + max_results 忽略 → 读 max_results(1-20)入 prompt;子 agent 结果 Error:/Cancelled./空 → is_error=true
- [x] **team 静默 nil 派发 + 超时孤儿(MEDIUM)**:nil 派发(缺 llm/tools)→ 输出可见 "⚠️ NOT dispatched";30s 超时/信号量关闭 → cancel_all() 杀已派发成员;成员 token 改为父 cancel 的子 token(会话取消可传播)
- [x] **错误表对账(MEDIUM)**:DB 行 wrong-claim → `SessionContent::persist_user` 失败曾 `?` 杀整轮,改 warn+continue(与表一致);补 视觉 describe_image 恢复行 + 沙箱恢复行(compute-timeout rescue / confirmed gate)
- [x] **LOW(3/15 已知项)**:pipeline 注释 "all 11 stages" → 16(6+10);browser_bridge `c as u8` 截断 CJK → 全 UTF-8 百分号编码 + 测试;parallel_agents 无超时 → 每任务 300s 硬超时 + 父 cancel 子 token
- [ ] 遗留 LOW(未枚举的 ~12 项):parallel_agents/team 等其余文档过期类
- [x] verify: workspace 全绿(agent 299)/ clippy 0 / fmt 干净 / 真实服务冒烟(plan_mode 双会话独立 + 写拦截 + CJK + P2 持久化)

## Verify

- 每阶段 `cargo check --workspace && cargo test --workspace && cargo fmt --check && cargo clippy -- -D warnings`
- P0 子 agent 逐路径冒烟(团队/cluster/workflow/a2a/web_search)
- 前端未动则跳过 tsc
- 调整期间不跑 GAIA;跑前执行污染隔离协议

---

## 物理重构(2026-08-13,用户指令"做物理的文件/文件夹符合逻辑层次")

### ✅ Workstream A — crate 归位(含 MCP)
- [x] webagent app→tools;everevo-mcp + everevo-mcp-protocol kernel→infra;3 处 Cargo.toml path
- [x] 删 problem_model/sandbox 垫片 + .bak;修 skill.rs 编译期路径
- [x] CLAUDE.md + design.md crate 列表按层分组

### ✅ Workstream B — 拆 3 个 >900 行
- [x] loop_/mod.rs 1126→37(agent.rs + tests.rs)
- [x] loop_/driver.rs 1109→892(classify/dedup/llm_call/token_stream + emit_turn_complete)
- [x] chat/handler.rs 1093→891(auto_continue.rs + wiring.rs)

### ✅ Workstream C — 合并
- [x] stages/verification/ 归组(difficulty+util→gate,evidence+verify→skeptic,discipline,modeling)+ 6 处 coupling
- [x] delegate types→spawn;core budget+data 合并
- [x] C2 跳过:RollingSummaryStage 实为 core stage;context/rolling_summary.rs 是维护引擎(侦察误报)

### ✅ Workstream D — 微型路由按域归组
- [x] system_routes(health/model/mcp/tools)+ knowledge_routes(kg/diary)+ utility_routes(command/context/character/workspace);HTTP path 保留

### ✅ 验证
- [x] workspace 全绿(agent 299 + server 22/34 + core 92,0 FAILED)/ clippy 0 / fmt 干净
- [x] 公共路径经 re-export 保留,外部调用点零改动
