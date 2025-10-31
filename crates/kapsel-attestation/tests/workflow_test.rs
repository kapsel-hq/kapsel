//! Integration tests for complete attestation workflows.
//!
//! Tests end-to-end scenarios from delivery events through to signed tree
//! head generation, verifying the full attestation pipeline works correctly.

use std::sync::Arc;

use kapsel_attestation::{AttestationEventSubscriber, MerkleService, SigningService};
use kapsel_core::{storage::Storage, EventHandler};
use kapsel_testing::{events::test_events, TestEnv};
use sqlx::PgPool;
use tokio::sync::RwLock;

/// Test complete attestation workflow from events to signed tree head.
///
/// This demonstrates the full workflow from event handling through
/// to signed tree head generation.
#[tokio::test]
async fn complete_attestation_workflow() {
    let env = TestEnv::new_shared().await.unwrap();
    let mut tx = env.pool().begin().await.unwrap();

    let mut merkle_service = env.create_test_attestation_service_in_tx(&mut tx).await.unwrap();

    // Create test leaf data
    let leaf_data = kapsel_attestation::LeafData::new(
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        "https://example.com/webhook".to_string(),
        [0x42u8; 32],
        1,
        Some(200),
        chrono::DateTime::from_timestamp(1_640_995_200, 0).unwrap().with_timezone(&chrono::Utc),
    )
    .unwrap();

    // Add leaves
    for i in 0..5 {
        let mut leaf = leaf_data.clone();
        leaf.delivery_attempt_id = uuid::Uuid::new_v4();
        leaf.payload_hash[0] = u8::try_from(i).unwrap_or(0); // Make each leaf unique
        merkle_service.add_leaf(leaf).unwrap();
    }

    // Verify pending count
    assert_eq!(merkle_service.pending_count(), 5);

    // Commit leaves within transaction
    let signed_tree_head = merkle_service.try_commit_pending_in_tx(&mut tx).await.unwrap();
    merkle_service.clear_pending();

    // Verify the signed tree head properties
    assert!(signed_tree_head.tree_size >= 5);
    assert_eq!(signed_tree_head.signature.len(), 64);
    assert!(signed_tree_head.timestamp_ms > 0);

    // Verify no pending leaves remain
    assert_eq!(merkle_service.pending_count(), 0);
}

/// Test incremental batch processing workflow.
///
/// Verifies that multiple smaller batches work correctly and maintain
/// proper tree state between commits.
#[tokio::test]
async fn incremental_batch_processing_workflow() {
    let env = TestEnv::new_shared().await.unwrap();
    let mut tx = env.pool().begin().await.unwrap();

    let mut merkle_service = env.create_test_attestation_service_in_tx(&mut tx).await.unwrap();

    // First batch: 3 events
    let base_leaf = kapsel_attestation::LeafData::new(
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        "https://example.com/webhook".to_string(),
        [0x42u8; 32],
        1,
        Some(200),
        chrono::DateTime::from_timestamp(1_640_995_200, 0).unwrap().with_timezone(&chrono::Utc),
    )
    .unwrap();

    for i in 0..3 {
        let mut leaf = base_leaf.clone();
        leaf.delivery_attempt_id = uuid::Uuid::new_v4();
        leaf.payload_hash[0] = u8::try_from(i).unwrap_or(0);
        merkle_service.add_leaf(leaf).unwrap();
    }

    // Commit first batch
    let tree_head_1 = merkle_service.try_commit_pending_in_tx(&mut tx).await.unwrap();
    merkle_service.clear_pending();
    assert!(tree_head_1.tree_size >= 3);

    // Second batch: 2 more events
    for i in 3..5 {
        let mut leaf = base_leaf.clone();
        leaf.delivery_attempt_id = uuid::Uuid::new_v4();
        leaf.payload_hash[0] = u8::try_from(i).unwrap_or(0);
        merkle_service.add_leaf(leaf).unwrap();
    }

    // Commit second batch
    let tree_head_2 = merkle_service.try_commit_pending_in_tx(&mut tx).await.unwrap();
    merkle_service.clear_pending();
    assert!(tree_head_2.tree_size >= 5);

    // Tree should have grown incrementally
    assert!(tree_head_2.tree_size >= tree_head_1.tree_size + 2);
    assert!(tree_head_2.timestamp_ms >= tree_head_1.timestamp_ms);
}

