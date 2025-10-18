//! Complete workflow tests for attestation system.
//!
//! Tests end-to-end attestation workflows from event ingestion through
//! merkle tree construction to signed tree head generation. Verifies
//! the complete cryptographic audit trail pipeline.

use std::sync::Arc;

use chrono::Utc;
use kapsel_attestation::{AttestationEventSubscriber, MerkleService, SigningService};
use kapsel_core::{
    models::{EventId, TenantId},
    DeliveryEvent, DeliverySucceededEvent, EventHandler,
};
use kapsel_testing::TestEnv;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Test the complete attestation workflow with batch processing.
///
/// This demonstrates the full workflow from event handling through
/// to signed tree head generation.
#[tokio::test]
async fn complete_attestation_workflow() {
    let env = TestEnv::new().await.expect("failed to create test environment");

    // Create ephemeral signing service and store key in database
    let signing_service = SigningService::ephemeral();
    let key_id = store_signing_key_in_db(&env, &signing_service).await;
    let signing_service = signing_service.with_key_id(key_id);

    let merkle_service_shared =
        Arc::new(RwLock::new(MerkleService::new(env.pool().clone(), signing_service)));

    let subscriber = AttestationEventSubscriber::new(merkle_service_shared.clone());

    // Process multiple delivery events
    for _ in 0..5 {
        let success_event = create_test_success_event();
        subscriber.handle_event(DeliveryEvent::Succeeded(success_event)).await;
    }

    // Commit pending leaves to generate signed tree head
    let mut service = merkle_service_shared.write().await;
    let signed_tree_head =
        service.try_commit_pending().await.expect("should commit pending leaves");

    // Verify the signed tree head properties
    assert_eq!(signed_tree_head.tree_size, 5);
    assert!(!signed_tree_head.signature.is_empty());
    assert!(signed_tree_head.timestamp_ms > 0);

    // Verify no pending leaves remain
    assert_eq!(service.pending_count().await.expect("should get pending count"), 0);
}

/// Test incremental batch processing workflow.
///
/// Verifies that multiple smaller batches work correctly and maintain
/// proper tree state between commits.
#[tokio::test]
async fn incremental_batch_processing_workflow() {
    let env = TestEnv::new().await.expect("failed to create test environment");

    let signing_service = SigningService::ephemeral();
    let key_id = store_signing_key_in_db(&env, &signing_service).await;
    let signing_service = signing_service.with_key_id(key_id);

    let merkle_service_shared =
        Arc::new(RwLock::new(MerkleService::new(env.pool().clone(), signing_service)));
    let subscriber = AttestationEventSubscriber::new(merkle_service_shared.clone());

    // First batch: 3 events
    for _ in 0..3 {
        let event = create_test_success_event();
        subscriber.handle_event(DeliveryEvent::Succeeded(event)).await;
    }

    // Commit first batch
    let mut service = merkle_service_shared.write().await;
    let tree_head_1 = service.try_commit_pending().await.expect("should commit first batch");
    assert_eq!(tree_head_1.tree_size, 3);
    drop(service);

    // Second batch: 2 more events
    for _ in 0..2 {
        let event = create_test_success_event();
        subscriber.handle_event(DeliveryEvent::Succeeded(event)).await;
    }

    // Commit second batch
    let mut service = merkle_service_shared.write().await;
    let tree_head_2 = service.try_commit_pending().await.expect("should commit second batch");
    assert_eq!(tree_head_2.tree_size, 5);

    // Tree should have grown incrementally
    assert!(tree_head_2.tree_size > tree_head_1.tree_size);
    assert!(tree_head_2.timestamp_ms >= tree_head_1.timestamp_ms);
}

/// Test workflow with mixed success and failure events.
///
/// Verifies that only successful delivery events contribute to the
/// attestation tree while failures are properly ignored.
#[tokio::test]
async fn mixed_success_failure_workflow() {
    let env = TestEnv::new().await.expect("failed to create test environment");

    let signing_service = SigningService::ephemeral();
    let key_id = store_signing_key_in_db(&env, &signing_service).await;
    let signing_service = signing_service.with_key_id(key_id);

    let merkle_service_shared =
        Arc::new(RwLock::new(MerkleService::new(env.pool().clone(), signing_service)));
    let subscriber = AttestationEventSubscriber::new(merkle_service_shared.clone());

    // Process mixed events: 4 successes, 3 failures
    for _ in 0..4 {
        let success_event = create_test_success_event();
        subscriber.handle_event(DeliveryEvent::Succeeded(success_event)).await;
    }

    for _ in 0..3 {
        let failure_event = create_test_failure_event();
        subscriber.handle_event(DeliveryEvent::Failed(failure_event)).await;
    }

    // Only success events should be pending
    let service = merkle_service_shared.read().await;
    assert_eq!(service.pending_count().await.expect("should get pending count"), 4);
    drop(service);

    // Commit should only include successful deliveries
    let mut service = merkle_service_shared.write().await;
    let tree_head = service.try_commit_pending().await.expect("should commit pending leaves");
    assert_eq!(tree_head.tree_size, 4); // Only successful deliveries
}

