//! Integration tests for Merkle tree service operations.
//!
//! Tests MerkleService with database persistence, batch leaf processing,
//! signed tree head generation, and cryptographic proof verification.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]

use chrono::{DateTime, Utc};
use kapsel_attestation::LeafData;
use kapsel_core::Clock;
use kapsel_testing::TestEnv;
use uuid::Uuid;

#[tokio::test]
async fn merkle_service_adds_leaf_to_pending_queue() {
    let env = TestEnv::new_shared().await.unwrap();
    let mut tx = env.pool().begin().await.unwrap();
    let mut service = env.create_test_attestation_service_in_tx(&mut tx).await.unwrap();

    let leaf = LeafData::new(
        Uuid::new_v4(),
        Uuid::new_v4(),
        "https://example.com/webhook".to_string(),
        [0x42u8; 32],
        1,
        Some(200),
        chrono::DateTime::<chrono::Utc>::from(env.clock.now_system()),
    )
    .unwrap();

    // Add leaf to pending queue
    service.add_leaf(leaf).unwrap();

    // Verify pending count
    assert_eq!(service.pending_count(), 1);
}

#[tokio::test]
async fn merkle_service_commits_batch_atomically() {
    let env = TestEnv::new_shared().await.unwrap();

    // Create test entities using TestEnv for proper isolation
    let mut tx = env.pool().begin().await.unwrap();
    let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await.unwrap();
    let endpoint_id =
        env.create_endpoint_tx(&mut tx, tenant_id, "https://example.com/webhook").await.unwrap();
    let delivery_attempt_id = Uuid::new_v4();

    // Create webhook event using repository
    let webhook = kapsel_testing::TestWebhook {
        tenant_id: tenant_id.0,
        endpoint_id: endpoint_id.0,
        source_event_id: "test-source-event".to_string(),
        idempotency_strategy: "source_id".to_string(),
        headers: std::collections::HashMap::new(),
        body: br#"{"test": "data"}"#.to_vec().into(),
        content_type: "application/json".to_string(),
    };
    let created_event_id = env.ingest_webhook_tx(&mut tx, &webhook).await.unwrap();

    // Create delivery attempt using repository
    let delivery_attempt = kapsel_core::models::DeliveryAttempt {
        id: delivery_attempt_id,
        event_id: created_event_id,
        attempt_number: 1,
        endpoint_id,
        request_headers: std::collections::HashMap::new(),
        request_body: b"request_body".to_vec(),
        response_status: Some(200),
        response_headers: Some(std::collections::HashMap::new()),
        response_body: Some(b"response_body".to_vec()),
        attempted_at: chrono::DateTime::<chrono::Utc>::from(env.clock.now_system()),
        succeeded: true,
        error_message: None,
    };
    env.storage().delivery_attempts.create_in_tx(&mut tx, &delivery_attempt).await.unwrap();

    // Create attestation service within transaction
    let mut service = env.create_test_attestation_service_in_tx(&mut tx).await.unwrap();

    let leaf = LeafData::new(
        delivery_attempt_id,
        created_event_id.0,
        "https://example.com/webhook".to_string(),
        [0x11u8; 32],
        1,
        Some(201),
        DateTime::from_timestamp(1_640_995_200, 0).unwrap().with_timezone(&Utc),
    )
    .unwrap();

    // Add leaf and commit
    service.add_leaf(leaf).unwrap();
    let signed_head = service.try_commit_pending_in_tx(&mut tx).await.unwrap();
    service.clear_pending();

    // Verify signed tree head - tree size should be at least 1 (our leaf was added)
    assert!(signed_head.tree_size >= 1);
    assert_eq!(signed_head.signature.len(), 64);
    assert_eq!(signed_head.root_hash.len(), 32);

    // Verify database persistence - filter by this test's delivery attempt
    let leaf_count = env
        .storage()
        .merkle_leaves
        .count_by_delivery_attempt_in_tx(&mut tx, delivery_attempt_id)
        .await
        .unwrap();
    assert_eq!(leaf_count, 1);

    // Verify tree head was created (check by tree_size since we restored existing
    // state)
    let tree_head_exists =
        env.storage().signed_tree_heads.exists_with_min_size_in_tx(&mut tx, 1).await.unwrap();
    assert!(tree_head_exists);

    // Verify pending queue is cleared
    assert_eq!(service.pending_count(), 0);

    // Transaction auto-rollbacks when dropped
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn merkle_service_handles_multiple_leaves_in_batch() {
    let env = TestEnv::new_isolated().await.unwrap();

    // Create test entities using TestEnv for proper isolation
    let mut tx = env.pool().begin().await.unwrap();
    let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await.unwrap();
    let endpoint_id =
        env.create_endpoint_tx(&mut tx, tenant_id, "https://example.com/webhook").await.unwrap();
    // Create webhook events and delivery attempts
    let mut delivery_attempt_ids = Vec::new();
    let mut event_ids = Vec::new();

    for source_id in ["test-source-1", "test-source-2"] {
        let delivery_attempt_id = Uuid::new_v4();
        // Create webhook event using repository
        let webhook = kapsel_testing::TestWebhook {
            tenant_id: tenant_id.0,
            endpoint_id: endpoint_id.0,
            source_event_id: source_id.to_string(),
            idempotency_strategy: "source_id".to_string(),
            headers: std::collections::HashMap::new(),
            body: br#"{"test": "data"}"#.to_vec().into(),
            content_type: "application/json".to_string(),
        };
        let created_event_id = env.ingest_webhook_tx(&mut tx, &webhook).await.unwrap();

        // Create delivery attempt using repository
        let delivery_attempt = kapsel_core::models::DeliveryAttempt {
            id: delivery_attempt_id,
            event_id: created_event_id,
            attempt_number: 1,
            endpoint_id,
            request_headers: std::collections::HashMap::new(),
            request_body: b"request_body".to_vec(),
            response_status: Some(200),
            response_headers: Some(std::collections::HashMap::new()),
            response_body: Some(b"response_body".to_vec()),
            attempted_at: chrono::DateTime::<chrono::Utc>::from(env.clock.now_system()),
            succeeded: true,
            error_message: None,
        };
        env.storage().delivery_attempts.create_in_tx(&mut tx, &delivery_attempt).await.unwrap();

        delivery_attempt_ids.push(delivery_attempt_id);
        event_ids.push(created_event_id);
    }

    let delivery_attempt_id_1 = delivery_attempt_ids[0];
    let event_id_1 = event_ids[0];
    let delivery_attempt_id_2 = delivery_attempt_ids[1];
    let event_id_2 = event_ids[1];

    tx.commit().await.unwrap();

    // Create attestation service using TestEnv for proper isolation
    let mut service = env.create_test_attestation_service().await.unwrap();

    let timestamp = DateTime::from_timestamp(1_640_995_200, 0).unwrap().with_timezone(&Utc);

    let leaf1 = LeafData::new(
        delivery_attempt_id_1,
        event_id_1.0,
        "https://example.com/webhook1".to_string(),
        [0x11u8; 32],
        1,
        Some(200),
        timestamp,
    )
    .unwrap();

    let leaf2 = LeafData::new(
        delivery_attempt_id_2,
        event_id_2.0,
        "https://example.com/webhook2".to_string(),
        [0x22u8; 32],
        1,
        Some(201),
        timestamp,
    )
    .unwrap();

    // Add both leaves
    service.add_leaf(leaf1).unwrap();
    service.add_leaf(leaf2).unwrap();
    assert_eq!(service.pending_count(), 2);

    // Commit batch
    let signed_head = service.try_commit_pending().await.unwrap();

    // Verify tree head reflects both leaves - should be at least 2
    assert!(signed_head.tree_size >= 2);

    // Verify both leaves are in database with correct tree indices
    let tree_indices = env
        .storage()
        .merkle_leaves
        .find_tree_indices_by_attempts(&delivery_attempt_ids)
        .await
        .unwrap();

    assert_eq!(tree_indices.len(), 2);
    // Tree indices should be consecutive (relative to current tree state)
    assert_eq!(tree_indices[0] + 1, tree_indices[1]);
}

#[tokio::test]
async fn merkle_service_fails_commit_with_empty_pending_queue() {
    let env = TestEnv::new_shared().await.unwrap();
    let mut service = env.create_test_attestation_service().await.unwrap();

    // Try to commit with no pending leaves
    let result = service.try_commit_pending().await;

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("no pending leaves"));
}

