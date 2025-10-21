//! Integration tests for delivery engine component.
//!
//! Tests webhook delivery engine processing of pending events from database
//! with engine lifecycle, event claiming, and coordination workflows.
//! - Error handling during event processing
//! - Integration with HTTP client and database

use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use http::StatusCode;
use kapsel_core::Clock;
use kapsel_delivery::worker::{DeliveryConfig, DeliveryEngine};
use kapsel_testing::{http::MockResponse, TestEnv};
use uuid::Uuid;

#[tokio::test]
async fn delivery_engine_processes_pending_events() {
    let env = TestEnv::new().await.expect("test env");

    // Setup: Create tenant, endpoint, and event using TestEnv
    let tenant_id = Uuid::new_v4();
    let endpoint_id = Uuid::new_v4();

    sqlx::query("INSERT INTO tenants (id, name, plan) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind("test-tenant")
        .bind("enterprise")
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

    // Create test webhook using TestWebhook builder
    let webhook = kapsel_testing::fixtures::WebhookBuilder::new()
        .tenant(tenant_id)
        .endpoint(endpoint_id)
        .source_event("source-123")
        .body(b"test payload".to_vec())
        .content_type("application/json")
        .build();

    let event_id = env.ingest_webhook(&webhook).await.unwrap();

    // Create delivery engine with test clock
    let config = DeliveryConfig {
        worker_count: 1, // Single worker for deterministic testing
        batch_size: 10,
        poll_interval: Duration::from_secs(1),
        ..DeliveryConfig::default()
    };

    let mut engine = DeliveryEngine::new(
        env.create_pool(),
        config,
        Arc::new(env.clock.clone()) as Arc<dyn Clock>,
    )
    .unwrap();
    engine.start().await.unwrap();

    // Process delivery deterministically
    env.run_delivery_cycle().await.unwrap();

    // Verify event is marked delivered
    let status = env.find_webhook_status(event_id).await.unwrap();
    assert_eq!(status, "delivered");

    engine.shutdown().await.unwrap();
}
