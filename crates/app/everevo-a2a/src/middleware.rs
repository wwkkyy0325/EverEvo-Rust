//! A2A middleware stack — body limits, path validation, authentication.
//!
//! Applied as Axum layers on the A2A router.
//!
//! ## Layer order (first to last)
//!
//! 1. `BodyLimitLayer` — reject payloads > 1 MB
//! 2. `PathValidationLayer` — prevent traversal attacks
//! 3. `AuthLayer` — JWT or API key verification (skips AgentCard)

use axum::http::{header, Request, StatusCode};
use axum::response::IntoResponse;
use std::future::Future;
use std::pin::Pin;
use tower::{Layer, Service};

// ── Body Limit Layer ──────────────────────────────────────────────────────

/// Reject requests with bodies larger than `max_bytes`.
pub fn body_limit_layer(max_bytes: usize) -> tower_http::limit::RequestBodyLimitLayer {
    tower_http::limit::RequestBodyLimitLayer::new(max_bytes)
}

// ── Auth Layer ────────────────────────────────────────────────────────────

/// Authentication configuration for A2A endpoints.
#[derive(Clone)]
pub struct A2aAuthConfig {
    /// If set, require Bearer JWT verification.
    pub jwt_secret: Option<String>,
    /// List of valid static API keys.
    pub api_keys: Vec<String>,
    /// If false, skip auth entirely (dev mode).
    pub enabled: bool,
}

impl A2aAuthConfig {
    pub fn dev_mode() -> Self {
        Self {
            jwt_secret: None,
            api_keys: vec![],
            enabled: false,
        }
    }

    pub fn production(jwt_secret: String, api_keys: Vec<String>) -> Self {
        Self {
            jwt_secret: Some(jwt_secret),
            api_keys,
            enabled: true,
        }
    }

    /// Verify an Authorization header value. Returns Ok(()) if valid.
    pub fn verify(&self, header_value: &str) -> Result<(), (StatusCode, String)> {
        if !self.enabled {
            return Ok(());
        }

        // API Key check
        if let Some(key) = header_value.strip_prefix("ApiKey ") {
            if self.api_keys.iter().any(|k| k == key) {
                return Ok(());
            }
            return Err((StatusCode::FORBIDDEN, "Invalid API key".into()));
        }

        // JWT check
        if let Some(token) = header_value.strip_prefix("Bearer ") {
            if let Some(ref secret) = self.jwt_secret {
                // Simple HMAC verification — full JWT library optional.
                // In production, use jsonwebtoken crate with RS256.
                if verify_simple_jwt(token, secret) {
                    return Ok(());
                }
            }
            return Err((StatusCode::UNAUTHORIZED, "Invalid JWT token".into()));
        }

        Err((
            StatusCode::UNAUTHORIZED,
            "Missing Authorization header (Bearer <token> or ApiKey <key>)".into(),
        ))
    }
}

/// JWT verification with HMAC-SHA256.
///
/// Parses a `header.payload.signature` token, verifies the HMAC-SHA256
/// signature against the shared secret.
fn verify_simple_jwt(token: &str, secret: &str) -> bool {
    use base64::Engine;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    if token.is_empty() || secret.is_empty() {
        return false;
    }

    let parts: Vec<&str> = token.splitn(3, '.').collect();
    if parts.len() != 3 {
        return false;
    }

    let header_b64 = parts[0];
    let payload_b64 = parts[1];
    let signature_b64 = parts[2];

    // Decode the expected signature from base64url
    let expected_sig = match base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(signature_b64)
    {
        Ok(s) => s,
        Err(_) => return false,
    };

    // Compute HMAC-SHA256(header.payload, secret) and verify constant-time
    let message = format!("{header_b64}.{payload_b64}");
    let mut mac = match Hmac::<Sha256>::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(message.as_bytes());
    mac.verify_slice(&expected_sig).is_ok()
}

// ── Tower Layer for Auth ──────────────────────────────────────────────────

/// Auth middleware — extracts Authorization header and validates.
#[derive(Clone)]
pub struct AuthLayer {
    config: A2aAuthConfig,
}

impl AuthLayer {
    pub fn new(config: A2aAuthConfig) -> Self {
        Self { config }
    }
}

impl<S> Layer<S> for AuthLayer {
    type Service = AuthService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        AuthService {
            inner,
            config: self.config.clone(),
        }
    }
}

#[derive(Clone)]
pub struct AuthService<S> {
    inner: S,
    config: A2aAuthConfig,
}

impl<S, ReqBody> Service<Request<ReqBody>> for AuthService<S>
where
    S: Service<Request<ReqBody>, Response = axum::response::Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
    ReqBody: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        // Skip auth for AgentCard discovery
        if req.uri().path() == "/.well-known/agent.json" || req.uri().path() == "/a2a/health" {
            return Box::pin(self.inner.call(req));
        }

        let auth_header = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok());

        match auth_header {
            Some(h) => {
                if let Err((code, msg)) = self.config.verify(h) {
                    let resp = (code, msg).into_response();
                    return Box::pin(async { Ok(resp) });
                }
            }
            None => {
                if self.config.enabled {
                    let resp = (StatusCode::UNAUTHORIZED, "Authorization required").into_response();
                    return Box::pin(async { Ok(resp) });
                }
            }
        }

        Box::pin(self.inner.call(req))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dev_mode_allows_all() {
        let config = A2aAuthConfig::dev_mode();
        assert!(config.verify("garbage").is_ok());
        assert!(!config.enabled);
    }

    #[test]
    fn test_production_rejects_missing_format() {
        let config = A2aAuthConfig::production("secret".into(), vec!["key1".into()]);
        assert!(config.verify("NotBearer xyz").is_err());
    }

    #[test]
    fn test_production_api_key() {
        let config = A2aAuthConfig::production("secret".into(), vec!["my-key".into()]);
        assert!(config.verify("ApiKey my-key").is_ok());
        assert!(config.verify("ApiKey wrong-key").is_err());
    }

    #[test]
    fn test_jwt_valid_token_verifies() {
        use base64::Engine;
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        let secret = "my-secret-key";
        // Build a minimal JWT
        let header = r#"{"alg":"HS256","typ":"JWT"}"#;
        let payload = r#"{"sub":"test","exp":9999999999}"#;
        let header_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(header);
        let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload);
        let message = format!("{header_b64}.{payload_b64}");

        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(message.as_bytes());
        let sig =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&mac.finalize().into_bytes());

        let token = format!("{header_b64}.{payload_b64}.{sig}");
        assert!(verify_simple_jwt(&token, secret));
    }

    #[test]
    fn test_jwt_invalid_signature_rejected() {
        let fake = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ0ZXN0In0.WRONG_SIGNATURE_HERE";
        assert!(!verify_simple_jwt(fake, "my-secret-key"));
    }

    #[test]
    fn test_jwt_empty_secret_rejected() {
        assert!(!verify_simple_jwt("a.b.c", ""));
        assert!(!verify_simple_jwt("", "secret"));
    }
}
