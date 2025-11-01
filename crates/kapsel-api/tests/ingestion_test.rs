//! Integration tests for webhook ingestion endpoint.
//!
//! Tests the `/ingest/{endpoint_id}` endpoint with authentication, payload
//! validation, signature verification, and error handling scenarios.

use std::sync::Arc;

use anyhow::Result;
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
async fn ingest_webhook_succeeds_with_valid_request() -> Result<()> {
    TestEnv::run_isolated_test(|env| async move {
        let tenant_id = env.create_tenant("test-tenant").await?;
        let (api_key, _key_hash) = env.create_api_key(tenant_id, "test-key").await?;
        let endpoint_id = env.create_endpoint(tenant_id, "https://example.com/webhook").await?;

        let app = create_test_router(env.pool().clone(), Arc::new(env.clock.clone()));

        // Prepare webhook payload
        let payload = json!({
            "event": "user.created",
            "data": {
                "id": 123,
                "email": "test@example.com"
            }
        });
        let payload_bytes = serde_json::to_vec(&payload)?;

        // Make ingestion request
        let request = Request::builder()
            .method("POST")
            .uri(format!("/ingest/{endpoint_id}"))
            .header(AUTHORIZATION, format!("Bearer {api_key}"))
            .header("content-type", "application/json")
            .header("x-idempotency-key", "test-event-001")
            .body(Body::from(payload_bytes.clone()))?;

        let response = app.oneshot(request).await?;

        // Verify successful response
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
        let response_json: serde_json::Value = serde_json::from_slice(&body)?;

        assert!(response_json["event_id"].is_string());
        assert_eq!(response_json["status"], "received");

        // Verify event was persisted to database using repository
        let events = env.storage().webhook_events.find_by_tenant(tenant_id, Some(10)).await?;
        assert_eq!(events.len(), 1, "exactly one event should be persisted");

        let event = &events[0];
        assert_eq!(event.endpoint_id, endpoint_id);
        assert_eq!(event.status.to_string(), "received");
        assert_eq!(event.body, payload_bytes);
        assert_eq!(event.content_type, "application/json");
        assert_eq!(event.payload_size, i32::try_from(payload_bytes.len()).unwrap_or(i32::MAX));
        assert_eq!(event.source_event_id, "test-event-001");

        Ok(())
    })
    .await
}

/// Test webhook ingestion fails with invalid authentication.
///
/// Verifies that requests without valid API keys are rejected
/// with appropriate error responses.
#[tokio::test]
async fn ingest_webhook_fails_with_invalid_auth() -> Result<()> {
    TestEnv::run_isolated_test(|env| async move {
        let app = create_test_router(env.pool().clone(), Arc::new(env.clock.clone()));

        let endpoint_id = Uuid::new_v4();
        let payload = json!({"event": "test"});
        let payload_bytes = serde_json::to_vec(&payload)?;

        let request = Request::builder()
            .method("POST")
            .uri(format!("/ingest/{endpoint_id}"))
            .header(AUTHORIZATION, "Bearer invalid-key")
            .header("content-type", "application/json")
            .body(Body::from(payload_bytes))?;

        let response = app.oneshot(request).await?;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        Ok(())
    })
    .await
}

/// Test webhook ingestion fails with missing authentication.
///
/// Verifies that requests without Authorization header are rejected.
#[tokio::test]
async fn ingest_webhook_fails_without_auth() -> Result<()> {
    TestEnv::run_isolated_test(|env| async move {
        let app = create_test_router(env.pool().clone(), Arc::new(env.clock.clone()));

        let endpoint_id = Uuid::new_v4();
        let payload = json!({"event": "test"});
        let payload_bytes = serde_json::to_vec(&payload)?;

        let request = Request::builder()
            .method("POST")
            .uri(format!("/ingest/{endpoint_id}"))
            .header("content-type", "application/json")
            .body(Body::from(payload_bytes))?;

        let response = app.oneshot(request).await?;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        Ok(())
    })
    .await
}

