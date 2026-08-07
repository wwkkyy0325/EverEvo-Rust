//! LLM Provider implementations.
//!
//! - `HttpClient`: Real HTTP client (Anthropic + OpenAI-compatible SSE streaming)
//! - `MockLlmProvider`: Deterministic mock for testing

pub mod http;
pub mod mock;

pub use http::HttpClient;
pub use mock::MockLlmProvider;
