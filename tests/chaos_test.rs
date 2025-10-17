//! True chaos testing for webhook delivery system resilience.
//!
//! These tests verify system behavior under failure conditions using
//! ScenarioBuilder for declarative chaos scenarios.

use std::time::Duration;

use anyhow::Result;
use kapsel_testing::{fixtures::WebhookBuilder, ScenarioBuilder, TestEnv};
use serde_json::json;

/// Tests webhook delivery resilience when HTTP endpoints fail intermittently.
///
/// Verifies the system correctly retries and eventually succeeds when
/// external services recover from temporary outages.
#[tokio::test]
async fn intermittent_endpoint_failures() -> Result<()> {
    let mut env = TestEnv::new().await?;

    let tenant_id = env.create_tenant("chaos-tenant").await?;
    let endpoint_id = env.create_endpoint(tenant_id, &env.http_mock.url()).await?;

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
        .json_body(json!({"event": "payment.failed", "attempts": 0}))
        .build();

    let event_id = env.ingest_webhook(&webhook).await?;

    ScenarioBuilder::new("intermittent endpoint failures chaos")
        // First attempt: 503 failure
        .run_delivery_cycle()
        .expect_delivery_attempts(event_id, 1)
        .expect_status(event_id, "pending")
        .advance_time(Duration::from_secs(1))

        // Second attempt: 408 timeout
        .run_delivery_cycle()
        .expect_delivery_attempts(event_id, 2)
        .expect_status(event_id, "pending")
        .advance_time(Duration::from_secs(2))

        // Third attempt: 500 error
        .run_delivery_cycle()
        .expect_delivery_attempts(event_id, 3)
        .expect_status(event_id, "pending")
        .advance_time(Duration::from_secs(4))

        // Fourth attempt: Another 503
        .run_delivery_cycle()
        .expect_delivery_attempts(event_id, 4)
        .expect_status(event_id, "pending")
        .advance_time(Duration::from_secs(8))

        // Fifth attempt: Finally succeeds
        .run_delivery_cycle()
        .expect_delivery_attempts(event_id, 5)
        .expect_status(event_id, "delivered")

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
    let mut env = TestEnv::new().await?;

    let tenant_id = env.create_tenant("chaos-tenant").await?;
    let endpoint_id = env
        .create_endpoint_with_config(
            tenant_id,
            &env.http_mock.url(),
            "permanent-fail-endpoint",
            4, // max_retries=4 means 5 total attempts (initial + 4 retries)
            30,
        )
        .await?;

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
        .json_body(json!({"event": "subscription.cancelled", "reason": "payment_failed"}))
        .build();

    let event_id = env.ingest_webhook(&webhook).await?;

    ScenarioBuilder::new("permanent endpoint failure chaos")
        // Attempt 1: Fail
        .run_delivery_cycle()
        .expect_delivery_attempts(event_id, 1)
        .expect_status(event_id, "pending")
        .advance_time(Duration::from_secs(1))

        // Attempt 2: Fail
        .run_delivery_cycle()
        .expect_delivery_attempts(event_id, 2)
        .expect_status(event_id, "pending")
        .advance_time(Duration::from_secs(2))

        // Attempt 3: Fail
        .run_delivery_cycle()
        .expect_delivery_attempts(event_id, 3)
        .expect_status(event_id, "pending")
        .advance_time(Duration::from_secs(4))

        // Attempt 4: Fail
        .run_delivery_cycle()
        .expect_delivery_attempts(event_id, 4)
        .expect_status(event_id, "pending")
        .advance_time(Duration::from_secs(8))

        // Attempt 5: Final attempt
        .run_delivery_cycle()
        .expect_delivery_attempts(event_id, 5)
        .expect_status(event_id, "pending")
        .advance_time(Duration::from_secs(16))

        // Sixth delivery cycle: Should mark as failed since failure_count > max_retries
        .run_delivery_cycle()
        .expect_delivery_attempts(event_id, 5) // Still 5 attempts, no new attempt made
        .expect_status(event_id, "failed")

        .run(&mut env)
        .await?;

    Ok(())
}

