//! Integration tests for Merkle tree service operations.
//!
//! Tests MerkleService with database persistence, batch leaf processing,
//! signed tree head generation, and cryptographic proof verification.

use chrono::{DateTime, Utc};
use kapsel_attestation::{LeafData, MerkleService, SigningService};
use kapsel_core::IdempotencyStrategy;
use kapsel_testing::database::TestDatabase;
use uuid::Uuid;

/// Create test delivery attempt with all required dependencies.
async fn create_test_delivery_attempt(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    delivery_attempt_id: Uuid,
    event_id: Uuid,
) -> Uuid {
    let tenant_id = Uuid::new_v4();
    let endpoint_id = Uuid::new_v4();

    // Create tenant
    sqlx::query("INSERT INTO tenants (id, name, plan) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind(format!("test-tenant-{}", uuid::Uuid::new_v4()))
        .bind("enterprise")
        .execute(&mut **tx)
        .await
        .unwrap();

    // Create endpoint
    sqlx::query("INSERT INTO endpoints (id, tenant_id, name, url) VALUES ($1, $2, $3, $4)")
        .bind(endpoint_id)
        .bind(tenant_id)
        .bind("test-endpoint")
        .bind("https://example.com/webhook")
        .execute(&mut **tx)
        .await
        .unwrap();

    // Create webhook event
    sqlx::query(
        "INSERT INTO webhook_events (id, tenant_id, endpoint_id, source_event_id, idempotency_strategy,
         headers, body, content_type, payload_size, status)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"
    )
    .bind(event_id)
    .bind(tenant_id)
    .bind(endpoint_id)
    .bind("test-source-event")
    .bind(IdempotencyStrategy::Header)
    .bind(serde_json::json!({}))
    .bind(&b"test payload"[..])
    .bind("application/json")
    .bind(12i32)
    .bind("pending")
    .execute(&mut **tx)
    .await
    .unwrap();

    // Create delivery attempt
    sqlx::query(
        "INSERT INTO delivery_attempts (id, event_id, attempt_number, request_url,
         request_headers, request_method, response_status, attempted_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())",
    )
    .bind(delivery_attempt_id)
    .bind(event_id)
    .bind(1i32)
    .bind("https://example.com/webhook")
    .bind(serde_json::json!({}))
    .bind("POST")
    .bind(200i32)
    .execute(&mut **tx)
    .await
    .unwrap();

    tenant_id
}

#[tokio::test]
async fn merkle_service_adds_leaf_to_pending_queue() {
    let db = TestDatabase::new().await.unwrap();
    let pool = db.pool();
    let signing = SigningService::ephemeral();
    let mut service = MerkleService::new(pool.clone(), signing);

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
    let db = TestDatabase::new().await.unwrap();
    let mut tx = db.pool().begin().await.unwrap();

    let delivery_attempt_id = Uuid::new_v4();
    let event_id = Uuid::new_v4();

    // Create test data
    create_test_delivery_attempt(&mut tx, delivery_attempt_id, event_id).await;
    tx.commit().await.unwrap();

    // Create service and store its key in database
    let signing = SigningService::ephemeral();
    let public_key = signing.public_key_as_bytes();

    // Insert attestation key into database
    let key_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO attestation_keys (public_key, is_active) VALUES ($1, TRUE) RETURNING id",
    )
    .bind(&public_key)
    .fetch_one(db.pool())
    .await
    .unwrap();

    // Update signing service with database key ID
    let signing = signing.with_key_id(key_id);
    let mut service = MerkleService::new(db.pool().clone(), signing);

    let leaf = LeafData::new(
        delivery_attempt_id,
        event_id,
        "https://example.com/webhook".to_string(),
        [0x11u8; 32],
        1,
        Some(201),
        DateTime::from_timestamp(1640995200, 0).unwrap().with_timezone(&Utc),
    )
    .unwrap();

    // Add leaf and commit
    service.add_leaf(leaf).unwrap();
    let signed_head = service.try_commit_pending().await.unwrap();

    // Verify signed tree head
    assert_eq!(signed_head.tree_size, 1);
    assert_eq!(signed_head.signature.len(), 64);
    assert_eq!(signed_head.root_hash.len(), 32);

    // Verify database persistence
    let leaf_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM merkle_leaves")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(leaf_count, 1);

    let tree_head_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM signed_tree_heads")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(tree_head_count, 1);

    // Verify pending queue is cleared
    assert_eq!(service.pending_count(), 0);
}

#[tokio::test]
async fn merkle_service_handles_multiple_leaves_in_batch() {
    let db = TestDatabase::new().await.unwrap();
    let mut tx = db.pool().begin().await.unwrap();

    let delivery_attempt_id_1 = Uuid::new_v4();
    let event_id_1 = Uuid::new_v4();
    let delivery_attempt_id_2 = Uuid::new_v4();
    let event_id_2 = Uuid::new_v4();

    // Create test data for both attempts
    create_test_delivery_attempt(&mut tx, delivery_attempt_id_1, event_id_1).await;
    create_test_delivery_attempt(&mut tx, delivery_attempt_id_2, event_id_2).await;
    tx.commit().await.unwrap();

    // Create service and store its key in database
    let signing = SigningService::ephemeral();
    let public_key = signing.public_key_as_bytes();

    // Insert attestation key into database
    let key_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO attestation_keys (public_key, is_active) VALUES ($1, TRUE) RETURNING id",
    )
    .bind(&public_key)
    .fetch_one(db.pool())
    .await
    .unwrap();

    // Update signing service with database key ID
    let signing = signing.with_key_id(key_id);
    let mut service = MerkleService::new(db.pool().clone(), signing);

    let timestamp = DateTime::from_timestamp(1640995200, 0).unwrap().with_timezone(&Utc);

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

    // Verify tree head reflects both leaves
    assert_eq!(signed_head.tree_size, 2);

    // Verify both leaves are in database with correct tree indices
    let leaves: Vec<(i64,)> =
        sqlx::query_as("SELECT tree_index FROM merkle_leaves ORDER BY tree_index")
            .fetch_all(db.pool())
            .await
            .unwrap();

    assert_eq!(leaves.len(), 2);
    assert_eq!(leaves[0].0, 0); // First leaf at index 0
    assert_eq!(leaves[1].0, 1); // Second leaf at index 1
}

