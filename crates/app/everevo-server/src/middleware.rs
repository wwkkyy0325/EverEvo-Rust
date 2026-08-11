//! Tower middleware — panic recovery, request logging, and shared utilities.
//!
//! Registered in [`build_app`](crate::build_app) as a global layer so every
//! handler is covered.

use axum::body::Body;
use axum::http::Request;
use axum::response::{IntoResponse, Response};
use everevo_core::ApiError;
use futures::FutureExt;
use std::panic::AssertUnwindSafe;

/// Catch panics in handlers and convert to a structured `ApiError` 500 response.
///
/// Without this, a panic in any handler takes down the entire server process.
/// With it, the error is logged, the client gets a proper JSON error envelope,
/// and the server keeps serving other requests.
pub async fn panic_recovery(request: Request<Body>, next: axum::middleware::Next) -> Response {
    let future = AssertUnwindSafe(next.run(request));
    match future.catch_unwind().await {
        Ok(response) => response,
        Err(panic) => {
            let msg = panic_message(&panic);
            tracing::error!(%msg, "Handler panicked — recovered by middleware");
            ApiError::internal("internal server error").into_response()
        }
    }
}

/// Extract a human-readable message from a panic payload.
fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}
