//! Integration tests for delivery attestation integration scenarios.
//!
//! Tests webhook delivery integration with Merkle tree attestation
//! and cryptographic audit trail creation.

use std::time::Duration;

use anyhow::Result;
use kapsel_core::EventStatus;
use kapsel_testing::{fixtures::WebhookBuilder, ScenarioBuilder, TestEnv};
use serde_json::json;
use serial_test::serial;

#[tokio::test]
async fn successful_delivery_creates_attestation_leaf() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        // Set up attestation service
        let merkle_service = env.create_test_attestation_service().await?;
        env.enable_attestation(merkle_service)?;

        // Create tenant and endpoint - no transaction needed for isolated tests
        let tenant_id = env.create_tenant("successful-delivery-test").await?;
        let endpoint_id = env.create_endpoint(tenant_id, &env.http_mock.url()).await?;

        let webhook = WebhookBuilder::new()
            .tenant(tenant_id.0)
            .endpoint(endpoint_id.0)
            .source_event("test-event-001")
            .json_body(&json!({"event": "test", "data": "successful delivery"}))
            .build();

        let event_id = env.ingest_webhook(&webhook).await?;

        ScenarioBuilder::new("successful delivery attestation")
        // Configure mock to succeed
        .inject_http_success()
        // Process delivery (will emit attestation events)
        .run_delivery_cycle()
        // Commit pending attestation leaves to database
        .run_attestation_commitment()
        // Verify delivery succeeded
        .expect_status(event_id, EventStatus::Delivered)
        // Core invariant: successful delivery creates exactly one leaf
        .expect_attestation_leaf_count(event_id, 1)
        // Verify leaf contains accurate metadata
        .expect_attestation_leaf_exists(event_id)
        .run(&mut env)
        .await?;

        Ok(())
    })
    .await
}

#[tokio::test]
async fn failed_delivery_preserves_attestation_invariants() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        let merkle_service = env.create_test_attestation_service().await?;
        env.enable_attestation(merkle_service)?;

        // Create tenant and endpoint - no transaction needed for isolated tests
        let tenant_id = env.create_tenant("failed-delivery-test").await?;
        let endpoint_id = env.create_endpoint(tenant_id, &env.http_mock.url()).await?;

        let webhook = WebhookBuilder::new()
            .tenant(tenant_id.0)
            .endpoint(endpoint_id.0)
            .source_event("test-event-002")
            .json_body(&json!({"event": "test", "data": "will fail"}))
            .build();

        let event_id = env.ingest_webhook(&webhook).await?;

        // Configure mock sequence: first fail, then succeed
        env.http_mock
            .mock_sequence()
            .respond_with(500, "Internal Server Error")
            .respond_with(200, "OK")
            .build()
            .await;

        ScenarioBuilder::new("failed delivery attestation")
        // Process first delivery (should fail)
        .run_delivery_cycle()
        // Commit any pending leaves (there should be none for failed delivery)
        .run_attestation_commitment()
        // Verify delivery failed and remains pending
        .expect_status(event_id, EventStatus::Pending) // Still pending for retry
        // Core invariant: failed deliveries do not create attestation leaves
        .expect_attestation_leaf_count(event_id, 0)
        // Verify first delivery attempt was recorded
        .expect_delivery_attempts(event_id, 1)
        // Advance time to trigger retry
        .advance_time(Duration::from_secs(2))
        // Process retry (should succeed and emit attestation event)
        .run_delivery_cycle()
        // Commit the attestation leaf that was created
        .run_attestation_commitment()
        // Now delivery should succeed
        .expect_status(event_id, EventStatus::Delivered)
        // Verify total delivery attempts
        .expect_delivery_attempts(event_id, 2)
        // Verify attestation leaf was created
        .expect_attestation_leaf_count(event_id, 1)
        .expect_attestation_leaf_attempt_number(event_id, 2)
        .run(&mut env)
        .await?;

        Ok(())
    })
    .await
}

#[tokio::test]
async fn attestation_disabled_preserves_delivery_behavior() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        // No attestation service configured - attestation disabled

        // Create tenant and endpoint - no transaction needed for isolated tests
        let tenant_id = env.create_tenant("attestation-disabled-test").await?;
        let endpoint_id = env.create_endpoint(tenant_id, &env.http_mock.url()).await?;

        let webhook = WebhookBuilder::new()
            .tenant(tenant_id.0)
            .endpoint(endpoint_id.0)
            .source_event("test-event-003")
            .json_body(&json!({"event": "test", "data": "no attestation"}))
            .build();

        let event_id = env.ingest_webhook(&webhook).await?;

        ScenarioBuilder::new("delivery without attestation")
        .inject_http_success()
        .run_delivery_cycle()
        // Core invariant: delivery works normally when attestation is disabled
        .expect_status(event_id, EventStatus::Delivered)
        .expect_attestation_leaf_count(event_id, 0)
        // Verify delivery attempt was still recorded for audit
        .expect_delivery_attempts(event_id, 1)
        .run(&mut env)
        .await?;

        Ok(())
    })
    .await
}

