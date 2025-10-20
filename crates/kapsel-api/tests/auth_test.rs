//! Integration tests for authentication middleware.
//!
//! Tests API key validation, tenant context injection, and error responses
//! through HTTP request scenarios.

use axum::{
    body::Body,
    extract::Extension,
    http::{header::AUTHORIZATION, Request, StatusCode},
    middleware,
    response::Json,
    routing::get,
    Router,
};
use kapsel_api::middleware::auth::auth_middleware;
use kapsel_testing::TestEnv;
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

/// Test successful authentication with valid API key.
///
/// Verifies that valid API keys are authenticated successfully and
/// tenant context is properly injected into the request.
#[tokio::test]
async fn authenticate_request_succeeds_with_valid_key() {
    let env = TestEnv::new().await.expect("test env setup");

    // Insert test tenant and API key
    let tenant_id = Uuid::new_v4();
    let api_key = "test-key-valid-123";
    let key_hash = sha256::digest(api_key.as_bytes());

    sqlx::query("INSERT INTO tenants (id, name, plan) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind("test-tenant")
        .bind("enterprise")
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

    // Create test app with auth middleware
    let app = create_test_app(env.pool().clone());

    // Make authenticated request
    let request = Request::builder()
        .uri("/test")
        .header(AUTHORIZATION, format!("Bearer {}", api_key))
        .body(Body::empty())
        .expect("request build");

    let response = app.oneshot(request).await.expect("request execution");

    assert_eq!(response.status(), StatusCode::OK);

    // Verify response contains tenant ID
    let body =
        axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("body extraction");
    let body_json: serde_json::Value = serde_json::from_slice(&body).expect("json deserialization");

    assert_eq!(body_json["tenant_id"], tenant_id.to_string());
}

/// Test authentication failure with invalid API key.
///
/// Verifies that invalid API keys are rejected with 401 Unauthorized.
#[tokio::test]
async fn authenticate_request_fails_with_invalid_key() {
    let env = TestEnv::new().await.expect("test env setup");
    let app = create_test_app(env.pool().clone());

    let request = Request::builder()
        .uri("/test")
        .header(AUTHORIZATION, "Bearer invalid-key-12345")
        .body(Body::empty())
        .expect("request build");

    let response = app.oneshot(request).await.expect("request execution");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// Test authentication failure with missing Authorization header.
///
/// Verifies that requests without Authorization header are rejected
/// with 401 Unauthorized.
#[tokio::test]
async fn authenticate_request_fails_without_auth_header() {
    let env = TestEnv::new().await.expect("test env setup");
    let app = create_test_app(env.pool().clone());

    let request = Request::builder().uri("/test").body(Body::empty()).expect("request build");

    let response = app.oneshot(request).await.expect("request execution");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// Test authentication failure with malformed Authorization header.
///
/// Verifies that malformed Authorization headers are rejected properly.
#[tokio::test]
async fn authenticate_request_fails_with_malformed_header() {
    let env = TestEnv::new().await.expect("test env setup");
    let app = create_test_app(env.pool().clone());

    // Test missing "Bearer " prefix
    let request = Request::builder()
        .uri("/test")
        .header(AUTHORIZATION, "test-api-key-no-bearer")
        .body(Body::empty())
        .expect("request build");

    let response = app.oneshot(request).await.expect("request execution");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // Test wrong authentication scheme
    let app = create_test_app(env.pool().clone());
    let request = Request::builder()
        .uri("/test")
        .header(AUTHORIZATION, "Basic dGVzdDp0ZXN0")
        .body(Body::empty())
        .expect("request build");

    let response = app.oneshot(request).await.expect("request execution");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// Test authentication with revoked API key.
///
/// Verifies that revoked API keys are rejected even if they were
/// previously valid.
#[tokio::test]
async fn authenticate_request_fails_with_revoked_key() {
    let env = TestEnv::new().await.expect("test env setup");

    let tenant_id = Uuid::new_v4();
    let api_key = "test-key-revoked-456";
    let key_hash = sha256::digest(api_key.as_bytes());

    // Insert tenant and revoked API key
    sqlx::query("INSERT INTO tenants (id, name, plan) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind("test-tenant")
        .bind("enterprise")
        .execute(env.pool())
        .await
        .expect("insert tenant");

    sqlx::query(
        "INSERT INTO api_keys (tenant_id, key_hash, name, revoked_at) VALUES ($1, $2, $3, NOW())",
    )
    .bind(tenant_id)
    .bind(&key_hash)
    .bind("revoked-key")
    .execute(env.pool())
    .await
    .expect("insert revoked api key");

    let app = create_test_app(env.pool().clone());

    let request = Request::builder()
        .uri("/test")
        .header(AUTHORIZATION, format!("Bearer {}", api_key))
        .body(Body::empty())
        .expect("request build");

    let response = app.oneshot(request).await.expect("request execution");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// Test authentication with expired API key.
///
/// Verifies that expired API keys are rejected properly.
#[tokio::test]
async fn authenticate_request_fails_with_expired_key() {
    let env = TestEnv::new().await.expect("test env setup");

    let tenant_id = Uuid::new_v4();
    let api_key = "test-key-expired-789";
    let key_hash = sha256::digest(api_key.as_bytes());

    // Insert tenant and expired API key
    sqlx::query("INSERT INTO tenants (id, name, plan) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind("test-tenant")
        .bind("enterprise")
        .execute(env.pool())
        .await
        .expect("insert tenant");

    // Insert expired API key with created_at in past to satisfy constraint
    sqlx::query(
        "INSERT INTO api_keys (tenant_id, key_hash, name, created_at, expires_at)
         VALUES ($1, $2, $3, NOW() - INTERVAL '2 days', NOW() - INTERVAL '1 day')",
    )
    .bind(tenant_id)
    .bind(&key_hash)
    .bind("expired-key")
    .execute(env.pool())
    .await
    .expect("insert expired api key");

    let app = create_test_app(env.pool().clone());

    let request = Request::builder()
        .uri("/test")
        .header(AUTHORIZATION, format!("Bearer {}", api_key))
        .body(Body::empty())
        .expect("request build");

    let response = app.oneshot(request).await.expect("request execution");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// Test that API key usage updates last_used_at timestamp.
///
/// Verifies that successful authentication updates the last_used_at
/// field for tracking API key usage.
#[tokio::test]
async fn authenticate_request_updates_last_used_timestamp() {
    let env = TestEnv::new().await.expect("test env setup");

    let tenant_id = Uuid::new_v4();
    let api_key = "test-key-usage-tracking";
    let key_hash = sha256::digest(api_key.as_bytes());

    // Insert tenant and API key
    sqlx::query("INSERT INTO tenants (id, name, plan) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind("test-tenant")
        .bind("enterprise")
        .execute(env.pool())
        .await
        .expect("insert tenant");

    sqlx::query("INSERT INTO api_keys (tenant_id, key_hash, name) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind(&key_hash)
        .bind("usage-tracking-key")
        .execute(env.pool())
        .await
        .expect("insert api key");

    let app = create_test_app(env.pool().clone());

    // Check that last_used_at is initially NULL
    let initial_usage: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT last_used_at FROM api_keys WHERE key_hash = $1")
            .bind(&key_hash)
            .fetch_one(env.pool())
            .await
            .expect("fetch initial last_used_at");

    assert!(initial_usage.is_none());

    // Make authenticated request
    let request = Request::builder()
        .uri("/test")
        .header(AUTHORIZATION, format!("Bearer {}", api_key))
        .body(Body::empty())
        .expect("request build");

    let response = app.oneshot(request).await.expect("request execution");
    assert_eq!(response.status(), StatusCode::OK);

    // Check that last_used_at has been updated
    let updated_usage: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT last_used_at FROM api_keys WHERE key_hash = $1")
            .bind(&key_hash)
            .fetch_one(env.pool())
            .await
            .expect("fetch updated last_used_at");

    assert!(updated_usage.is_some());
}

/// Creates a test Axum app with auth middleware for testing.
fn create_test_app(pool: sqlx::PgPool) -> Router {
    Router::new()
        .route("/test", get(test_handler))
        .layer(middleware::from_fn_with_state(pool.clone(), auth_middleware))
        .with_state(pool)
}

/// Test handler that returns the authenticated tenant ID.
async fn test_handler(Extension(tenant_id): Extension<Uuid>) -> Json<serde_json::Value> {
    Json(json!({
        "status": "authenticated",
        "tenant_id": tenant_id
    }))
}
