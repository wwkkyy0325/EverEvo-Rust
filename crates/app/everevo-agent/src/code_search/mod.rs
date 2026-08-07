//! EverEvo code search — hybrid FTS5 + regex codebase indexing and retrieval.
//!
//! ## Design (based on Claude Code + Sourcegraph Cody + academic RAG research)
//!
//! 1. **Scanner**: regex-based symbol extraction (Rust, Python, JS/TS, Go).
//!    No Tree-sitter dependency. JetBrains 2025: line-based ≈ AST-aware.
//! 2. **Indexer**: SQLite FTS5 full-text search with BM25-like ranking.
//!    Cody-inspired: no IDF weighting (PR #912, zoekt).
//! 3. **Retriever**: keyword search, kind-filtered search, file listing.
//! 4. **LLM tool**: `code_search` — query the index from the agent.
//!
//! Claude Code insight: grep-first. The index complements grep, not replaces it.

pub mod indexer;
pub mod scanner;
pub mod watcher;

pub use indexer::{format_search_results, CodeIndex, IndexStats, SearchConfig, SearchResult};
pub use scanner::{scan_file, CodeSymbol, SymbolKind};
pub use watcher::has_changes_since;
