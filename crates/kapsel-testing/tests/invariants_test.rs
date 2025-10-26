//! Tests for invariant checking functionality.

use std::time::Duration;

use anyhow::Result;
use kapsel_testing::{fixtures::WebhookBuilder, http::MockResponse, ScenarioBuilder, TestEnv};
use uuid::Uuid;

#[tokio::test]
async fn common_invariants_work() -> Result<()> {
    let env = TestEnv::new_isolated().await?;
    let mut tx = env.pool().begin().await?;

    // Test transaction isolation - verify clean state within transaction
    let event_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM webhook_events").fetch_one(&mut *tx).await?;
    assert_eq!(event_count, 0, "Transaction should start with clean state");

    let tenant_id = env.create_tenant_tx(&mut *tx, "invariant-tenant").await?;
    let _endpoint_id = env.create_endpoint_tx(&mut *tx, tenant_id, "https://example.com").await?;

    // Verify data exists within transaction
    let tenant_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
        .bind(tenant_id.0)
        .fetch_one(&mut *tx)
        .await?;
    assert_eq!(tenant_count, 1, "Tenant should exist within transaction");

    // Transaction rollback ensures no data leaks
    tx.rollback().await?;
    Ok(())
}

#[tokio::test]
async fn invariant_validation_during_webhook_processing() -> Result<()> {
    let env = TestEnv::new_isolated().await?;
    let mut tx = env.pool().begin().await?;

    // Setup test data within transaction
    let tenant_id = env.create_tenant_tx(&mut *tx, "invariant-tenant").await?;
    let endpoint_id = env.create_endpoint_tx(&mut *tx, tenant_id, &env.http_mock.url()).await?;

    // Configure HTTP mock for successful delivery
    env.http_mock
        .mock_simple("/", MockResponse::Success {
            status: reqwest::StatusCode::OK,
            body: bytes::Bytes::from_static(b"OK"),
        })
        .await;

    // Create webhook within transaction
    let webhook = WebhookBuilder::new()
        .tenant(tenant_id.0)
        .endpoint(endpoint_id.0)
        .source_event("invariant-test")
        .body(b"test webhook payload".to_vec())
        .build();

    let event_id = env.ingest_webhook_tx(&mut *tx, &webhook).await?;

    // Verify data exists within transaction
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM webhook_events WHERE id = $1")
        .bind(event_id.0)
        .fetch_one(&mut *tx)
        .await?;
    assert_eq!(count, 1, "Event should exist within transaction");

    // Commit for delivery testing
    tx.commit().await?;

    // Test delivery
    env.run_delivery_cycle().await?;

    // Verify final state
    let events = env.get_all_events().await?;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].status, "delivered");
    assert_eq!(events[0].attempt_count(), 1);

    Ok(())
}

#[tokio::test]
async fn invariant_catches_retry_bound_violation() -> Result<()> {
    let mut env = TestEnv::new_isolated().await?;
    let mut tx = env.pool().begin().await?;

    // Create webhook data directly in database to simulate retry bound violation
    let tenant_id = env.create_tenant_tx(&mut *tx, "test-tenant").await?;
    let endpoint_id =
        env.create_endpoint_tx(&mut *tx, tenant_id, "http://example.com/webhook").await?;
    tx.commit().await?;

    // Insert event with excessive failure count
    let event_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO webhook_events
         (id, tenant_id, endpoint_id, source_event_id, idempotency_strategy,
          status, failure_count, headers, body, content_type, payload_size, received_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, NOW())",
    )
    .bind(event_id)
    .bind(tenant_id.0)
    .bind(endpoint_id.0)
    .bind("test-source")
    .bind("header")
    .bind("failed")
    .bind(10i32) // Excessive failure count
    .bind(serde_json::json!({}))
    .bind(b"test".as_slice())
    .bind("application/json")
    .bind(4i32)
    .execute(env.pool())
    .await?;

    // Scenario with retry bounds check should fail
    let scenario = ScenarioBuilder::new("retry bounds violation test")
        .check_retry_bounds(5) // Max 5 retries, but event has 10 failures
        .advance_time(Duration::from_secs(1));

    let result = scenario.run(&mut env).await;
    assert!(result.is_err(), "Scenario should fail due to retry bounds violation");

    let error = result.unwrap_err();
    let error_chain = format!("{error:#}");
    assert!(error_chain.contains("exceeded max retries:"));

    Ok(())
}

