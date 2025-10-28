//! Integration tests for webhook ingestion endpoint.
//!
//! Tests the `/ingest/{endpoint_id}` endpoint with authentication, payload
//! validation, signature verification, and error handling scenarios.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{header::AUTHORIZATION, Request, StatusCode},
};
use kapsel_api::create_test_router;
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
    let env = TestEnv::new_isolated().await.expect("test env setup");

    let mut tx = env.pool().begin().await.expect("begin transaction");

    let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await.expect("create tenant");
    let (api_key, _key_hash) =
        env.create_api_key_tx(&mut tx, tenant_id, "test-key").await.expect("create api key");
    let endpoint_id = env
        .create_endpoint_tx(&mut tx, tenant_id, "https://example.com/webhook")
        .await
        .expect("create endpoint");

    tx.commit().await.expect("commit transaction");

    let app = create_test_router(env.pool().clone(), Arc::new(env.clock.clone()));

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
        .uri(format!("/ingest/{endpoint_id}"))
        .header(AUTHORIZATION, format!("Bearer {api_key}"))
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

    // Verify event was persisted to database using repository
    let events = env
        .storage()
        .webhook_events
        .find_by_tenant(tenant_id, Some(10))
        .await
        .expect("find events");
    assert_eq!(events.len(), 1, "exactly one event should be persisted");

    let event = &events[0];
    assert_eq!(event.endpoint_id, endpoint_id);
    assert_eq!(event.status.to_string(), "received");
    assert_eq!(event.body, payload_bytes);
    assert_eq!(event.content_type, "application/json");
    assert_eq!(event.payload_size, i32::try_from(payload_bytes.len()).unwrap_or(i32::MAX));
    assert_eq!(event.source_event_id, "test-event-001");
}

