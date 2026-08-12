# EverEvo Memory Architecture — Design Document v2
> **状态**:⛔ 已过时(归档)— 已被 [04-memory.md](../../architecture/04-memory.md) 取代
> **来源**:2026-07-18 | **归档**:2026-08-12。本文是设计愿景,以代码现状文档为准。

---


## References

| # | Source | Type | Key Insight |
|---|--------|------|-------------|
| 1 | OpenClaw Dreaming | Production system | 3-phase consolidation (Light→REM→Deep), 6-dim scoring gates, live dreaming + grounded backfill |
| 2 | Mem0 (arXiv:2504.19413) | Academic paper | Extract→Consolidate pipeline, ADD/UPDATE/DELETE/NOOP, Mem0ᵍ graph variant, 90%+ token savings |
| 3 | GraphRAG Hybrid (arXiv:2508.05660) | Academic paper | Dual-store (Neo4j+FAISS), agentic routing between GraphRAG and VectorRAG |
| 4 | Grounded Memory (arXiv:2505.06328) | Academic paper | Neo4j KG + vector embeddings, Text2Cypher + semantic + graph expansion retrieval |
| 5 | TierMem (arXiv:2602.17913) | Academic paper | **2-tier memory: immutable raw log (Tier-2) + provenance index (Tier-1)**, 54% token savings, 61% latency reduction |
| 6 | DEG-RAG (arXiv:2510.14271) | Academic paper | **KG entity resolution**: type-aware blocking, KG embedding matching, 40% graph size reduction while improving QA |
| 7 | Agentic-KGR (arXiv:2510.09156) | Academic paper | Multi-agent RL for KG construction, **98.5% entity deduplication**, co-evolution LLM↔KG |
| 8 | AgentRank (PyPI, 2025) | Production model | Temporal-aware memory embeddings, cross-encoder reranking, **21-22% MRR improvement** |
| 9 | Mem0ᵍ Conflict Detector | Academic paper | Contradicting relations marked invalid not deleted, temporal validity fields |
| 10 | A-MEM (NeurIPS 2025) | Academic paper | Memory evolution taxonomy: EVOLVE/CONFLICT/EXPAND/NEW, 2-hop BFS graph expansion |

---

## 0. First Principle: Raw Data is Sacred

### The Rule

```
┌──────────────────────────────────────────────────────────────┐
│  RULE ZERO: 原始对话数据永不修改、永不删除                       │
│                                                              │
│  SQLite sessions/messages 表 = APPEND-ONLY IMMUTABLE LEDGER   │
│  ├── INSERT only (never UPDATE, never DELETE)                  │
│  ├── 每条 message 写入后不可变                                  │
│  ├── 所有下游数据 (chunks, entities, wiki) 都是 PROJECTIONS    │
│  └── 每个 projection 包含 source pointer → 原始数据            │
└──────────────────────────────────────────────────────────────┘
```

### 借鉴: TierMem 2-Tier Architecture

```
Tier-2: Immutable Paged Raw Log (Source of Truth)
  ├── SQLite sessions + messages + audit.jsonl
  ├── Append-only, never mutated
  ├── 每条记录有稳定的 content-hash ID
  └── 这是整个记忆系统的"宪法"——一切派生数据由此投影

Tier-1: Provenance Index (Fast Semantic Access)
  ├── Vector chunks (LanceDB) + Graph entities (Oxigraph)
  ├── 每个条目包含 source pointer → (session_id, message_id, content_hash)
  ├── 当 summary 不足以回答时，可从 Tier-1 回退到 Tier-2 原始数据
  └── 选择性升级: 仅当 provenance index 证据不足时才加载原始日志
```

**TierMem 论文的验证数据**: 2-tier 架构达到 0.851 准确率（vs 全量原始数据 0.873），但减少 54% token、61% 延迟。

### Source Pointer 设计