/// Test empty batch commit handling.
///
/// Verifies that attempting to commit when no events are pending
/// behaves correctly without creating invalid tree states.
#[tokio::test]
async fn empty_batch_commit_workflow() {
    let env = TestEnv::new().await.expect("failed to create test environment");

    let signing_service = SigningService::ephemeral();
    let key_id = store_signing_key_in_db(&env, &signing_service).await;
    let signing_service = signing_service.with_key_id(key_id);

    let merkle_service =
        Arc::new(RwLock::new(MerkleService::new(env.pool().clone(), signing_service)));

    // Try to commit with no pending events
    let mut service = merkle_service.write().await;
    let result = service.try_commit_pending().await;

    // Should either return an error or a valid empty result
    // The key is that it doesn't panic or create invalid state
    match result {
        Ok(tree_head) => {
            assert_eq!(tree_head.tree_size, 0);
        },
        Err(_) => {
            // Expected behavior for empty batch
        },
    }

    // Service should remain operational
    assert_eq!(service.pending_count().await.expect("should get pending count"), 0);
}

/// Test large batch processing workflow.
///
/// Verifies that the system can handle larger batches of events
/// without performance degradation or memory issues.
#[tokio::test]
async fn large_batch_processing_workflow() {
    let env = TestEnv::new().await.expect("failed to create test environment");

    let signing_service = SigningService::ephemeral();
    let key_id = store_signing_key_in_db(&env, &signing_service).await;
    let signing_service = signing_service.with_key_id(key_id);

    let merkle_service_shared =
        Arc::new(RwLock::new(MerkleService::new(env.pool().clone(), signing_service)));
    let subscriber = AttestationEventSubscriber::new(merkle_service_shared.clone());

    // Process a larger batch (100 events)
    const BATCH_SIZE: usize = 100;
    for _ in 0..BATCH_SIZE {
        let event = create_test_success_event();
        subscriber.handle_event(DeliveryEvent::Succeeded(event)).await;
    }

    // Verify all events are pending
    let service = merkle_service_shared.read().await;
    assert_eq!(service.pending_count().await.expect("should get pending count"), BATCH_SIZE);
    drop(service);

    // Commit large batch
    let mut service = merkle_service_shared.write().await;
    let tree_head = service.try_commit_pending().await.expect("should commit large batch");

    assert_eq!(tree_head.tree_size, BATCH_SIZE as u64);
    assert!(!tree_head.signature.is_empty());
    assert_eq!(service.pending_count().await.expect("should get pending count"), 0);
}

/// Helper function to store ephemeral signing key in database.
///
/// This is needed for integration tests because the database schema has a
/// foreign key constraint requiring keys to be in attestation_keys table.
async fn store_signing_key_in_db(env: &TestEnv, signing_service: &SigningService) -> uuid::Uuid {
    let key_id = uuid::Uuid::new_v4();
    let public_key_bytes = signing_service.public_key_as_bytes();

    sqlx::query(
        r#"
        INSERT INTO attestation_keys (id, public_key, is_active, created_at)
        VALUES ($1, $2, true, NOW())
        "#,
    )
    .bind(key_id)
    .bind(&public_key_bytes)
    .execute(env.pool())
    .await
    .expect("should store signing key in database");

    key_id
}

// Helper functions for creating test events

fn create_test_success_event() -> DeliverySucceededEvent {
    DeliverySucceededEvent {
        delivery_attempt_id: Uuid::new_v4(),
        event_id: EventId::new(),
        tenant_id: TenantId::new(),
        endpoint_url: "https://example.com/webhook".to_string(),
        response_status: 200,
        attempt_number: 1,
        delivered_at: Utc::now(),
        payload_hash: {
            // Use random hash to avoid duplicates
            let mut hash = [0u8; 32];
            hash[0..4].copy_from_slice(&rand::random::<u32>().to_be_bytes());
            hash
        },
        payload_size: 1024,
    }
}

fn create_test_failure_event() -> kapsel_core::DeliveryFailedEvent {
    kapsel_core::DeliveryFailedEvent {
        delivery_attempt_id: Uuid::new_v4(),
        event_id: EventId::new(),
        tenant_id: TenantId::new(),
        endpoint_url: "https://example.com/webhook".to_string(),
        response_status: Some(500),
        attempt_number: 1,
        failed_at: Utc::now(),
        error_message: "Internal server error".to_string(),
        is_retryable: true,
    }
}