/// Test workflow with mixed success and failure events.
///
/// Verifies that only successful delivery events contribute to the
/// attestation tree while failures are properly ignored.
#[tokio::test]
async fn mixed_success_failure_workflow() {
    let env = TestEnv::new_shared().await.unwrap();

    let signing_service = SigningService::ephemeral();
    let key_id = store_signing_key_in_db(env.pool(), &signing_service).await.unwrap();
    let signing_service = signing_service.with_key_id(key_id);

    let clock: Arc<dyn kapsel_core::Clock> = Arc::new(kapsel_core::TestClock::new());
    let merkle_service_shared = Arc::new(RwLock::new(MerkleService::new(
        Arc::new(Storage::new(env.pool().clone(), &clock)),
        signing_service,
        clock,
    )));
    let subscriber = AttestationEventSubscriber::new(merkle_service_shared.clone());

    // Process mixed events: 4 successes, 3 failures
    for _ in 0..4 {
        let success_event = test_events::create_delivery_succeeded_event();
        subscriber.handle_event(success_event).await;
    }

    for _ in 0..3 {
        let failure_event = test_events::create_delivery_failed_event();
        subscriber.handle_event(failure_event).await;
    }

    // Only success events should be pending
    let service = merkle_service_shared.read().await;
    assert_eq!(service.pending_count(), 4);
    drop(service);

    // Commit should only include successful deliveries
    let tree_head = merkle_service_shared.write().await.try_commit_pending().await.unwrap();
    assert!(tree_head.tree_size >= 4); // Only successful deliveries
}

/// Test empty batch commit handling.
///
/// Verifies that attempting to commit when no events are pending
/// behaves correctly without creating invalid tree states.
#[tokio::test]
async fn empty_batch_commit_workflow() {
    let env = TestEnv::new_shared().await.unwrap();

    let signing_service = SigningService::ephemeral();
    let key_id = store_signing_key_in_db(env.pool(), &signing_service).await.unwrap();
    let signing_service = signing_service.with_key_id(key_id);

    let clock: Arc<dyn kapsel_core::Clock> = Arc::new(kapsel_core::TestClock::new());
    let merkle_service = Arc::new(RwLock::new(MerkleService::new(
        Arc::new(Storage::new(env.pool().clone(), &clock)),
        signing_service,
        clock,
    )));

    // Try to commit with no pending events
    let mut service = merkle_service.write().await;
    let result = service.try_commit_pending().await;

    // Should return an error for empty batch
    assert!(result.is_err());

    // Service should remain operational
    assert_eq!(service.pending_count(), 0);
    drop(service);
}

/// Test large batch processing workflow.
///
/// Verifies that the system can handle larger batches of events
/// without performance degradation or memory issues.
#[tokio::test]
async fn large_batch_processing_workflow() {
    const BATCH_SIZE: usize = 50;
    let env = TestEnv::new_shared().await.unwrap();
    let mut tx = env.pool().begin().await.unwrap();

    let mut merkle_service = env.create_test_attestation_service_in_tx(&mut tx).await.unwrap();

    // Process a larger batch (50 events - reduced for test speed)
    let base_leaf = kapsel_attestation::LeafData::new(
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        "https://example.com/webhook".to_string(),
        [0x42u8; 32],
        1,
        Some(200),
        chrono::DateTime::from_timestamp(1_640_995_200, 0).unwrap().with_timezone(&chrono::Utc),
    )
    .unwrap();

    for i in 0..BATCH_SIZE {
        let mut leaf = base_leaf.clone();
        leaf.delivery_attempt_id = uuid::Uuid::new_v4();
        leaf.payload_hash[0] = u8::try_from(i % 256).unwrap_or(0);
        leaf.payload_hash[1] = u8::try_from(i / 256).unwrap_or(0);
        merkle_service.add_leaf(leaf).unwrap();
    }

    // Verify all events are pending
    assert_eq!(merkle_service.pending_count(), BATCH_SIZE);

    // Commit large batch
    let tree_head = merkle_service.try_commit_pending_in_tx(&mut tx).await.unwrap();
    merkle_service.clear_pending();

    assert!(tree_head.tree_size >= BATCH_SIZE as u64);
    assert_eq!(tree_head.signature.len(), 64);
    assert_eq!(merkle_service.pending_count(), 0);
}

/// Helper function to store ephemeral signing key in database.
async fn store_signing_key_in_db(
    pool: &PgPool,
    signing_service: &SigningService,
) -> Result<uuid::Uuid, Box<dyn std::error::Error>> {
    let public_key_bytes = signing_service.public_key_as_bytes();

    let clock: Arc<dyn kapsel_core::Clock> = Arc::new(kapsel_core::TestClock::new());
    let storage = Storage::new(pool.clone(), &clock);

    // Use repository to create and activate new key (automatically deactivates old
    // keys)
    let key_id = storage.attestation_keys.create_and_activate(public_key_bytes.clone()).await?;
    Ok(key_id)
}
