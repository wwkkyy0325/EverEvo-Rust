# EverEvo Domain Knowledge Base — Design Document
> **状态**:⛔ 已过时(归档)— 已被 [10-domain-docs.md](../../architecture/10-domain-docs.md) 取代
> **来源**:2026-07-19 | **归档**:2026-08-12。本文是设计愿景,以代码现状文档为准。

---


## References

| # | Source | Type | Key Insight |
|---|--------|------|-------------|
| 1 | AGENTiGraph (arXiv:2508.02999) | Academic paper | Multi-agent KG framework, Intent Agent 95.12% classification accuracy, dynamic KG updates via LLM-Cypher |
| 2 | AutoGraph (ArangoDB, 2025) | Production system | **Corpus Graph**: auto-discovers natural knowledge domains via graph clustering; MegaGraph for cross-domain unification |
| 3 | Kamat et al. (KBS, 2025) | Academic paper | LLM embedding classification: **96.5% accuracy**, cross-domain validation, 3 diverse domains |
| 4 | OBOE Framework (Applied Sciences, 2025) | Academic paper | KG embedding + hierarchical clustering → **92 semantic domains across 17 topics**; XAI for domain discovery |
| 5 | AnythingLLM | Open-source | Workspace isolation model, LanceDB vector store, 3-level permission (Admin/Manager/User) |
| 6 | TopoChunker (arXiv:2603.18409) | Academic paper | Topology-aware agentic chunking, 83.26% Recall@3, dual-agent (Inspector+Refiner) |
| 7 | Closed-Loop RAG (Fractal, 2025) | Industry paper | 3-phase self-healing: Instrumentation→Diagnosis→Agentic Intervention; failure matrix routing |

---

## 0. Core Design Principles

### 0.1 Auto-Trigger on File Drop

```
data/domain/inbox/     ← 用户扔文件到这里
        │
        ▼ (inotify / filesystem watcher 自动检测)
  新文件到达 → 自动触发解析 → 分类 → 索引 → 图谱
```

**不做** "上传按钮+手动点解析"。AnythingLLM 的做法是上传即处理，我们的做法更激进——文件系统监听，**零点击**。

### 0.2 Domain as First-Class Entity

```
领域 = {
  id: kebab-case-slug,
  name: 人类可读名称,
  description: LLM生成的领域描述,
  centroid_vector: 领域内所有文档的平均向量,
  parent_domain: Option<id>,     ← 领域可以嵌套
  related_domains: [id],         ← 跨领域关联
  document_count: usize,
  created_at, updated_at
}
```

### 0.3 Domains Are INTERCONNECTED

借鉴 AutoGraph 的 **Corpus Graph + MegaGraph** 模型：

```
           ┌──────────────────────────────┐
           │         MegaGraph             │
           │    (跨领域统一知识图谱)          │
           └──────────┬───────────────────┘
                      │
    ┌─────────────────┼─────────────────┐
    ▼                 ▼                 ▼
┌────────┐      ┌────────┐       ┌────────┐
│ Rust   │←────→│  AI    │←─────→│ 数据库  │
│ Domain │ 关联  │ Domain │ 关联  │ Domain │
└────────┘      └────────┘       └────────┘
    │                 │                 │
    ▼                 ▼                 ▼
  chunks           chunks            chunks
  entities         entities          entities
  relations        relations         relations
```

**领域之间的关联自动发现**：
- 共享实体检测：两个领域都提到同一概念 → 建立关联
- 交叉引用检测：领域A的文档引用了领域B的术语 → 建立关联
- 向量距离：两个领域的 centroid 余弦相似度 > 阈值 → 建议关联

---

## 1. Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                   FILE SYSTEM WATCHER                            │
│                                                                 │
│  data/domain/inbox/   ← 用户扔文件                                │
│  data/domain/{domain}/inbox/  ← 扔到特定领域目录                    │
│                                                                 │
│  inotify / poll 检测: 新文件 / 修改 / 删除                         │
│  → 去重检测 (content hash)                                       │
│  → 自动触发 Pipeline                                             │
└────────────────────────┬────────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────────┐
│                   DOMAIN CLASSIFIER                              │
│                                                                 │
│  Step 1: 提取文档全文 → embedding (fastembed-rs)                  │
│                                                                 │
│  Step 2: 与所有现有领域的 centroid_vector 做余弦相似度             │
│    ├── max_sim > 0.75 → 归类到该领域                             │
│    ├── 0.45 < max_sim < 0.75 → LLM判断 (最相关的1-2个领域)        │
│    └── max_sim < 0.45 → 新建领域 (LLM 生成名称+描述)             │
│                                                                 │
│  Step 3: LLM 建议领域关联                                         │
│    - "这个文档涉及 Rust 和 AI，建议在 Rust领域和AI领域之间建立关联"  │
│                                                                 │
│  参考: Kamat et al. 96.5% accuracy with LLM embeddings           │
└────────────────────────┬────────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────────┐
│                   PARSE & CHUNK                                   │
│                                                                 │
│  Parser: Markdown / PDF(text) / Code(AST-aware) / 纯文本         │
│  Chunker: semantic chunking (embedding distance at sentence      │
│           boundaries → split at 95th percentile)                 │
│  Dedup: SHA-256 content hash → 已存在 → skip                     │
│                                                                 │
│  参考: TopoChunker dual-agent, semantic percentile chunking      │
└────────────────────────┬────────────────────────────────────────┘
                         │
          ┌──────────────┴──────────────┐
          ▼                             ▼
