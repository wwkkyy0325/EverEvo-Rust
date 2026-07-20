# EverEvo Persona System — Design Document

## References

| # | Source | Key Insight |
|---|--------|-------------|
| 1 | LD-Agent (NAACL 2025) | 3-module: Event Perception + Dynamic Persona Extraction (user & agent) + Response Generation |
| 2 | O-Mem (OPPO 2025) | 3-layer memory: Persona(长期属性) + Working(上下文) + Episodic(事件触发回忆) |
| 3 | PersonaMem-v2 (2025) | RL-trained agentic memory, 2K token compact profile for 32K context, user-auditable |
| 4 | JARVIS (ACM 2025) | Dual-hemisphere: subjective(personalization) + objective(rational), dream-based adaptation |
| 5 | SPARK (2025) | Persona Coordinator + tripartite memory (working/episodic/semantic) |
| 6 | Chameleon LLMs (EMNLP 2025) | LLM adapts personality to user (Agreeableness/Extraversion most affected) |

---

## 0. Position in Architecture

```
┌──────────────────────────────────────────────────┐
│  Agent 协同层  (主Agent/子Agent编排)              │  ← 下一阶段
├──────────────────────────────────────────────────┤
│  人格系统      (沟通风格 + 思维范式 + 行为模式)    │  ← 本设计
├──────────────────────────────────────────────────┤
│  记忆系统      (facts/diary/wiki)                │
│  领域知识库    (domains/documents)               │  ← 已完成
├──────────────────────────────────────────────────┤
│  基础设施      (db/sandbox/vector/kg/retrieval)   │
└──────────────────────────────────────────────────┘
```

**人格不是独立的顶层**。它是 Agent 协同层的**前置注入层**——每个 Agent 在推理前，人格系统通过 ContextPipeline 注入当前用户的沟通偏好、思维模式、行为习惯。

---

## 1. Three Dimensions of Persona

### 1.1 Communication Style (沟通风格)

从对话历史中提取：

| 维度 | 示例 | 提取方式 |
|------|------|---------|
| 语言偏好 | 中文为主 / 英文为主 / 混合 | 统计对话语言分布 |
| 简洁度 | 简短直接 / 详细解释 | 消息长度中位数 |
| 正式度 | 正式("请帮我") / 随意("帮我搞一下") | LLM判断 |
| 代码优先 | 先给代码 / 先解释 | 对话模式分析 |
| 反馈风格 | 直接指出错误 / 委婉建议 | 情感分析 |

### 1.2 Thinking Paradigm (思维范式)

从用户决策和问题解决方式中提取：

| 维度 | 示例 | 提取方式 |
|------|------|---------|
| 问题分解 | 自顶向下 / 自底向上 | 任务描述结构分析 |
| 理论 vs 实践 | 先问原理 / 直接要结果 | 问题类型分布 |
| 细节关注 | 关注边界条件 / 只关注主流程 | LLM分析 |
| 决策速度 | 快速决定 / 反复权衡 | 对话轮次分析 |

### 1.3 Behavioral Patterns (行为模式)

从用户的实际操作和决策中提取：

| 维度 | 示例 |
|------|------|
| 技术栈偏好 | Python > JS, Rust > Go |
| 工作流程 | 先写测试 / 先写实现 |
| 工具偏好 | VSCode / CLI / Web |
| 时间模式 | 深夜工作 / 工作日活跃 |

---

## 2. How Persona Integrates with Agentic AI

```
每个 Agent turn 的上下文注入顺序:

1. System Prompt     ← Agent 基础能力定义
2. Persona Profile   ← 用户沟通风格 + 思维范式  ← 人格注入
3. Memory Index      ← 相关事实 (facts)
4. Domain Context    ← 相关领域知识
5. Session Metadata  ← 当前会话环境
6. Conversation      ← 对话历史
7. User Message      ← 最新消息
```

**关键设计**：人格在 Memory 和 Domain 之前注入。因为人格决定了"以什么方式回答"，而 Memory/Domain 决定了"回答什么内容"。

---

## 3. Persona Extraction Pipeline

复用记忆系统的 Dreaming Pipeline，但输出到 `data/memory/persona/`：

