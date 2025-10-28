//! End-to-end tests for complete webhook delivery workflows.
//!
//! Exercises the full system from HTTP ingestion through delivery with
//! failure scenarios, retry logic, circuit breakers, and tenant isolation.

use std::time::Duration;

use anyhow::Result;
use kapsel_core::EventStatus;
use kapsel_testing::{fixtures::WebhookBuilder, ScenarioBuilder, TestEnv};
use serde_json::json;

/// The golden path: webhook delivery with exponential backoff.
///
/// Verifies deterministic retry timing, idempotency, and successful delivery.
#[tokio::test]
async fn golden_webhook_delivery_with_retry_backoff() -> Result<()> {
    let mut env = TestEnv::new_isolated().await?;

    // Setup test infrastructure
    let mut tx = env.pool().begin().await?;
    let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await?;
    let endpoint_id = env.create_endpoint_tx(&mut tx, tenant_id, &env.http_mock.url()).await?;
    tx.commit().await?;

    // Configure mock to fail 3 times, then succeed
    env.http_mock
        .mock_sequence()
        .respond_with(503, "Service Unavailable")
        .respond_with(503, "Service Unavailable")
        .respond_with(503, "Service Unavailable")
        .respond_with_json(200, &json!({"status": "processed", "id": "dest_123"}))
        .build()
        .await;

    // Create webhook
    let webhook = WebhookBuilder::new()
        .tenant(tenant_id.0)
        .endpoint(endpoint_id.0)
        .source_event("payment_123_idempotent")
        .json_body(&json!({
            "id": "evt_stripe_123",
            "type": "payment.completed",
            "data": {
                "payment_id": "pay_123",
                "amount": 2000,
                "currency": "usd"
            }
        }))
        .build();

    // Execute the golden scenario
    let event_id = env.ingest_webhook(&webhook).await?;

    ScenarioBuilder::new("golden webhook delivery")
        // First attempt - fails with 503, backoff 1s
        .run_delivery_cycle()
        .expect_delivery_attempts(event_id, 1)
        .expect_status(event_id, EventStatus::Pending)
        .advance_time(Duration::from_secs(1))

        // Second attempt - fails with 503, backoff 2s
        .run_delivery_cycle()
        .expect_delivery_attempts(event_id, 2)
        .expect_status(event_id, EventStatus::Pending)
        .advance_time(Duration::from_secs(2))

        // Third attempt - fails with 503, backoff 4s
        .run_delivery_cycle()
        .expect_delivery_attempts(event_id, 3)
        .expect_status(event_id, EventStatus::Pending)
        .advance_time(Duration::from_secs(4))

        // Fourth attempt - succeeds with 200
        .run_delivery_cycle()
        .expect_delivery_attempts(event_id, 4)
        .expect_status(event_id, EventStatus::Delivered)

        // Verify total processing time
        .assert_state(|env| {
            assert_eq!(
                env.elapsed(),
                Duration::from_secs(7), // 1 + 2 + 4 seconds
                "Total processing time should match exponential backoff"
            );
            Ok(())
        })

        .run(&mut env)
        .await?;

    // Test idempotency using scenario
    verify_idempotency_scenario(&mut env, webhook, event_id).await?;

    Ok(())
}

/// Verifies idempotency using ScenarioBuilder.
async fn verify_idempotency_scenario(
    env: &mut TestEnv,
    original: kapsel_testing::fixtures::TestWebhook,
    original_id: kapsel_core::models::EventId,
) -> Result<()> {
    // Create duplicate with same idempotency key but different payload
    let duplicate = WebhookBuilder::new()
        .tenant(original.tenant_id)
        .endpoint(original.endpoint_id)
        .source_event(original.source_event_id.clone()) // Same source_event_id = idempotent
        .json_body(&json!({"different": "payload"}))
        .build();

    let duplicate_id = env.ingest_webhook(&duplicate).await?;

    ScenarioBuilder::new("idempotency verification")
        .assert_state(move |_env| {
            // Should return same event ID due to idempotency
            assert_eq!(
                duplicate_id.0, original_id.0,
                "Duplicate webhook should return original event ID"
            );
            Ok(())
        })
        // No new delivery attempts should be created
        .expect_delivery_attempts(original_id, 4)
        .run(env)
        .await
}

/// Basic batch processing test (circuit breaker logic not yet implemented).
#[tokio::test]
async fn batch_webhook_processing() -> Result<()> {
    let mut env = TestEnv::new_isolated().await?;
    let mut tx = env.pool().begin().await?;

    let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await?;
    let endpoint_id = env.create_endpoint_tx(&mut tx, tenant_id, &env.http_mock.url()).await?;

    // Configure mock to fail first 3, then succeed
    env.http_mock
        .mock_sequence()
        .respond_with(503, "Service Unavailable")
        .respond_with(503, "Service Unavailable")
        .respond_with(503, "Service Unavailable")
        .respond_with(200, "OK")
        .respond_with(200, "OK")
        .build()
        .await;

    // Create batch of webhooks within transaction
    let mut event_ids = Vec::new();
    for i in 0..5 {
        let webhook = WebhookBuilder::new()
            .tenant(tenant_id.0)
            .endpoint(endpoint_id.0)
            .source_event(format!("batch-test-{}", i))
            .json_body(&json!({"message": format!("Batch webhook {}", i)}))
            .build();

        let event_id = env.ingest_webhook_tx(&mut tx, &webhook).await?;
        event_ids.push(event_id);
    }

    // Verify events exist within transaction
    assert_eq!(event_ids.len(), 5, "Should have created 5 webhooks");
    for event_id in &event_ids {
        let event = env.storage().webhook_events.find_by_id_in_tx(&mut tx, *event_id).await?;
        assert!(event.is_some(), "Event should exist within transaction");
    }

    // Commit transaction to make data available for delivery testing
    tx.commit().await?;

    // Process all webhooks in one cycle
    env.run_delivery_cycle().await?;

    // Verify all webhooks were attempted
    for (i, event_id) in event_ids.iter().enumerate() {
        let attempt_count = env.count_delivery_attempts(*event_id).await?;
        assert_eq!(attempt_count, 1, "Event {} should have exactly one delivery attempt", i);

        // Verify status based on mock responses (first 3 fail with 503, last 2 succeed
        // with 200)
        let status = env.find_webhook_status(*event_id).await?;
        let expected_status = if i >= 3 { EventStatus::Delivered } else { EventStatus::Pending };
        assert_eq!(
            status, expected_status,
            "Event {} should have status '{}' but got '{}'",
            i, expected_status, status
        );
    }

    Ok(())
}