┌──────────────────┐          ┌──────────────────┐
│  DOMAIN INDEX     │          │  UNIFIED GRAPH    │
│  (LanceDB)       │          │  (Oxigraph)       │
│                  │          │                   │
│  复用everevo-    │          │  复用everevo-kg   │
│  vector crate    │          │  crate            │
│                  │          │                   │
│  per-chunk:      │          │  领域实体:         │
│  + embedding     │          │  :Entity domain:  │
│  + domain_id     │          │    "rust"         │
│  + source文档    │          │                   │
│  + chunk类型     │          │  跨领域关联:       │
│                  │          │  :Rust :relatedTo │
│  domain centroid │          │    :AI .          │
│  实时更新        │          │                   │
└──────────────────┘          └──────────────────┘
```

---

## 2. Domain Lifecycle

### 2.1 Domain Auto-Creation

```
新文档 → classify → max_sim < 0.45
  → LLM: "给这个文档集合起一个 kebab-case 名称和中文描述"
  → 创建 domain entry
  → 文档 = 该领域的第一个成员
  → 初始化 centroid_vector
```

### 2.2 Domain Merging

```
触发条件: 用户手动 / LLM 检测到两个领域高度重叠
  (centroid cosine > 0.85 且共享 3+ 实体)

合并流程:
  1. 选 canonical domain (文档数多的为主)
  2. 迁移所有文档: doc.domain_id = canonical
  3. 迁移所有 chunks: chunk.domain_id = canonical
  4. 合并实体: EntityResolver.resolve_all()
  5. 重新计算 centroid_vector
  6. 旧 domain 标记 merged_into = canonical_id (不删除!)
  7. 追加 changelog
```

### 2.3 Domain Splitting

```
触发条件: LLM 检测到领域内存在明显子主题
  (领域内文档聚类产生 2+ 个 high-silhouette 簇)

拆分流程:
  1. 对领域内所有文档向量做 K-Means (K=2~3)
  2. LLM 为每个簇生成名称+描述
  3. 创建新 domain(s)
  4. 迁移文档到对应的新领域
  5. 建立 parent_domain ← 原领域
  6. 追加 changelog
```

### 2.4 Domain Relationship Graph

借鉴 **AutoGraph MegaGraph** 模型：

```
Domain A ──related_to── Domain B    (共享实体 >= 3)
Domain A ──parent_of─── Domain C    (C 是从 A 拆分出来的)
Domain A ──merged_from── Domain D   (D 被合并到 A)
Domain A ──references── Domain B    (A 的文档引用了 B 的术语)
```

存储为 Oxigraph RDF:
```turtle
<domain/rust> :relatedTo <domain/wasm> .
<domain/rust> :parentOf <domain/rust-async> .
<domain/ai> :references <domain/math> .
```

---

## 3. Smart Dedup

```
文档级去重:
  1. 文件到达 → SHA-256 全文 hash
  2. 查询 domain index: content_hash 已存在?
     → Yes + 同领域: skip (完全重复)
     → Yes + 不同领域: 建议跨领域关联
     → No: 继续处理

Chunk级去重:
  3. Semantic chunking 后, 每个 chunk 的 embedding → cosine top-3
  4. cosine > 0.95: near-duplicate → merge metadata, skip
  5. 0.85 < cosine < 0.95: LLM 判断是否合并

更新检测:
  6. 文件名相同 + content_hash 不同 → 文档已更新
     → 标记旧版本 superseded, 重新解析新版本
     → 旧 chunks 不删除 (保留历史版本)