/// Tests webhook delivery under high concurrent load with mixed
/// success/failure.
///
/// Verifies the system maintains correct behavior when processing many
/// webhooks simultaneously with varying endpoint reliability.
#[tokio::test]
async fn concurrent_load_with_mixed_outcomes() -> Result<()> {
    let mut env = TestEnv::new().await?;

    let tenant_id = env.create_tenant("chaos-load-tenant").await?;
    let stable_endpoint_id = env.create_endpoint(tenant_id, &env.http_mock.url()).await?;

    // Chaos pattern: Mixed success/failure for batch processing
    env.http_mock
        .mock_sequence()
        .respond_with(200, "OK")        // Webhook 1: Success
        .respond_with(503, "Busy")      // Webhook 2: Fail
        .respond_with(200, "OK")        // Webhook 3: Success
        .respond_with(408, "Timeout")   // Webhook 4: Fail
        .respond_with(200, "OK")        // Webhook 5: Success
        .respond_with(500, "Error")     // Webhook 6: Fail
        .respond_with(200, "OK")        // Webhook 7: Success
        .respond_with(503, "Busy")      // Webhook 8: Fail
        .build()
        .await;

    // Create batch of webhooks
    let mut event_ids = Vec::new();
    for i in 0..8 {
        let webhook = WebhookBuilder::new()
            .tenant(tenant_id.0)
            .endpoint(stable_endpoint_id.0)
            .source_event(format!("chaos_batch_{:03}", i))
            .json_body(json!({"batch_id": i, "event": "user.created"}))
            .build();

        let event_id = env.ingest_webhook(&webhook).await?;
        event_ids.push(event_id);
    }

    ScenarioBuilder::new("concurrent load chaos")
        // Process entire batch in one delivery cycle
        .run_delivery_cycle()

        // Verify all webhooks were attempted once
        .expect_delivery_attempts(event_ids[0], 1)
        .expect_delivery_attempts(event_ids[1], 1)
        .expect_delivery_attempts(event_ids[2], 1)
        .expect_delivery_attempts(event_ids[3], 1)
        .expect_delivery_attempts(event_ids[4], 1)
        .expect_delivery_attempts(event_ids[5], 1)
        .expect_delivery_attempts(event_ids[6], 1)
        .expect_delivery_attempts(event_ids[7], 1)

        // Verify outcomes match mock responses
        .expect_status(event_ids[0], "delivered")  // 200 OK
        .expect_status(event_ids[1], "pending")    // 503 Busy
        .expect_status(event_ids[2], "delivered")  // 200 OK
        .expect_status(event_ids[3], "pending")    // 408 Timeout
        .expect_status(event_ids[4], "delivered")  // 200 OK
        .expect_status(event_ids[5], "pending")    // 500 Error
        .expect_status(event_ids[6], "delivered")  // 200 OK
        .expect_status(event_ids[7], "pending")    // 503 Busy

        // Advance time for retry backoff
        .advance_time(Duration::from_secs(1))

        // Second delivery cycle should retry failed webhooks
        .run_delivery_cycle()
        .expect_delivery_attempts(event_ids[1], 2) // Retried
        .expect_delivery_attempts(event_ids[3], 2) // Retried
        .expect_delivery_attempts(event_ids[5], 2) // Retried
        .expect_delivery_attempts(event_ids[7], 2) // Retried
        // Successful ones should not be retried
        .expect_delivery_attempts(event_ids[0], 1) // Not retried
        .expect_delivery_attempts(event_ids[2], 1) // Not retried
        .expect_delivery_attempts(event_ids[4], 1) // Not retried
        .expect_delivery_attempts(event_ids[6], 1) // Not retried

        .run(&mut env)
        .await?;

    Ok(())
}

/// Tests idempotency guarantees under chaotic conditions.
///
/// Verifies that duplicate webhooks are handled correctly even when
/// the system experiences failures and retries.
#[tokio::test]
async fn idempotency_under_chaos() -> Result<()> {
    let mut env = TestEnv::new().await?;

    let tenant_id = env.create_tenant("chaos-idempotency").await?;
    let endpoint_id = env.create_endpoint(tenant_id, &env.http_mock.url()).await?;

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
        .json_body(json!({"amount": 5000, "currency": "usd"}))
        .build();

    let original_event_id = env.ingest_webhook(&original_webhook).await?;

    ScenarioBuilder::new("idempotency chaos scenario")
        // Process original webhook successfully
        .run_delivery_cycle()
        .expect_delivery_attempts(original_event_id, 1)
        .expect_status(original_event_id, "delivered")

        .run(&mut env)
        .await?;

    // Test idempotency by ingesting duplicate webhook after scenario
    let duplicate_webhook = WebhookBuilder::new()
        .tenant(tenant_id.0)
        .endpoint(endpoint_id.0)
        .source_event("idempotent_chaos_001") // Same source_event_id
        .json_body(json!({"amount": 9999, "currency": "eur"})) // Different payload
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
        .expect_status(original_event_id, "delivered")

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
    let mut env = TestEnv::new().await?;

    // Test that delivery cycle commits data
    let tenant_id = env.create_tenant("tx-chaos").await?;
    let endpoint_id = env.create_endpoint(tenant_id, &env.http_mock.url()).await?;

    env.http_mock.mock_sequence().respond_with(200, "Success").build().await;

    let webhook = WebhookBuilder::new()
        .tenant(tenant_id.0)
        .endpoint(endpoint_id.0)
        .source_event("tx_chaos_001")
        .json_body(json!({"test": "transaction_chaos"}))
        .build();

    let event_id = env.ingest_webhook(&webhook).await?;

    ScenarioBuilder::new("database transaction chaos")
        // Verify webhook was ingested in transaction
        .expect_status(event_id, "pending")

        // Process webhook - this commits the transaction
        .run_delivery_cycle()
        .expect_delivery_attempts(event_id, 1)
        .expect_status(event_id, "delivered")

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
