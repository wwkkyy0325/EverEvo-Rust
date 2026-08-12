# Phase 3: Knowledge Layer
> **状态**:✅ 已完成(归档)— 阶段 3 计划,代码已落地;未勾选项如仍需跟进请新建任务

---


**Goal:** RAG pipeline, knowledge graph, llmwiki integration. Agent retrieves and reasons over stored knowledge.

---

## Tasks

### 3.1 — RAG Pipeline
- [ ] `everevo-rag`: chunker (fixed, recursive, semantic strategies)
- [ ] Document parsing: Markdown (pulldown-cmark), PDF (lopdf), Code (tree-sitter)
- [ ] Embedder: fastembed-rs (local ONNX) + OpenAI embeddings API fallback
- [ ] Vector store: LanceDB integration (embedded, file-based)
- [ ] Ingestion pipeline: `POST /api/rag/ingest` (upload file → chunk → embed → store)
- [ ] Retrieval: `POST /api/rag/search` (query → embed → ANN search → rerank → top-K)
- [ ] Hybrid search: semantic + BM25 keyword matching
- **Verify:** Upload a design doc → ask "what architecture does this project use?" → gets correct answer

### 3.2 — Knowledge Graph
- [ ] `everevo-kg`: Oxigraph wrapper (MemoryStore for dev, RocksDB for prod)
- [ ] Entity extraction: LLM extracts (subject, predicate, object) triples from text
- [ ] Relation extraction: identify entity relationships from conversation
- [ ] SPARQL query builder: programmatic query construction
- [ ] `POST /api/kg/query` — SPARQL endpoint
- [ ] `GET /api/kg/entity/:name` — lookup entity and its relations
- [ ] Auto-extraction: agent conversations automatically feed into KG
- **Verify:** Agent: "UserService has a login method" → query "what methods does UserService have?" → returns ["login"]

### 3.3 — llmwiki Manager
- [ ] `everevo-llmwiki`: read/write/index `docs/llmwiki/` directory
- [ ] Parse frontmatter from markdown files
- [ ] Index all llmwiki docs into RAG pipeline
- [ ] Agent tool: `llmwiki_read(path)` — read a doc
- [ ] Agent tool: `llmwiki_search(query)` — search across all docs
- [ ] Agent tool: `llmwiki_suggest(path, content)` — propose an edit (requires user approval)
- [ ] Auto-index on startup + watch for file changes
- **Verify:** Agent can answer "what's our tech stack?" by reading design.md

### 3.4 — Knowledge Integration
- [ ] Agent prompt injects relevant RAG chunks + KG entities + llmwiki docs
- [ ] Priority: recent messages > KG context > RAG results > llmwiki
- [ ] Source attribution in agent responses
- [ ] `POST /api/chat` accepts optional `context_sources` flag
- **Verify:** Conversation about specific code → agent cites design.md, ingested docs, and KG entities

### 3.5 — Agent Trait + Multi-Agent Orchestration (from Audit)
- [ ] Define `Agent` trait in `everevo-core` (pattern: same as `Tool` trait)
  ```rust
  #[async_trait]
  pub trait Agent: Send + Sync {
      fn name(&self) -> &str;
      async fn run(&self, input: &str, context: &AgentContext) -> Result<AgentOutput>;
  }
  ```
- [ ] Implement ADK patterns: SequentialAgent, ParallelAgent wrappers
- [ ] `ToolRegistry` supports registering an Agent as a Tool (agent-as-tool pattern)
- **Verify:** Two agents collaborate — Agent A delegates to Agent B via tool call

### 3.6 — Crate Split (from Audit)
- [ ] When `everevo-agent` exceeds ~25 files, split into focused crates:
  - `everevo-tools/` — all built-in tool impls
  - `everevo-sandbox/` — TieredSandbox + WASM + Docker executors
  - `everevo-kg/` — Oxigraph wrapper
  - `everevo-rag/` — LanceDB + fastembed pipeline
  - `everevo-llmwiki/` — project knowledge base manager
- **Trigger:** any module exceeds 500 lines or needs a different dependency profile

### 3.7 — Error Handling Consistency (from Audit)
- [ ] Add `From<DownloadError> for EverEvoError` impl
- [ ] Add `Bootstrap` variant to `EverEvoError` (not reuse `Config` variant)
- [ ] Audit all `map_err(|e| EverEvoError::Tool(format!(...)))` → proper From impls
- **Verify:** Error chain preserves original error type, not just string formatting