```

---

## 4. Self-Healing Pipeline

借鉴 **Closed-Loop RAG (Fractal, 2025)** 的三阶段模型：

### 4.1 Instrumentation (运行时指标)

| 指标 | 检测方式 |
|------|---------|
| Chunk relevance | LLM-as-Judge: 检索的chunk与查询相关吗? Yes/No |
| Answer faithfulness | 生成的回答是否被检索到的chunks支撑? |
| Domain coverage | 该领域是否有足够的文档覆盖所有子主题? |
| Entity completeness | 关键实体是否缺少属性? |

### 4.2 Diagnosis (故障矩阵)

| 场景 | 诊断 | 自愈动作 |
|------|------|---------|
| retrieval_score < 0.5 | 检索器未找到相关chunks | Query rewrite + re-retrieve |
| faithfulness < 0.5 | 幻觉, 生成内容无支撑 | Rewrite with citations + stricter grounding |
| coverage < 0.4 | 领域文档不足 | 标记 Knowledge Gap, 建议用户补充 |
| entity_missing_props | 实体信息不完整 | LLM 从 chunks 中补充属性 |

### 4.3 Agentic Intervention (自动修复)

```
每天晚上 (定时器):
  1. 选取 N 个近期查询作为测试集
  2. 运行检索 → 评分 → 诊断
  3. 对每个故障场景执行自愈
  4. 追加 Changelog
  5. 发送报告 (可选)
```

---

## 5. Storage Layout

```
data/domain/
├── inbox/                    ← 文件监听的入口目录 (未分类)
├── rust/                     ← rust 领域
│   ├── inbox/                ← rust 领域的入口 (自动分类)
│   ├── documents/            ← 原始文档 (不可变)
│   │   ├── rust-book.md
│   │   └── async-paper.pdf
│   └── wiki/                 ← 领域 wiki (自动生成)
│       └── concepts.md
├── ai/                       ← ai 领域
│   ├── documents/
│   └── wiki/
├── database/                 ← 数据库领域
│   └── ...
│
├── shared/
│   ├── vector/               ← LanceDB (所有领域共享一个store, 按domain_id分区)
│   │   └── chunks.lance/
│   ├── graph/                ← Oxigraph (统一知识图谱, 跨领域)
│   │   └── knowledge.ttl
│   └── index.db              ← SQLite (文档元数据 + FTS5 + 领域信息)
│
└── domains.json              ← 领域注册表 (手动/自动维护)
```

---

## 6. API Endpoints

```
# 领域管理
GET    /api/domains                      ← 列出所有领域 + 关联
POST   /api/domains                      ← 手动创建领域
PUT    /api/domains/{id}                 ← 更新领域信息
DELETE /api/domains/{id}                 ← 删除领域 (不删文档)
POST   /api/domains/{id}/merge           ← 合并两个领域
POST   /api/domains/{id}/split           ← 拆分为子领域

# 文档管理
GET    /api/domains/{id}/documents       ← 列出领域文档
GET    /api/domains/{id}/documents/{did} ← 查看文档详情
DELETE /api/domains/{id}/documents/{did} ← 删除文档 + 索引

# 检索
POST   /api/domain/search               ← 跨领域混合检索
POST   /api/domain/{id}/search           ← 单领域检索

# 自愈
POST   /api/domain/verify               ← 触发自检修正
GET    /api/domain/health                ← 查看各领域健康状态

# 文件监听状态
GET    /api/domain/watcher               ← 文件监听器状态
```

---

## 7. New Crate: everevo-domain

```
everevo-domain/
  Cargo.toml
  src/
    lib.rs              ← re-exports
    domain.rs           ← Domain struct, DomainRegistry
    classifier.rs       ← embedding-based auto-classification
    watcher.rs          ← filesystem watcher (inotify / poll)
    document.rs         ← Document/Chunk types
    parser.rs           ← Markdown / PDF / Code parser
    chunker.rs          ← Semantic percentile chunker
    indexer.rs          ← Dual write Vector + Graph + FTS5
    corrector.rs        ← Self-healing: Instrument → Diagnose → Repair
    retrieval.rs        ← Hybrid retrieval (vector+keyword+graph) + Rerank
    graph.rs            ← Domain relationship graph (MegaGraph)
```

---

## 8. Implementation Phases

### Phase 3a: Foundation (本周)
```
[ ] everevo-domain crate 创建
[ ] Domain + DomainRegistry 类型
[ ] File watcher (inotify/poll) → auto-detect new files
[ ] Document parser (Markdown + text)
[ ] Semantic percentile chunker
[ ] Embedding-based auto-classifier (centroid cosine)
[ ] Dual write: LanceDB + Oxigraph (reuse crates)
[ ] Content-hash dedup
```

### Phase 3b: Intelligence (下周)
```
[ ] Domain merging + splitting logic
[ ] Domain relationship graph (AutoGraph MegaGraph model)
[ ] LLM-based grey-area classification (0.45 < sim < 0.75)
[ ] Smart chunk dedup (cosine + LLM judgment)
[ ] Self-healing loop (Instrument→Diagnose→Repair)
```

### Phase 3c: UI + Integration (后续)
```
[ ] Frontend domain panel (领域列表+关系图可视化)
[ ] Agent 集成: domain_search tool
[ ] Cross-domain reasoning
```