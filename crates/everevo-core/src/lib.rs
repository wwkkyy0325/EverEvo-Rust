//! EverEvo shared types, traits, errors, configuration, and telemetry.
//!
//! Architectural **sink** — all crates depend on it. Telemetry module adds
//! sqlx for the background writer; types stay I/O-free.

pub mod agent;
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
// ConfigCenter struct is defined but unused — available for future A/B experiment config
pub use agent::{Agent, AgentContext, AgentOutput};
pub use context::{
    default_pipeline, ContextBuildContext, ContextFragment, ContextPipeline, ContextStage,
};
pub use error::{ApiError, ErrorCode, EverEvoError};
pub use llm::{
    FinishReason, ImageData, LlmMessage, LlmProvider, LlmResponse, LlmRole, StreamEvent, ToolSchema,
};
pub use memory::{FactType, MemoryFact, MemoryIndexEntry, ProjectionMetadata, SourcePointer};
pub use provider::{BootstrapProvider, BootstrapStatus};
pub use sandbox::{ExecutionConfig, ExecutionResult, SandboxProvider};
pub use telemetry::{
    AgentTurnRecord, RetrievalRecord, SpanGuard, Telemetry, TelemetryConfig, Trace,
};
pub use tool::{Tool, ToolHook, ToolOutput, ToolRegistry};
pub use types::*;
