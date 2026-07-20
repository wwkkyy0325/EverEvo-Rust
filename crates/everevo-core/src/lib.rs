//! EverEvo shared types, traits, errors, and configuration.
//!
//! This crate is the architectural **sink** — it has zero heavy I/O dependencies
//! (no `tokio`, `reqwest`, `sqlx`, `oxigraph`, `lancedb`, or `wasmtime`).
//! All other crates depend on it; it depends on nothing but the standard library
//! and serialization helpers.

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
pub mod tool;
pub mod types;

// Re-export the public API surface
pub use config::AppConfig;
pub use config_center::ConfigCenter;
pub use context::{ContextBuildContext, ContextFragment, ContextPipeline, ContextStage, default_pipeline};
pub use agent::{Agent, AgentContext, AgentOutput};
pub use error::EverEvoError;
pub use provider::{BootstrapProvider, BootstrapStatus, DownloadProvider, DownloadResult};
pub use llm::{FinishReason, LlmMessage, LlmProvider, LlmResponse, LlmRole, StreamEvent, ToolSchema};
pub use memory::{FactType, MemoryFact, MemoryIndexEntry, ProjectionMetadata, SourcePointer};
pub use sandbox::{ExecutionConfig, ExecutionResult, SandboxProvider};
pub use tool::{Tool, ToolOutput, ToolRegistry};
pub use types::*;
