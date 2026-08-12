# Code Search Architecture — Design Document
> **状态**:⛔ 已过时(归档)— 已被 [14-code-search.md](../../architecture/14-code-search.md) 取代
> **来源**:2026-07-27 | **归档**:2026-08-12。本文是设计愿景,以代码现状文档为准。

---


> Written: 2026-07-27 | Status: implementing

## Research Summary

| Source | Approach | Key Insight |
|--------|----------|-------------|
| **Claude Code** | grep-first agentic search | Switched FROM embeddings TO grep. "Search, don't index." Simplicity > complexity. |
| **Sourcegraph Cody** | BM25 (!IDF) + SCIP code graph + agentic | Hybrid: exact search + structured code graph. Removed IDF from BM25 for code. |
| **cAST paper (EMNLP 2025)** | AST-aware chunking | +4.3 recall@5 vs fixed-size. Recursively break large nodes, merge siblings. |
| **JetBrains (2025)** | Practical RAG at scale | Line-based chunking ≈ AST-aware across budgets. Don't over-engineer. |
| **CodeWisp (IEEE 2025)** | AST-guided segmentation | 0.96 recall, 0.95 MRR. Structure-aware beats flat chunking. |

## Design Decision

**Hybrid approach**: grep-first (Claude Code) + FTS5 index (Cody) + optional embeddings (our ONNX).

```
Priority 1: Grep/Glob tools (already in shell tool) → zero-cost, always works
Priority 2: FTS5 code index (new) → fast keyword search with symbol awareness
Priority 3: Embedding search (future) → semantic similarity, needs ONNX models
```

## Architecture

```
┌──────────────────────────────────────────────┐
│                LLM Context                     │
│  ┌──────────────────────────────────────────┐ │
│  │  CodeSearchStage (new)                    │ │
│  │  Injects relevant code chunks into prompt │ │
│  └──────────────────────────────────────────┘ │
└────────────────────┬─────────────────────────┘
                     │
┌────────────────────▼─────────────────────────┐
│           CodeIndex (new crate)                │
│  ┌─────────┐  ┌─────────┐  ┌──────────────┐  │
│  │ Scanner │  │ Indexer │  │   Retriever  │  │
│  │ (walk)  │  │ (FTS5)  │  │ (search API) │  │
│  └─────────┘  └─────────┘  └──────────────┘  │
└────────────────────┬─────────────────────────┘
                     │
┌────────────────────▼─────────────────────────┐
│              SQLite FTS5                        │
│  code_symbols: name, kind, file, line, parent  │
│  code_chunks: file, start_line, end_line, hash │
└──────────────────────────────────────────────┘
```

## Tools

| Tool | Type | Description |
|------|------|-------------|
| `code_search` | LLM-callable | Search codebase: query → ranked results (file:line, symbol, snippet) |
| `code_map` | LLM-callable | Return a Markdown directory overview |
| `code_index` | Administrative | Trigger re-indexing of the workspace |

## Symbol Kinds (regex-extracted, no Tree-sitter dependency)

| Kind | Rust pattern | Python pattern | JS/TS pattern |
|------|-------------|----------------|---------------|
| `fn` | `fn \w+` | `def \w+` | `function \w+\|const \w+ = (` |
| `struct` | `struct \w+` | `class \w+` | `class \w+` |
| `impl` | `impl \w+` | — | — |
| `trait` | `trait \w+` | — | — |
| `enum` | `enum \w+` | — | — |
| `mod` | `mod \w+` | `import \| from` | `import \| require` |
| `type` | `type \w+` | — | `type \w+ \| interface \w+` |
| `const` | `const \w+` | `\w+ = ` | `const \w+` |

## FTS5 Schema

```sql
CREATE VIRTUAL TABLE code_symbols USING fts5(
    name,       -- symbol name
    kind,       -- fn, struct, impl, trait, enum, mod, type, const
    file,       -- relative file path
    line,       -- line number (1-based)
    parent,     -- parent module/class
    signature,  -- full signature line
    content='code_symbols_content',
    content_rowid='rowid'
);
```

## Implementation Plan

| Phase | Task | Effort | Files |
|-------|------|--------|-------|
| 1 | `CodeIndex` struct: scan, FTS5 schema, insert | S | `crates/everevo-code-search/src/indexer.rs` |
| 2 | `CodeScanner`: regex-based symbol extraction per language | S | `crates/everevo-code-search/src/scanner.rs` |
| 3 | `code_search` tool | S | `everevo-agent/src/tools/builtins/code_search.rs` |
| 4 | `code_map` tool — directory overview | S | `everevo-agent/src/tools/builtins/code_search.rs` |
| 5 | `CodeSearchStage` — context injection | S | `everevo-agent/src/stages/code_search.rs` |
| 6 | Register + wire into registries + test | S | various |

**Total effort: ~2 sessions**

## References

- Anthropic. "Best practices for large codebases with Claude." (2025)
- Sourcegraph. "Removing IDF from BM25 for code search." PR #912, zoekt. (2025)
- Zhang et al. "cAST: Enhancing Code RAG with Structural Chunking via AST." EMNLP 2025.
- JetBrains Research. "Practical Code RAG at Scale." arXiv:2510.20609. (2025)
- CodeWisp. "AST Guided Retrieval Augmented Generation for Code." IEEE 2025.