/// Test webhook ingestion fails with invalid authentication.
///
/// Verifies that requests without valid API keys are rejected
/// with appropriate error responses.
#[tokio::test]
async fn ingest_webhook_fails_with_invalid_auth() {
    let env = TestEnv::new_isolated().await.expect("test env setup");
    let app = create_test_router(env.pool().clone(), Arc::new(env.clock.clone()));

    let endpoint_id = Uuid::new_v4();
    let payload = json!({"event": "test"});
    let payload_bytes = serde_json::to_vec(&payload).expect("serialize payload");

    let request = Request::builder()
        .method("POST")
        .uri(format!("/ingest/{endpoint_id}"))
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
    let env = TestEnv::new_isolated().await.expect("test env setup");
    let app = create_test_router(env.pool().clone(), Arc::new(env.clock.clone()));

    let endpoint_id = Uuid::new_v4();
    let payload = json!({"event": "test"});
    let payload_bytes = serde_json::to_vec(&payload).expect("serialize payload");

    let request = Request::builder()
        .method("POST")
        .uri(format!("/ingest/{endpoint_id}"))
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
    let env = TestEnv::new_isolated().await.expect("test env setup");

    // Create valid tenant and API key using TestEnv helpers
    let mut tx = env.pool().begin().await.expect("begin transaction");
    let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await.expect("create tenant");
    let (api_key, _key_hash) =
        env.create_api_key_tx(&mut tx, tenant_id, "test-key").await.expect("create api key");
    tx.commit().await.expect("commit transaction");

    let app = create_test_router(env.pool().clone(), Arc::new(env.clock.clone()));

    // Use non-existent endpoint ID
    let endpoint_id = Uuid::new_v4();
    let payload = json!({"event": "test"});
    let payload_bytes = serde_json::to_vec(&payload).expect("serialize payload");

    let request = Request::builder()
        .method("POST")
        .uri(format!("/ingest/{endpoint_id}"))
        .header(AUTHORIZATION, format!("Bearer {api_key}"))
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
    let env = TestEnv::new_isolated().await.expect("test env setup");

    // Create tenant, API key, and endpoint using TestEnv helpers
    let mut tx = env.pool().begin().await.expect("begin transaction");
    let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await.expect("create tenant");
    let (api_key, _key_hash) =
        env.create_api_key_tx(&mut tx, tenant_id, "test-key").await.expect("create api key");
    let endpoint_id = env
        .create_endpoint_tx(&mut tx, tenant_id, "https://example.com/webhook")
        .await
        .expect("create endpoint");
    tx.commit().await.expect("commit transaction");

    let app = create_test_router(env.pool().clone(), Arc::new(env.clock.clone()));

    // Create payload larger than 10MB limit
    let large_payload = vec![b'x'; 11 * 1024 * 1024]; // 11MB

    let request = Request::builder()
        .method("POST")
        .uri(format!("/ingest/{endpoint_id}"))
        .header(AUTHORIZATION, format!("Bearer {api_key}"))
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
    let env = TestEnv::new_isolated().await.expect("test env setup");

    // Create tenant, API key, and endpoint using TestEnv helpers
    let mut tx = env.pool().begin().await.expect("begin transaction");
    let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await.expect("create tenant");
    let (api_key, _key_hash) =
        env.create_api_key_tx(&mut tx, tenant_id, "test-key").await.expect("create api key");
    let endpoint_id = env
        .create_endpoint_tx(&mut tx, tenant_id, "https://example.com/webhook")
        .await
        .expect("create endpoint");
    tx.commit().await.expect("commit transaction");

    let payload = json!({
        "event": "user.updated",
        "data": {"id": 456}
    });
    let payload_bytes = serde_json::to_vec(&payload).expect("serialize payload");

    // First request with idempotency key
    let app = create_test_router(env.pool().clone(), Arc::new(env.clock.clone()));
    let request1 = Request::builder()
        .method("POST")
        .uri(format!("/ingest/{endpoint_id}"))
        .header(AUTHORIZATION, format!("Bearer {api_key}"))
        .header("content-type", "application/json")
        .header("x-idempotency-key", "idempotent-test-001")
        .body(Body::from(payload_bytes.clone()))
        .expect("build first request");

    let response1 = app.clone().oneshot(request1).await.expect("execute first request");
    assert_eq!(response1.status(), StatusCode::OK);

    let body1 = axum::body::to_bytes(response1.into_body(), usize::MAX)
        .await
        .expect("read first response body");
    let response1_json: serde_json::Value =
        serde_json::from_slice(&body1).expect("parse first response json");

    let first_event_id = response1_json["event_id"].as_str().expect("extract event ID");

    // Second request with same idempotency key
    let app2 = create_test_router(env.pool().clone(), Arc::new(env.clock.clone()));
    let request2 = Request::builder()
        .method("POST")
        .uri(format!("/ingest/{endpoint_id}"))
        .header(AUTHORIZATION, format!("Bearer {api_key}"))
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
    let events = env
        .storage()
        .webhook_events
        .find_by_tenant(tenant_id, Some(10))
        .await
        .expect("find events");
    assert_eq!(events.len(), 1, "should have exactly one event due to idempotency");
}

