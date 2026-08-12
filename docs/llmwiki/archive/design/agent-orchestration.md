# EverEvo Agent Orchestration — Design Document
> **状态**:⛔ 已过时(归档)— 已被 [05-orchestration.md](../../architecture/05-orchestration.md) 取代
> **来源**:2026-07-19 | **归档**:2026-08-12。本文是设计愿景,以代码现状文档为准。

---


## References

| # | Source | Key Insight |
|---|--------|-------------|
| 1 | Anthropic Agent Skills (Dec 2025) | Open standard: progressive disclosure (YAML→body→resources), code execution pattern, hierarchical composition |
| 2 | OpenAI Agents SDK (2025) | Two orchestration patterns: Handoff (transfer control) + Agent-as-Tool (manager synthesis) |
| 3 | SoK: Agentic Skills (2025) | Formal definition S=(C,π,T,R): applicability, policy, termination, reusable interface |
| 4 | LangGraph + Temporal (2025) | Framework (what) + Engine (how it survives): 70% of agents need only a custom loop |
| 5 | CrewAI (2025) | Manager-Worker: task-level tool scoping, Principle of Least Privilege per task |
| 6 | Cascade MCP (2025) | Cache-first, subagent parallelism, one-level-deep constraint |
| 7 | PRISM Persona (IJCNLP 2025) | Expert persona = better alignment but worse accuracy; Persona Switch for dynamic selection |
| 8 | PERSONAMEM-v2 (2025) | Reasoning > context length for personalization; agentic memory with RL training |

---

## 0. Core Answer: Tools vs Skills vs Agents

```
Tool   = 执行能力 (executable function)   → "调用 shell 执行命令"
Skill  = 专业知识 + 执行流程 (expertise)  → "如何读 Figma 设计稿并生成代码"
Agent  = 自主决策者 (autonomous decider)  → "主Agent拆解任务、分派子Agent"
```

**Skill = Tool + 领域知识 + 执行流程 + 判断标准。**

读图、下载、代码审查——这些都是 **Skill**，不是独立的 Agent。

---

## 1. Skill Architecture

### 1.1 Progressive Disclosure (Anthropic Standard)

```
data/skills/
├── read-diagram/
│   ├── SKILL.md           ← YAML frontmatter (name, description, when to use)
│   ├── reference.md       ← 详细指南 (按需加载)
│   └── scripts/           ← 可执行脚本 (不进上下文, 直接调)
│       └── ocr.py
│
├── download-files/
│   ├── SKILL.md
│   └── reference.md
│
├── code-review/
│   ├── SKILL.md
│   └── reference.md
│
├── project-init/
│   ├── SKILL.md
│   └── templates/
│
└── user-defined/          ← 用户可以创建自己的 Skill
    └── my-skill/
        └── SKILL.md
```

### 1.2 Skill = SKILL.md

```markdown
---
name: read-diagram
description: >
  Extract structured information from images and diagrams.
  Use when the user provides a screenshot, architecture diagram,
  flowchart, or UI mockup and asks for implementation or analysis.
tools: [shell]
when_to_use:
  - User provides an image or diagram
  - User asks "read this diagram" or "implement from this screenshot"
  - User mentions Figma, architecture, flowchart, UI mockup
---

# Read Diagram

## Process
1. If the diagram is a file path, use `shell` with OCR tools
2. Extract: components, connections, labels, hierarchy
3. Output structured representation (JSON or Markdown)
4. If user wants implementation, generate code from the extracted structure

## Reference
See `reference.md` for detailed OCR tool setup and troubleshooting.
```

### 1.3 Skill Lifecycle

```
注册:
  ├── 系统预设: data/skills/ 下的所有 SKILL.md 自动加载
  ├── 用户创建: POST /api/skills { name, content } → 写入 SKILL.md
  └── 市场安装: 从 registry 下载 skill 目录

发现:
  ├── Stage 1: 所有 skill 的 YAML 加载到 system prompt (~100 tokens/skill)
  ├── Stage 2: 主 Agent 判断相关 → 加载完整 SKILL.md 到上下文
  └── Stage 3: 执行过程中需要 → 加载 reference.md / scripts

执行:
  ├── 主 Agent 按 skill 定义的流程执行
  ├── skill 的 scripts/ 通过 shell 工具调用 (不进入上下文)
  └── 复杂 skill 可以 spawn 子 Agent
```

---

## 2. Agent Orchestration

### 2.1 Two Patterns (OpenAI SDK Model)

