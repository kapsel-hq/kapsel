//! Integration tests for authentication middleware.
//!
//! Tests API key validation, tenant context injection, and error responses
//! through HTTP request scenarios.

use std::sync::Arc;

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
use kapsel_core::{storage::Storage, Clock};
use kapsel_testing::TestEnv;
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

/// Test successful authentication with valid API key.
///
/// Verifies that valid API keys are authenticated successfully and
/// tenant context is properly injected into the request.
#[tokio::test]
async fn authenticate_request_succeeds_with_valid_key() -> anyhow::Result<()> {
    TestEnv::run_isolated_test(|env| async move {
        let mut tx = env.pool().begin().await.expect("begin transaction");

        // Create test data using transaction-aware methods
        let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await.expect("create tenant");

        let (api_key, _key_hash) = env
            .create_api_key_tx(&mut tx, tenant_id, "test-key-valid")
            .await
            .expect("create api key");

        // Commit so API handlers can see the data
        tx.commit().await.expect("commit transaction");

        // Create test app with auth middleware
        let app = create_test_app(
            env.pool().clone(),
            &(Arc::new(env.clock.clone()) as Arc<dyn kapsel_core::Clock>),
        );

        // Make authenticated request
        let request = Request::builder()
            .uri("/test")
            .header(AUTHORIZATION, format!("Bearer {api_key}"))
            .body(Body::empty())
            .expect("request build");

        let response = app.oneshot(request).await.expect("request execution");

        assert_eq!(response.status(), StatusCode::OK);

        // Verify response body
        let body =
            axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("body extraction");
        let body_json: serde_json::Value =
            serde_json::from_slice(&body).expect("json deserialization");

        assert_eq!(body_json["tenant_id"], tenant_id.to_string());

        Ok(())
    })
    .await
}