#[tokio::test]
async fn merkle_service_stores_batch_metadata() {
    let env = TestEnv::new_isolated().await.unwrap();

    // Create test entities using TestEnv for proper isolation
    let mut tx = env.pool().begin().await.unwrap();
    let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await.unwrap();
    let endpoint_id =
        env.create_endpoint_tx(&mut tx, tenant_id, "https://example.com/webhook").await.unwrap();
    let delivery_attempt_id = Uuid::new_v4();
    let event_id = Uuid::new_v4();

    // Create webhook event
    // Create webhook event using repository
    let webhook = kapsel_testing::TestWebhook {
        tenant_id: tenant_id.0,
        endpoint_id: endpoint_id.0,
        source_event_id: "test-source-event".to_string(),
        idempotency_strategy: "source_id".to_string(),
        headers: std::collections::HashMap::new(),
        body: br#"{"test": "data"}"#.to_vec().into(),
        content_type: "application/json".to_string(),
    };
    let created_event_id = env.ingest_webhook_tx(&mut tx, &webhook).await.unwrap();

    // Create delivery attempt using repository
    let delivery_attempt = kapsel_core::models::DeliveryAttempt {
        id: delivery_attempt_id,
        event_id: created_event_id,
        attempt_number: 1,
        endpoint_id,
        request_headers: std::collections::HashMap::new(),
        request_body: b"request_body".to_vec(),
        response_status: Some(200),
        response_headers: Some(std::collections::HashMap::new()),
        response_body: Some(b"response_body".to_vec()),
        attempted_at: chrono::DateTime::<chrono::Utc>::from(env.clock.now_system()),
        succeeded: true,
        error_message: None,
    };
    env.storage().delivery_attempts.create_in_tx(&mut tx, &delivery_attempt).await.unwrap();

    tx.commit().await.unwrap();

    // Create attestation service using TestEnv for proper isolation
    let mut service = env.create_test_attestation_service().await.unwrap();

    let leaf = LeafData::new(
        delivery_attempt_id,
        event_id,
        "https://example.com/webhook".to_string(),
        [0x33u8; 32],
        1,
        Some(200),
        chrono::DateTime::<chrono::Utc>::from(env.clock.now_system()),
    )
    .unwrap();

    service.add_leaf(leaf).unwrap();
    let signed_head = service.try_commit_pending().await.unwrap();

    // Verify signed tree head contains expected batch metadata - should be at least
    // 1
    assert!(signed_head.tree_size >= 1);
    assert!(!signed_head.signature.is_empty());
    assert_eq!(signed_head.signature.len(), 64); // Ed25519 signature length

    // Verify batch metadata exists for this test's leaf
    let leaf_batch_id = env
        .storage()
        .merkle_leaves
        .find_batch_id_by_delivery_attempt(delivery_attempt_id)
        .await
        .unwrap()
        .unwrap(); // Unwrap the Option since we expect it to exist

    assert!(!leaf_batch_id.is_nil());

    // Verify tree head exists for this batch
    let batch_exists =
        env.storage().signed_tree_heads.exists_for_batch(leaf_batch_id).await.unwrap();

    assert!(batch_exists);
}