```rust
/// Every derived memory artifact carries this pointer back to source.
struct SourcePointer {
    session_id: Uuid,       // SQLite sessions.id
    message_id: Uuid,       // SQLite messages.id
    content_hash: String,   // SHA-256 of original message content
    offset_range: Option<(usize, usize)>, // byte range within message
    derived_at: DateTime<Utc>,  // when this artifact was created
}

/// All downstream artifacts are PROJECTIONS of the immutable raw log.
/// They can be rebuilt from scratch by replaying the pipeline.
struct ProjectionMetadata {
    pipeline_version: String,  // "2.0.0" — if pipeline changes, rebuild
    model_used: String,         // "deepseek-v4" — if model changes, rebuild
    source_pointers: Vec<SourcePointer>,
    confidence: f32,            // 0.0-1.0
}
```

**关键保证**: 如果 LLM 提取出错、pipeline 升级、模型更换——所有 projection 都可以从原始对话完全重建，零信息损失。

---

## 1. Architecture Overview

```
┌──────────────────────────────────────────────────────────────────────┐
│                     IMMUTABLE RAW LOG (Tier-2)                        │
│                                                                       │
│  everevo-db (SQLite: APPEND-ONLY)                                    │
│    ├── sessions / messages  ← 永不修改，永不删除                       │
│    ├── tool_calls           ← 每次工具调用完整记录                      │
│    └── audit.jsonl          ← 沙箱审计日志                             │
│                                                                       │
│  设计约束:                                                             │
│    - INSERT only，禁止 UPDATE/DELETE                                  │
│    - content_hash (SHA-256) 保证完整性                                 │
│    - 定时备份到独立存储 (防误删)                                        │
└────────────────────────────┬─────────────────────────────────────────┘
                             │ READ-ONLY ── 下游管线只读不写
              ┌──────────────┴──────────────┐
              │   DREAMING PIPELINE          │
              │   (定时 / Nudge触发)          │
              │                              │
              │  Phase 1: LIGHT              │
              │    ├── SELECT 原始对话 (只读)   │
              │    ├── LLM 裁剪冗余             │
              │    ├── 保留 source pointers    │
              │    └── 输出: Daily Notes       │
              │                              │
              │  Phase 2: REM                │
              │    ├── 跨日记主题提取           │
              │    └── 输出: Theme Summaries   │
              │                              │
              │  Phase 3: DEEP               │
              │    ├── 6维评分 + 3道门控       │
              │    └── 输出: Long-Term Facts   │
              └──────────────┬──────────────┘
                             │
         ┌───────────────────┴───────────────────┐
         │                                       │
┌────────┴────────┐                    ┌────────┴────────┐
│  VECTOR STORE    │                    │   GRAPH STORE    │
│  (LanceDB)      │                    │   (Oxigraph)     │
│  Tier-1 index    │                    │   Tier-1 index   │
│                  │                    │                  │
│  Source pointers │◄─── 双向链接 ────►│  Source pointers │
│  指向原始对话      │                    │  指向原始对话      │
└────────┬────────┘                    └────────┬────────┘
         │                                       │
         │         ┌───────────────────┐         │
         │         │   RERANK PIPELINE  │         │
         │         │   (cross-encoder)  │         │
         │         │   提高检索精度      │         │
         │         └─────────┬─────────┘         │
         │                   │                   │
         └───────────────────┼───────────────────┘
                             │
              ┌──────────────┴──────────────┐
              │   RETRIEVAL ROUTER           │
              │   (LLM Router + RRF merge)   │
              └──────────────┬──────────────┘
                             │
                             ▼
              ┌──────────────────────────────┐
              │   LLMWIKI GENERATOR           │
              │   • 整合检索结果               │
              │   • 生成/更新 wiki             │
              │   • Projection 可重建          │
              └──────────────────────────────┘
```

---

## 2. Rerank Pipeline — 检索精度保证

### 为什么需要 Rerank

| 阶段 | 方法 | 速度 | 精度 | 用途 |
|------|------|------|------|------|
| Stage 1: Recall | Bi-encoder (embedding) | ms | 中等 | 从 100K+ chunks 召回 top-50~100 |
| Stage 2: Rerank | Cross-encoder | ~100ms/doc | 高 | 从 top-50 精选 top-5~10 送入 LLM |

**核心原理**: Bi-encoder 把 query 和 document 分别编码为向量再比余弦相似度——快但不精确。Cross-encoder 把 (query, doc) 作为 pair 一起过 attention——慢但能捕捉微妙语义关系。

### 技术选型: 两条路径

