# Remaining Features — Upper-Layer Module Plan

> 底层已完成且稳定。以下为上层建筑，各自独立，不耦合底层。

---

## 1. MCP (Model Context Protocol)

**对标**: Claude Code MCP tools (`mcp__*`)
**耦合度**: 弱 — 纯协议适配层
**依赖**: `Tool` trait + SSE + ToolRegistry

### 任务拆解
- [ ] MCP Server 启动管理（`mcp serve` 子命令）
- [ ] MCP Client 协议实现（stdio JSON-RPC）
- [ ] 动态工具发现：MCP server → ToolRegistry 自动注册
- [ ] 资源/提示词协议支持
- [ ] MCP OAuth 认证流程
- [ ] 前端 MCP 配置面板

### 技术要点
- JSON-RPC 2.0 over stdio
- `ServerCapabilities { tools, resources, prompts }`
- 工具动态注册/卸载

---

## 2. Hook System

**对标**: Claude Code PreToolUse / PostToolUse / Stop hooks
**耦合度**: 弱 — 拦截点注入
**依赖**: `Tool::execute()` + Session lifespan

### 任务拆解
- [ ] Hook trait 定义（`BeforeTool` / `AfterTool` / `OnStop`）
- [ ] Hook 注册机制（`.claude/hooks/` 目录扫描）
- [ ] PreToolUse：执行前拦截（允许/拒绝/修改参数）
- [ ] PostToolUse：执行后回调（日志/通知/连锁）
- [ ] Stop hook：回合结束时触发（测试验证/格式检查）
- [ ] Exit code 约定：0=ok, 2=block, 其他=warning

---

## 3. Context Autocompact

**对标**: Claude Code 5-layer context compression (Budget→Snip→Microcompact→Collapse→Autocompact)
**耦合度**: 弱 — 独立上下文管理
**依赖**: LLM client + DB + AgentLoop

### 任务拆解
- [ ] Token-based context measurement（替换 char-based）
- [ ] Layer 1-2: Budget + Snip（已有，需 token 化）
- [ ] Layer 3: Microcompact（时间衰减压缩）
- [ ] Layer 4: Autocompact — LLM 生成历史摘要
- [ ] 紧凑边界消息注入
- [ ] 紧凑后消息重建（`buildPostCompactMessages`）

---

## 4. Daemon / Background Sessions

**对标**: Claude Code `--daemon` + `--bg` sessions
**耦合度**: 弱 — 独立进程管理
**依赖**: AgentLoop + Session state

### 任务拆解
- [ ] Daemon supervisor 进程
- [ ] 后台会话管理（`claude bg list/kill/attach`）
- [ ] 会话持久化与恢复
- [ ] 自动 commit + push + PR（`--auto-commit`）
- [ ] 定时任务（CronCreate/CronDelete）

---

## 5. Agent Teams / Coordinator

**对标**: Claude Code Agent Teams + Coordinator mode
**耦合度**: 中 — 多 AgentLoop 实例编排
**依赖**: AgentLoop + TaskTool + CancellationToken

### 任务拆解
- [ ] Team 定义（`.claude/teams/` 配置）
- [ ] Peer-to-peer 消息传递（SendMessage tool）
- [ ] Coordinator 任务分发 + 结果聚合
- [ ] 共享任务列表（自分配模式）
- [ ] 跨 session 安全（消息不带用户权限跨 session）
- [ ] Agent swarm 模式（Dynamic Workflows 替代）

---

## 6. Workflow Script Engine

**对标**: Claude Code Workflow tool — JS scripting with `agent()` / `parallel()` / `pipeline()`
**耦合度**: 弱 — DSL 调用已有工具
**依赖**: 11 built-in tools + AgentLoop

### 任务拆解
- [ ] Workflow script 格式定义（`export const meta = {...}`）
- [ ] `agent(prompt, opts)` — 启动子 agent
- [ ] `parallel(thunks)` — 并发执行
- [ ] `pipeline(items, stage1, stage2)` — 流水线
- [ ] `phase(title)` / `log(message)` — 进度报告
- [ ] Budget 管理（token 预算限制）

---

## 7. Templates System

**对标**: Claude Code Templates（`claude new/list/reply`）
**耦合度**: 弱 — 独立模板引擎
**依赖**: Session + Messages

### 任务拆解
- [ ] 模板定义格式（`.claude/templates/`）
- [ ] `claude new <template>` — 从模板创建会话
- [ ] `claude list` — 列出可用模板
- [ ] 模板变量替换（`{{variable}}`）
- [ ] 模板分类与搜索

---

## Priority Order

| 优先级 | 功能 | 理由 |
|--------|------|------|
| **P0** | MCP 协议 | 扩展性最强，生态最大 |
| **P1** | Hook 系统 | 安全拦截，CI 集成 |
| **P1** | Autocompact | 长对话质量 |
| **P2** | Daemon/BG | 自动化场景 |
| **P2** | Workflow 脚本 | 复杂编排 |
| **P3** | Agent Teams | 高级多 agent |
| **P3** | Templates | 重复任务加速 |

---

## 架构原则（不变）

```
1. 新功能通过 Tool trait 或 ContextStage trait 注入 — 不修改底层
2. 每个功能独立开发、独立测试、独立开关
3. Claude Code 源码作参考，不照搬
4. 优先完成 P0，再迭代 P1-P3
```
