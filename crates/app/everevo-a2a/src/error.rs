//! A2A JSON-RPC 2.0 error codes — full taxonomy per the 2025 spec.
//!
//! ## Categories
//!
//! - `-32700..-32600` — standard JSON-RPC 2.0 errors (protocol-level)
//! - `-32001..-32006` — A2A-specific errors (task-level)

use serde::Serialize;

/// Standard + A2A-specific JSON-RPC error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum A2aErrorCode {
    // ── JSON-RPC 2.0 Standard ──
    ParseError = -32700,
    InvalidRequest = -32600,
    MethodNotFound = -32601,
    InvalidParams = -32602,
    InternalError = -32603,

    // ── A2A Protocol-Specific ──
    TaskNotFound = -32001,
    TaskNotCancelable = -32002,
    PushNotSupported = -32003,
    UnsupportedOperation = -32004,
    TaskRejected = -32005,
    AuthRequired = -32006,
}

impl A2aErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ParseError => "Parse error",
            Self::InvalidRequest => "Invalid Request",
            Self::MethodNotFound => "Method not found",
            Self::InvalidParams => "Invalid params",
            Self::InternalError => "Internal error",
            Self::TaskNotFound => "Task not found",
            Self::TaskNotCancelable => "Task not cancelable",
            Self::PushNotSupported => "Push Notification not supported",
            Self::UnsupportedOperation => "Unsupported operation",
            Self::TaskRejected => "Task rejected",
            Self::AuthRequired => "Authentication required",
        }
    }
}

/// A rich A2A error — carries code, message, optional data, and retry hint.
#[derive(Debug, Clone, Serialize)]
pub struct A2aError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// Seconds the client should wait before retrying (only for rate-limit / 503).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u64>,
    /// Whether this error is retryable.
    #[serde(skip_serializing)]
    pub retryable: bool,
}

impl A2aError {
    // ── Constructors ──────────────────────────────────────────────────

    pub fn new(code: A2aErrorCode, message: &str, retryable: bool) -> Self {
        Self {
            code: code as i32,
            message: message.into(),
            data: None,
            retry_after_secs: None,
            retryable,
        }
    }

    pub fn task_not_found(task_id: &str) -> Self {
        Self::new(
            A2aErrorCode::TaskNotFound,
            &format!("Task '{task_id}' not found"),
            false,
        )
    }

    pub fn task_not_cancelable(task_id: &str, state: &str) -> Self {
        Self::new(
            A2aErrorCode::TaskNotCancelable,
            &format!("Task '{task_id}' in terminal state '{state}' — cannot cancel"),
            false,
        )
    }

    pub fn internal(detail: &str) -> Self {
        Self::new(A2aErrorCode::InternalError, detail, false)
    }

    pub fn invalid_params(detail: &str) -> Self {
        Self::new(A2aErrorCode::InvalidParams, detail, false)
    }

    pub fn method_not_found(method: &str) -> Self {
        Self::new(
            A2aErrorCode::MethodNotFound,
            &format!("Method '{method}' not found"),
            false,
        )
    }

    // ── Builder methods ───────────────────────────────────────────────

    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
    }

    pub fn retry_after(mut self, secs: u64) -> Self {
        self.retry_after_secs = Some(secs);
        self.retryable = true;
        self
    }

    // ── Conversion ────────────────────────────────────────────────────

    /// Convert to a JSON-RPC error envelope.
    pub fn to_jsonrpc_error(&self, id: serde_json::Value) -> crate::types::JsonRpcResponse {
        crate::types::JsonRpcResponse {
            jsonrpc: "2.0".into(),
            result: None,
            error: Some(crate::types::JsonRpcError {
                code: self.code,
                message: self.message.clone(),
                data: self.data.clone(),
            }),
            id,
        }
    }
}

impl std::fmt::Display for A2aError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for A2aError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_code_values() {
        assert_eq!(A2aErrorCode::ParseError as i32, -32700);
        assert_eq!(A2aErrorCode::TaskNotFound as i32, -32001);
        assert_eq!(A2aErrorCode::TaskRejected as i32, -32005);
    }

    #[test]
    fn test_task_not_found() {
        let err = A2aError::task_not_found("abc-123");
        assert_eq!(err.code, -32001);
        assert!(!err.retryable);
        assert!(err.message.contains("abc-123"));
    }

    #[test]
    fn test_jsonrpc_error_envelope() {
        let err = A2aError::method_not_found("tasks/delete");
        let resp = err.to_jsonrpc_error(serde_json::Value::Number(1.into()));
        assert_eq!(resp.error.unwrap().code, -32601);
        assert!(resp.result.is_none());
    }
}
