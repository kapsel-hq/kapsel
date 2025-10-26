//! Integration tests for Merkle tree service operations.
//!
//! Tests MerkleService with database persistence, batch leaf processing,
//! signed tree head generation, and cryptographic proof verification.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]

use chrono::{DateTime, Utc};
use kapsel_attestation::LeafData;
use kapsel_testing::TestEnv;
use uuid::Uuid;

#[tokio::test]
async fn merkle_service_adds_leaf_to_pending_queue() {
    let env = TestEnv::new_isolated().await.unwrap();
    let mut service = env.create_test_attestation_service().await.unwrap();

    let leaf = LeafData::new(
        Uuid::new_v4(),
        Uuid::new_v4(),
        "https://example.com/webhook".to_string(),
        [0x42u8; 32],
        1,
        Some(200),
        Utc::now(),
    )
    .unwrap();

    // Add leaf to pending queue
    service.add_leaf(leaf).unwrap();

    // Verify pending count
    assert_eq!(service.pending_count(), 1);
}

#[tokio::test]
async fn merkle_service_commits_batch_atomically() {
    let env = TestEnv::new_isolated().await.unwrap();

    // Create test entities using TestEnv for proper isolation
    let mut tx = env.pool().begin().await.unwrap();
    let tenant_id = env.create_tenant_tx(&mut *tx, "test-tenant").await.unwrap();
    let endpoint_id =
        env.create_endpoint_tx(&mut *tx, tenant_id, "https://example.com/webhook").await.unwrap();
    let delivery_attempt_id = Uuid::new_v4();
    let event_id = Uuid::new_v4();

    // Create webhook event
    sqlx::query(
        "INSERT INTO webhook_events (id, tenant_id, endpoint_id, source_event_id, idempotency_strategy, status, headers, body, content_type, payload_size)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"
    )
    .bind(event_id)
    .bind(tenant_id.0)
    .bind(endpoint_id.0)
    .bind("test-source-event")
    .bind("source_id")
    .bind("pending")
    .bind(serde_json::json!({}))
    .bind(br#"{"test": "data"}"#)
    .bind("application/json")
    .bind(i32::try_from(r#"{"test": "data"}"#.len()).expect("payload size fits in i32"))
    .execute(&mut *tx)
    .await
    .unwrap();

    // Create delivery attempt
    sqlx::query(
        "INSERT INTO delivery_attempts (id, event_id, attempt_number, request_url, request_headers, response_status, attempted_at)
         VALUES ($1, $2, 1, $3, $4, 200, NOW())"
    )
    .bind(delivery_attempt_id)
    .bind(event_id)
    .bind("https://example.com/webhook")
    .bind(serde_json::json!({}))
    .execute(&mut *tx)
    .await
    .unwrap();

    tx.commit().await.unwrap();

    // Create attestation service using TestEnv for proper isolation
    let mut service = env.create_test_attestation_service().await.unwrap();

    let leaf = LeafData::new(
        delivery_attempt_id,
        event_id,
        "https://example.com/webhook".to_string(),
        [0x11u8; 32],
        1,
        Some(201),
        DateTime::from_timestamp(1_640_995_200, 0).unwrap().with_timezone(&Utc),
    )
    .unwrap();

    // Add leaf and commit
    service.add_leaf(leaf).unwrap();
    let signed_head = service.try_commit_pending().await.unwrap();

    // Verify signed tree head - tree size should be at least 1 (our leaf was added)
    assert!(signed_head.tree_size >= 1);
    assert_eq!(signed_head.signature.len(), 64);
    assert_eq!(signed_head.root_hash.len(), 32);

    // Verify database persistence - filter by this test's delivery attempt
    let leaf_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM merkle_leaves WHERE delivery_attempt_id = $1")
            .bind(delivery_attempt_id)
            .fetch_one(env.pool())
            .await
            .unwrap();
    assert_eq!(leaf_count, 1);

    // Verify tree head was created (check by tree_size since we restored existing
    // state)
    let tree_head_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM signed_tree_heads WHERE tree_size >= 1)")
            .fetch_one(env.pool())
            .await
            .unwrap();
    assert!(tree_head_exists);

    // Verify pending queue is cleared
    assert_eq!(service.pending_count(), 0);
}

#[tokio::test]
async fn merkle_service_handles_multiple_leaves_in_batch() {
    let env = TestEnv::new_isolated().await.unwrap();

    // Create test entities using TestEnv for proper isolation
    let mut tx = env.pool().begin().await.unwrap();
    let tenant_id = env.create_tenant_tx(&mut *tx, "test-tenant").await.unwrap();
    let endpoint_id =
        env.create_endpoint_tx(&mut *tx, tenant_id, "https://example.com/webhook").await.unwrap();
    let delivery_attempt_id_1 = Uuid::new_v4();
    let event_id_1 = Uuid::new_v4();
    let delivery_attempt_id_2 = Uuid::new_v4();
    let event_id_2 = Uuid::new_v4();

    // Create webhook events and delivery attempts
    for (event_id, delivery_attempt_id, source_id) in [
        (event_id_1, delivery_attempt_id_1, "test-source-1"),
        (event_id_2, delivery_attempt_id_2, "test-source-2"),
    ] {
        sqlx::query(
            "INSERT INTO webhook_events (id, tenant_id, endpoint_id, source_event_id, idempotency_strategy, status, headers, body, content_type, payload_size)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"
        )
        .bind(event_id)
        .bind(tenant_id.0)
        .bind(endpoint_id.0)
        .bind(source_id)
        .bind("source_id")
        .bind("pending")
        .bind(serde_json::json!({}))
        .bind(br#"{"test": "data"}"#)
        .bind("application/json")
        .bind(i32::try_from(r#"{"test": "data"}"#.len()).expect("payload size fits in i32"))
        .execute(&mut *tx)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO delivery_attempts (id, event_id, attempt_number, request_url, request_headers, response_status, attempted_at)
             VALUES ($1, $2, 1, $3, $4, 200, NOW())"
        )
        .bind(delivery_attempt_id)
        .bind(event_id)
        .bind("https://example.com/webhook")
        .bind(serde_json::json!({}))
        .execute(&mut *tx)
        .await
        .unwrap();
    }

    tx.commit().await.unwrap();

    // Create attestation service using TestEnv for proper isolation
    let mut service = env.create_test_attestation_service().await.unwrap();

    let timestamp = DateTime::from_timestamp(1_640_995_200, 0).unwrap().with_timezone(&Utc);

    let leaf1 = LeafData::new(
        delivery_attempt_id_1,
        event_id_1,
        "https://example.com/webhook1".to_string(),
        [0x11u8; 32],
        1,
        Some(200),
        timestamp,
    )
    .unwrap();

    let leaf2 = LeafData::new(
        delivery_attempt_id_2,
        event_id_2,
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

    // Verify both leaves are in database with correct tree indices - filter by this
    // test's attempts
    let leaves: Vec<(i64,)> = sqlx::query_as(
        "SELECT tree_index FROM merkle_leaves
         WHERE delivery_attempt_id IN ($1, $2)
         ORDER BY tree_index",
    )
    .bind(delivery_attempt_id_1)
    .bind(delivery_attempt_id_2)
    .fetch_all(env.pool())
    .await
    .unwrap();

    assert_eq!(leaves.len(), 2);
    // Tree indices should be consecutive (relative to current tree state)
    assert_eq!(leaves[0].0 + 1, leaves[1].0);
}

#[tokio::test]
async fn merkle_service_fails_commit_with_empty_pending_queue() {
    let env = TestEnv::new_isolated().await.unwrap();
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
    let tenant_id = env.create_tenant_tx(&mut *tx, "test-tenant").await.unwrap();
    let endpoint_id =
        env.create_endpoint_tx(&mut *tx, tenant_id, "https://example.com/webhook").await.unwrap();
    let delivery_attempt_id = Uuid::new_v4();
    let event_id = Uuid::new_v4();

    // Create webhook event
    sqlx::query(
        "INSERT INTO webhook_events (id, tenant_id, endpoint_id, source_event_id, idempotency_strategy, status, headers, body, content_type, payload_size)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"
    )
    .bind(event_id)
    .bind(tenant_id.0)
    .bind(endpoint_id.0)
    .bind("test-source-event")
    .bind("source_id")
    .bind("pending")
    .bind(serde_json::json!({}))
    .bind(br#"{"test": "data"}"#)
    .bind("application/json")
    .bind(i32::try_from(r#"{"test": "data"}"#.len()).expect("payload size fits in i32"))
    .execute(&mut *tx)
    .await
    .unwrap();

    // Create delivery attempt
    sqlx::query(
        "INSERT INTO delivery_attempts (id, event_id, attempt_number, request_url, request_headers, response_status, attempted_at)
         VALUES ($1, $2, 1, $3, $4, 200, NOW())"
    )
    .bind(delivery_attempt_id)
    .bind(event_id)
    .bind("https://example.com/webhook")
    .bind(serde_json::json!({}))
    .execute(&mut *tx)
    .await
    .unwrap();

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
        Utc::now(),
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
    let leaf_batch_id: Uuid =
        sqlx::query_scalar("SELECT batch_id FROM merkle_leaves WHERE delivery_attempt_id = $1")
            .bind(delivery_attempt_id)
            .fetch_one(env.pool())
            .await
            .unwrap();

    assert!(!leaf_batch_id.is_nil());

    // Verify tree head exists for this batch
    let batch_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM signed_tree_heads WHERE batch_id = $1)")
            .bind(leaf_batch_id)
            .fetch_one(env.pool())
            .await
            .unwrap();

    assert!(batch_exists);
}

#[tokio::test]
async fn merkle_service_rejects_invalid_attempt_numbers() {
    // Try to create leaf with invalid attempt number (should fail at LeafData::new)
    let result = LeafData::new(
        Uuid::new_v4(),
        Uuid::new_v4(),
        "https://example.com".to_string(),
        [0x42u8; 32],
        0, // Invalid: not positive
        Some(200),
        Utc::now(),
    );

    assert!(result.is_err(), "Zero attempt number should be rejected");

    let result = LeafData::new(
        Uuid::new_v4(),
        Uuid::new_v4(),
        "https://example.com".to_string(),
        [0x42u8; 32],
        -1, // Invalid: negative
        Some(200),
        Utc::now(),
    );

    assert!(result.is_err(), "Negative attempt number should be rejected");
}

#[tokio::test]
async fn merkle_service_preserves_leaf_ordering() {
    let env = TestEnv::new_isolated().await.unwrap();

    // Create test entities using TestEnv for proper isolation
    let mut tx = env.pool().begin().await.unwrap();
    let tenant_id = env.create_tenant_tx(&mut *tx, "test-tenant").await.unwrap();
    let endpoint_id =
        env.create_endpoint_tx(&mut *tx, tenant_id, "https://example.com/webhook").await.unwrap();

    // Create multiple delivery attempts
    let mut delivery_attempt_ids = Vec::new();
    let mut event_ids = Vec::new();

    for i in 0..5 {
        let delivery_id = Uuid::new_v4();
        let event_id = Uuid::new_v4();
        delivery_attempt_ids.push(delivery_id);
        event_ids.push(event_id);

        // Create webhook event
        sqlx::query(
            "INSERT INTO webhook_events (id, tenant_id, endpoint_id, source_event_id, idempotency_strategy, status, headers, body, content_type, payload_size)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"
        )
        .bind(event_id)
        .bind(tenant_id.0)
        .bind(endpoint_id.0)
        .bind(format!("test-source-{i}"))
        .bind("source_id")
        .bind("pending")
        .bind(serde_json::json!({}))
        .bind(br#"{"test": "data"}"#)
        .bind("application/json")
        .bind(i32::try_from(r#"{"test": "data"}"#.len()).expect("payload size fits in i32"))
        .execute(&mut *tx)
        .await
        .unwrap();

        // Create delivery attempt
        sqlx::query(
            "INSERT INTO delivery_attempts (id, event_id, attempt_number, request_url, request_headers, response_status, attempted_at)
             VALUES ($1, $2, 1, $3, $4, 200, NOW())"
        )
        .bind(delivery_id)
        .bind(event_id)
        .bind("https://example.com/webhook")
        .bind(serde_json::json!({}))
        .execute(&mut *tx)
        .await
        .unwrap();
    }

    tx.commit().await.unwrap();

    // Create attestation service using TestEnv for proper isolation
    let mut service = env.create_test_attestation_service().await.unwrap();

    let timestamp = Utc::now();

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

    // Verify leaves are stored in correct order - filter by this test's attempts
    let stored_leaves: Vec<(Uuid, i64)> = sqlx::query_as(
        "SELECT delivery_attempt_id, tree_index FROM merkle_leaves
         WHERE delivery_attempt_id = ANY($1) ORDER BY tree_index",
    )
    .bind(&delivery_attempt_ids)
    .fetch_all(env.pool())
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
