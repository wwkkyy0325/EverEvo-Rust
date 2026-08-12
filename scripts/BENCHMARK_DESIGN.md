# EverEvo Agent 基准测试设计文档

## 设计原则

1. **评分客观**: 不用 LLM 互评（EQ-Bench 例外，使用所有人共用的同一个 judge）
2. **沙箱隔离**: 所有测试在 Docker 容器或独立 TempDir 中，不碰生产数据
3. **可复现**: temperature=0.0, 固定 seed, 记录完整参数
4. **防作弊**: 使用闭源测试集 + 私有 ground truth

---

## 测试矩阵

```
┌──────────────────┬────────────────┬──────────────────┬──────────────┐
│ 能力维度          │ 权威基准        │ 评分方式           │ 客观性       │
├──────────────────┼────────────────┼──────────────────┼──────────────┤
│ 工具调用 (核心)   │ BFCL v3/v4     │ AST 结构匹配       │ ★★★★★ 客观  │
│ Agent 综合        │ AgentBench FC  │ 8环境加权得分       │ ★★★★★ 客观  │
│ 代码工程          │ SWE-bench Ver. │ Docker test pass   │ ★★★★★ 客观  │
│ 检索+RAG          │ BEIR + RAGAS   │ NDCG + Context Rec │ ★★★★★ 客观  │
│ 情感/对话          │ EQ-Bench v3    │ LLM judge (共用)   │ ★★★☆☆ 半客观 │
│ 通用知识           │ MMLU-Pro       │ 选择题正确率        │ ★★★★★ 客观  │
└──────────────────┴────────────────┴──────────────────┴──────────────┘
```

---

## 一、BFCL — 工具调用 (最优先)

### 为什么测
EverEvo 有 22 个 tool，tool 选择准确性直接决定 agent 能力。BFCL 是 ICML 2025 发表的
最权威 function calling 基准，被 GPT-4、Claude、Gemini 等所有主流模型使用。

### 评分方式 (完全客观)
**AST 结构匹配**: 解析模型输出 → 构建抽象语法树 → 与 ground truth AST 逐节点对比。
不需要执行代码，不需要 LLM 评分。

### 子类别
| 类别 | 题量 | 测什么 |
|------|------|--------|
| simple | ~200 | 单次函数调用 |
| multiple | ~200 | 多次顺序调用 |
| parallel | ~200 | 并行独立调用 |
| parallel_multiple | ~200 | 并行多函数 |
| multi_turn | ~200 | 多轮对话调用 |
| live_simple | ~200 | 真实 API 调用 |

### 怎么跑
```bash
pip install bfcl-eval
# 注册 EverEvo 作为评测对象 (通过 chat API 适配器)
bfcl-eval --model everevo --api-base http://127.0.0.1:13456/api/chat
```

### 对比基线
| 模型 | Overall Accuracy |
|------|:---:|
| GPT-4o | 88.5% |
| Claude 3.5 Sonnet | 85% |
| Gemini 1.5 Pro | 82% |
| Llama 3.1 405B | 78% |

### 需要做的适配工作
- 写一个 `everevo_adapter.py` 把 BFCL 的函数调用 prompt 转成 EverEvo chat API 格式
- 从 SSE 响应中提取 tool_call_start/end 事件作为函数调用结果
- 传给 BFCL 的 AST 评估器

---

## 二、AgentBench FC — Agent 综合能力

### 为什么测
清华 ICLR 2024 发表，8 个 Docker 化环境，是最全面的 agent 基准。
2025 年 10 月发布了 FC (Function Calling) 版本，支持 Docker Compose 一键部署。

### 8 个环境 & 评分
| 环境 | 类型 | 评分指标 |
|------|------|---------|
| OS (Ubuntu bash) | 命令行 | Success Rate |
| Database (MySQL) | SQL | Success Rate |
| Knowledge Graph | SPARQL | Answer F1 |
| Web Shopping | 电商 | Reward Score |
| Web Browsing | 浏览器 | Step Success |
| Card Game | 策略 | Win Rate |
| Puzzles | 推理 | Progress |
| Household | 规划 | Completion |

### 评分方式 (完全客观)
每个环境有独立的自动评分脚本，基于 ground truth 比对。
总分 = 8 个环境归一化后的加权平均。

### 怎么跑
```bash
git clone https://github.com/THUDM/AgentBench
cd AgentBench
# 用 Docker Compose 启动所有环境
docker compose -f extra/docker-compose.yml up -d
# 注册 EverEvo adapter
python run.py --model everevo --api-base http://127.0.0.1:13456/api/chat
```

### 对比基线 (FC 版本)
| 模型 | Overall |
|------|:---:|
| Qwen2.5-32B | 70.4% |
| GLM-4-9B | 65.0% |
| GPT-4 (original) | 40% |

### 需要做的适配工作
- 写 `everevo_agentbench_adapter.py` 把 AgentBench 的各环境 prompt 转成 EverEvo chat API
- 处理多轮交互 (AgentBench 是状态化的多轮对话)
- Docker 环境完全隔离，零系统风险

---

## 三、SWE-bench Verified — 代码工程