```
路径 A (自托管 / 无API依赖):
  召回:  fastembed-rs (all-MiniLM-L6-v2, 384dim, CPU可跑)
  重排:  bge-reranker-v2-m3 (ONNX, 568M params, 多语言)
  GPU:   可选，CPU推理 ~50ms/doc
  成本:  零 API 费用

路径 B (最高质量 / API):
  召回:  fastembed-rs (本地)
  重排:  Cohere Rerank 4 Pro (API)
  延迟:  ~200ms
  成本:  $0.002/search
```

**EverEvo 选择路径 A**（自托管优先），但接口支持切换。

### 记忆检索特有的 Rerank 策略

借鉴 **AgentRank** 论文: 标准 embedding 把 "昨天" 和 "6 个月前" 同等对待——

```
AgentRank 的三维编码:
  ├── Content Embedding:  语义内容向量
  ├── Temporal Embedding:  10个时间桶 (1h/6h/24h/3d/7d/30d/90d/180d/365d/inf)
  └── Memory Type Embedding: episodic(事件) / semantic(事实) / procedural(流程)

检索→重排流程:
  1. fastembed-rs 召回 top-50 (用 content embedding)
  2. bge-reranker-v2-m3 重排 top-50 → top-10 (用 content)
  3. Temporal decay boost: 时间较近的记忆在分数上加权
  4. Memory type boost: 用户偏好类记忆(procedural)查询时加权
  5. 输出 top-5 结果给 LLM
```

### Rerank 的适用范围

| 场景 | 是否 Rerank | 原因 |
|------|------------|------|
| 用户问 "我上次怎么修的bug" | ✅ 重排 | 语义查询，需要精确匹配 |
| 结构化查询 "我有几个项目" | ❌ 跳过 | 走 SPARQL，不需要向量检索 |
| wiki 生成时的背景记忆检索 | ✅ 重排 | 需要高质量上下文 |
| 图遍历扩展实体时 | ❌ 跳过 | 走关系遍历，不走语义搜索 |
| 每日 dreaming LIGHT phase | ❌ 跳过 | 处理量大(全量)，用粗召回即可 |

---

## 3. Graph Entity Resolution — 图谱匹配核心

### 为什么图匹配是记忆系统最大的难点

你说了 "图谱匹配要做好"——这是**整个系统最关键的环节**。

错误示范（没有好的 ER）:
```
对话1: "我在做 EverEvo"    → 实体 "everevo"
对话2: "EverEvo 的 sandbox" → 实体 "EverEvo"  ← 两个实体！应该合并
对话3: "Ever Evo Rust版"   → 实体 "Ever Evo"  ← 三个了！全指同一个项目
```

### DEG-RAG 论文的三阶段实体消解

```
Stage 1: BLOCKING — 用低成本方法把可能相同的实体分组
  ├── 语义 blocking: 用 sentence-transformer 做实体名聚类
  │   例: "EverEvo" / "everevo" / "EverEvo-Rust" 分到一组
  ├── 类型 blocking: Person/Project/Tool/File 按类型分组
  │   (DEG-RAG 验证: type-aware blocking 最有效)
  └── 输出: candidate pairs (可能重复的实体对)

Stage 2: MATCHING — 对每个 candidate pair 做精确匹配
  ├── 方法 1: KG embedding 相似度 (TransE/DistMult → cosine)
  ├── 方法 2: LLM embedding 相似度 (用 LLM 编码实体名+属性→向量)
  ├── 方法 3: LLM 直接判断 ("这两个实体是同一个吗? 输出 YES/NO + 理由")
  │   (DEG-RAG 验证: 传统 KG embedding 可以匹敌 LLM embedding!)
  └── 输出: matched pairs + confidence score

Stage 3: MERGING — 合并确认重复的实体
  ├── 属性合并: 两个实体的属性取并集
  ├── 关系合并: 所有入边/出边重定向到 canonical entity
  ├── 旧实体标记: merged_into → canonical_id (不删除!)
  └── 保留: merged_from 列表 (可回溯)
```

### 匹配策略组合 (借鉴 GraphMem 95% 准确率)

