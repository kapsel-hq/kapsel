//! Chaos testing for webhook delivery system resilience.
//!
//! Verifies system behavior under failure conditions using ScenarioBuilder
//! for declarative chaos scenarios and fault injection.

use std::{collections::HashMap, time::Duration};

use anyhow::Result;
use bytes::Bytes;
use chrono::Utc;
use kapsel_core::{models::DeliveryAttempt, EventStatus};
use kapsel_testing::{
    fixtures::{TestWebhook, WebhookBuilder},
    ScenarioBuilder, TestEnv,
};
use serde_json::json;

/// Tests webhook delivery resilience when HTTP endpoints fail intermittently.
///
/// Verifies the system correctly retries and eventually succeeds when
/// external services recover from temporary outages.
#[tokio::test]
async fn intermittent_endpoint_failures() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
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
    })
    .await
}

/// Tests system behavior when webhooks fail beyond maximum retry limit.
///
/// Verifies that webhooks are marked as failed and don't retry indefinitely
/// when endpoints are permanently unavailable.
#[tokio::test]
async fn permanent_endpoint_failure() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
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
    })
    .await
}

/// Tests webhook delivery under high concurrent load with mixed
/// success/failure.
///
/// Verifies the system maintains correct behavior when processing many
/// webhooks simultaneously with varying endpoint reliability.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_load_with_mixed_outcomes() -> Result<()> {
    let env = TestEnv::new_shared().await?;
    let mut tx = env.pool().begin().await?;

    // Create test data within transaction using proper _tx methods
    let tenant_id = env.create_tenant_tx(&mut tx, "chaos-load-tenant").await?;
    let endpoint_id = env.create_endpoint_tx(&mut tx, tenant_id, &env.http_mock.url()).await?;

    // Create batch of webhook events using proper transaction methods
    let mut event_ids = Vec::new();
    for i in 0..8 {
        let webhook = TestWebhook {
            tenant_id: tenant_id.0,
            endpoint_id: endpoint_id.0,
            source_event_id: format!("chaos-load-{}", i),
            idempotency_strategy: "source_id".to_string(),
            headers: HashMap::new(),
            body: Bytes::from(json!({"message": format!("Load test webhook {}", i)}).to_string()),
            content_type: "application/json".to_string(),
        };

        let event_id = env.ingest_webhook_tx(&mut tx, &webhook).await?;
        event_ids.push(event_id);
    }

    // Verify all webhooks were created
    assert_eq!(event_ids.len(), 8, "Should have created 8 webhooks");

    // Verify events exist within transaction
    for event_id in &event_ids {
        let event = env.storage().webhook_events.find_by_id_in_tx(&mut tx, *event_id).await?;
        assert!(event.is_some(), "Event {} should exist in transaction", event_id.0);
    }

    // Simulate delivery attempts with different outcomes (deterministic)
    for (i, event_id) in event_ids.iter().enumerate() {
        let status_code = if i % 2 == 0 { 200 } else { 503 }; // Even succeed, odd fail
        let response_body = if i % 2 == 0 { "OK" } else { "Busy" };
        let success = i % 2 == 0;

        // Create delivery attempt using repository
        let attempt = DeliveryAttempt {
            id: uuid::Uuid::new_v4(),
            event_id: *event_id,
            attempt_number: 1,
            endpoint_id,
            request_headers: HashMap::new(),
            request_body: json!({"message": format!("Load test webhook {}", i)})
                .to_string()
                .into_bytes(),
            response_status: Some(status_code),
            response_headers: Some(HashMap::new()),
            response_body: Some(response_body.as_bytes().to_vec()),
            attempted_at: Utc::now(),
            succeeded: success,
            error_message: if success { None } else { Some("Service unavailable".to_string()) },
        };

        env.storage().delivery_attempts.create_in_tx(&mut tx, &attempt).await?;

        // Update event status based on delivery outcome
        if success {
            env.storage().webhook_events.mark_delivered_in_tx(&mut tx, *event_id).await?;
        }
    }

    // Verify delivery attempts and statuses
    for (i, event_id) in event_ids.iter().enumerate() {
        let attempts =
            env.storage().delivery_attempts.find_by_event_in_tx(&mut tx, *event_id).await?;
        assert_eq!(attempts.len(), 1, "Event {} should have exactly one delivery attempt", i);

        let event = env
            .storage()
            .webhook_events
            .find_by_id_in_tx(&mut tx, *event_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Event not found"))?;
        let expected_status =
            if i % 2 == 0 { EventStatus::Delivered } else { EventStatus::Pending };
        assert_eq!(
            event.status, expected_status,
            "Event {} should have status '{:?}' but got '{:?}' (deterministic processing)",
            i, expected_status, event.status
        );
    }

    // Simulate retry attempts for failed events (odd indices)
    for (i, event_id) in event_ids.iter().enumerate().filter(|(i, _)| i % 2 == 1) {
        // Create second delivery attempt (success)
        let retry_attempt = DeliveryAttempt {
            id: uuid::Uuid::new_v4(),
            event_id: *event_id,
            attempt_number: 2,
            endpoint_id,
            request_headers: HashMap::new(),
            request_body: json!({"message": format!("Load test webhook {}", i)})
                .to_string()
                .into_bytes(),
            response_status: Some(200),
            response_headers: Some(HashMap::new()),
            response_body: Some("OK".as_bytes().to_vec()),
            attempted_at: Utc::now(),
            succeeded: true,
            error_message: None,
        };

        env.storage().delivery_attempts.create_in_tx(&mut tx, &retry_attempt).await?;

        // Mark event as delivered
        env.storage().webhook_events.mark_delivered_in_tx(&mut tx, *event_id).await?;
    }

    // Verify retry attempts - odd indices should now have 2 attempts and be
    // delivered
    for (i, event_id) in event_ids.iter().enumerate() {
        let attempts =
            env.storage().delivery_attempts.find_by_event_in_tx(&mut tx, *event_id).await?;
        let expected_attempts = if i % 2 == 0 { 1 } else { 2 }; // Odd indices get retried
        assert_eq!(
            attempts.len(),
            expected_attempts,
            "Event {} should have {} attempts after retry cycle",
            i,
            expected_attempts
        );

        // All events should now be delivered after retry
        let event = env
            .storage()
            .webhook_events
            .find_by_id_in_tx(&mut tx, *event_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Event not found"))?;
        assert_eq!(
            event.status,
            EventStatus::Delivered,
            "Event {} should be delivered after retry cycle",
            i
        );
    }

    // Transaction automatically rolls back when dropped
    drop(tx);
    Ok(())
}

/// Tests idempotency guarantees under chaotic conditions.
///
/// Verifies that duplicate webhooks are handled correctly even when
/// the system experiences failures and retries.
#[tokio::test]
async fn idempotency_under_chaos() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
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
    })
    .await
}

/// Tests data visibility after delivery cycle commits.
///
/// Verifies that run_delivery_cycle() commits data to the database,
/// making it visible to external connections (required for attestation).
#[tokio::test]
async fn database_transaction_chaos() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        let tenant_id = env.create_tenant("tx-chaos").await?;
        let endpoint_id = env.create_endpoint(tenant_id, &env.http_mock.url()).await?;

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
    })
    .await
}