#[tokio::test]
async fn attestation_preserves_idempotency_guarantees() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        let merkle_service = env.create_test_attestation_service().await?;
        env.enable_attestation(merkle_service)?;

        // Create tenant and endpoint - no transaction needed for isolated tests
        let tenant_id = env.create_tenant("idempotency-test").await?;
        let endpoint_id = env.create_endpoint(tenant_id, &env.http_mock.url()).await?;

        let source_id = "idempotency-test-001";
        let original = WebhookBuilder::new()
            .tenant(tenant_id.0)
            .endpoint(endpoint_id.0)
            .source_event(source_id)
            .json_body(&json!({"event": "original"}))
            .build();

        let duplicate = WebhookBuilder::new()
        .tenant(tenant_id.0)
        .endpoint(endpoint_id.0)
        .source_event(source_id) // Same source ID
        .json_body(&json!({"event": "duplicate"}))
        .build();

        let original_event_id = env.ingest_webhook(&original).await?;
        let duplicate_event_id = env.ingest_webhook(&duplicate).await?;

        ScenarioBuilder::new("attestation idempotency preservation")
        // Core invariant: duplicate webhooks return same event ID
        .expect_idempotent_event_ids(original_event_id, duplicate_event_id)
        .inject_http_success()
        .run_delivery_cycle()
        // Commit attestation leaves to database
        .run_attestation_commitment()
        .expect_status(original_event_id, EventStatus::Delivered)
        // Core invariant: only one attestation leaf for idempotent events
        .expect_attestation_leaf_count(original_event_id, 1)
        .run(&mut env)
        .await?;

        Ok(())
    })
    .await
}

#[tokio::test]
#[serial]
async fn concurrent_deliveries_maintain_attestation_integrity() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        let merkle_service = env.create_test_attestation_service().await?;
        env.enable_attestation(merkle_service)?;

        // Create tenant and endpoint - no transaction needed for isolated tests
        let tenant_id = env.create_tenant("concurrent-deliveries-test").await?;
        let endpoint_id = env.create_endpoint(tenant_id, &env.http_mock.url()).await?;

        // Create multiple concurrent webhooks
        let mut event_ids = Vec::new();
        for i in 0..5 {
            let webhook = WebhookBuilder::new()
                .tenant(tenant_id.0)
                .endpoint(endpoint_id.0)
                .source_event(format!("concurrent-{i}"))
                .json_body(&json!({"batch": i, "data": "concurrent test"}))
                .build();

            let event_id = env.ingest_webhook(&webhook).await?;
            event_ids.push(event_id);
        }

        let mut scenario =
            ScenarioBuilder::new("concurrent deliveries maintain attestation integrity");

        // Configure mock to succeed for all requests
        scenario = scenario.inject_http_success();

        // Process all deliveries
        scenario = scenario.run_delivery_cycle();

        // Commit attestation leaves
        scenario = scenario.run_attestation_commitment();

        // Verify each webhook was delivered successfully
        for &event_id in &event_ids {
            scenario = scenario.expect_status(event_id, EventStatus::Delivered);
            scenario = scenario.expect_attestation_leaf_count(event_id, 1);
            scenario = scenario.expect_attestation_leaf_exists(event_id);
        }

        scenario.run(&mut env).await?;

        Ok(())
    })
    .await
}

#[tokio::test]
async fn attestation_batch_commitment_scenario() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        let merkle_service = env.create_test_attestation_service().await?;
        env.enable_attestation(merkle_service)?;

        // Create tenant and endpoint - no transaction needed for isolated tests
        let tenant_id = env.create_tenant("batch-commitment-test").await?;
        let endpoint_id = env.create_endpoint(tenant_id, &env.http_mock.url()).await?;

        // Create batch of webhooks
        let batch_size = 3;
        let mut event_ids = Vec::new();

        for i in 0..batch_size {
            let webhook = WebhookBuilder::new()
                .tenant(tenant_id.0)
                .endpoint(endpoint_id.0)
                .source_event(format!("batch-commitment-{i}"))
                .json_body(&json!({"batch_item": i}))
                .build();

            let event_id = env.ingest_webhook(&webhook).await?;
            event_ids.push(event_id);
        }

        ScenarioBuilder::new("attestation batch commitment")
        .inject_http_success()
        // Deliver all webhooks (will emit attestation events)
        .run_delivery_cycle()
        // Verify all delivered
        .expect_all_events_delivered(event_ids.clone())
        // Capture baseline tree size before commitment
        .capture_tree_size()
        // Commit all pending attestation leaves as a batch
        .run_attestation_commitment()
        // Core invariant: batch commitment creates signed tree head with expected growth
        .expect_signed_tree_head_with_size(batch_size)
        .run(&mut env)
        .await?;

        Ok(())
    })
    .await
}