```
Hybrid Matching = Lexical (30%) + Semantic (50%) + LLM (20%)

Lexical (快速, 30% 权重):
  ├── 归一化: lowercase, 去标点, 去空格
  ├── Levenshtein 编辑距离 (threshold: < 3)
  ├── Jaro-Winkler 相似度 (> 0.85)
  └── 首字母缩写匹配 ("EverEvo Rust" ↔ "EER")

Semantic (平衡, 50% 权重):
  ├── fastembed-rs 编码实体名+所有属性值 → 向量
  ├── cosine similarity > 0.92 → match
  └── 跨类型不比较 (Person 不和 Project 比)

LLM (最精确但最贵, 20% 权重):
  ├── 仅当 lexical + semantic 得分在灰色区间 (0.75-0.92) 时调用
  ├── Prompt: "Are these two entities the same? {entity_A} vs {entity_B}"
  ├── 输出: YES/NO + confidence + reasoning
  └── 用于解决模糊案例，而非全量处理
```

### Agentic-KGR 的去重保证

借鉴 Agentic-KGR 的 98.5% 去重准确率:
- **唯一约束**: 实体名 + 类型 作为 natural key，在写入前检查
- **索引**: Oxigraph 的 SPARQL 查询在插入前验证 `ASK WHERE { ?s rdf:type :Person ; :name "Alice" }`
- **去重前置**: 在 dual write 之前执行 ER，不写重复实体

### 冲突关系处理 (Mem0ᵍ 模式)

```
场景: Alice→lives_in→SF → Alice→lives_in→NY (矛盾!)

处理:
  ├── 不删除旧关系!
  ├── 旧关系标记: valid_until = 新关系的 valid_from
  ├── 关系状态: "active" | "superseded" | "contradicted"
  ├── 检索时: 默认只返回 active，可查询历史
  └── Temporal reasoning: "Alice 什么时候搬家的?" → 遍历 valid_from 时间线
```

---

## 4. Your Design — Validation Against Papers

### ✅ 论文验证正确的部分

| 你的设计 | 论文支持 |
|----------|---------|
| 对话→日记→长期记忆 pipeline | OpenClaw 3-phase + Mem0 Extract→Consolidate |
| 向量+图双写 | GraphRAG (arXiv:2508.05660): 双库比单库 faithfulness +0.63 |
| LLM 抽实体关系 | Mem0ᵍ: Entity Extractor + Relations Generator 两阶段 |
| Wiki 双向链接 | Grounded Memory: Neo4j + vector 单库双向遍历 |

### ⚠️ 需要强化的部分 (论文指出的盲区)

| # | 你的盲区 | 论文证据 | 我们的补救方案 |
|---|---------|---------|-------------|
| **1** | **原始数据可能被污染** | TierMem: 2-tier 架构避免 54% token 但仍可回溯原始日志 | 第 0 节: Immutable Raw Log + Source Pointers + Projections |
| **2** | **检索精度不够** (只用向量) | AgentRank: 标准 embedding 不区分时间，+22% MRR with temporal rerank | 第 2 节: Rerank Pipeline (bge-reranker-v2-m3 + temporal decay) |
| **3** | **实体重复爆炸** | DEG-RAG: LLM 生成的 KG 有 40% 冗余; Agentic-KGR: 98.5% 去重 | 第 3 节: Lexical + Semantic + LLM 三阶段 ER |
| **4** | **无冲突检测** | Mem0ᵍ: 矛盾关系必须标记 invalid 保留 | 第 3 节: superseded/contradicted 标记, valid_from/until |
| **5** | **无评分门控** | OpenClaw: 6维评分; Mem0: ADD/UPDATE/DELETE/NOOP | Phase 3 DEEP: scoring + gating |
| **6** | **检索无路由** | GraphRAG: LLM Router 分类查询 | 第 3.4 节: Agentic Routing |
| **7** | **无时序意识** | AgentRank: 10 temporal buckets | 第 2 节: temporal embedding |

---

## 5. Complete Pipeline Data Flow