#[tokio::test]
async fn merkle_service_preserves_leaf_ordering() {
    let env = TestEnv::new_isolated().await.unwrap();

    // Create test entities using TestEnv for proper isolation
    let mut tx = env.pool().begin().await.unwrap();
    let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await.unwrap();
    let endpoint_id =
        env.create_endpoint_tx(&mut tx, tenant_id, "https://example.com/webhook").await.unwrap();

    // Create multiple delivery attempts
    let mut delivery_attempt_ids = Vec::new();
    let mut event_ids = Vec::new();

    for i in 0..5 {
        let delivery_id = Uuid::new_v4();
        delivery_attempt_ids.push(delivery_id);

        // Create webhook event
        // Create webhook event using repository
        let webhook = kapsel_testing::TestWebhook {
            tenant_id: tenant_id.0,
            endpoint_id: endpoint_id.0,
            source_event_id: format!("test-source-event-{i}"),
            idempotency_strategy: "source_id".to_string(),
            headers: std::collections::HashMap::new(),
            body: br#"{"test": "data"}"#.to_vec().into(),
            content_type: "application/json".to_string(),
        };
        let created_event_id = env.ingest_webhook_tx(&mut tx, &webhook).await.unwrap();
        event_ids.push(created_event_id.0);

        // Create delivery attempt using repository
        let delivery_attempt = kapsel_core::models::DeliveryAttempt {
            id: delivery_id,
            event_id: created_event_id,
            attempt_number: 1,
            endpoint_id,
            request_headers: std::collections::HashMap::new(),
            request_body: b"request_body".to_vec(),
            response_status: Some(200),
            response_headers: Some(std::collections::HashMap::new()),
            response_body: Some(b"response_body".to_vec()),
            attempted_at: chrono::DateTime::<chrono::Utc>::from(env.clock.now_system()),
            succeeded: true,
            error_message: None,
        };
        env.storage().delivery_attempts.create_in_tx(&mut tx, &delivery_attempt).await.unwrap();
    }

    tx.commit().await.unwrap();

    // Create attestation service using TestEnv for proper isolation
    let mut service = env.create_test_attestation_service().await.unwrap();

    let timestamp = chrono::DateTime::<chrono::Utc>::from(env.clock.now_system());

    // Add leaves in specific order
    for (i, (delivery_id, event_id)) in
        delivery_attempt_ids.iter().zip(event_ids.iter()).enumerate()
    {
        let leaf = LeafData::new(
            *delivery_id,
            *event_id,
            format!("https://example.com/webhook{i}"),
            [u8::try_from(i).unwrap_or(0); 32],
            1,
            Some(200),
            timestamp,
        )
        .unwrap();

        service.add_leaf(leaf).unwrap();
    }

    // Commit batch
    service.try_commit_pending().await.unwrap();

    // Verify leaves are stored in correct order
    let stored_leaves = env
        .storage()
        .merkle_leaves
        .find_attempts_with_tree_indices(&delivery_attempt_ids)
        .await
        .unwrap();

    // Should have 5 leaves with consecutive indices
    assert_eq!(stored_leaves.len(), 5);

    // Verify ordering is preserved (indices should be consecutive)
    for i in 1..stored_leaves.len() {
        assert_eq!(stored_leaves[i].1, stored_leaves[i - 1].1 + 1);
    }

    // Verify all our delivery attempts are present
    let stored_ids: Vec<Uuid> = stored_leaves.iter().map(|(id, _)| *id).collect();
    for expected_id in &delivery_attempt_ids {
        assert!(stored_ids.contains(expected_id));
    }
}
