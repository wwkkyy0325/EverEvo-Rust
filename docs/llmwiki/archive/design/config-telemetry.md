# EverEvo Configuration Center & Telemetry — Design Document
> **状态**:⛔ 已过时(归档)— 已被 [08-config-and-state.md](../../architecture/08-config-and-state.md) + [13-telemetry.md](../../architecture/13-telemetry.md) 取代
> **来源**:2026-07-19 | **归档**:2026-08-12。本文是设计愿景,以代码现状文档为准。

---


## References

| # | Source | Key Insight |
|---|--------|-------------|
| 1 | Claude Code settings.json | 5-level hierarchy: Managed→CLI→Local Project→Shared Project→Global; hot-reload vs restart |
| 2 | LaunchDarkly AI Configs | Decouple prompts/models/params from code; progressive rollouts + instant rollback |
| 3 | Langfuse (MIT, 17.9k★) | OpenTelemetry-native; prompt CMS with versioning + production tags; RAGAS integration |
| 4 | Unleash + OpenFeature | CNCF vendor-neutral flag abstraction; stale flag detection + lifecycle management |
| 5 | RAGAS (2025) | Faithfulness, Answer Relevance, Context Precision/Recall — gold standard for RAG eval |
| 6 | Vero eval (Apache 2.0) | Component-level retrieval metrics: Precision@k, MRR, NDCG, Sufficiency |
| 7 | Stochastic Agent Evals (2025) | ICC analysis: multi-trial for statistical significance; single-run accuracy is unreliable |
| 8 | OTel GenAI conventions (2024) | Standardized span attributes: model name, token usage, cache hits, guardrail actions |

---

## 0. Architecture

```
data/config/
├── defaults.toml              ← 出厂默认值 (内置于二进制, 不可改)
├── config.toml                 ← 用户配置 (手动编辑, 覆盖 defaults)
├── experiments/
│   └── {exp_id}.toml          ← A/B 实验定义 (变体参数)
└── overrides/
    └── runtime.toml            ← 运行时覆盖 (API 写入, 最高优先级, 热加载)

data/telemetry/
├── traces/                     ← 每次对话一个 trace JSONL
├── metrics.db                  ← SQLite 聚合查询
└── experiments/                ← 实验结果存储
```

### 配置优先级 (Claude Code 5-level 模型简化版)

```
Priority 1 (最高): API 运行时覆盖    POST /api/config/override
Priority 2:       环境变量           EVEREVO_MODEL, EVEREVO_EFFORT, etc.
Priority 3:       用户配置文件        data/config/config.toml
Priority 4:       系统默认值          data/config/defaults.toml (内置)
```

---

## 1. 配置项全景

```toml
# data/config/config.toml

[model]
provider = "deepseek"           # anthropic | openai | deepseek | ollama
model = "deepseek-v4"           # 模型 ID
effort = "high"                 # low | medium | high | xhigh | max
max_tokens = 4096
temperature = 0.7

[retrieval]
rrf_k = 60                      # RRF 融合常数
fusion_weights = [0.5, 0.3, 0.2]  # vector/fts5/graph 权重
rerank_enabled = false          # 是否启用 cross-encoder 重排
recall_top_k = 50               # 粗召回数量
final_top_k = 5                 # 最终注入数量

[memory]
nudge_turn_threshold = 10       # Nudge 触发轮数
nudge_cooldown_secs = 1800      # Nudge 冷却 (秒)
active_light_interval_hours = 3 # 活跃时 LIGHT 间隔
idle_light_interval_hours = 12  # 空闲时 LIGHT 间隔
max_facts = 200                 # 最大事实数

[domain]
classifier_high_threshold = 0.75  # 归类阈值
classifier_low_threshold = 0.45   # 新建领域阈值
min_docs_for_new_domain = 3       # 最小文档数
long_content_threshold = 10000    # 长内容阈值 (字符)

[agent]
max_turns = 15                 # 主 Agent 最大轮数
subagent_max_turns = 5         # 子 Agent 最大轮数
subagent_timeout_secs = 300    # 子 Agent 超时 (秒)
max_parallel_subagents = 3     # 最大并行子 Agent

[telemetry]
enabled = true                 # 是否启用埋点
sample_rate = 1.0              # 采样率 (1.0 = 全量)
trace_retention_days = 30      # trace 保留天数
auto_cleanup = true            # 自动清理过期数据

[experiments]
active_experiment = ""         # 当前激活的实验 ID
```

