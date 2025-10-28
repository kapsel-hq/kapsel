//! Integration tests for attestation database schema validation.
//!
//! Tests database table structure, constraints, and relationships
//! for attestation components including Merkle leaves and signed tree heads.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]

use std::collections::HashMap;

use kapsel_core::{models::DeliveryAttempt, Clock};
use kapsel_testing::TestEnv;

#[tokio::test]
async fn attestation_tables_exist() {
    let env = TestEnv::new().await.unwrap();
    let pool = env.pool();

    // Verify all 4 attestation tables exist
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.tables
         WHERE table_schema = 'public'
         AND table_name IN ('attestation_keys', 'merkle_leaves', 'signed_tree_heads', 'proof_cache')"
    )
    .fetch_one(pool)
    .await
    .unwrap();

    assert_eq!(count, 4, "All attestation tables must exist");
}

#[tokio::test]
async fn only_one_active_attestation_key_allowed() {
    let env = TestEnv::new_isolated().await.unwrap();
    let mut tx = env.pool().begin().await.unwrap();

    // Deactivate any existing active keys within this transaction
    env.storage().attestation_keys.deactivate_all_in_tx(&mut tx).await.unwrap();

    let key1 = vec![1u8; 32];
    let key2 = vec![2u8; 32];

    // First active key succeeds
    let attestation_key1 = kapsel_core::storage::attestation_keys::AttestationKey {
        id: uuid::Uuid::new_v4(),
        public_key: key1.clone(),
        is_active: true,
        created_at: chrono::DateTime::<chrono::Utc>::from(env.clock.now_system()),
        deactivated_at: None,
    };
    env.storage().attestation_keys.create_in_tx(&mut tx, &attestation_key1).await.unwrap();

    // Second active key fails
    // Attempt to insert second active key should fail due to unique constraint
    let attestation_key2 = kapsel_core::storage::attestation_keys::AttestationKey {
        id: uuid::Uuid::new_v4(),
        public_key: key2,
        is_active: true,
        created_at: chrono::DateTime::<chrono::Utc>::from(env.clock.now_system()),
        deactivated_at: None,
    };
    let result = env.storage().attestation_keys.create_in_tx(&mut tx, &attestation_key2).await;

    assert!(result.is_err(), "Only one active key allowed");

    // Rollback transaction to prevent connection leak
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn merkle_leaves_enforces_constraints() {
    let env = TestEnv::new_isolated().await.unwrap();
    let mut tx = env.pool().begin().await.unwrap();

    // Create test delivery attempt
    let attempt_id = create_test_delivery_attempt(&env, &mut tx).await;

    // Valid leaf succeeds
    let leaf_hash = vec![0u8; 32];
    let payload_hash = vec![1u8; 32];

    sqlx::query(
        "INSERT INTO merkle_leaves
         (leaf_hash, delivery_attempt_id, event_id, tenant_id, endpoint_url,
          payload_hash, attempt_number, attempted_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())",
    )
    .bind(&leaf_hash)
    .bind(attempt_id)
    .bind(uuid::Uuid::new_v4())
    .bind(uuid::Uuid::new_v4())
    .bind("https://example.com")
    .bind(&payload_hash)
    .bind(1i32)
    .execute(&mut *tx)
    .await
    .unwrap();

    // Invalid hash length fails
    let bad_hash = vec![0u8; 31]; // Wrong length!
    let result: Result<_, sqlx::Error> = sqlx::query(
        "INSERT INTO merkle_leaves
         (leaf_hash, delivery_attempt_id, event_id, tenant_id, endpoint_url,
          payload_hash, attempt_number, attempted_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())",
    )
    .bind(&bad_hash)
    .bind(attempt_id)
    .bind(uuid::Uuid::new_v4())
    .bind(uuid::Uuid::new_v4())
    .bind("https://example.com")
    .bind(&payload_hash)
    .bind(1i32)
    .execute(&mut *tx)
    .await;

    assert!(result.is_err(), "leaf_hash must be exactly 32 bytes");

    // Rollback transaction to prevent connection leak
    tx.rollback().await.unwrap();
}

async fn create_test_delivery_attempt(
    env: &TestEnv,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> uuid::Uuid {
    // Create minimal test data chain: tenant -> endpoint -> event -> attempt
    let tenant_id = env.create_tenant_tx(tx, "test").await.unwrap();
    let endpoint_id = env.create_endpoint_tx(tx, tenant_id, "https://example.com").await.unwrap();

    let webhook = kapsel_testing::TestWebhook {
        tenant_id: tenant_id.0,
        endpoint_id: endpoint_id.0,
        source_event_id: "test-event".to_string(),
        idempotency_strategy: "header".to_string(),
        headers: HashMap::new(),
        body: b"test".to_vec().into(),
        content_type: "application/json".to_string(),
    };
    let event_id = env.ingest_webhook_tx(tx, &webhook).await.unwrap();

    let attempt_id = uuid::Uuid::new_v4();
    let delivery_attempt = DeliveryAttempt {
        id: attempt_id,
        event_id,
        attempt_number: 1,
        endpoint_id,
        request_headers: HashMap::new(),
        request_body: b"test".to_vec(),
        response_status: Some(200),
        response_headers: Some(HashMap::new()),
        response_body: Some(b"response".to_vec()),
        attempted_at: chrono::DateTime::<chrono::Utc>::from(env.clock.now_system()),
        succeeded: true,
        error_message: None,
    };

    env.storage().delivery_attempts.create_in_tx(tx, &delivery_attempt).await.unwrap();

    attempt_id
}