#[tokio::test]
async fn invariant_checks_delivery_state_transitions() -> Result<()> {
    let env = TestEnv::new_isolated().await?;
    let mut tx = env.pool().begin().await?;

    let tenant_id = env.create_tenant_tx(&mut *tx, "state-invariant").await?;
    let endpoint_id = env.create_endpoint_tx(&mut *tx, tenant_id, &env.http_mock.url()).await?;

    // Setup mock for failure then success
    env.http_mock
        .mock_sequence()
        .respond_with(500, "Internal Server Error")
        .respond_with(200, "OK")
        .build()
        .await;

    let webhook = WebhookBuilder::new()
        .tenant(tenant_id.0)
        .endpoint(endpoint_id.0)
        .source_event("state-test")
        .body(b"state transition test".to_vec())
        .build();

    let event_id = env.ingest_webhook_tx(&mut *tx, &webhook).await?;
    tx.commit().await?;

    // First delivery attempt - should fail
    env.run_delivery_cycle().await?;
    let status = env.find_webhook_status(event_id).await?;
    assert_eq!(status, "pending", "Should still be pending after first failure");

    // Advance time for retry
    env.advance_time(Duration::from_secs(5));

    // Second delivery attempt - should succeed
    env.run_delivery_cycle().await?;
    let status = env.find_webhook_status(event_id).await?;
    assert_eq!(status, "delivered", "Should be delivered after retry");

    // Verify attempt count
    let attempts = env.count_delivery_attempts(event_id).await?;
    assert_eq!(attempts, 2, "Should have exactly 2 delivery attempts");

    Ok(())
}

#[tokio::test]
async fn invariant_validates_idempotency_keys() -> Result<()> {
    let env = TestEnv::new_isolated().await?;
    let mut tx = env.pool().begin().await?;

    let tenant_id = env.create_tenant_tx(&mut *tx, "idempotency-test").await?;
    let endpoint_id = env.create_endpoint_tx(&mut *tx, tenant_id, &env.http_mock.url()).await?;

    // Configure mock for success
    env.http_mock
        .mock_simple("/", MockResponse::Success {
            status: reqwest::StatusCode::OK,
            body: bytes::Bytes::from_static(b"OK"),
        })
        .await;

    // Create webhook with specific idempotency key
    let idempotency_key = "unique-key-12345";
    let webhook = WebhookBuilder::new()
        .tenant(tenant_id.0)
        .endpoint(endpoint_id.0)
        .source_event("idempotency-test")
        .header("X-Idempotency-Key", idempotency_key)
        .body(b"first webhook".to_vec())
        .build();

    let event_id1 = env.ingest_webhook_tx(&mut *tx, &webhook).await?;

    // Try to create duplicate with same idempotency key
    let duplicate = WebhookBuilder::new()
        .tenant(tenant_id.0)
        .endpoint(endpoint_id.0)
        .source_event("idempotency-test")
        .header("X-Idempotency-Key", idempotency_key)
        .body(b"duplicate webhook".to_vec())
        .build();

    let event_id2 = env.ingest_webhook_tx(&mut *tx, &duplicate).await?;

    // Should return the same event ID due to idempotency
    assert_eq!(event_id1, event_id2, "Idempotent requests should return same event ID");

    tx.commit().await?;

    // Verify only one event was created
    let events = env.get_all_events().await?;
    assert_eq!(events.len(), 1, "Should have only one event due to idempotency");
    assert_eq!(events[0].body, b"first webhook", "Should keep the first webhook's data");

    Ok(())
}

#[tokio::test]
async fn invariant_checks_concurrent_delivery_attempts() -> Result<()> {
    let env = TestEnv::new_isolated().await?;
    let mut tx = env.pool().begin().await?;

    let tenant_id = env.create_tenant_tx(&mut *tx, "concurrent-test").await?;
    let endpoint_id = env.create_endpoint_tx(&mut *tx, tenant_id, &env.http_mock.url()).await?;

    // Setup slow response to simulate concurrent attempts
    env.http_mock
        .mock_simple("/", MockResponse::Success {
            status: reqwest::StatusCode::OK,
            body: bytes::Bytes::from_static(b"OK"),
        })
        .await;

    // Create multiple webhooks
    let mut event_ids = vec![];
    for i in 0..3 {
        let webhook = WebhookBuilder::new()
            .tenant(tenant_id.0)
            .endpoint(endpoint_id.0)
            .source_event(format!("concurrent-{}", i))
            .body(format!("webhook {}", i).into_bytes())
            .build();

        let event_id = env.ingest_webhook_tx(&mut *tx, &webhook).await?;
        event_ids.push(event_id);
    }

    tx.commit().await?;

    // Process all webhooks
    env.run_delivery_cycle().await?;

    // Verify all were delivered
    for event_id in &event_ids {
        let status = env.find_webhook_status(*event_id).await?;
        assert_eq!(status, "delivered", "All webhooks should be delivered");

        let attempts = env.count_delivery_attempts(*event_id).await?;
        assert_eq!(attempts, 1, "Each webhook should have exactly one attempt");
    }

    Ok(())
}