---

## 2. 配置中心 API

```
GET    /api/config                    ← 当前生效的全部配置
GET    /api/config/{section}          ← 读取特定 section
PUT    /api/config/{section}          ← 更新 section (写 config.toml)
POST   /api/config/override           ← 运行时覆盖 (不写文件, 热加载)
DELETE /api/config/override/{key}     ← 移除覆盖

# 实验
GET    /api/experiments               ← 列出所有实验
POST   /api/experiments               ← 创建实验
PUT    /api/experiments/{id}/start    ← 激活实验
POST   /api/experiments/{id}/stop     ← 停止实验
GET    /api/experiments/{id}/results  ← 实验结果
```

---

## 3. 埋点体系

### 3.1 埋点位置 (全部 crate)

```
everevo-core:
  context.rs           → pipeline 组装耗时

everevo-agent:
  loop_.rs             → turn 次数, tool_call 次数/类型/成败
  memory/mod.rs        → MemoryStage 检索耗时, recall_k, top_k
  memory/engine.rs     → LIGHT/REM/DEEP 每阶段耗时+状态
  memory/scheduler.rs  → Nudge触发次数, 冷却命中次数
  memory/consolidator.rs→ ADD/UPDATE/DELETE/NOOP 计数
  llm.rs               → API 调用耗时, tokens, 模型名

everevo-sandbox:
  provider.rs          → 命令执行耗时, 确认/拒绝次数, 超时次数
  permission.rs        → Allow/Deny/Confirm 决策分布

everevo-domain:
  lib.rs               → 分类准确率, 检索 recall, 文档处理量

everevo-vector:
  lib.rs               → 向量搜索耗时, top-k 相关性

everevo-kg:
  lib.rs               → 图遍历耗时, 实体/关系数量

everevo-server:
  main.rs              → 请求数, 并发数, 内存/CPU
  routes/chat.rs       → SSE 事件数, 用户消息数
```

### 3.2 数据结构

```sql
-- SQLite schema for telemetry

CREATE TABLE spans (
    id TEXT PRIMARY KEY,
    trace_id TEXT NOT NULL,
    parent_id TEXT,
    name TEXT NOT NULL,           -- "retrieval.search", "agent.turn", "llm.call"
    started_at TEXT NOT NULL,
    duration_ms INTEGER NOT NULL,
    status TEXT NOT NULL,         -- "ok" | "error" | "timeout"
    metadata JSON,                -- {"query": "...", "top_k": 50, ...}
    metrics JSON                  -- {"recall_k": 12, "precision@5": 0.8, ...}
);

CREATE TABLE retrievals (
    id TEXT PRIMARY KEY,
    trace_id TEXT NOT NULL,
    query TEXT NOT NULL,
    source TEXT NOT NULL,         -- "vector" | "fts5" | "graph" | "hybrid"
    recall_k INTEGER NOT NULL,
    precision_at_5 REAL,
    mrr REAL,
    nDCG REAL,
    latency_ms INTEGER NOT NULL,
    experiment_id TEXT,
    variant TEXT
);

CREATE TABLE agent_turns (
    id TEXT PRIMARY KEY,
    trace_id TEXT NOT NULL,
    turn_number INTEGER NOT NULL,
    tool_calls_total INTEGER,
    tool_calls_success INTEGER,
    task_completed INTEGER,       -- 0/1, LLM judged
    plan_steps_total INTEGER,
    plan_steps_completed INTEGER,
    latency_ms INTEGER NOT NULL,
    tokens_input INTEGER,
    tokens_output INTEGER,
    experiment_id TEXT,
    variant TEXT
);

CREATE TABLE experiments (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    status TEXT NOT NULL,         -- "draft" | "active" | "stopped"
    variants JSON NOT NULL,       -- [{"name": "A", "config": {...}}, {"name": "B", "config": {...}}]
    traffic_split JSON,           -- {"A": 0.5, "B": 0.5}
    started_at TEXT,
    stopped_at TEXT
);

CREATE INDEX idx_spans_trace ON spans(trace_id);
CREATE INDEX idx_retrievals_experiment ON retrievals(experiment_id, variant);
CREATE INDEX idx_agent_turns_experiment ON agent_turns(experiment_id, variant);
```

### 3.3 TelemetrySpan API

