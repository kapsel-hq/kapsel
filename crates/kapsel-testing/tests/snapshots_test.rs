//! Tests for snapshot functionality used in regression testing.

use std::time::Duration;

use anyhow::Result;
use kapsel_core::EventStatus;
use kapsel_testing::{fixtures::WebhookBuilder, http::MockResponse, TestEnv};

#[tokio::test]
async fn snapshot_events_table_works() -> Result<()> {
    let env = TestEnv::new_isolated().await?;

    // Create test data within transaction
    let mut tx = env.pool().begin().await?;
    let tenant_id = env.create_tenant_tx(&mut tx, "snapshot-test").await?;
    let endpoint_id = env.create_endpoint_tx(&mut tx, tenant_id, &env.http_mock.url()).await?;

    let webhook = WebhookBuilder::new()
        .tenant(tenant_id.0)
        .endpoint(endpoint_id.0)
        .source_event("snap-test-001")
        .body(b"snapshot test data".to_vec())
        .build();

    let _event_id = env.ingest_webhook_tx(&mut tx, &webhook).await?;

    // Commit for snapshot testing
    tx.commit().await?;

    // Test direct snapshot method
    let snapshot = env.snapshot_events_table().await?;
    assert!(snapshot.contains("Events Table Snapshot"));
    assert!(snapshot.contains("snap-test-001"));
    assert!(snapshot.contains("status: pending"));

    Ok(())
}

#[tokio::test]
async fn snapshot_delivery_attempts_works() -> Result<()> {
    let mut env = TestEnv::new_isolated().await?;
    let mut tx = env.pool().begin().await?;

    // Setup successful delivery within transaction
    let tenant_id = env.create_tenant_tx(&mut tx, "delivery-snapshot").await?;
    let endpoint_id = env.create_endpoint_tx(&mut tx, tenant_id, &env.http_mock.url()).await?;

    env.http_mock
        .mock_simple("/", MockResponse::Success {
            status: reqwest::StatusCode::OK,
            body: bytes::Bytes::from_static(b"OK"),
        })
        .await;

    let webhook = WebhookBuilder::new()
        .tenant(tenant_id.0)
        .endpoint(endpoint_id.0)
        .source_event("delivery-snap-001")
        .build();

    let _event_id = env.ingest_webhook_tx(&mut tx, &webhook).await?;

    // Commit for delivery testing
    tx.commit().await?;
    env.run_delivery_cycle().await?;

    // Test delivery attempts snapshot
    let snapshot = env.snapshot_delivery_attempts().await?;
    assert!(snapshot.contains("Delivery Attempts Snapshot"));
    assert!(snapshot.contains("attempt_number: 1"));
    assert!(snapshot.contains("response_status: Some(200)"));

    Ok(())
}

#[tokio::test]
async fn snapshot_database_schema_works() -> Result<()> {
    let env = TestEnv::new_isolated().await?;
    let snapshot = env.snapshot_database_schema().await?;
    assert!(snapshot.contains("Database Schema Snapshot"));
    assert!(snapshot.contains("Table: webhook_events"));
    assert!(snapshot.contains("Table: tenants"));
    assert!(snapshot.contains("Indexes:"));
    Ok(())
}

#[tokio::test]
async fn comprehensive_end_to_end_snapshot_test() -> Result<()> {
    let mut env = TestEnv::new_isolated().await?;
    let mut tx = env.pool().begin().await?;

    // Testing complete snapshot functionality with transaction isolation
    let tenant_id = env.create_tenant_tx(&mut tx, "e2e-snapshot").await?;
    let endpoint_id = env.create_endpoint_tx(&mut tx, tenant_id, &env.http_mock.url()).await?;

    // TEST 1: Verify clean transaction state
    let event_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM webhook_events").fetch_one(&mut *tx).await?;
    assert_eq!(event_count, 0, "Transaction should start with clean state");

    // Setup HTTP responses: fail -> succeed pattern
    env.http_mock
        .mock_sequence()
        .respond_with(503, "Service Unavailable") // First attempt fails
        .respond_with(200, "OK")                  // Second attempt succeeds
        .build()
        .await;

    // Create and ingest webhook within transaction
    let webhook = WebhookBuilder::new()
        .tenant(tenant_id.0)
        .endpoint(endpoint_id.0)
        .source_event("comprehensive-test")
        .body(b"comprehensive test payload".to_vec())
        .build();

    let event_id = env.ingest_webhook_tx(&mut tx, &webhook).await?;

    // Verify event exists within transaction
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM webhook_events WHERE id = $1")
        .bind(event_id.0)
        .fetch_one(&mut *tx)
        .await?;
    assert_eq!(count, 1, "Event should exist within transaction");

    // Commit for delivery testing
    tx.commit().await?;

    // Test delivery processing - first attempt fails (503), second succeeds (200)
    env.run_delivery_cycle().await?; // Gets 503, stays pending

    // Advance time to make webhook ready for retry after backoff delay
    env.advance_time(Duration::from_secs(5));

    env.run_delivery_cycle().await?; // Gets 200, becomes delivered

    // Verify event was processed successfully
    let status = env.find_webhook_status(event_id).await?;
    assert_eq!(status, EventStatus::Delivered, "Event should have correct status after delivery");

    // Verify delivery attempts were recorded (first fails with 503, second succeeds
    // with 200)
    let attempt_count = env.count_delivery_attempts(event_id).await?;
    assert_eq!(attempt_count, 2, "Should have two delivery attempts (one failed, one successful)");

    Ok(())
}