```
Step 1: RAW LOG (immutable, append-only)
  SQLite INSERT message → content_hash = SHA-256 → 写入

Step 2: DREAMING — LIGHT (定时 / Nudge)
  SELECT 未处理的 messages (只读) → LLM 裁剪 → Daily Notes + source_pointers
  输出: memory/YYYY-MM-DD.md

Step 3: DREAMING — REM (定时)
  SELECT 最近 7 天 Daily Notes → LLM 主题提取 → themes.jsonl
  输出: memory/.dreams/themes.jsonl

Step 4: DREAMING — DEEP (定时)
  themes.jsonl → 6维评分 → 三道门控 → 晋升候选
  输出: candidate_facts[]

Step 5: CHUNK EXTRACTION (per candidate)
  message_pair + 5-pair context → LLM Extract → chunk content + metadata
  → ADD/UPDATE/DELETE/NOOP 判断 (向量搜索 top-10 → LLM 决定)

Step 6: ENTITY RESOLUTION (before dual write)
  candidate entities → Blocking → Matching → Merging
  → unique canonical entities

Step 7: DUAL WRITE
  Entity → Oxigraph (SPARQL INSERT)
  Relation → Oxigraph (SPARQL INSERT)
  Chunk → LanceDB (vector INSERT)
  Source pointers → both stores

Step 8: RETRIEVAL (on query)
  Query → LLM Router → Strategy
    ├── Graph: SPARQL → subgraph → entities → related chunks
    ├── Vector: embedding → top-50 → RERANK → top-5
    └── Hybrid: vector top-50 → extract entities → 2-hop BFS → rerank → top-5

Step 9: WIKI GENERATION (triggered by DEEP promotion)
  New/updated facts → check existing wiki pages → generate/update
  → bidirectional links (wiki ↔ chunk_id ↔ entity_uri ↔ source_pointer)
```

---

## 6. Implementation Phases (Revised)

### Phase 2a: Foundation + Data Protection (本周)
```
[ ] 1. Immutable Raw Log 强化
      - content_hash 字段 (SHA-256)
      - INSERT-only 约束验证
      - SourcePointer 类型实现
      - 备份策略 (定时导出)
[ ] 2. LanceDB 集成 (everevo-vector)
      - fastembed-rs (all-MiniLM-L6-v2)
      - chunk CRUD + cosine search
      - ProjectionMetadata 实现
[ ] 3. MemoryManager 实现
      - memory/ 目录 + MEMORY.md 索引
      - Frontmatter 解析器
[ ] 4. Dreaming Phase 1 (LIGHT)
      - SELECT → LLM 裁剪 → Daily Notes
```

### Phase 2b: Graph + Rerank + ER (两周内)
```
[ ] 5. Oxigraph KG (everevo-kg)
      - Entity/Relation extraction (LLM)
      - SPARQL query interface
      - Graph expansion (2-hop BFS)
[ ] 6. Entity Resolution Pipeline
      - Lexical matching (Levenshtein + Jaro-Winkler)
      - Semantic matching (embedding cosine)
      - LLM resolution (灰色区间)
      - Merging + valid_from/until
[ ] 7. Rerank Pipeline
      - bge-reranker-v2-m3 ONNX 集成
      - Temporal decay boost
      - Memory type boost
[ ] 8. Dreaming Phase 2+3 (REM + DEEP)
```

### Phase 2c: Retrieval + Wiki (三周内)
```
[ ] 9. Retrieval Router
      - LLM query classifier
      - Graph/Vector/Hybrid routing
      - RRF merge
[ ] 10. llmwiki Generator
      - Wiki generation from facts
      - Bidirectional linking
      - Rebuild from source (projection replay)
```

---

## 7. Theoretical Guarantees

| 保证 | 依赖 | 验证方式 |
|------|------|---------|
| **原始数据不可变性** | SQLite INSERT-only + content_hash | SHA-256 校验 |
| **可重建性** | ProjectionMetadata + SourcePointers | 从原始对话重跑 pipeline 应得到相同结果 |
| **实体唯一性** | Blocking→Matching→Merging 三阶段 | ER accuracy > 95% (Agentic-KGR 基准) |
| **检索精度** | Rerank cross-encoder + temporal boost | MRR > 0.65 (AgentRank 基准) |
| **无信息丢失** | DELETE→标记 invalid; 旧关系保留 | 可查询任意时间点的知识状态 |
| **模型可替换** | pipeline 版本化 + 模型标识 | 更换模型后重建所有 projection |