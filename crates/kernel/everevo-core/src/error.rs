use thiserror::Error;

/// Unified error type for the EverEvo application.
#[derive(Debug, Error)]
pub enum EverEvoError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Database error: {0}")]
    Database(String),

    #[error("LLM provider error: {0}")]
    LlmProvider(String),

    #[error("Agent loop error: {0}")]
    Agent(String),

    #[error("Tool '{tool}' execution error: {message}")]
    Tool { tool: String, message: String },

    #[error("Sandbox error: {0}")]
    Sandbox(String),

    #[error("Bootstrap error: {0}")]
    Bootstrap(String),

    #[error("Download error: {0}")]
    Download(String),

    #[error("Knowledge graph error: {0}")]
    KnowledgeGraph(String),

    #[error("Vector store error: {0}")]
    Vector(String),

    #[error("RAG pipeline error: {0}")]
    Rag(String),

    #[error("Network error: {0}")]
    Network(String),

    #[error("llmwiki error: {0}")]
    Llmwiki(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl EverEvoError {
    /// Attach structured context to any error. Returns the error for chaining.
    #[must_use = "error context is only applied when the result is used"]
    pub fn context(self, ctx: impl Into<String>) -> Self {
        use EverEvoError::*;
        let ctx = ctx.into();
        match self {
            Config(s) => Config(format!("{s} [{ctx}]")),
            Database(s) => Database(format!("{s} [{ctx}]")),
            LlmProvider(s) => LlmProvider(format!("{s} [{ctx}]")),
            Agent(s) => Agent(format!("{s} [{ctx}]")),
            Sandbox(s) => Sandbox(format!("{s} [{ctx}]")),
            Bootstrap(s) => Bootstrap(format!("{s} [{ctx}]")),
            Download(s) => Download(format!("{s} [{ctx}]")),
            KnowledgeGraph(s) => KnowledgeGraph(format!("{s} [{ctx}]")),
            Vector(s) => Vector(format!("{s} [{ctx}]")),
            Rag(s) => Rag(format!("{s} [{ctx}]")),
            Network(s) => Network(format!("{s} [{ctx}]")),
            Llmwiki(s) => Llmwiki(format!("{s} [{ctx}]")),
            NotFound(s) => NotFound(format!("{s} [{ctx}]")),
            InvalidInput(s) => InvalidInput(format!("{s} [{ctx}]")),
            Internal(s) => Internal(format!("{s} [{ctx}]")),
            other => other,
        }
    }
}

// ── HTTP API Error ──────────────────────────────────────────────────────

use axum_core::response::{IntoResponse, Response};
use http::StatusCode;
use serde::Serialize;

/// Machine-readable error code — one per failure mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    NotFound,
    InvalidInput,
    Conflict,
    Forbidden,
    Unauthorized,
    TooManyRequests,
    Internal,
    DatabaseError,
    LlmProviderError,
    SandboxError,
    NetworkError,
    IoError,
    ConfigError,
    AgentError,
    ToolError,
    BootstrapError,
    Timeout,
    ServiceUnavailable,
}

impl ErrorCode {
    pub fn status_code(self) -> StatusCode {
        match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::InvalidInput => StatusCode::BAD_REQUEST,
            Self::Conflict => StatusCode::CONFLICT,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::TooManyRequests => StatusCode::TOO_MANY_REQUESTS,
            Self::Timeout => StatusCode::GATEWAY_TIMEOUT,
            Self::ServiceUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::NetworkError => StatusCode::BAD_GATEWAY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// Unified HTTP API error. Every route returns this.
/// JSON envelope: `{"error":{"code":"NOT_FOUND","message":"...","details":null}}`
#[derive(Debug)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    pub details: Option<serde_json::Value>,
}

impl ApiError {
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::NotFound,
            message: msg.into(),
            details: None,
        }
    }
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::InvalidInput,
            message: msg.into(),
            details: None,
        }
    }
    pub fn conflict(msg: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::Conflict,
            message: msg.into(),
            details: None,
        }
    }
    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::Forbidden,
            message: msg.into(),
            details: None,
        }
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::Internal,
            message: msg.into(),
            details: None,
        }
    }
    pub fn timeout(msg: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::Timeout,
            message: msg.into(),
            details: None,
        }
    }

    #[allow(dead_code)]
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code.status_code().as_u16(), self.message)
    }
}

impl std::error::Error for ApiError {}

/// Map EverEvoError → ApiError with correct status codes.
impl From<EverEvoError> for ApiError {
    fn from(e: EverEvoError) -> Self {
        let code = match &e {
            EverEvoError::NotFound(_) => ErrorCode::NotFound,
            EverEvoError::InvalidInput(_) => ErrorCode::InvalidInput,
            EverEvoError::Config(_) => ErrorCode::ConfigError,
            EverEvoError::Database(_) => ErrorCode::DatabaseError,
            EverEvoError::LlmProvider(_) => ErrorCode::LlmProviderError,
            EverEvoError::Agent(_) => ErrorCode::AgentError,
            EverEvoError::Tool { .. } => ErrorCode::ToolError,
            EverEvoError::Sandbox(_) => ErrorCode::SandboxError,
            EverEvoError::Bootstrap(_) => ErrorCode::BootstrapError,
            EverEvoError::Network(_) => ErrorCode::NetworkError,
            EverEvoError::Io(_) => ErrorCode::IoError,
            _ => ErrorCode::Internal,
        };
        Self {
            code,
            message: e.to_string(),
            details: None,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.code.status_code();
        let body = serde_json::json!({
            "error": {
                "code": self.code,
                "message": self.message,
                "details": self.details,
            }
        });
        Response::builder()
            .status(status)
            .header(
                http::header::CONTENT_TYPE,
                http::header::HeaderValue::from_static("application/json"),
            )
            .body(axum_core::body::Body::new(
                serde_json::to_string(&body).unwrap_or_default(),
            ))
            .unwrap()
    }
}

pub type Result<T> = std::result::Result<T, EverEvoError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = EverEvoError::Config("missing API key".into());
        assert!(err.to_string().contains("missing API key"));
        assert!(err.to_string().contains("Configuration"));
    }

    #[test]
    fn test_io_error_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let everevo_err: EverEvoError = io_err.into();
        assert!(matches!(everevo_err, EverEvoError::Io(_)));
        assert!(everevo_err.to_string().contains("file not found"));
    }

    #[test]
    fn test_serde_error_conversion() {
        let serde_err = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let everevo_err: EverEvoError = serde_err.into();
        assert!(matches!(everevo_err, EverEvoError::Serde(_)));
    }

    #[test]
    fn test_error_debug() {
        let err = EverEvoError::NotFound("session abc".into());
        let debug = format!("{err:?}");
        assert!(debug.contains("NotFound"));
        assert!(debug.contains("session abc"));
    }
}
