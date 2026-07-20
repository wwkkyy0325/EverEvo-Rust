//! EverEvo agent core.
//!
//! Contains the ReAct agent loop, tool registry, sandbox executors,
//! memory manager, knowledge graph, RAG pipeline, and llmwiki manager.

pub mod domain_stage;
pub mod llm;
pub mod tools;
pub mod sandbox;
pub mod memory;
pub mod kg;
pub mod rag;
pub mod llmwiki;
pub mod loop_;
pub mod orchestration;
pub mod persona;
pub mod skill;
pub mod subagent_context;

// Re-export implementations (traits + types are in everevo_core)
pub use domain_stage::DomainKnowledgeStage;
pub use llm::{HttpClient, MockLlmProvider};
pub use loop_::{AgentEvent, AgentLoop};
pub use tools::build_registry;

// Re-export knowledge layer
pub use everevo_vector;
pub use kg::AgentKnowledgeGraph;
pub use llmwiki::{index_llmwiki_into_rag, LlmwikiManager};
pub use rag::{make_chunk, RagPipeline};