/// Test webhook ingestion fails with nonexistent endpoint.
///
/// Verifies that requests to invalid endpoint IDs are rejected
/// with 404 Not Found.
#[tokio::test]
async fn ingest_webhook_fails_with_nonexistent_endpoint() -> Result<()> {
    TestEnv::run_isolated_test(|env| async move {
        // Create valid tenant and API key using TestEnv helpers
        let tenant_id = env.create_tenant("test-tenant").await?;
        let (api_key, _key_hash) = env.create_api_key(tenant_id, "test-key").await?;

        let app = create_test_router(env.pool().clone(), Arc::new(env.clock.clone()));

        // Use non-existent endpoint ID
        let endpoint_id = Uuid::new_v4();
        let payload = json!({"event": "test"});
        let payload_bytes = serde_json::to_vec(&payload)?;

        let request = Request::builder()
            .method("POST")
            .uri(format!("/ingest/{endpoint_id}"))
            .header(AUTHORIZATION, format!("Bearer {api_key}"))
            .header("content-type", "application/json")
            .body(Body::from(payload_bytes))?;

        let response = app.oneshot(request).await?;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        Ok(())
    })
    .await
}

/// Test webhook ingestion enforces payload size limits.
///
/// Verifies that payloads exceeding 10MB are rejected with
/// 413 Payload Too Large.
#[tokio::test]
async fn ingest_webhook_enforces_payload_size_limit() -> Result<()> {
    TestEnv::run_isolated_test(|env| async move {
        // Create tenant, API key, and endpoint using TestEnv helpers
        let tenant_id = env.create_tenant("test-tenant").await?;
        let (api_key, _key_hash) = env.create_api_key(tenant_id, "test-key").await?;
        let endpoint_id = env.create_endpoint(tenant_id, "https://example.com/webhook").await?;

        let app = create_test_router(env.pool().clone(), Arc::new(env.clock.clone()));

        // Create payload larger than 10MB (10 * 1024 * 1024 + 1 bytes)
        let payload_size = 10 * 1024 * 1024 + 1;
        let large_payload = vec![b'x'; payload_size];

        let request = Request::builder()
            .method("POST")
            .uri(format!("/ingest/{endpoint_id}"))
            .header(AUTHORIZATION, format!("Bearer {api_key}"))
            .header("content-type", "application/octet-stream")
            .body(Body::from(large_payload))?;

        let response = app.oneshot(request).await?;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

        Ok(())
    })
    .await
}

/// Test webhook ingestion handles idempotency correctly.
///
/// Verifies that duplicate requests with same idempotency key
/// return the same event ID without creating duplicates.
#[tokio::test]
async fn ingest_webhook_handles_idempotency_correctly() -> Result<()> {
    TestEnv::run_isolated_test(|env| async move {
        // Create tenant, API key, and endpoint using TestEnv helpers
        let tenant_id = env.create_tenant("test-tenant").await?;
        let (api_key, _key_hash) = env.create_api_key(tenant_id, "test-key").await?;
        let endpoint_id = env.create_endpoint(tenant_id, "https://example.com/webhook").await?;

        let payload = json!({
            "event": "user.updated",
            "data": {"id": 456}
        });
        let payload_bytes = serde_json::to_vec(&payload)?;

        // First request with idempotency key
        let app = create_test_router(env.pool().clone(), Arc::new(env.clock.clone()));
        let request1 = Request::builder()
            .method("POST")
            .uri(format!("/ingest/{endpoint_id}"))
            .header(AUTHORIZATION, format!("Bearer {api_key}"))
            .header("content-type", "application/json")
            .header("x-idempotency-key", "idempotent-test-001")
            .body(Body::from(payload_bytes.clone()))?;

        let response1 = app.clone().oneshot(request1).await?;
        assert_eq!(response1.status(), StatusCode::OK);

        let body1 = axum::body::to_bytes(response1.into_body(), usize::MAX).await?;
        let response1_json: serde_json::Value = serde_json::from_slice(&body1)?;

        let first_event_id = response1_json["event_id"].as_str().expect("extract event ID");

        // Second request with same idempotency key
        let request2 = Request::builder()
            .method("POST")
            .uri(format!("/ingest/{endpoint_id}"))
            .header(AUTHORIZATION, format!("Bearer {api_key}"))
            .header("content-type", "application/json")
            .header("x-idempotency-key", "idempotent-test-001")
            .body(Body::from(payload_bytes))?;

        let response2 = app.oneshot(request2).await?;
        assert_eq!(response2.status(), StatusCode::OK);

        let body2 = axum::body::to_bytes(response2.into_body(), usize::MAX).await?;
        let response2_json: serde_json::Value = serde_json::from_slice(&body2)?;

        let second_event_id = response2_json["event_id"].as_str().expect("extract event ID");

        // Should return same event ID
        assert_eq!(first_event_id, second_event_id);

        // Should only have one event in database
        let events = env.storage().webhook_events.find_by_tenant(tenant_id, Some(10)).await?;
        assert_eq!(events.len(), 1, "should have exactly one event due to idempotency");

        Ok(())
    })
    .await
}

