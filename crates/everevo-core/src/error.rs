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

    #[error("Tool execution error: {0}")]
    Tool(String),

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