```rust
// everevo-telemetry/src/lib.rs

pub struct Telemetry {
    db: SqlitePool,
    config: Arc<RwLock<TelemetryConfig>>,
}

impl Telemetry {
    /// Start a new trace for a conversation session.
    pub fn start_trace(&self, session_id: Uuid) -> Trace { ... }

    /// Query aggregated metrics for experiments.
    pub fn experiment_results(&self, experiment_id: &str) -> Result<ExperimentReport> { ... }

    /// Compare two variants with statistical significance.
    pub fn compare_variants(&self, exp_id: &str, metric: &str) -> Result<VariantComparison> { ... }
}

pub struct Trace {
    trace_id: Uuid,
    telemetry: Arc<Telemetry>,
    spans: Vec<Span>,
}

impl Trace {
    /// Start a child span. Auto-closes on drop.
    pub fn span(&mut self, name: &str) -> SpanGuard { ... }
}

pub struct SpanGuard {
    span: Span,
    telemetry: Arc<Telemetry>,
}

impl SpanGuard {
    pub fn with(mut self, key: &str, value: impl Into<Value>) -> Self { ... }
    pub fn metric(mut self, key: &str, value: f64) -> Self { ... }
}

impl Drop for SpanGuard {
    fn drop(&mut self) {
        self.span.duration_ms = self.start.elapsed().as_millis();
        self.telemetry.write_span(&self.span);
    }
}
```

### 3.4 使用示例

```rust
// 一行埋点 — 检索
let mut span = trace.span("retrieval.search")
    .with("query", query)
    .with("sources", "vector+fts5");
let results = hybrid.search(query, 50).await;
span.metric("recall_k", results.len() as f64)
    .metric("latency_ms", elapsed.as_millis() as f64);

// 一行埋点 — Agent turn
let mut span = trace.span("agent.turn")
    .with("turn", turn_number);
let result = agent.execute().await;
span.metric("tool_calls", result.tool_calls.len() as f64)
    .metric("tool_success", result.success_count as f64);
```

---

## 4. A/B 实验

### 4.1 实验定义

```toml
# data/config/experiments/rrf-k-tuning.toml

[experiment]
id = "rrf-k-tuning"
name = "RRF k 值调优"
status = "active"

[[variants]]
name = "A"
traffic = 0.5
[variants.config.retrieval]
rrf_k = 60

[[variants]]
name = "B"
traffic = 0.5
[variants.config.retrieval]
rrf_k = 30
```

### 4.2 实验结果分析

```sql
-- 对比两个变体的检索精度
SELECT
    variant,
    AVG(precision_at_5) as avg_precision,
    AVG(mrr) as avg_mrr,
    AVG(recall_k) as avg_recall,
    COUNT(*) as sample_size
FROM retrievals
WHERE experiment_id = 'rrf-k-tuning'
  AND created_at > datetime('now', '-7 days')
GROUP BY variant;

-- t-test 显著性 (应用层计算)
-- Cohen's d 效应量
```

### 4.3 实验 API

```
POST /api/experiments
  { "name": "RRF k值调优",
    "variants": [
      {"name": "A", "config": {"retrieval": {"rrf_k": 60}}},
      {"name": "B", "config": {"retrieval": {"rrf_k": 30}}}
    ],
    "traffic_split": {"A": 0.5, "B": 0.5}
  }

PUT /api/experiments/{id}/start    → 激活实验
GET /api/experiments/{id}/results  → 实时结果
POST /api/experiments/{id}/stop    → 停止, 选择 winner
```

---

## 5. 实施

| Phase | 内容 | 复杂度 |
|-------|------|--------|
| **Phase 5a** (与 Agent协同并行) | `everevo-telemetry` crate + spans/retrievals/agent_turns 表 + TelemetrySpan API | 中等 |
| **Phase 5b** | 配置中心: defaults.toml + config.toml + override API + 热加载 | 轻量 |
| **Phase 5c** | A/B 实验: experiment 表 + variant traffic split + 结果查询 + t-test | 中等 |
| **Phase 5d** | 前端: 配置面板 + 实验管理 + 指标仪表盘 | 轻量 |

配置中心用 **TOML 文件 + API override**（不需要独立服务）。对标 Claude Code 的 settings.json 模式——本地优先，热加载，分层覆盖。不需要 LaunchDarkly 那样的外部服务。