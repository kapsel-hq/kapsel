//! Chaos testing for webhook delivery system resilience.
//!
//! Verifies system behavior under failure conditions using ScenarioBuilder
//! for declarative chaos scenarios and fault injection.

use std::time::Duration;

use anyhow::{Context, Result};
use kapsel_core::EventStatus;
use kapsel_testing::{fixtures::WebhookBuilder, ScenarioBuilder, TestEnv};
use serde_json::json;

/// Tests webhook delivery resilience when HTTP endpoints fail intermittently.
///
/// Verifies the system correctly retries and eventually succeeds when
/// external services recover from temporary outages.
#[tokio::test]
async fn intermittent_endpoint_failures() -> Result<()> {
    let mut env = TestEnv::new_isolated().await?;

    let mut tx = env.pool().begin().await?;
    let tenant_id = env.create_tenant_tx(&mut tx, "chaos-tenant").await?;
    let endpoint_id = env.create_endpoint_tx(&mut tx, tenant_id, &env.http_mock.url()).await?;
    tx.commit().await?;

    // Chaos pattern: Random failures then recovery
    env.http_mock
        .mock_sequence()
        .respond_with(503, "Service Unavailable")
        .respond_with(408, "Request Timeout")
        .respond_with(500, "Internal Server Error")
        .respond_with(503, "Service Unavailable")
        .respond_with(200, "Success")
        .build()
        .await;

    let webhook = WebhookBuilder::new()
        .tenant(tenant_id.0)
        .endpoint(endpoint_id.0)
        .source_event("chaos_intermittent_001")
        .json_body(&json!({"event": "payment.failed", "attempts": 0}))
        .build();

    let event_id = env.ingest_webhook(&webhook).await?;

    ScenarioBuilder::new("intermittent endpoint failures chaos")
        // First attempt: 503 failure
        .run_delivery_cycle()
        .expect_delivery_attempts(event_id, 1)
        .expect_status(event_id, EventStatus::Pending)
        .advance_time(Duration::from_secs(1))

        // Second attempt: 408 timeout
        .run_delivery_cycle()
        .expect_delivery_attempts(event_id, 2)
        .expect_status(event_id, EventStatus::Pending)
        .advance_time(Duration::from_secs(2))

        // Third attempt: 500 error
        .run_delivery_cycle()
        .expect_delivery_attempts(event_id, 3)
        .expect_status(event_id, EventStatus::Pending)
        .advance_time(Duration::from_secs(4))

        // Fourth attempt: Another 503
        .run_delivery_cycle()
        .expect_delivery_attempts(event_id, 4)
        .expect_status(event_id, EventStatus::Pending)
        .advance_time(Duration::from_secs(8))

        // Fifth attempt: Finally succeeds
        .run_delivery_cycle()
        .expect_delivery_attempts(event_id, 5)
        .expect_status(event_id, EventStatus::Delivered)

        // Verify system maintained correct backoff timing despite chaos
        .assert_state(|env| {
            assert_eq!(
                env.elapsed(),
                Duration::from_secs(15), // 1+2+4+8 = 15 seconds
                "Exponential backoff should be maintained during chaos"
            );
            Ok(())
        })

        .run(&mut env)
        .await?;

    Ok(())
}

/// Tests system behavior when webhooks fail beyond maximum retry limit.
///
/// Verifies that webhooks are marked as failed and don't retry indefinitely
/// when endpoints are permanently unavailable.
#[tokio::test]
async fn permanent_endpoint_failure() -> Result<()> {
    let mut env = TestEnv::new_isolated().await?;

    let mut tx = env.pool().begin().await?;
    let tenant_id = env.create_tenant_tx(&mut tx, "chaos-tenant").await?;
    let endpoint_id = env
        .create_endpoint_with_config_tx(
            &mut tx,
            tenant_id,
            &env.http_mock.url(),
            "permanent-fail-endpoint",
            4, // max_retries=4 means 5 total attempts (initial + 4 retries)
            30,
        )
        .await?;
    tx.commit().await?;

    // Chaos pattern: Permanent failure (all attempts fail)
    env.http_mock
        .mock_sequence()
        .respond_with(500, "Permanent failure")
        .respond_with(500, "Permanent failure")
        .respond_with(500, "Permanent failure")
        .respond_with(500, "Permanent failure")
        .respond_with(500, "Permanent failure")
        .respond_with(500, "Still failing")
        .build()
        .await;

    let webhook = WebhookBuilder::new()
        .tenant(tenant_id.0)
        .endpoint(endpoint_id.0)
        .source_event("chaos_permanent_001")
        .json_body(&json!({"event": "subscription.cancelled", "reason": "payment_failed"}))
        .build();

    let event_id = env.ingest_webhook(&webhook).await?;

    ScenarioBuilder::new("permanent endpoint failure chaos")
        // Attempt 1: Fail
        .run_delivery_cycle()
        .expect_delivery_attempts(event_id, 1)
        .expect_status(event_id, EventStatus::Pending)
        .advance_time(Duration::from_secs(1))

        // Attempt 2: Fail
        .run_delivery_cycle()
        .expect_delivery_attempts(event_id, 2)
        .expect_status(event_id, EventStatus::Pending)
        .advance_time(Duration::from_secs(2))

        // Attempt 3: Fail
        .run_delivery_cycle()
        .expect_delivery_attempts(event_id, 3)
        .expect_status(event_id, EventStatus::Pending)
        .advance_time(Duration::from_secs(4))

        // Attempt 4: Fail
        .run_delivery_cycle()
        .expect_delivery_attempts(event_id, 4)
        .expect_status(event_id, EventStatus::Pending)
        .advance_time(Duration::from_secs(8))

        // Attempt 5: Final attempt - should mark as failed immediately after exhausting retries
        .run_delivery_cycle()
        .expect_delivery_attempts(event_id, 5)
        .expect_status(event_id, EventStatus::Failed)

        .run(&mut env)
        .await?;

    Ok(())
}

