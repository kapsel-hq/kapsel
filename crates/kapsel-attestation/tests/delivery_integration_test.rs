//! Integration tests for delivery attestation integration scenarios.
//!
//! Tests webhook delivery integration with Merkle tree attestation
//! and cryptographic audit trail creation.

use std::time::Duration;

use anyhow::Result;
use kapsel_attestation::{MerkleService, SigningService};
use kapsel_testing::{fixtures::WebhookBuilder, ScenarioBuilder, TestEnv};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn successful_delivery_creates_attestation_leaf() -> Result<()> {
    let mut env = TestEnv::new().await?;

    // Set up attestation service
    let merkle_service = create_test_attestation_service(&mut env).await?;
    env.enable_attestation(merkle_service);

    // Create tenant and endpoint first
    let tenant_id = env.create_tenant("test-tenant").await?;
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
        // Process delivery
        .run_delivery_cycle()
        // Commit attestation leaves to database
        .run_attestation_commitment()
        // Verify delivery succeeded
        .expect_status(event_id, "delivered")
        // Core invariant: successful delivery creates exactly one leaf
        .expect_attestation_leaf_count(event_id, 1)
        // Verify leaf contains accurate metadata
        .expect_attestation_leaf_exists(event_id)
        .run(&mut env)
        .await
}

#[tokio::test]
async fn failed_delivery_preserves_attestation_invariants() -> Result<()> {
    let mut env = TestEnv::new().await?;

    let merkle_service = create_test_attestation_service(&mut env).await?;
    env.enable_attestation(merkle_service);

    // Create tenant and endpoint first
    let tenant_id = env.create_tenant("test-tenant").await?;
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
        // Process delivery (should fail)
        .run_delivery_cycle()
        // Verify delivery failed
        .expect_status(event_id, "pending") // Still pending for retry
        // Core invariant: failed deliveries do not create attestation leaves
        .expect_attestation_leaf_count(event_id, 0)
        // Advance time and retry
        .advance_time(Duration::from_secs(2))
        // Process retry (should succeed)
        .run_delivery_cycle()
        // Commit attestation leaves to database
        .run_attestation_commitment()
        // Now delivery should succeed and create leaf
        .expect_status(event_id, "delivered")
        .expect_attestation_leaf_count(event_id, 1)
        .expect_attestation_leaf_attempt_number(event_id, 2)
        .run(&mut env)
        .await
}

#[tokio::test]
async fn attestation_disabled_preserves_delivery_behavior() -> Result<()> {
    let mut env = TestEnv::new().await?;

    // No attestation service configured - attestation disabled

    // Create tenant and endpoint first
    let tenant_id = env.create_tenant("test-tenant").await?;
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
        .expect_status(event_id, "delivered")
        .expect_attestation_leaf_count(event_id, 0)
        // Verify delivery attempt was still recorded for audit
        .expect_delivery_attempts(event_id, 1)
        .run(&mut env)
        .await
}

#[tokio::test]
async fn attestation_preserves_idempotency_guarantees() -> Result<()> {
    let mut env = TestEnv::new().await?;

    let merkle_service = create_test_attestation_service(&mut env).await?;
    env.enable_attestation(merkle_service);

    // Create tenant and endpoint first
    let tenant_id = env.create_tenant("test-tenant").await?;
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
        .expect_status(original_event_id, "delivered")
        // Core invariant: only one attestation leaf for idempotent events
        .expect_attestation_leaf_count(original_event_id, 1)
        .run(&mut env)
        .await
}

#[tokio::test]
async fn concurrent_deliveries_maintain_attestation_integrity() -> Result<()> {
    let mut env = TestEnv::new().await?;

    let merkle_service = create_test_attestation_service(&mut env).await?;
    env.enable_attestation(merkle_service);

    // Create tenant and endpoint first
    let tenant_id = env.create_tenant("test-tenant").await?;
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

    ScenarioBuilder::new("concurrent attestation integrity")
        .inject_http_success()
        // Process all webhooks concurrently
        .run_delivery_cycle()
        // Commit attestation leaves to database
        .run_attestation_commitment()
        // Core invariant: each delivery creates exactly one leaf
        .expect_concurrent_attestation_integrity(event_ids)
        .run(&mut env)
        .await
}

#[tokio::test]
async fn attestation_batch_commitment_scenario() -> Result<()> {
    let mut env = TestEnv::new().await?;

    let merkle_service = create_test_attestation_service(&mut env).await?;
    env.enable_attestation(merkle_service);

    // Create tenant and endpoint first
    let tenant_id = env.create_tenant("test-tenant").await?;
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
        // Deliver all webhooks
        .run_delivery_cycle()
        // Verify all delivered
        .expect_all_events_delivered(event_ids.clone())
        // Trigger batch commitment
        .advance_time(Duration::from_secs(10)) // Trigger commitment interval
        .run_attestation_commitment()
        // Core invariant: batch commitment creates signed tree head
        .expect_signed_tree_head_with_size(batch_size)
        .run(&mut env)
        .await
}

async fn create_test_attestation_service(env: &TestEnv) -> Result<MerkleService> {
    let signing_service = SigningService::ephemeral();
    let public_key = signing_service.public_key_as_bytes();

    // Store signing key in database
    let key_id: Uuid = sqlx::query_scalar(
        "INSERT INTO attestation_keys (public_key, is_active) VALUES ($1, TRUE) RETURNING id",
    )
    .bind(&public_key)
    .fetch_one(env.pool())
    .await?;

    let signing_service = signing_service.with_key_id(key_id);
    let merkle_service = MerkleService::new(env.pool().clone(), signing_service);

    Ok(merkle_service)
}
