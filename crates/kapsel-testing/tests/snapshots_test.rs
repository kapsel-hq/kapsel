//! Tests for snapshot functionality used in regression testing.

use std::time::Duration;

use anyhow::Result;
use kapsel_core::EventStatus;
use kapsel_testing::{fixtures::WebhookBuilder, http::MockResponse, TestEnv};

#[tokio::test]
async fn snapshot_events_table_works() -> Result<()> {
    let env = TestEnv::new_shared().await?;

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

    // Test transaction-aware snapshot method
    let snapshot = env.snapshot_events_table_in_tx(&mut tx).await?;
    assert!(snapshot.contains("Events Table Snapshot"));
    assert!(snapshot.contains("snap-test-001"));
    assert!(snapshot.contains("status: pending"));

    // Transaction auto-rollbacks when dropped
    Ok(())
}

#[tokio::test]
async fn snapshot_delivery_attempts_works() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        // Setup successful delivery - no transaction needed for isolated tests
        let tenant_id = env.create_tenant("delivery-snapshot").await?;
        let endpoint_id = env.create_endpoint(tenant_id, &env.http_mock.url()).await?;

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

        let _event_id = env.ingest_webhook(&webhook).await?;

        env.run_delivery_cycle().await?;

        // Test delivery attempts snapshot
        let snapshot = env.snapshot_delivery_attempts().await?;
        assert!(snapshot.contains("Delivery Attempts Snapshot"));
        assert!(snapshot.contains("attempt_number: 1"));
        assert!(snapshot.contains("response_status: Some(200)"));

        Ok(())
    })
    .await
}

#[tokio::test]
async fn snapshot_database_schema_works() -> Result<()> {
    let env = TestEnv::new_shared().await?;
    let snapshot = env.snapshot_database_schema().await?;
    assert!(snapshot.contains("Database Schema Snapshot"));
    assert!(snapshot.contains("Table: webhook_events"));
    assert!(snapshot.contains("Table: tenants"));
    assert!(snapshot.contains("Indexes:"));
    Ok(())
}

#[tokio::test]
async fn comprehensive_end_to_end_snapshot_test() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        // Testing complete snapshot functionality - no transaction needed for isolated
        // tests
        let tenant_id = env.create_tenant("e2e-snapshot").await?;
        let endpoint_id = env.create_endpoint(tenant_id, &env.http_mock.url()).await?;

        // TEST 1: Verify clean initial state
        let event_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM webhook_events").fetch_one(env.pool()).await?;
        assert_eq!(event_count, 0, "Should start with clean state");

        // Setup HTTP responses: fail -> succeed pattern
        env.http_mock
            .mock_sequence()
            .respond_with(503, "Service Unavailable") // First attempt fails
            .respond_with(200, "OK")                  // Second attempt succeeds
            .build()
            .await;

        // Create and ingest webhook
        let webhook = WebhookBuilder::new()
            .tenant(tenant_id.0)
            .endpoint(endpoint_id.0)
            .source_event("comprehensive-test")
            .body(b"comprehensive test payload".to_vec())
            .build();

        let event_id = env.ingest_webhook(&webhook).await?;

        // Verify event exists
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM webhook_events WHERE id = $1")
            .bind(event_id.0)
            .fetch_one(env.pool())
            .await?;
        assert_eq!(count, 1, "Event should exist");

        // Test delivery processing - first attempt fails (503), second succeeds (200)
        env.run_delivery_cycle().await?; // Gets 503, stays pending

        // Advance time to make webhook ready for retry after backoff delay
        env.advance_time(Duration::from_secs(5));

        env.run_delivery_cycle().await?; // Gets 200, becomes delivered

        // Verify event was processed successfully
        let status = env.find_webhook_status(event_id).await?;
        assert_eq!(
            status,
            EventStatus::Delivered,
            "Event should have correct status after delivery"
        );

        // Verify delivery attempts were recorded (first fails with 503, second succeeds
        // with 200)
        let attempt_count = env.count_delivery_attempts(event_id).await?;
        assert_eq!(
            attempt_count, 2,
            "Should have two delivery attempts (one failed, one successful)"
        );

        Ok(())
    })
    .await
}

