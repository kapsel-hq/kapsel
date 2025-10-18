//! API key authentication middleware with tenant isolation.
//!
//! Validates API keys from Authorization headers, performs database lookup
//! with SHA256 hashing, and injects tenant context for downstream handlers.

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use sqlx::PgPool;
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
async fn validate_api_key(db: &PgPool, api_key: &str) -> Result<Uuid, AuthError> {
    let key_hash = sha256::digest(api_key.as_bytes());

    let row: Option<(Uuid,)> = sqlx::query_as(
        r"
        SELECT tenant_id
        FROM api_keys
        WHERE key_hash = $1
          AND revoked_at IS NULL
          AND (expires_at IS NULL OR expires_at > NOW())
        ",
    )
    .bind(&key_hash)
    .fetch_optional(db)
    .await
    .map_err(|e| AuthError::Database(e.to_string()))?;

    match row {
        Some((tenant_id,)) => {
            let _ = sqlx::query("UPDATE api_keys SET last_used_at = NOW() WHERE key_hash = $1")
                .bind(&key_hash)
                .execute(db)
                .await;

            Ok(tenant_id)
        },
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
    State(db): State<PgPool>,
    mut req: Request<Body>,
    next: Next,
) -> Result<Response, AuthError> {
    let headers = req.headers();

    let api_key = extract_api_key(headers).ok_or(AuthError::MissingHeader)?;

    let tenant_id = validate_api_key(&db, &api_key).await?;

    req.extensions_mut().insert(tenant_id);

    Ok(next.run(req).await)
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

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

    #[tokio::test]
    async fn authenticate_request_succeeds_with_valid_key() {
        use kapsel_testing::TestEnv;

        let env = TestEnv::new().await.expect("test env setup");

        let tenant_id = Uuid::new_v4();
        let api_key = "test-key-abc123";
        let key_hash = sha256::digest(api_key.as_bytes());

        sqlx::query("INSERT INTO tenants (id, name, plan, api_key) VALUES ($1, $2, $3, $4)")
            .bind(tenant_id)
            .bind("test-tenant")
            .bind("free")
            .bind(api_key)
            .execute(env.pool())
            .await
            .expect("insert tenant");

        sqlx::query("INSERT INTO api_keys (tenant_id, key_hash, name) VALUES ($1, $2, $3)")
            .bind(tenant_id)
            .bind(&key_hash)
            .bind("test-key")
            .execute(env.pool())
            .await
            .expect("insert api key");

        let result = validate_api_key(env.pool(), api_key).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), tenant_id);
    }
}