/// Test authentication failure with invalid API key.
///
/// Verifies that invalid API keys are rejected with 401 Unauthorized.
#[tokio::test]
async fn authenticate_request_fails_with_invalid_key() {
    let env = TestEnv::new_shared().await.expect("test env setup");
    let app = create_test_app(
        env.pool().clone(),
        &(Arc::new(env.clock.clone()) as Arc<dyn kapsel_core::Clock>),
    );

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
    let env = TestEnv::new_shared().await.expect("test env setup");
    let app = create_test_app(
        env.pool().clone(),
        &(Arc::new(env.clock.clone()) as Arc<dyn kapsel_core::Clock>),
    );

    let request = Request::builder().uri("/test").body(Body::empty()).expect("request build");

    let response = app.oneshot(request).await.expect("request execution");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// Test authentication failure with malformed Authorization header.
///
/// Verifies that malformed Authorization headers are rejected properly.
#[tokio::test]
async fn authenticate_request_fails_with_malformed_header() {
    let env = TestEnv::new_shared().await.expect("test env setup");
    let app = create_test_app(
        env.pool().clone(),
        &(Arc::new(env.clock.clone()) as Arc<dyn kapsel_core::Clock>),
    );

    // Test missing "Bearer " prefix
    let request = Request::builder()
        .uri("/test")
        .header(AUTHORIZATION, "test-api-key-no-bearer")
        .body(Body::empty())
        .expect("request build");

    let response = app.oneshot(request).await.expect("request execution");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // Test wrong authentication scheme
    let app = create_test_app(
        env.pool().clone(),
        &(Arc::new(env.clock.clone()) as Arc<dyn kapsel_core::Clock>),
    );
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
async fn authenticate_request_fails_with_revoked_key() -> anyhow::Result<()> {
    TestEnv::run_isolated_test(|env| async move {
        let mut tx = env.pool().begin().await.expect("begin transaction");

        // Create test data using transaction-aware methods
        let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await.expect("create tenant");

        let (api_key, key_hash) = env
            .create_api_key_tx(&mut tx, tenant_id, "test-key-revoked")
            .await
            .expect("create api key");

        // Commit first so we can use the revoke method
        tx.commit().await.expect("commit transaction");

        // Mark the key as revoked
        env.storage().api_keys.revoke(&key_hash).await.expect("revoke key");

        let app = create_test_app(
            env.pool().clone(),
            &(Arc::new(env.clock.clone()) as Arc<dyn kapsel_core::Clock>),
        );

        let request = Request::builder()
            .uri("/test")
            .header(AUTHORIZATION, format!("Bearer {api_key}"))
            .body(Body::empty())
            .expect("request build");

        let response = app.oneshot(request).await.expect("request execution");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        Ok(())
    })
    .await
}

/// Test authentication with expired API key.
///
/// Verifies that expired API keys are rejected properly.
#[tokio::test]
async fn authenticate_request_fails_with_expired_key() -> anyhow::Result<()> {
    TestEnv::run_isolated_test(|env| async move {
        let mut tx = env.pool().begin().await.expect("begin transaction");

        // Create test data using transaction-aware methods
        let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await.expect("create tenant");

        let (api_key, key_hash) = env
            .create_api_key_tx(&mut tx, tenant_id, "test-key-expired")
            .await
            .expect("create api key");

        // Commit first so we can use the set_expiration method
        tx.commit().await.expect("commit transaction");

        // Set expiration to current time plus 1 day (valid constraint)
        let expires_at = chrono::DateTime::<chrono::Utc>::from(env.clock.now_system())
            + chrono::Duration::days(1);
        env.storage()
            .api_keys
            .set_expiration(&key_hash, Some(expires_at))
            .await
            .expect("set expiration");

        // Advance clock by 2 days to make the key expired
        env.clock.advance(std::time::Duration::from_secs(2 * 24 * 60 * 60));

        let app = create_test_app(
            env.pool().clone(),
            &(Arc::new(env.clock.clone()) as Arc<dyn kapsel_core::Clock>),
        );

        let request = Request::builder()
            .uri("/test")
            .header(AUTHORIZATION, format!("Bearer {api_key}"))
            .body(Body::empty())
            .expect("request build");

        let response = app.oneshot(request).await.expect("request execution");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        Ok(())
    })
    .await
}

/// Test that API key usage updates last_used_at timestamp.
///
/// Verifies that successful authentication updates the last_used_at
/// field for tracking API key usage.
#[tokio::test]
async fn authenticate_request_updates_last_used_timestamp() -> anyhow::Result<()> {
    TestEnv::run_isolated_test(|env| async move {
        let mut tx = env.pool().begin().await.expect("begin transaction");

        // Create test data using transaction-aware methods
        let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await.expect("create tenant");

        let (api_key, key_hash) = env
            .create_api_key_tx(&mut tx, tenant_id, "test-key-usage")
            .await
            .expect("create api key");

        // Commit so API handlers can see the data
        tx.commit().await.expect("commit transaction");

        let app = create_test_app(
            env.pool().clone(),
            &(Arc::new(env.clock.clone()) as Arc<dyn kapsel_core::Clock>),
        );

        // Check that last_used_at is initially NULL
        let initial_usage = env
            .storage()
            .api_keys
            .find_by_hash(&key_hash)
            .await
            .expect("find api key")
            .expect("api key exists")
            .last_used_at;

        assert!(initial_usage.is_none());

        // Make authenticated request
        let request = Request::builder()
            .uri("/test")
            .header(AUTHORIZATION, format!("Bearer {api_key}"))
            .body(Body::empty())
            .expect("request build");

        let response = app.oneshot(request).await.expect("request execution");
        assert_eq!(response.status(), StatusCode::OK);

        // Check that last_used_at has been updated
        let updated_usage = env
            .storage()
            .api_keys
            .find_by_hash(&key_hash)
            .await
            .expect("find api key")
            .expect("api key exists")
            .last_used_at;

        assert!(updated_usage.is_some());

        Ok(())
    })
    .await
}

/// Creates a test Axum app with auth middleware for testing.
fn create_test_app(pool: sqlx::PgPool, clock: &Arc<dyn kapsel_core::Clock>) -> Router {
    Router::new()
        .route("/test", get(test_handler))
        .layer(middleware::from_fn_with_state(
            Arc::new(Storage::new(pool.clone(), clock)),
            auth_middleware,
        ))
        .with_state(pool)
}

/// Test handler that returns the authenticated tenant ID.
async fn test_handler(Extension(tenant_id): Extension<Uuid>) -> Json<serde_json::Value> {
    Json(json!({
        "status": "authenticated",
        "tenant_id": tenant_id
    }))
}
