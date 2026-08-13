//! EverEvo agent core.
//!
//! ## Module map
//!
//! | Module | Content |
//! |--------|---------|
//! | `loop_` | ReAct agent loop (AgentLoop, AgentEvent) |
//! | `llm` | LLM providers (HttpClient, MockLlmProvider) |
//! | `tools` | Built-in tool registry |
//! | `stages` | Context pipeline stages (persona, skills, best practices, domain, memory) |
//! | `memory` | Persistent memory system (facts, diary, dreaming) |
//! | `knowledge` | Knowledge layer (graph, domain, RAG, wiki) |
//! | `subagent_context` | Sub-agent context assembly |
//!
//! ## Architecture docs (llmwiki)
//!
//! - [00-overview.md](../../../../docs/llmwiki/architecture/00-overview.md) — §5 决策 13 (agent loop)
//! - [01-entry-pipelines.md](../../../../docs/llmwiki/architecture/01-entry-pipelines.md) — §1 D/E 阶段、§2 子代理、§5 dreaming
//! - [02-agent-loop.md](../../../../docs/llmwiki/architecture/02-agent-loop.md) — ReAct 回合、收敛、压缩三层
//! - [03-context-pipeline.md](../../../../docs/llmwiki/architecture/03-context-pipeline.md) — 14-stage 上下文管线
//! - [04-memory.md](../../../../docs/llmwiki/architecture/04-memory.md) — 事实、做梦、consolidator、召回

pub mod code_search;
pub mod context;
pub mod llm;
pub mod llmwiki;
pub mod loop_;
pub mod memory;
pub mod rag;
pub mod skill;
pub mod stages;
pub mod subagent_context;
pub mod subagent_pool;
pub mod subagent_roles;
pub mod task_type;
pub mod tools;

// ── Public API ──────────────────────────────────────────────────────────

// Agent loop
pub use loop_::{AgentEvent, AgentLoop, EscalationLevel, ProactivityState};

// LLM providers
pub use llm::{HttpClient, MockLlmProvider};

// Knowledge layer — re-exported from everevo-knowledge crate
pub use llmwiki::LlmwikiManager;
pub use rag::{make_chunk, RagPipeline};

// Context stages — all in one place
pub use stages::{
    build_character_block, load_character, synthesize_character, AgentCharacter,
    AgentCharacterStage, BestPracticesStage, DomainKnowledgeStage, MemoryStage, PersonaStage,
    SkillStage, SynthesisReport,
};

// Tool registry (legacy constructor — prefer orchestration::tools::assemble())
pub use tools::build_registry;

// ── Server integration surface ────────────────────────────────────────────
// These types are consumed by everevo-server. Breaking changes here may
// require coordinated server updates.
pub use memory::diary::DiaryManager;
pub use memory::engine::DreamingEngine;
pub use memory::facts::FactManager;
pub use memory::scheduler::DreamingScheduler;
pub use skill::SkillRegistry;
pub use subagent_context::SubAgentContext;
pub use tools::builtins::TodoStore;