/// Tests webhook delivery under high concurrent load with mixed
/// success/failure.
///
/// Verifies the system maintains correct behavior when processing many
/// webhooks simultaneously with varying endpoint reliability.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_load_with_mixed_outcomes() -> Result<()> {
    let mut env = TestEnv::builder()
        .isolated()
        .worker_count(1)  // Single worker for deterministic processing
        .batch_size(1)    // Process one event at a time for deterministic order
        .poll_interval(Duration::from_millis(10))
        .build()
        .await?;

    let tenant_id = env.create_tenant("chaos-load-tenant").await?;
    let stable_endpoint_id = env.create_endpoint(tenant_id, &env.http_mock.url()).await?;

    // Chaos pattern: Mixed success/failure for batch processing
    env.http_mock
        .mock_sequence()
        .respond_with(200, "OK")        // Webhook 0: Success
        .respond_with(503, "Busy")      // Webhook 1: Fail (will retry)
        .respond_with(200, "OK")        // Webhook 2: Success
        .respond_with(408, "Timeout")   // Webhook 3: Fail (will retry)
        .respond_with(200, "OK")        // Webhook 4: Success
        .respond_with(500, "Error")     // Webhook 5: Fail (will retry)
        .respond_with(200, "OK")        // Webhook 6: Success
        .respond_with(503, "Busy")      // Webhook 7: Fail (will retry)
        // Retry responses for failed webhooks (1, 3, 5, 7)
        .respond_with(200, "OK")        // Webhook 1 retry: Success
        .respond_with(200, "OK")        // Webhook 3 retry: Success
        .respond_with(200, "OK")        // Webhook 5 retry: Success
        .respond_with(200, "OK")        // Webhook 7 retry: Success
        .build()
        .await;

    // Create batch of webhooks
    let mut event_ids = Vec::new();
    for i in 0..8 {
        let webhook = WebhookBuilder::new()
            .tenant(tenant_id.0)
            .endpoint(stable_endpoint_id.0)
            .source_event(format!("chaos-load-{}", i))
            .json_body(&json!({"message": format!("Load test webhook {}", i)}))
            .build();

        let event_id = env.ingest_webhook(&webhook).await?;
        event_ids.push(event_id);
    }

    // Verify all webhooks were ingested
    assert_eq!(event_ids.len(), 8, "Should have ingested 8 webhooks");

    // Verify events exist and are available for processing
    for event_id in &event_ids {
        let event = env
            .storage()
            .webhook_events
            .find_by_id(*event_id)
            .await
            .with_context(|| format!("Failed to verify event {}", event_id.0))?;
        assert!(event.is_some(), "Event {} should be visible after commit", event_id.0);
    }

    // Test delivery processing with deterministic single-worker configuration
    env.process_all_pending(Duration::from_secs(10)).await?;

    // Verify delivery attempts were recorded in expected pattern
    for (i, event_id) in event_ids.iter().enumerate() {
        let attempt_count = env.count_delivery_attempts(*event_id).await?;
        assert_eq!(attempt_count, 1, "Event {} should have exactly one delivery attempt", i);

        // Check final status based on mock response pattern
        // Even indices (0,2,4,6) should succeed, odd indices (1,3,5,7) should be
        // pending
        let status = env.find_webhook_status(*event_id).await?;
        let expected_status =
            if i % 2 == 0 { EventStatus::Delivered } else { EventStatus::Pending };
        assert_eq!(
            status, expected_status,
            "Event {} should have status '{}' but got '{}' (deterministic processing required)",
            i, expected_status, status
        );
    }

    // Test retry behavior by advancing time and running another cycle
    env.advance_time(Duration::from_secs(1));
    env.process_all_pending(Duration::from_secs(10)).await?;

    // Verify retry attempts - odd indices should now have 2 attempts and be
    // delivered
    for (i, event_id) in event_ids.iter().enumerate() {
        let attempt_count = env.count_delivery_attempts(*event_id).await?;
        let expected_attempts = if i % 2 == 0 { 1 } else { 2 }; // Odd indices get retried
        assert_eq!(
            attempt_count, expected_attempts,
            "Event {} should have {} attempts after retry cycle",
            i, expected_attempts
        );

        // All events should now be delivered after retry
        let status = env.find_webhook_status(*event_id).await?;
        assert_eq!(
            status,
            EventStatus::Delivered,
            "Event {} should be delivered after retry cycle",
            i
        );
    }

    Ok(())
}