```
DEEP phase (记忆管线完成后)
  ↓
Persona Extraction:
  1. 读取最近 N 轮对话 (user messages + agent responses)
  2. LLM 提取人格维度:
     - Communication Style 变化
     - Thinking Paradigm 模式
     - Behavioral Pattern 线索
  3. 与现有人格 profile 对比:
     - 一致 → 强化置信度 (confidence += 0.1)
     - 变化 → 标记待确认 (pending_change)
     - 矛盾 → LLM 判断是否更新
  4. 写入 data/memory/persona/profile.json
```

### Profile 格式

```json
{
  "user_id": "default",
  "updated_at": "2026-07-19T10:00:00Z",
  "confidence": 0.72,
  "communication_style": {
    "language": "zh-CN",
    "verbosity": "concise",
    "formality": "casual",
    "code_first": true,
    "feedback_style": "direct"
  },
  "thinking_paradigm": {
    "decomposition": "top-down",
    "theory_vs_practice": "practice",
    "detail_orientation": "medium"
  },
  "behavioral_patterns": {
    "tech_stack": ["Rust", "TypeScript", "Python"],
    "workflow": "test-first",
    "active_hours": "late_night"
  },
  "system_prompt_injection": "用户偏好简洁直接的回复，先给代码再解释。使用中文交流。当用户提出需求时优先给出可执行的方案而非理论分析。"
}
```

---

## 4. PersonaStage (ContextPipeline Integration)

```rust
pub struct PersonaStage {
    profile_path: PathBuf,
}

impl ContextStage for PersonaStage {
    fn priority(&self) -> i32 {
        1  // 在 system prompt(0) 之后，memory(3) 之前
    }

    fn build(&self, ctx: &ContextBuildContext) -> Option<ContextFragment> {
        let profile = load_profile(&self.profile_path)?;

        let injection = format!(
            "## User Persona\n\
             {system_prompt_injection}\n\n\
             Communication: {style}\n\
             Thinking: {paradigm}\n\
             Tech stack: {stack}",
            system_prompt_injection = profile.system_prompt_injection,
            style = format_style(&profile.communication_style),
            paradigm = format_paradigm(&profile.thinking_paradigm),
            stack = profile.behavioral_patterns.tech_stack.join(", "),
        );

        Some(ContextFragment {
            label: "Persona Profile".into(),
            messages: vec![LlmMessage::user(&injection)],
        })
    }
}
```

---

## 5. Threading Paradigm into Agent

### 5.1 思维范式注入

Agent 在规划任务时，PersonaStage 注入的思维范式会影响它的决策：

```
用户偏好 top-down 分解:
  Agent 收到"做记忆系统"
    → 先列出整体架构 → 拆分子任务 → 逐个实现

用户偏好 practice-first:
  Agent 收到"解释 RRF"
    → 先给代码示例 → 再解释原理
```

### 5.2 子 Agent 人格继承

主 Agent 派发任务给子 Agent 时，人格 profile 随任务一起传递：

```
主 Agent:
  task = "实现 domain classifier"
  persona_context = load_profile()  // 用户偏好 Rust, concise, code-first

子 Agent:
  system_prompt += persona_context
  → 用 Rust 实现 → 给代码而非长篇解释 → 直接返回结果
```

---

## 6. Implementation Priority

| Phase | 内容 | 复杂度 |
|-------|------|--------|
| **Phase 4a** (与 Agent协同同步) | PersonaStage + profile.json + ContextPipeline注入 | 轻量，复用记忆管线 |
| **Phase 4b** | Persona Extraction (DEEP后提取行为模式) | 中等，LLM提取 |
| **Phase 4c** | 思维范式注入 + 子Agent人格继承 | 与协同层耦合 |

---

## 7. Recommendation

**人格和 Agent 协同可以同步做**。人格系统的 Phase 4a 只需要一个 PersonaStage（~100行代码），在 Agent 协同开发过程中并行推进。理由：

1. **PersonaStage 就是一个 ContextStage**——和 MemoryStage 完全一样的模式，半小时能写完
2. **Agent 协同层需要人格注入**——子 Agent 没有 persona context，行为会和主 Agent 不一致
3. **profile.json 可以先手工维护**——LLM 自动提取是锦上添花，手动写一个 profile 就能验证整个链路
