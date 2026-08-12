# EverEvo Agent 权威基准测试方案 v3

## 核心区分

```
测 Agent (EverEvo):  prompt → POST /api/chat → context pipeline → agent loop → 22 tools → SSE
测 Model (裸 LLM):   prompt → 裸 LLM API → response
```

**只有通过 EverEvo chat API 的才算测 Agent。** 裸调 API 的分数只能作为 "Agent 大脑基础分" 参考，不是 Agent 成绩。

---

## 真正测 Agent 的 5 个权威基准

### 1. Terminal-Bench 2.0 ⭐ 最优先 (本周可交付)

| 维度 | 详情 |
|------|------|
| **发表** | 2025, Laude Institute / Harbor Framework |
| **测什么** | 89 个真实终端任务：编译、调试、系统管理、数据处理 |
| **评分** | 二进制 pass/fail，100% 客观，不依赖 LLM judge |
| **运行方式** | Harbor → Docker 容器 → everevo-server 在容器内运行 |
| **对比基线** | GPT-5.3-Codex 77.3%, LangChain 66.5%, Claude Code ~70-80% |
| **状态** | adapter 已写好，job config 已就绪 |

### 2. SWE-bench Verified ⭐ 代码 Agent 金标准

| 维度 | 详情 |
|------|------|
| **发表** | NeurIPS 2024, Princeton |
| **测什么** | 500 个真实 GitHub issue，Docker 容器里修代码 |
| **评分** | `pytest` pass/fail，100% 客观 |
| **对比基线** | Claude Code 72.7%, Devin 55%, SWE-Agent+GPT-4 12.5% |

### 3. GAIA ⭐ 多步推理+工具编排

| 维度 | 详情 |
|------|------|
| **发表** | ICLR 2024, Meta |
| **测什么** | 165 个多步推理题：search → parse → compute → answer |
| **评分** | Exact Match，100% 客观 |
| **对比基线** | CustomGPT.ai 93.4%, GPT-4+plugins 15% |

### 4. AgentBench ⭐ 最全面覆盖

| 维度 | 详情 |
|------|------|
| **发表** | ICLR 2024, 清华 THUDM |
| **测什么** | 8 个 Docker 环境 |
| **评分** | 每个环境独立自动评分，加权平均 |
| **对比基线** | Qwen2.5-32B 70.4%, GLM-4-9B 65%, GPT-4 40% |

### 5. TAU-bench ⭐ 策略遵循

| 维度 | 详情 |
|------|------|
| **发表** | 2025, Sierra Research |
| **测什么** | 航空/零售客服场景，policy adherence |
| **评分** | 数据库状态对比 ground truth，100% 客观 |

---

## ❌ 不是 Agent 基准的

| 基准 | 为什么不是 |
|------|-----------|
| **BFCL** | 裸调 LLM API 测 function calling，绕过 Agent 框架 |
| **MMLU/MATH** | 无工具调用，无 agent loop |
| **HumanEval** | 无文件操作、bash、context pipeline |
| **EQ-Bench** | 不涉及 tool orchestration |

---

## Terminal-Bench 2.0 快速启动

```powershell
# 前置条件：启动 Docker Desktop
# Step 1: 构建 Linux 二进制
bash scripts/build_linux_binary.sh

# Step 2: 冒烟测试（1 个任务）
harbor run --dataset terminal-bench@2.0 \
  --agent scripts.everevo_harbor_agent:EverEvoAgent \
  --model glm-5.2 --n-tasks 1 --debug

# Step 3: 全量跑（89 任务）
harbor run --config scripts/terminal_bench_config.yaml
```
