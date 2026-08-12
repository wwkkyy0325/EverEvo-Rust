//! EverEvo shared types, traits, errors, configuration, and telemetry.
//!
//! Architectural **sink** — all crates depend on it. Telemetry module adds
//! sqlx for the background writer; types stay I/O-free.
//!
//! ## Architecture docs (llmwiki)
//!
//! - [00-overview.md](../../../../docs/llmwiki/architecture/00-overview.md) — §3 ContextStage pipeline, §5 决策 1/6/8-12
//!   (context pipeline contract, memory paradigm, vector store)
//! - [03-context-pipeline.md](../../../../docs/llmwiki/architecture/03-context-pipeline.md) — context/stages.rs, ContextBuildContext
//! - [04-memory.md](../../../../docs/llmwiki/architecture/04-memory.md) — 记忆范式、向量存储契约
//! - [06-tool-system.md](../../../../docs/llmwiki/architecture/06-tool-system.md) — Tool trait (tool.rs)
//! - [08-config-and-state.md](../../../../docs/llmwiki/architecture/08-config-and-state.md) — AppConfig (config.rs)

pub mod config;
pub mod config_center;
pub mod context;
pub mod error;
pub mod llm;
pub mod memory;
pub mod provider;
pub mod retrieval;
pub mod sandbox;
pub mod slash_command;
pub mod telemetry;
pub mod tool;
pub mod types;

// Re-export the public API surface
pub use config::{AppConfig, McpServerConfig};
pub use context::{
    default_pipeline, ContextBuildContext, ContextFragment, ContextPipeline, ContextStage,
    RollingSummaryStage,
};
pub use error::{ApiError, ErrorCode, EverEvoError};
pub use llm::{
    FinishReason, ImageData, LlmMessage, LlmProvider, LlmResponse, LlmRole, StreamEvent, ToolSchema,
};
pub use memory::{FactType, MemoryFact, MemoryIndexEntry, ProjectionMetadata, SourcePointer};
pub use provider::{BootstrapProvider, BootstrapStatus};
pub use sandbox::{ExecutionConfig, ExecutionResult, SandboxProvider};
pub use telemetry::{
    default_telemetry_pipeline, AgentTurnRecord, RetrievalRecord, SpanGuard, Telemetry,
    TelemetryConfig, TelemetryEmitContext, TelemetryPipeline, TelemetryRecord, TelemetrySnapshot,
    TelemetryStage, Trace,
};
pub use tool::{Tool, ToolHook, ToolOutput, ToolRegistry};
pub use types::*;