#[tokio::test]
async fn validation_first_snapshot_test() -> Result<()> {
    let mut env = TestEnv::new_isolated().await?;
    let mut tx = env.pool().begin().await?;

    let tenant_id = env.create_tenant_tx(&mut tx, "validation").await?;
    let endpoint_id = env.create_endpoint_tx(&mut tx, tenant_id, &env.http_mock.url()).await?;

    // Setup HTTP behavior
    env.http_mock
        .mock_simple("/", MockResponse::Success {
            status: reqwest::StatusCode::OK,
            body: bytes::Bytes::from_static(b"OK"),
        })
        .await;

    let webhook = WebhookBuilder::new()
        .tenant(tenant_id.0)
        .endpoint(endpoint_id.0)
        .source_event("validation-test")
        .body(b"validation test payload".to_vec())
        .build();

    // STEP 1: Validate initial clean state within transaction
    let initial_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM webhook_events").fetch_one(&mut *tx).await?;
    assert_eq!(initial_count, 0, "Transaction should start with clean state");

    // STEP 2: Ingest webhook within transaction
    let event_id = env.ingest_webhook_tx(&mut tx, &webhook).await?;

    let post_ingestion_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM webhook_events WHERE id = $1")
            .bind(event_id.0)
            .fetch_one(&mut *tx)
            .await?;
    assert_eq!(post_ingestion_count, 1, "Should have exactly one event within transaction");

    // STEP 3: Commit and test delivery
    tx.commit().await?;

    env.run_delivery_cycle().await?;
    let status = env.find_webhook_status(event_id).await?;
    assert_eq!(status, EventStatus::Delivered, "Event should be delivered");

    let attempt_count = env.count_delivery_attempts(event_id).await?;
    assert_eq!(attempt_count, 1, "Should have exactly one delivery attempt");

    Ok(())
}

#[tokio::test]
async fn snapshot_with_multiple_tenants() -> Result<()> {
    let mut env = TestEnv::new_isolated().await?;
    let mut tx = env.pool().begin().await?;

    // Create multiple tenants with webhooks
    let tenant1 = env.create_tenant_tx(&mut tx, "tenant-1").await?;
    let endpoint1 = env.create_endpoint_tx(&mut tx, tenant1, &env.http_mock.url()).await?;

    let tenant2 = env.create_tenant_tx(&mut tx, "tenant-2").await?;
    let endpoint2 = env.create_endpoint_tx(&mut tx, tenant2, &env.http_mock.url()).await?;

    // Configure mock for success
    env.http_mock
        .mock_simple("/", MockResponse::Success {
            status: reqwest::StatusCode::OK,
            body: bytes::Bytes::from_static(b"OK"),
        })
        .await;

    // Create webhooks for both tenants
    let webhook1 = WebhookBuilder::new()
        .tenant(tenant1.0)
        .endpoint(endpoint1.0)
        .source_event("tenant1-event")
        .body(b"tenant 1 data".to_vec())
        .build();

    let webhook2 = WebhookBuilder::new()
        .tenant(tenant2.0)
        .endpoint(endpoint2.0)
        .source_event("tenant2-event")
        .body(b"tenant 2 data".to_vec())
        .build();

    let event1 = env.ingest_webhook_tx(&mut tx, &webhook1).await?;
    let event2 = env.ingest_webhook_tx(&mut tx, &webhook2).await?;

    tx.commit().await?;

    // Deliver all webhooks
    env.run_delivery_cycle().await?;

    // Verify both events were delivered
    let status1 = env.find_webhook_status(event1).await?;
    let status2 = env.find_webhook_status(event2).await?;
    assert_eq!(status1, EventStatus::Delivered);
    assert_eq!(status2, EventStatus::Delivered);

    // Test snapshot captures both tenants
    let snapshot = env.snapshot_events_table().await?;
    assert!(snapshot.contains("tenant1-event"));
    assert!(snapshot.contains("tenant2-event"));

    Ok(())
}

#[tokio::test]
async fn snapshot_with_failed_attempts() -> Result<()> {
    let mut env = TestEnv::new_isolated().await?;
    let mut tx = env.pool().begin().await?;

    let tenant_id = env.create_tenant_tx(&mut tx, "failure-snapshot").await?;
    let endpoint_id = env.create_endpoint_tx(&mut tx, tenant_id, &env.http_mock.url()).await?;

    // Configure mock to always fail
    env.http_mock
        .mock_simple("/", MockResponse::ServerError { status: 500, body: b"Error".to_vec() })
        .await;

    let webhook = WebhookBuilder::new()
        .tenant(tenant_id.0)
        .endpoint(endpoint_id.0)
        .source_event("fail-test")
        .body(b"will fail".to_vec())
        .build();

    let event_id = env.ingest_webhook_tx(&mut tx, &webhook).await?;
    tx.commit().await?;

    // Attempt delivery (will fail)
    env.run_delivery_cycle().await?;

    // Verify webhook is still pending
    let status = env.find_webhook_status(event_id).await?;
    assert_eq!(status, EventStatus::Pending, "Webhook should remain pending after failure");

    // Snapshot should show failed attempt
    let snapshot = env.snapshot_delivery_attempts().await?;
    assert!(snapshot.contains("response_status: Some(500)"));

    Ok(())
}
