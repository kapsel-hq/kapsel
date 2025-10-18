//! Integration tests for delivery engine component.
//!
//! Tests webhook delivery engine processing of pending events from database
//! with engine lifecycle, event claiming, and coordination workflows.
//! - Error handling during event processing
//! - Integration with HTTP client and database

use bytes::Bytes;
use http::StatusCode;
use kapsel_testing::{http::MockResponse, TestEnv};
use uuid::Uuid;

#[tokio::test]
async fn delivery_engine_processes_pending_events() {
    let env = TestEnv::new().await.expect("test env");

    // Setup: Create tenant, endpoint, and event
    let tenant_id = Uuid::new_v4();
    let endpoint_id = Uuid::new_v4();
    let event_id = Uuid::new_v4();

    sqlx::query("INSERT INTO tenants (id, name, plan, api_key) VALUES ($1, $2, $3, $4)")
        .bind(tenant_id)
        .bind("test-tenant")
        .bind("free")
        .bind("test-key")
        .execute(env.pool())
        .await
        .unwrap();

    // Mock expects webhook delivery
    env.http_mock
        .mock_simple("/webhook", MockResponse::Success {
            status: StatusCode::OK,
            body: Bytes::from_static(b"OK"),
        })
        .await;

    let webhook_url = env.http_mock.endpoint_url("/webhook");
    sqlx::query(
        "INSERT INTO endpoints (id, tenant_id, name, url, signing_secret, max_retries, timeout_seconds, circuit_state)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"
    )
    .bind(endpoint_id)
    .bind(tenant_id)
    .bind("test-endpoint")
    .bind(&webhook_url)
    .bind("secret")
    .bind(5)
    .bind(30)
    .bind("closed")
    .execute(env.pool())
    .await
    .unwrap();

    // Insert pending event
    sqlx::query(
        "INSERT INTO webhook_events
         (id, tenant_id, endpoint_id, source_event_id, idempotency_strategy, status,
          headers, body, content_type, payload_size, failure_count, received_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, NOW())",
    )
    .bind(event_id)
    .bind(tenant_id)
    .bind(endpoint_id)
    .bind("source-123")
    .bind("header")
    .bind("pending")
    .bind(serde_json::json!({}))
    .bind(b"test payload".as_slice())
    .bind("application/json")
    .bind(12i32)
    .bind(0i32)
    .execute(env.pool())
    .await
    .unwrap();

    // Start delivery engine
    let config = kapsel_delivery::worker::DeliveryConfig::default();
    let mut engine =
        kapsel_delivery::worker::DeliveryEngine::new(env.create_pool(), config).unwrap();

    engine.start().await.unwrap();

    // Wait for processing
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    // Verify event is marked delivered
    let status: String = sqlx::query_scalar("SELECT status FROM webhook_events WHERE id = $1")
        .bind(event_id)
        .fetch_one(env.pool())
        .await
        .unwrap();

    assert_eq!(status, "delivered");

    engine.shutdown().await.unwrap();
}
