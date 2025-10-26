//! API key authentication middleware with tenant isolation.
//!
//! Validates API keys from Authorization headers, performs database lookup
//! with SHA256 hashing, and injects tenant context for downstream handlers.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use kapsel_core::storage::Storage;
use uuid::Uuid;

/// Extracts API key from Authorization header.
/// Supports Bearer token format: "Bearer <api-key>"
fn extract_api_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(String::from)
}

/// Validates API key and returns tenant ID.
async fn validate_api_key(storage: &Storage, api_key: &str) -> Result<Uuid, AuthError> {
    let key_hash = sha256::digest(api_key.as_bytes());

    match storage
        .api_keys
        .validate(&key_hash)
        .await
        .map_err(|e| AuthError::Database(e.to_string()))?
    {
        Some(tenant_id) => Ok(tenant_id.0),
        None => Err(AuthError::InvalidApiKey),
    }
}

/// Errors that can occur during API key authentication.
#[derive(Debug)]
pub enum AuthError {
    /// The provided API key is invalid, expired, or revoked.
    InvalidApiKey,
    /// A database error occurred while validating the API key.
    Database(String),
    /// The Authorization header is missing from the request.
    MissingHeader,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::InvalidApiKey => (StatusCode::UNAUTHORIZED, "Invalid API key"),
            Self::MissingHeader => (StatusCode::UNAUTHORIZED, "Missing Authorization header"),
            Self::Database(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Internal error"),
        };

        (status, message).into_response()
    }
}

/// Axum middleware that authenticates requests using API keys.
pub async fn auth_middleware(
    State(storage): State<Arc<Storage>>,
    mut req: Request<Body>,
    next: Next,
) -> Result<Response, AuthError> {
    let headers = req.headers();

    let api_key = extract_api_key(headers).ok_or(AuthError::MissingHeader)?;

    let tenant_id = validate_api_key(&storage, &api_key).await?;

    req.extensions_mut().insert(tenant_id);

    Ok(next.run(req).await)
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    #[allow(clippy::panic)] // panic is acceptable in test setup
    #[test]
    fn extract_api_key_from_bearer_token() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer test-api-key-12345"));

        let result = extract_api_key(&headers);
        assert_eq!(result, Some("test-api-key-12345".to_string()));
    }

    #[test]
    fn extract_api_key_returns_none_without_auth_header() {
        let headers = HeaderMap::new();
        let result = extract_api_key(&headers);
        assert_eq!(result, None);
    }
}