### 为什么测
最权威的代码 agent 基准，500 个真实 GitHub issue，Docker 沙箱执行。
Claude Code 72.7%, Devin 55%, GPT-4 约 5%。

### 评分方式 (完全客观)
- Agent 在一个 Docker 容器中修改代码
- 跑 `FAIL_TO_PASS` 和 `PASS_TO_PASS` 测试
- 全部通过 = 成功，否则 = 失败
- **pass@1** 指标: 一次提交的成功率

### 怎么跑
```bash
git clone https://github.com/SWE-bench/SWE-bench.git
# 需要 ~100GB Docker 镜像
python -m swebench.harness.run_evaluation \
  --dataset princeton-nlp/SWE-bench_Verified \
  --predictions everevo_predictions.json \
  --max_workers 4
```

### 对比基线
| Agent | pass@1 |
|-------|:---:|
| Claude Code | 72.7% |
| Devin | 55.0% |
| OpenAI Codex | 53.0% |
| SWE-Agent + GPT-4 | 12.5% |

### 需要做的适配工作
- 写 `everevo_swebench_adapter.py` 把 GitHub issue 转成 EverEvo prompt
- 监控 agent 的 write_file / shell 工具调用
- 从容器 diff 收集代码改动

---

## 四、EQ-Bench v3 — 情感/对话

### 为什么测
唯一的情感智能基准，45 个长场景，LLM judge 评分。
注意: 这是唯一用 LLM judge 的，但它"所有人用同一个 judge (Claude)"，
所以基线可对比——不是我们自己评自己。

### 评分方式
- **Rubric 评分 (0-100)**: 6 个维度 (Empathy, Pragmatic EQ, Insight, Social Dexterity,
  Emotional Reasoning, Message Tailoring)
- **Elo 排名**: 两两盲评，位置互换去偏
- 所有人用同一个 judge (Claude Opus/Sonnet) → 结果可横向对比

### 怎么跑
```bash
git clone https://github.com/EQ-bench/eqbench3.git
# 通过 EverEvo chat API 跑 45 个场景
python run_eqbench.py --adapter everevo --api-base http://127.0.0.1:13456/api/chat
```

### 对比基线
| 模型 | Rubric (0-100) | Elo |
|------|:---:|:---:|
| Gemini 3 Pro | 87 | 1643 |
| Claude 3.5 Sonnet | 82 | 1550 |
| GPT-4o | 78 | 1480 |

### 需要做的适配工作
- 写 `everevo_eqbench_adapter.py` 把 45 个场景发送到 EverEvo chat API
- 收集完整回复 (四段式: 感受 → 推理 → 回复 → 反思)

---

## 五、RAG 检索 — BEIR + RAGAS (已完成)

上一轮已实现:
- NFCorpus 基准: NDCG@10 = 0.31 (Dummy), 0.31 (ONNX)
- SciFact 基准: NDCG@10 = 0.64 (ONNX)
- RAGAS 评估脚本: `scripts/ragas_eval.py`

---

## 执行优先级 & 投入预估

| 优先级 | 基准 | 适配工作量 | 运行耗时 | 产出 |
|--------|------|:---:|:---:|------|
| ⭐⭐⭐⭐⭐ | **BFCL v3** | 2-3天 | 2h | 工具调用准确率 vs GPT-4/Claude |
| ⭐⭐⭐⭐ | **EQ-Bench v3** | 1天 | 30min | 情感能力分数 + Elo |
| ⭐⭐⭐⭐ | **AgentBench FC** | 3-4天 | 4h | 8环境综合得分 |
| ⭐⭐⭐ | **SWE-bench Ver** | 4-5天 | 4h | 代码工程能力 |
| ✅ 完成 | **BEIR + RAGAS** | 已完成 | 20min | NDCG + Context Recall |

---

## 防作弊措施

| 措施 | BFCL | AgentBench | SWE-bench | EQ-Bench |
|------|:---:|:---:|:---:|:---:|
| 闭源/私有测试集 | ✅ V4 私有 | ✅ 部分开放 | ✅ 可去污染 | ✅ 45题公开但 judge 盲评 |
| 确定性输出 temp=0 | ✅ | ✅ | ✅ | ✅ |
| Docker 沙箱 | ✅ AST 不需执行 | ✅ 全 Docker | ✅ 全 Docker | N/A 纯文本 |
| 评分防操纵 | AST 结构匹配 | 自动脚本 | test pass/fail | 位置互换盲评 |
| 基线可比性 | 公开 leaderboard | 公开论文数据 | 公开 leaderboard | 公开 Elo 榜 |

---

## 实施路线图

```
Week 1: BFCL 适配 + 首次跑通
  ├── 写 everevo_adapter.py (BFCL prompt → chat API → SSE tool_call 提取)
  ├── 跑 simple + multiple 子集
  └── 出第一份工具调用报告

Week 2: EQ-Bench + AgentBench
  ├── EQ-Bench: 45 场景全量跑，拿到 Rubric + Elo
  └── AgentBench: Docker 环境搭建 + adapter

Week 3: SWE-bench + 总结
  ├── SWE-bench: 适配 + 首次跑 (可先跑 50 题子集)
  └── 汇总报告: EverEvo vs 所有公开 baseline
```
