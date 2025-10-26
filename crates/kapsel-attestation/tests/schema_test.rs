//! Integration tests for attestation database schema validation.
//!
//! Tests database table structure, constraints, and relationships
//! for attestation components including Merkle leaves and signed tree heads.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]

use kapsel_core::IdempotencyStrategy;
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
    sqlx::query("UPDATE attestation_keys SET is_active = FALSE WHERE is_active = TRUE")
        .execute(&mut *tx)
        .await
        .unwrap();

    let key1 = vec![1u8; 32];
    let key2 = vec![2u8; 32];

    // First active key succeeds
    sqlx::query("INSERT INTO attestation_keys (public_key, is_active) VALUES ($1, TRUE)")
        .bind(&key1)
        .execute(&mut *tx)
        .await
        .unwrap();

    // Second active key fails
    let result: Result<_, sqlx::Error> =
        sqlx::query("INSERT INTO attestation_keys (public_key, is_active) VALUES ($1, TRUE)")
            .bind(&key2)
            .execute(&mut *tx)
            .await;

    assert!(result.is_err(), "Only one active key allowed");

    // Rollback transaction to prevent connection leak
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn merkle_leaves_enforces_constraints() {
    let env = TestEnv::new_isolated().await.unwrap();
    let mut tx = env.pool().begin().await.unwrap();

    // Create test delivery attempt
    let attempt_id = create_test_delivery_attempt(&mut tx).await;

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
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> uuid::Uuid {
    // Create minimal test data chain: tenant -> endpoint -> event -> attempt
    let tenant_id = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, plan) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind("test")
        .bind("free")
        .execute(&mut **tx)
        .await
        .unwrap();

    let endpoint_id = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO endpoints (id, tenant_id, name, url) VALUES ($1, $2, $3, $4)")
        .bind(endpoint_id)
        .bind(tenant_id)
        .bind("test-endpoint")
        .bind("https://example.com")
        .execute(&mut **tx)
        .await
        .unwrap();

    let event_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO webhook_events (id, tenant_id, endpoint_id, source_event_id, idempotency_strategy, headers, body, content_type, payload_size)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"
    )
    .bind(event_id)
    .bind(tenant_id)
    .bind(endpoint_id)
    .bind("test-event")
    .bind(IdempotencyStrategy::Header)
    .bind(serde_json::json!({}))
    .bind(&b"test"[..])
    .bind("application/json")
    .bind(4i32)
    .execute(&mut **tx)
    .await
    .unwrap();

    let attempt_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO delivery_attempts (id, event_id, attempt_number, request_url, request_headers, request_method)
         VALUES ($1, $2, $3, $4, $5, $6)"
    )
    .bind(attempt_id)
    .bind(event_id)
    .bind(1i32)
    .bind("https://example.com")
    .bind(serde_json::json!({}))
    .bind("POST")
    .execute(&mut **tx)
    .await
    .unwrap();

    attempt_id
}