```
Pattern A: Agent-as-Tool (Manager keeps control)
  Supervisor → call SubAgent → get result → synthesize

Pattern B: Handoff (transfer ownership)
  Supervisor → handoff to Specialist → Specialist responds directly
```

**EverEvo 用 Pattern A 为主**。主 Agent 始终控制对话，子 Agent 作为可调用的"工具"执行有界任务。

### 2.2 Supervisor Agent

```rust
struct SupervisorAgent {
    // 上下文注入
    persona: PersonaProfile,     // 沟通风格 + 思维范式
    memory: HybridFusion,        // RRF 融合检索
    domain: DomainRetriever,     // 领域知识检索
    skills: SkillRegistry,       // 已注册的所有 skill

    // 编排能力
    task_decomposer: TaskDecomposer,  // LLM 拆解任务
    agent_pool: AgentPool,            // 子 Agent 池
}

impl SupervisorAgent {
    async fn run_turn(&mut self, user_message: &str) -> Response {
        // 1. 加载上下文 (Persona + Memory + Domain + Skills)
        let ctx = self.load_context(user_message);

        // 2. 分析任务 → 决定策略
        let strategy = self.analyze(user_message, &ctx);
        // strategy: DirectAnswer | DecomposeToSubtasks | CallSkill

        match strategy {
            DirectAnswer => self.generate_response(&ctx),
            DecomposeToSubtasks(subtasks) => {
                // 3. 并行执行子任务
                let results = self.agent_pool.execute_parallel(subtasks).await;
                // 4. 汇总结果
                self.synthesize(results, &ctx)
            }
            CallSkill(skill_name) => {
                // 加载 skill 完整内容 → 按流程执行
                self.execute_skill(skill_name, &ctx).await
            }
        }
    }
}
```

### 2.3 SubAgent

```rust
struct SubAgent {
    id: Uuid,
    task: TaskDescription,
    context: AgentContext,  // 继承自主 Agent 的 Persona + Memory + Domain
    tools: Vec<ToolName>,   // 最小权限工具集 (CrewAI task-level scoping)
    sandbox: SandboxSession,
    max_turns: usize,       // 子 Agent 限制 5 turns (防无限循环)
    timeout: Duration,      // 默认 5 分钟
}

impl SubAgent {
    async fn execute(&self) -> SubAgentResult {
        // 1. 独立 ReAct loop (max 5 turns)
        // 2. 独立沙箱 (data/sandbox/{id}/)
        // 3. 独立审计 (audit.jsonl)
        // 4. 返回结构化结果
    }
}
```

### 2.4 SubAgent Lifecycle

```
SPAWN:
  task = "审查 crates/everevo-sandbox 的安全问题"
  context = { persona: "简洁直接, 代码优先", memory: [sandbox-confirm-flow, ...] }
  tools = [shell, memory]   ← 不需要 download/bootstrap
  timeout = 5min

RUN:
  SubAgent ReAct loop:
    Turn 1: LLM → "先看目录结构" → shell: ls crates/everevo-sandbox/src/
    Turn 2: LLM → "读 provider.rs" → shell: cat provider.rs
    Turn 3: LLM → "发现 lock().unwrap() 风险" → 记录
    Turn 4: LLM → "总结发现" → 输出结果

RETURN:
  result = {
    stdout: "安全审查完成, 发现 3 个问题: ...",
    exit_code: 0,
    tool_calls: 4,
    audit: "data/sandbox/{id}/audit.jsonl"
  }

DESTROY:
  - 清理沙箱
  - 子 Agent 实例销毁
```

---

## 3. Workflow = Skill Composition, NOT Separate Engine

### 3.1 为什么不做独立工作流引擎

Anthropic 和 OpenAI 的共识：**Skill 本身就是工作流**。

```
传统想法:
  用户 → 拖拽节点 → 创建工作流 → Agent 执行

正确做法:
  用户 → 说需求 → SupervisorAgent 自动选 Skill/拆任务/分派子Agent
```

"读 Figma 设计稿并生成代码" 不是一个需要用户提前配置的工作流。它就是一个 Skill——包含了"怎么读图、怎么提取结构、怎么生成代码"的完整流程。Agent 看到用户说"帮我实现这个设计稿"，自动匹配 `read-diagram` skill，按 skill 定义的流程执行。

### 3.2 Skill 可以链式组合