#[tokio::test]
async fn merkle_service_fails_commit_with_empty_pending_queue() {
    let db = TestDatabase::new().await.unwrap();
    let signing = SigningService::ephemeral();
    let public_key = signing.public_key_as_bytes();

    // Insert attestation key into database
    let key_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO attestation_keys (public_key, is_active) VALUES ($1, TRUE) RETURNING id",
    )
    .bind(&public_key)
    .fetch_one(db.pool())
    .await
    .unwrap();

    // Update signing service with database key ID
    let signing = signing.with_key_id(key_id);
    let mut service = MerkleService::new(db.pool().clone(), signing);

    // Try to commit with no pending leaves
    let result = service.try_commit_pending().await;

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("no pending leaves"));
}

#[tokio::test]
async fn merkle_service_stores_batch_metadata() {
    let db = TestDatabase::new().await.unwrap();
    let mut tx = db.pool().begin().await.unwrap();

    let delivery_attempt_id = Uuid::new_v4();
    let event_id = Uuid::new_v4();

    create_test_delivery_attempt(&mut tx, delivery_attempt_id, event_id).await;
    tx.commit().await.unwrap();

    // Create service and store its key in database
    let signing = SigningService::ephemeral();
    let public_key = signing.public_key_as_bytes();

    // Insert attestation key into database
    let key_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO attestation_keys (public_key, is_active) VALUES ($1, TRUE) RETURNING id",
    )
    .bind(&public_key)
    .fetch_one(db.pool())
    .await
    .unwrap();

    // Update signing service with database key ID and add leaf
    let signing = signing.with_key_id(key_id);
    let mut service = MerkleService::new(db.pool().clone(), signing);

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

    // Verify signed tree head contains expected batch metadata
    assert_eq!(signed_head.tree_size, 1);
    assert!(!signed_head.signature.is_empty());
    assert_eq!(signed_head.signature.len(), 64); // Ed25519 signature length

    // Verify batch metadata in signed tree head
    let (batch_size, batch_id): (i32, Uuid) =
        sqlx::query_as("SELECT batch_size, batch_id FROM signed_tree_heads WHERE tree_size = 1")
            .fetch_one(db.pool())
            .await
            .unwrap();

    assert_eq!(batch_size, 1);
    assert!(!batch_id.is_nil());

    // Verify leaf has same batch_id
    let leaf_batch_id: Uuid =
        sqlx::query_scalar("SELECT batch_id FROM merkle_leaves WHERE delivery_attempt_id = $1")
            .bind(delivery_attempt_id)
            .fetch_one(db.pool())
            .await
            .unwrap();

    assert_eq!(leaf_batch_id, batch_id);
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
    let db = TestDatabase::new().await.unwrap();
    let mut tx = db.pool().begin().await.unwrap();

    // Create multiple delivery attempts
    let mut delivery_attempt_ids = Vec::new();
    let mut event_ids = Vec::new();

    for _ in 0..5 {
        let delivery_id = Uuid::new_v4();
        let event_id = Uuid::new_v4();
        create_test_delivery_attempt(&mut tx, delivery_id, event_id).await;
        delivery_attempt_ids.push(delivery_id);
        event_ids.push(event_id);
    }
    tx.commit().await.unwrap();

    let signing = SigningService::ephemeral();
    let public_key = signing.public_key_as_bytes();

    // Insert attestation key into database
    let key_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO attestation_keys (public_key, is_active) VALUES ($1, TRUE) RETURNING id",
    )
    .bind(&public_key)
    .fetch_one(db.pool())
    .await
    .unwrap();

    // Update signing service with database key ID
    let signing = signing.with_key_id(key_id);
    let mut service = MerkleService::new(db.pool().clone(), signing);

    let timestamp = Utc::now();

    // Add leaves in specific order
    for (i, (delivery_id, event_id)) in
        delivery_attempt_ids.iter().zip(event_ids.iter()).enumerate()
    {
        let leaf = LeafData::new(
            *delivery_id,
            *event_id,
            format!("https://example.com/webhook{}", i),
            [i as u8; 32],
            1,
            Some(200),
            timestamp,
        )
        .unwrap();

        service.add_leaf(leaf).unwrap();
    }

    // Commit batch
    service.try_commit_pending().await.unwrap();

    // Verify leaves are stored in correct tree order
    let stored_leaves: Vec<(Uuid, i64)> = sqlx::query_as(
        "SELECT delivery_attempt_id, tree_index FROM merkle_leaves ORDER BY tree_index",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();

    assert_eq!(stored_leaves.len(), 5);

    for (i, (stored_id, tree_index)) in stored_leaves.iter().enumerate() {
        assert_eq!(*stored_id, delivery_attempt_ids[i]);
        assert_eq!(*tree_index, i as i64);
    }
}