/// Test webhook ingestion handles different content types correctly.
///
/// Verifies that various content types are properly stored and
/// the content-type header is preserved.
#[tokio::test]
async fn ingest_webhook_handles_different_content_types() -> Result<()> {
    TestEnv::run_isolated_test(|env| async move {
        // Create tenant, API key, and endpoint using TestEnv helpers
        let tenant_id = env.create_tenant("test-tenant").await?;
        let (api_key, _key_hash) = env.create_api_key(tenant_id, "test-key").await?;
        let endpoint_id = env.create_endpoint(tenant_id, "https://example.com/webhook").await?;

        // Test JSON content type
        let app = create_test_router(env.pool().clone(), Arc::new(env.clock.clone()));
        let json_payload = json!({"content_type": "test"});
        let json_bytes = serde_json::to_vec(&json_payload)?;

        let request = Request::builder()
            .method("POST")
            .uri(format!("/ingest/{endpoint_id}"))
            .header(AUTHORIZATION, format!("Bearer {api_key}"))
            .header("content-type", "application/json")
            .header("x-idempotency-key", "content-type-json")
            .body(Body::from(json_bytes.clone()))?;

        let response = app.oneshot(request).await?;
        assert_eq!(response.status(), StatusCode::OK);

        // Verify JSON content type was stored
        let stored_content_type: String = env
            .storage()
            .webhook_events
            .find_by_tenant(tenant_id, Some(10))
            .await?
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
            .body(Body::from(text_payload))?;

        let response = app.oneshot(request).await?;
        assert_eq!(response.status(), StatusCode::OK);

        // Verify text content type was stored
        let stored_content_type: String = env
            .storage()
            .webhook_events
            .find_by_tenant(tenant_id, Some(10))
            .await?
            .into_iter()
            .find(|e| e.source_event_id == "content-type-text")
            .expect("find text event")
            .content_type;

        assert_eq!(stored_content_type, "text/plain");

        Ok(())
    })
    .await
}

/// Test webhook ingestion with empty idempotency key.
///
/// Verifies that webhooks without idempotency keys are processed
/// normally without deduplication.
#[tokio::test]
async fn ingest_webhook_without_idempotency_key() -> Result<()> {
    TestEnv::run_isolated_test(|env| async move {
        let tenant_id = env.create_tenant("test-tenant").await?;
        let (api_key, _key_hash) = env.create_api_key(tenant_id, "test-key").await?;
        let endpoint_id = env.create_endpoint(tenant_id, "https://example.com/webhook").await?;

        let app = create_test_router(env.pool().clone(), Arc::new(env.clock.clone()));

        let payload = json!({"event": "no_idempotency_test"});
        let payload_bytes = serde_json::to_vec(&payload)?;

        // First request without idempotency key
        let request1 = Request::builder()
            .method("POST")
            .uri(format!("/ingest/{endpoint_id}"))
            .header(AUTHORIZATION, format!("Bearer {api_key}"))
            .header("content-type", "application/json")
            .body(Body::from(payload_bytes.clone()))?;

        let response1 = app.clone().oneshot(request1).await?;
        assert_eq!(response1.status(), StatusCode::OK);

        // Second identical request without idempotency key
        let request2 = Request::builder()
            .method("POST")
            .uri(format!("/ingest/{endpoint_id}"))
            .header(AUTHORIZATION, format!("Bearer {api_key}"))
            .header("content-type", "application/json")
            .body(Body::from(payload_bytes))?;

        let response2 = app.oneshot(request2).await?;
        assert_eq!(response2.status(), StatusCode::OK);

        // Verify both events were created (no deduplication without idempotency key)
        let events = env.storage().webhook_events.find_by_tenant(tenant_id, Some(10)).await?;
        assert_eq!(events.len(), 2, "without idempotency key, both events should be created");

        // Verify both events have different IDs
        assert_ne!(events[0].id, events[1].id);

        Ok(())
    })
    .await
}
