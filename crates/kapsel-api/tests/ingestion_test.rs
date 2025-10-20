//! Integration tests for webhook ingestion endpoint.
//!
//! Tests the `/ingest/{endpoint_id}` endpoint with authentication, payload
//! validation, signature verification, and error handling scenarios.

use axum::{
    body::Body,
    http::{header::AUTHORIZATION, Request, StatusCode},
};
use kapsel_api::create_router;
use kapsel_testing::TestEnv;
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

/// Test successful webhook ingestion with valid authentication and payload.
///
/// Verifies the complete happy path from HTTP request through database
/// persistence, including proper event creation and status tracking.
#[tokio::test]
async fn ingest_webhook_succeeds_with_valid_request() {
    let env = TestEnv::new().await.expect("test env setup");

    // Create tenant and endpoint
    let tenant_id = Uuid::new_v4();
    let endpoint_id = Uuid::new_v4();
    let api_key = "test-key-valid-ingestion";
    let key_hash = sha256::digest(api_key.as_bytes());

    // Insert tenant
    sqlx::query("INSERT INTO tenants (id, name, plan) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind("test-tenant")
        .bind("enterprise")
        .execute(env.pool())
        .await
        .expect("insert tenant");

    // Insert API key
    sqlx::query("INSERT INTO api_keys (tenant_id, key_hash, name) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind(&key_hash)
        .bind("test-key")
        .execute(env.pool())
        .await
        .expect("insert api key");

    // Insert endpoint
    sqlx::query(
        "INSERT INTO endpoints (id, tenant_id, name, url, is_active, max_retries, timeout_seconds, circuit_state)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(endpoint_id)
    .bind(tenant_id)
    .bind("test-endpoint")
    .bind("https://example.com/webhook")
    .bind(true)
    .bind(3)
    .bind(30)
    .bind("closed")
    .execute(env.pool())
    .await
    .expect("insert endpoint");

    let app = create_router(env.pool().clone());

    // Prepare webhook payload
    let payload = json!({
        "event": "user.created",
        "data": {
            "id": 123,
            "email": "test@example.com"
        }
    });
    let payload_bytes = serde_json::to_vec(&payload).expect("serialize payload");

    // Make ingestion request
    let request = Request::builder()
        .method("POST")
        .uri(format!("/ingest/{}", endpoint_id))
        .header(AUTHORIZATION, format!("Bearer {}", api_key))
        .header("content-type", "application/json")
        .header("x-idempotency-key", "test-event-001")
        .body(Body::from(payload_bytes.clone()))
        .expect("build request");

    let response = app.oneshot(request).await.expect("execute request");

    // Verify successful response
    assert_eq!(response.status(), StatusCode::OK);

    let body =
        axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("read response body");
    let response_json: serde_json::Value =
        serde_json::from_slice(&body).expect("parse response json");

    assert!(response_json["event_id"].is_string());
    assert_eq!(response_json["status"], "received");

    // Verify event was persisted to database
    let event_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM webhook_events WHERE tenant_id = $1 AND endpoint_id = $2",
    )
    .bind(tenant_id)
    .bind(endpoint_id)
    .fetch_one(env.pool())
    .await
    .expect("count events");

    assert_eq!(event_count, 1);

    // Verify event details
    let event_row: (String, String, Vec<u8>, String, i32) = sqlx::query_as(
        "SELECT source_event_id, status, body, content_type, payload_size
         FROM webhook_events WHERE tenant_id = $1 AND endpoint_id = $2",
    )
    .bind(tenant_id)
    .bind(endpoint_id)
    .fetch_one(env.pool())
    .await
    .expect("fetch event details");

    let (source_event_id, status, body, content_type, payload_size) = event_row;
    assert_eq!(source_event_id, "test-event-001");
    assert_eq!(status, "received");
    assert_eq!(body, payload_bytes);
    assert_eq!(content_type, "application/json");
    assert_eq!(payload_size, payload_bytes.len() as i32);
}

/// Test webhook ingestion fails with invalid authentication.
///
/// Verifies that requests without valid API keys are rejected
/// with appropriate error responses.
#[tokio::test]
async fn ingest_webhook_fails_with_invalid_auth() {
    let env = TestEnv::new().await.expect("test env setup");
    let app = create_router(env.pool().clone());

    let endpoint_id = Uuid::new_v4();
    let payload = json!({"event": "test"});
    let payload_bytes = serde_json::to_vec(&payload).expect("serialize payload");

    let request = Request::builder()
        .method("POST")
        .uri(format!("/ingest/{}", endpoint_id))
        .header(AUTHORIZATION, "Bearer invalid-key")
        .header("content-type", "application/json")
        .body(Body::from(payload_bytes))
        .expect("build request");

    let response = app.oneshot(request).await.expect("execute request");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// Test webhook ingestion fails with missing authentication.
///
/// Verifies that requests without Authorization header are rejected.
#[tokio::test]
async fn ingest_webhook_fails_without_auth() {
    let env = TestEnv::new().await.expect("test env setup");
    let app = create_router(env.pool().clone());

    let endpoint_id = Uuid::new_v4();
    let payload = json!({"event": "test"});
    let payload_bytes = serde_json::to_vec(&payload).expect("serialize payload");

    let request = Request::builder()
        .method("POST")
        .uri(format!("/ingest/{}", endpoint_id))
        .header("content-type", "application/json")
        .body(Body::from(payload_bytes))
        .expect("build request");

    let response = app.oneshot(request).await.expect("execute request");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// Test webhook ingestion fails with nonexistent endpoint.
///
/// Verifies that requests to invalid endpoint IDs are rejected
/// with 404 Not Found.
#[tokio::test]
async fn ingest_webhook_fails_with_nonexistent_endpoint() {
    let env = TestEnv::new().await.expect("test env setup");

    // Create valid tenant and API key
    let tenant_id = Uuid::new_v4();
    let api_key = "test-key-nonexistent-endpoint";
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

    let app = create_router(env.pool().clone());

    // Use non-existent endpoint ID
    let endpoint_id = Uuid::new_v4();
    let payload = json!({"event": "test"});
    let payload_bytes = serde_json::to_vec(&payload).expect("serialize payload");

    let request = Request::builder()
        .method("POST")
        .uri(format!("/ingest/{}", endpoint_id))
        .header(AUTHORIZATION, format!("Bearer {}", api_key))
        .header("content-type", "application/json")
        .body(Body::from(payload_bytes))
        .expect("build request");

    let response = app.oneshot(request).await.expect("execute request");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// Test webhook ingestion enforces payload size limits.
///
/// Verifies that payloads exceeding 10MB are rejected with
/// 413 Payload Too Large.
#[tokio::test]
async fn ingest_webhook_enforces_payload_size_limit() {
    let env = TestEnv::new().await.expect("test env setup");

    // Create tenant, API key, and endpoint
    let tenant_id = Uuid::new_v4();
    let endpoint_id = Uuid::new_v4();
    let api_key = "test-key-size-limit";
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

    sqlx::query(
        "INSERT INTO endpoints (id, tenant_id, name, url, is_active, max_retries, timeout_seconds, circuit_state)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(endpoint_id)
    .bind(tenant_id)
    .bind("test-endpoint")
    .bind("https://example.com/webhook")
    .bind(true)
    .bind(3)
    .bind(30)
    .bind("closed")
    .execute(env.pool())
    .await
    .expect("insert endpoint");

    let app = create_router(env.pool().clone());

    // Create payload larger than 10MB limit
    let large_payload = vec![b'x'; 11 * 1024 * 1024]; // 11MB

    let request = Request::builder()
        .method("POST")
        .uri(format!("/ingest/{}", endpoint_id))
        .header(AUTHORIZATION, format!("Bearer {}", api_key))
        .header("content-type", "application/octet-stream")
        .body(Body::from(large_payload))
        .expect("build request");

    let response = app.oneshot(request).await.expect("execute request");

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

/// Test webhook ingestion handles idempotency correctly.
///
/// Verifies that duplicate requests with same idempotency key
/// return the same event ID without creating duplicates.
#[tokio::test]
async fn ingest_webhook_handles_idempotency_correctly() {
    let env = TestEnv::new().await.expect("test env setup");

    // Create tenant, API key, and endpoint
    let tenant_id = Uuid::new_v4();
    let endpoint_id = Uuid::new_v4();
    let api_key = "test-key-idempotency";
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

    sqlx::query(
        "INSERT INTO endpoints (id, tenant_id, name, url, is_active, max_retries, timeout_seconds, circuit_state)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(endpoint_id)
    .bind(tenant_id)
    .bind("test-endpoint")
    .bind("https://example.com/webhook")
    .bind(true)
    .bind(3)
    .bind(30)
    .bind("closed")
    .execute(env.pool())
    .await
    .expect("insert endpoint");

    let payload = json!({
        "event": "user.updated",
        "data": {"id": 456}
    });
    let payload_bytes = serde_json::to_vec(&payload).expect("serialize payload");

    // First request with idempotency key
    let app1 = create_router(env.pool().clone());
    let request1 = Request::builder()
        .method("POST")
        .uri(format!("/ingest/{}", endpoint_id))
        .header(AUTHORIZATION, format!("Bearer {}", api_key))
        .header("content-type", "application/json")
        .header("x-idempotency-key", "idempotent-test-001")
        .body(Body::from(payload_bytes.clone()))
        .expect("build first request");

    let response1 = app1.oneshot(request1).await.expect("execute first request");
    assert_eq!(response1.status(), StatusCode::OK);

    let body1 = axum::body::to_bytes(response1.into_body(), usize::MAX)
        .await
        .expect("read first response body");
    let response1_json: serde_json::Value =
        serde_json::from_slice(&body1).expect("parse first response json");

    let first_event_id = response1_json["event_id"].as_str().expect("extract event ID");

    // Second request with same idempotency key
    let app2 = create_router(env.pool().clone());
    let request2 = Request::builder()
        .method("POST")
        .uri(format!("/ingest/{}", endpoint_id))
        .header(AUTHORIZATION, format!("Bearer {}", api_key))
        .header("content-type", "application/json")
        .header("x-idempotency-key", "idempotent-test-001")
        .body(Body::from(payload_bytes))
        .expect("build second request");

    let response2 = app2.oneshot(request2).await.expect("execute second request");
    assert_eq!(response2.status(), StatusCode::OK);

    let body2 = axum::body::to_bytes(response2.into_body(), usize::MAX)
        .await
        .expect("read second response body");
    let response2_json: serde_json::Value =
        serde_json::from_slice(&body2).expect("parse second response json");

    let second_event_id = response2_json["event_id"].as_str().expect("extract event ID");

    // Should return same event ID
    assert_eq!(first_event_id, second_event_id);

    // Should only have one event in database
    let event_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM webhook_events WHERE tenant_id = $1 AND endpoint_id = $2",
    )
    .bind(tenant_id)
    .bind(endpoint_id)
    .fetch_one(env.pool())
    .await
    .expect("count events");

    assert_eq!(event_count, 1);
}

/// Test webhook ingestion handles different content types correctly.
///
/// Verifies that various content types are properly stored and
/// the content-type header is preserved.
#[tokio::test]
async fn ingest_webhook_handles_different_content_types() {
    let env = TestEnv::new().await.expect("test env setup");

    // Create tenant, API key, and endpoint
    let tenant_id = Uuid::new_v4();
    let endpoint_id = Uuid::new_v4();
    let api_key = "test-key-content-types";
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

    sqlx::query(
        "INSERT INTO endpoints (id, tenant_id, name, url, is_active, max_retries, timeout_seconds, circuit_state)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(endpoint_id)
    .bind(tenant_id)
    .bind("test-endpoint")
    .bind("https://example.com/webhook")
    .bind(true)
    .bind(3)
    .bind(30)
    .bind("closed")
    .execute(env.pool())
    .await
    .expect("insert endpoint");

    // Test JSON content type
    let app = create_router(env.pool().clone());
    let json_payload = json!({"type": "json"});
    let json_bytes = serde_json::to_vec(&json_payload).expect("serialize json");

    let request = Request::builder()
        .method("POST")
        .uri(format!("/ingest/{}", endpoint_id))
        .header(AUTHORIZATION, format!("Bearer {}", api_key))
        .header("content-type", "application/json")
        .header("x-idempotency-key", "content-type-json")
        .body(Body::from(json_bytes.clone()))
        .expect("build json request");

    let response = app.oneshot(request).await.expect("execute json request");
    assert_eq!(response.status(), StatusCode::OK);

    // Verify JSON content type was stored
    let stored_content_type: String =
        sqlx::query_scalar("SELECT content_type FROM webhook_events WHERE source_event_id = $1")
            .bind("content-type-json")
            .fetch_one(env.pool())
            .await
            .expect("fetch content type");

    assert_eq!(stored_content_type, "application/json");

    // Test plain text content type
    let app = create_router(env.pool().clone());
    let text_payload = "plain text payload";

    let request = Request::builder()
        .method("POST")
        .uri(format!("/ingest/{}", endpoint_id))
        .header(AUTHORIZATION, format!("Bearer {}", api_key))
        .header("content-type", "text/plain")
        .header("x-idempotency-key", "content-type-text")
        .body(Body::from(text_payload))
        .expect("build text request");

    let response = app.oneshot(request).await.expect("execute text request");
    assert_eq!(response.status(), StatusCode::OK);

    // Verify text content type was stored
    let stored_content_type: String =
        sqlx::query_scalar("SELECT content_type FROM webhook_events WHERE source_event_id = $1")
            .bind("content-type-text")
            .fetch_one(env.pool())
            .await
            .expect("fetch content type");

    assert_eq!(stored_content_type, "text/plain");
}

/// Test webhook ingestion with empty idempotency key.
///
/// Verifies that webhooks without idempotency keys are processed
/// normally without deduplication.
#[tokio::test]
async fn ingest_webhook_without_idempotency_key() {
    let env = TestEnv::new().await.expect("test env setup");

    // Create tenant, API key, and endpoint
    let tenant_id = Uuid::new_v4();
    let endpoint_id = Uuid::new_v4();
    let api_key = "test-key-no-idempotency";
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

    sqlx::query(
        "INSERT INTO endpoints (id, tenant_id, name, url, is_active, max_retries, timeout_seconds, circuit_state)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(endpoint_id)
    .bind(tenant_id)
    .bind("test-endpoint")
    .bind("https://example.com/webhook")
    .bind(true)
    .bind(3)
    .bind(30)
    .bind("closed")
    .execute(env.pool())
    .await
    .expect("insert endpoint");

    let payload = json!({"event": "no_idempotency"});
    let payload_bytes = serde_json::to_vec(&payload).expect("serialize payload");

    // First request without idempotency key
    let app1 = create_router(env.pool().clone());
    let request1 = Request::builder()
        .method("POST")
        .uri(format!("/ingest/{}", endpoint_id))
        .header(AUTHORIZATION, format!("Bearer {}", api_key))
        .header("content-type", "application/json")
        .body(Body::from(payload_bytes.clone()))
        .expect("build first request");

    let response1 = app1.oneshot(request1).await.expect("execute first request");
    assert_eq!(response1.status(), StatusCode::OK);

    // Second identical request without idempotency key
    let app2 = create_router(env.pool().clone());
    let request2 = Request::builder()
        .method("POST")
        .uri(format!("/ingest/{}", endpoint_id))
        .header(AUTHORIZATION, format!("Bearer {}", api_key))
        .header("content-type", "application/json")
        .body(Body::from(payload_bytes))
        .expect("build second request");

    let response2 = app2.oneshot(request2).await.expect("execute second request");
    assert_eq!(response2.status(), StatusCode::OK);

    // Should have created two separate events
    let event_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM webhook_events WHERE tenant_id = $1 AND endpoint_id = $2",
    )
    .bind(tenant_id)
    .bind(endpoint_id)
    .fetch_one(env.pool())
    .await
    .expect("count events");

    assert_eq!(event_count, 2);
}