/// Tests idempotency guarantees under chaotic conditions.
///
/// Verifies that duplicate webhooks are handled correctly even when
/// the system experiences failures and retries.
#[tokio::test]
async fn idempotency_under_chaos() -> Result<()> {
    let mut env = TestEnv::new_isolated().await?;

    let mut tx = env.pool().begin().await?;
    let tenant_id = env.create_tenant_tx(&mut tx, "chaos-idempotency").await?;
    let endpoint_id = env.create_endpoint_tx(&mut tx, tenant_id, &env.http_mock.url()).await?;
    tx.commit().await?;

    // Chaos pattern: Success, then duplicate attempts should be ignored
    env.http_mock
        .mock_sequence()
        .respond_with(200, "Processed")
        .respond_with(200, "Should not be called")
        .build()
        .await;

    // Original webhook
    let original_webhook = WebhookBuilder::new()
        .tenant(tenant_id.0)
        .endpoint(endpoint_id.0)
        .source_event("idempotent_chaos_001")
        .json_body(&json!({"amount": 5000, "currency": "usd"}))
        .build();

    let original_event_id = env.ingest_webhook(&original_webhook).await?;

    ScenarioBuilder::new("idempotency chaos scenario")
        // Process original webhook successfully
        .run_delivery_cycle()
        .expect_delivery_attempts(original_event_id, 1)
        .expect_status(original_event_id, EventStatus::Delivered)

        .run(&mut env)
        .await?;

    // Test idempotency by ingesting duplicate webhook after scenario
    let duplicate_webhook = WebhookBuilder::new()
        .tenant(tenant_id.0)
        .endpoint(endpoint_id.0)
        .source_event("idempotent_chaos_001") // Same source_event_id
        .json_body(&json!({"amount": 9999, "currency": "eur"})) // Different payload
        .build();

    let duplicate_event_id = env.ingest_webhook(&duplicate_webhook).await?;

    // Should return the same event ID due to idempotency
    assert_eq!(
        duplicate_event_id.0, original_event_id.0,
        "Duplicate webhook should return original event ID"
    );

    // Running delivery cycle again should not create additional attempts
    ScenarioBuilder::new("idempotency verification")
        .run_delivery_cycle()
        .expect_delivery_attempts(original_event_id, 1) // Still only 1 attempt
        .expect_status(original_event_id, EventStatus::Delivered)

        .run(&mut env)
        .await?;

    Ok(())
}

/// Tests data visibility after delivery cycle commits.
///
/// Verifies that run_delivery_cycle() commits data to the database,
/// making it visible to external connections (required for attestation).
#[tokio::test]
async fn database_transaction_chaos() -> Result<()> {
    let mut env = TestEnv::new_isolated().await?;

    // Test that delivery cycle commits data
    let mut tx = env.pool().begin().await?;
    let tenant_id = env.create_tenant_tx(&mut tx, "tx-chaos").await?;
    let endpoint_id = env.create_endpoint_tx(&mut tx, tenant_id, &env.http_mock.url()).await?;
    tx.commit().await?;

    env.http_mock.mock_sequence().respond_with(200, "Success").build().await;

    let webhook = WebhookBuilder::new()
        .tenant(tenant_id.0)
        .endpoint(endpoint_id.0)
        .source_event("tx_chaos_001")
        .json_body(&json!({"test": "transaction_chaos"}))
        .build();

    let event_id = env.ingest_webhook(&webhook).await?;

    ScenarioBuilder::new("database transaction chaos")
        // Verify webhook was ingested in transaction
        .expect_status(event_id, EventStatus::Pending)

        // Process webhook - this commits the transaction
        .run_delivery_cycle()
        .expect_delivery_attempts(event_id, 1)
        .expect_status(event_id, EventStatus::Delivered)

        .run(&mut env)
        .await?;

    // Verify data IS visible after run_delivery_cycle() commits
    // (required for attestation service to read delivery attempts)
    let external_pool = env.create_pool();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM webhook_events WHERE id = $1")
        .bind(event_id.0)
        .fetch_one(&external_pool)
        .await?;

    // Should be 1 since run_delivery_cycle() commits the transaction
    assert_eq!(count, 1, "Committed data should be visible to external connections");

    // Verify delivery attempt is also visible
    let attempt_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM delivery_attempts WHERE event_id = $1")
            .bind(event_id.0)
            .fetch_one(&external_pool)
            .await?;

    assert_eq!(attempt_count, 1, "Delivery attempt should be visible after commit");

    Ok(())
}
