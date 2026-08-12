# EverEvo Agent 权威基准测试方案

## 行业标准

所有主流 agent (Claude Code, Devin, GPT-4, Gemini) 都在这四个基准上发布成绩:

| 基准 | 发表 | 评分方式 | Claude Code | Devin | GPT-4 |
|------|------|---------|:---:|:---:|:---:|
| **SWE-bench Verified** | NeurIPS 2024 | Docker test pass/fail | 72.7% | 55.0% | 1.7% |
| **GAIA** | ICLR 2024 | Exact match | 未公开 | 未公开 | 15% |
| **BFCL** | ICML 2025 | AST 结构匹配 | 未公开 | 未公开 | 88.5% |
| **AgentBench** | ICLR 2024 | 8环境加权 | 未公开 | 未公开 | 40% |

**结论**: SWE-bench 是代码 agent 的唯一硬通货。BFCL 是 tool-use 标准。GAIA 是通用推理标准。

---

## 一、BFCL — 最优先，零摩擦

### 能不能跑？完全没问题

BFCL 的标准评测流程是:**直接调 LLM API，不经过 agent 框架**。它发 function definitions 给模型，检查模型返回的 function call JSON 是否正确。

对于 EverEvo，我们**直接对 glm-5.2 / deepseek-v4-pro 的 API 跑 BFCL**。这不是绕过 agent——这正是 BFCL 设计的用法。BFCL 测的是"你的 agent 的大脑会不会正确调函数"，这是 tool-use 能力的基础指标。

### 权威性保证

| 项 | 如何保证 |
|----|---------|
| 测试集 | BFCL v4 官方数据集，5,106 题，闭源 |
| 评分 | 官方 AST 评估器，纯结构匹配，零主观 |
| 可复现 | temperature=0, 固定 prompt template |
| 可比性 | 和 BFCL 公开 leaderboard 上所有模型同台对比 |
| 透明 | 公布 raw results + 完整运行参数 |

### 实施

```bash
pip install bfcl-eval
# 对 glm-5.2 (EverEvo 主模型)
bfcl generate --model glm-5.2 --api-key xxx --base-url https://open.bigmodel.cn/api/anthropic
bfcl evaluate --model glm-5.2

# 对 deepseek-v4-pro (EverEvo 备选模型)
bfcl generate --model deepseek-v4-pro --api-key xxx --base-url https://api.deepseek.com/anthropic
bfcl evaluate --model deepseek-v4-pro
```

**报告格式**: "EverEvo (glm-5.2 backend) BFCL v4 Overall Accuracy: XX.X%"

### 潜在问题 & 对策

| 问题 | 对策 |
|------|------|
| API 格式兼容 | Anthropic-compatible API 已在 BFCL 支持列表中 |
| 多语言 (Java/JS) | BFCL v4 有 Java/JS 题，如果 glm-5.2 不支持则只跑 Python 子集，公开声明 |
| 成本 | ~$2-5 的 API 费用 (5,106 题 × 短回复) |

---

## 二、SWE-bench Verified — 代码 Agent 旗舰

### 能不能跑？需要适配工作

SWE-bench 需要 agent 在 Docker 容器里修改代码。EverEvo 的流程是:
1. 收到 GitHub issue → 启动 session
2. Agent 用 read_file/write_file/bash 工具操作代码
3. Agent 生成 patch

需要写一个 adapter 把 SWE-bench 的 issue 转成 EverEvo chat prompt，让 agent 在容器里工作。

### 权威性保证

| 项 | 如何保证 |
|----|---------|
| 测试集 | SWE-bench Verified 官方 500 题 |
| 评分 | 官方 harness: `pip install swebench` → `python -m swebench.harness.run_evaluation` |
| 沙箱 | Docker 容器，无网络，每实例独立 |
| 防作弊 | 测试补丁隐藏，PASS_TO_PASS 回归检测 |

### 实施

```bash
git clone https://github.com/SWE-bench/SWE-bench.git
pip install swebench
# 1. Agent 生成 patches
python everevo_swebench.py --dataset SWE-bench_Verified --max_workers 4
# 2. 官方 harness 评估
python -m swebench.harness.run_evaluation \
  --dataset princeton-nlp/SWE-bench_Verified \
  --predictions everevo_predictions.json
```

### 潜在问题 & 对策

| 问题 | 对策 |
|------|------|
| Docker 镜像 ~100GB | 分批下载，4 worker 并行 |
| 每题需要 2-10 min | 500 题 × 4 worker ≈ 4-8 小时 |
| glm-5.2 代码能力可能弱 | 预期分数不会高(10-20%)，但这正是真实结果，不美化 |
| EverEvo 工具适配 | 需要确保容器内有 bash/read_file/write_file 可正常调用 |

---

## 三、GAIA — 通用推理 (可选)

GAIA 测试多步推理: 搜索 → 解析 → 计算 → 回答。EverEvo 有 web_search/web_fetch 工具，理论上可跑。

### 潜在问题

GAIA 需要文件解析 (Excel, PDF, 图片)、音频处理、复杂 web 交互。
EverEvo 的 web_search 可能不够精细。**建议先跑 Level 1 (53 题) 快速验证**。

### 实施

```bash
git clone https://huggingface.co/datasets/gaia-benchmark/GAIA
# EverEvo adapter: issue → chat API → exact match scoring
```

---

## 四、AgentBench — 广度覆盖 (可选)

8 个 Docker 化环境，最全面。但搭建复杂 (Docker Compose + Redis + 多 worker)。

**建议**: BFCL + SWE-bench 跑完后，再考虑 AgentBench 作为补充。

---

## 最终推荐: 两步走

### Step 1: 本周可交付 (BFCL)

```bash
# 1. 安装
pip install bfcl-eval

# 2. 跑 glm-5.2 (EverEvo 主模型)
bfcl generate --model glm-5.2 \
  --api-key "$GLM_API_KEY" \
  --base-url "https://open.bigmodel.cn/api/anthropic"

# 3. 评分
bfcl evaluate --model glm-5.2

# 4. 跑 deepseek-v4-pro (备选)
bfcl generate --model deepseek-v4-pro \
  --api-key "$DEEPSEEK_API_KEY" \
  --base-url "https://api.deepseek.com/anthropic"
bfcl evaluate --model deepseek-v4-pro
```

**产出**: 一份可直接和 BFCL leaderboard 对比的分数。

### Step 2: 下周可交付 (SWE-bench)

写 EverEvo SWE-bench adapter → 跑 500 题 → 官方 harness 评估。

---

## 对比表: 我们的方案 vs 其他 Agent

| Agent | BFCL | SWE-bench | GAIA | AgentBench |
|-------|:---:|:---:|:---:|:---:|
| Claude Code | ❓ | 72.7% | ❓ | ❓ |
| Devin | ❓ | 55.0% | ❓ | ❓ |
| GPT-4 + tools | 88.5% | 1.7% | 15% | 40.1% |
| **EverEvo (计划)** | ✅ 本周 | ✅ 下周 | ⏳ 可选 | ⏳ 可选 |

---

## 防作弊清单

- [ ] BFCL: 使用官方 `bfcl-eval` 包，禁止修改评估逻辑
- [ ] BFCL: temperature=0.0，单次运行，不刷 best-of-N
- [ ] SWE-bench: 使用官方 harness，禁止访问测试补丁
- [ ] SWE-bench: 公布完整 prompt template 和 agent 配置
- [ ] 所有: 公布 raw output，允许第三方复现
- [ ] 所有: 不 cherry-pick 结果，报告全部数据