```
用户: "审查这个项目，生成报告"

SupervisorAgent 分析:
  ├── 子任务 1: 扫描目录结构 → 直接用 shell
  ├── 子任务 2: 代码审查 → spawn 子Agent + code-review skill
  ├── 子任务 3: 安全审查 → spawn 子Agent + security-review skill
  └── 子任务 4: 生成报告 → 汇总结果 + report-generation skill

Skill "code-review":
  ├── Step 1: 读文件 (shell tool)
  ├── Step 2: 检查模式 (pattern matching, embedded in SKILL.md)
  ├── Step 3: 生成反馈 (LLM, guided by SKILL.md instructions)
  └── Step 4: 输出结构化结果

Skill "security-review":
  ├── Step 1: 审计日志分析 (shell audit.jsonl)
  ├── Step 2: 模式检测 (dangerous patterns from permission.rs)
  ├── Step 3: 风险评估 (LLM + reference.md 的安全标准)
  └── Step 4: 输出发现列表
```

### 3.3 用户如何参与

```
预设 Skill:
  ├── data/skills/read-diagram/
  ├── data/skills/code-review/
  ├── data/skills/download-files/
  └── ... (系统预设, 覆盖常见场景)

用户创建 Skill:
  POST /api/skills { "name": "my-deploy-flow", "content": "..." }
  → 写入 data/skills/my-deploy-flow/SKILL.md
  → 以后每次对话自动可用

用户不需要"编排工作流":
  ├── 不需要拖拽连线
  ├── 不需要预设流程
  └── 只需要说"帮我部署", Agent 自动找 my-deploy-flow skill 执行
```

---

## 4. Personality Integration

### 4.1 人格的两个维度

| 维度 | 内容 | 存储 | 注入方式 |
|------|------|------|---------|
| **Persona Profile** | 沟通风格 + 思维范式 | data/memory/persona/profile.json | PersonaStage → system prompt (显式) |
| **Skill Persona** | 特定场景的"大腕"角色 | data/skills/{name}/SKILL.md 的 persona 字段 | Skill 激活时注入 |

### 4.2 思维范式绑定人格吗？

**不绑定。**

PRISM 论文 (IJCNLP 2025): "Expert persona improves alignment but degrades accuracy."

人格和思维范式分两个 Stage 注入，独立控制：

```
ContextPipeline:
  priority 0: SystemPromptStage
  priority 1: PersonaStage        ← "说话简洁直接，先给代码再解释"
  priority 2: ThinkingParadigm    ← "自顶向下分解问题" (独立于人格)
  priority 3: MemoryStage
  priority 5: SessionMetadata
  priority 80: ConversationHistory
```

### 4.3 "大腕人格"= 带 persona 的 Skill

```
data/skills/rust-expert/
  SKILL.md:
    ---
    name: rust-expert
    persona: >
      You are a senior Rust systems engineer. Use precise technical language.
      Prefer safe Rust patterns. Cite the Rust Reference when possible.
      Never use unwrap() in production code recommendations.
    ---
    当用户问 Rust 问题时，激活这个 skill。
    Agent 自动以 Rust 专家的语气和知识体系回答。
```

---

## 5. Implementation Phases

### Phase 4a: Foundation (本周)
```
[ ] SkillRegistry: 扫描 data/skills/ → 加载 YAML → 构建索引
[ ] SkillStage: 注入可用 skill 列表到 system prompt (Stage 1 only)
[ ] SupervisorAgent: 任务分析 + 策略选择 (DirectAnswer/Decompose/CallSkill)
[ ] SubAgent: 独立 ReAct loop + 上下文继承 + 沙箱隔离
[ ] AgentPool: spawn/execute/await/timeout/destroy
[ ] TaskDecomposer: LLM 驱动的任务拆解
```

### Phase 4b: Skills (下周)
```
[ ] 预设 Skills: read-diagram, code-review, download-files, project-init
[ ] Skill 激活: 主 Agent 判断 → load full SKILL.md → execute
[ ] 用户自定义 Skill: POST /api/skills CRUD
[ ] Skill 内的 scripts/ 执行 (通过 shell tool, 不进上下文)
```

### Phase 4c: Persona + Polish (后续)
```
[ ] PersonaStage: 注入 communication_style + thinking_paradigm
[ ] Skill Persona: SKILL.md 的 persona 字段
[ ] 子 Agent 人格继承
[ ] Exe→Re-plan→Execute loop
[ ] 前端 Agent 面板 (查看运行中的 Agent, 审计日志)
```