/// Test webhook ingestion handles different content types correctly.
///
/// Verifies that various content types are properly stored and
/// the content-type header is preserved.
#[tokio::test]
async fn ingest_webhook_handles_different_content_types() {
    let env = TestEnv::new_isolated().await.expect("test env setup");

    // Create tenant, API key, and endpoint using TestEnv helpers
    let mut tx = env.pool().begin().await.expect("begin transaction");
    let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await.expect("create tenant");
    let (api_key, _key_hash) =
        env.create_api_key_tx(&mut tx, tenant_id, "test-key").await.expect("create api key");
    let endpoint_id = env
        .create_endpoint_tx(&mut tx, tenant_id, "https://example.com/webhook")
        .await
        .expect("create endpoint");
    tx.commit().await.expect("commit transaction");

    // Test JSON content type
    let app = create_test_router(env.pool().clone(), Arc::new(env.clock.clone()));
    let json_payload = json!({"type": "json"});
    let json_bytes = serde_json::to_vec(&json_payload).expect("serialize json");

    let request = Request::builder()
        .method("POST")
        .uri(format!("/ingest/{endpoint_id}"))
        .header(AUTHORIZATION, format!("Bearer {api_key}"))
        .header("content-type", "application/json")
        .header("x-idempotency-key", "content-type-json")
        .body(Body::from(json_bytes.clone()))
        .expect("build json request");

    let response = app.oneshot(request).await.expect("execute json request");
    assert_eq!(response.status(), StatusCode::OK);

    // Verify JSON content type was stored
    let stored_content_type: String = env
        .storage()
        .webhook_events
        .find_by_tenant(tenant_id, Some(10))
        .await
        .expect("find events")
        .into_iter()
        .find(|e| e.source_event_id == "content-type-json")
        .expect("find json event")
        .content_type;

    assert_eq!(stored_content_type, "application/json");

    // Test plain text content type
    let app = create_test_router(env.pool().clone(), Arc::new(env.clock.clone()));
    let text_payload = "plain text payload";

    let request = Request::builder()
        .method("POST")
        .uri(format!("/ingest/{endpoint_id}"))
        .header(AUTHORIZATION, format!("Bearer {api_key}"))
        .header("content-type", "text/plain")
        .header("x-idempotency-key", "content-type-text")
        .body(Body::from(text_payload))
        .expect("build text request");

    let response = app.oneshot(request).await.expect("execute text request");
    assert_eq!(response.status(), StatusCode::OK);

    // Verify text content type was stored
    let stored_content_type: String = env
        .storage()
        .webhook_events
        .find_by_tenant(tenant_id, Some(10))
        .await
        .expect("find events")
        .into_iter()
        .find(|e| e.source_event_id == "content-type-text")
        .expect("find text event")
        .content_type;

    assert_eq!(stored_content_type, "text/plain");
}

/// Test webhook ingestion with empty idempotency key.
///
/// Verifies that webhooks without idempotency keys are processed
/// normally without deduplication.
#[tokio::test]
async fn ingest_webhook_without_idempotency_key() {
    let env = TestEnv::new_isolated().await.expect("test env setup");

    let mut tx = env.pool().begin().await.expect("begin transaction");
    let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await.expect("create tenant");
    let (api_key, _key_hash) =
        env.create_api_key_tx(&mut tx, tenant_id, "test-key").await.expect("create api key");
    let endpoint_id = env
        .create_endpoint_tx(&mut tx, tenant_id, "https://example.com/webhook")
        .await
        .expect("create endpoint");

    tx.commit().await.expect("commit transaction");

    let payload = json!({"event": "no_idempotency"});
    let payload_bytes = serde_json::to_vec(&payload).expect("serialize payload");

    // First request without idempotency key
    let app = create_test_router(env.pool().clone(), Arc::new(env.clock.clone()));
    let request1 = Request::builder()
        .method("POST")
        .uri(format!("/ingest/{endpoint_id}"))
        .header(AUTHORIZATION, format!("Bearer {api_key}"))
        .header("content-type", "application/json")
        .body(Body::from(payload_bytes.clone()))
        .expect("build first request");

    let response1 = app.clone().oneshot(request1).await.expect("execute first request");
    assert_eq!(response1.status(), StatusCode::OK);

    // Second identical request without idempotency key
    let app2 = create_test_router(env.pool().clone(), Arc::new(env.clock.clone()));
    let request2 = Request::builder()
        .method("POST")
        .uri(format!("/ingest/{endpoint_id}"))
        .header(AUTHORIZATION, format!("Bearer {api_key}"))
        .header("content-type", "application/json")
        .body(Body::from(payload_bytes))
        .expect("build second request");

    let response2 = app2.oneshot(request2).await.expect("execute second request");
    assert_eq!(response2.status(), StatusCode::OK);

    // Should have created two separate events
    let event_count = env
        .storage()
        .webhook_events
        .count_by_tenant_and_endpoint(tenant_id, endpoint_id)
        .await
        .expect("count events");

    assert_eq!(event_count, 2);
}