#[tokio::test]
async fn validation_first_snapshot_test() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        let tenant_id = env.create_tenant("validation").await?;
        let endpoint_id = env.create_endpoint(tenant_id, &env.http_mock.url()).await?;

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

        // STEP 1: Validate initial clean state
        let initial_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM webhook_events").fetch_one(env.pool()).await?;
        assert_eq!(initial_count, 0, "Should start with clean state");

        // STEP 2: Ingest webhook
        let event_id = env.ingest_webhook(&webhook).await?;

        let post_ingestion_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM webhook_events WHERE id = $1")
                .bind(event_id.0)
                .fetch_one(env.pool())
                .await?;
        assert_eq!(post_ingestion_count, 1, "Should have exactly one event");

        // STEP 3: Test delivery
        env.run_delivery_cycle().await?;
        let status = env.find_webhook_status(event_id).await?;
        assert_eq!(status, EventStatus::Delivered, "Event should be delivered");

        let attempt_count = env.count_delivery_attempts(event_id).await?;
        assert_eq!(attempt_count, 1, "Should have exactly one delivery attempt");

        Ok(())
    })
    .await
}

#[tokio::test]
async fn snapshot_with_multiple_tenants() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        // Create multiple tenants with webhooks - no transaction needed for isolated
        // tests
        let tenant1 = env.create_tenant("tenant-1").await?;
        let endpoint1 = env.create_endpoint(tenant1, &env.http_mock.url()).await?;

        let tenant2 = env.create_tenant("tenant-2").await?;
        let endpoint2 = env.create_endpoint(tenant2, &env.http_mock.url()).await?;

        // Setup HTTP mock for success
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
            .source_event("tenant-1-event")
            .body(b"tenant 1 payload".to_vec())
            .build();

        let webhook2 = WebhookBuilder::new()
            .tenant(tenant2.0)
            .endpoint(endpoint2.0)
            .source_event("tenant-2-event")
            .body(b"tenant 2 payload".to_vec())
            .build();

        let event1_id = env.ingest_webhook(&webhook1).await?;
        let event2_id = env.ingest_webhook(&webhook2).await?;

        // Verify both events exist
        let total_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM webhook_events").fetch_one(env.pool()).await?;
        assert_eq!(total_count, 2, "Should have both events");

        // Process deliveries
        env.run_delivery_cycle().await?;

        // Verify both events were delivered
        let status1 = env.find_webhook_status(event1_id).await?;
        let status2 = env.find_webhook_status(event2_id).await?;

        assert_eq!(status1, EventStatus::Delivered, "Tenant 1 event should be delivered");
        assert_eq!(status2, EventStatus::Delivered, "Tenant 2 event should be delivered");

        Ok(())
    })
    .await
}

#[tokio::test]
async fn snapshot_with_failed_attempts() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        let tenant_id = env.create_tenant("failure-snapshot").await?;
        let endpoint_id = env.create_endpoint(tenant_id, &env.http_mock.url()).await?;

        // Configure mock to always fail
        env.http_mock
            .mock_simple("/", MockResponse::ServerError { status: 500, body: b"Error".to_vec() })
            .await;

        let webhook = WebhookBuilder::new()
            .tenant(tenant_id.0)
            .endpoint(endpoint_id.0)
            .source_event("failure-test")
            .body(b"failure test payload".to_vec())
            .build();

        let event_id = env.ingest_webhook(&webhook).await?;

        // Run delivery cycle - should fail and schedule retries
        env.run_delivery_cycle().await?;

        // Verify the failed attempt was recorded
        let attempt_count = env.count_delivery_attempts(event_id).await?;
        assert!(attempt_count >= 1, "Should have at least one delivery attempt");

        // Test snapshot includes failed attempts
        let snapshot = env.snapshot_delivery_attempts().await?;
        assert!(snapshot.contains("response_status: Some(500)"));
        assert!(snapshot.contains("succeeded: false"));

        Ok(())
    })
    .await
}
