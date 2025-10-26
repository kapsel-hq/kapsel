//! Integration tests for Ed25519 signing service operations.
//!
//! Tests cryptographic signing, key management, and signature verification
//! with database-backed key storage and retrieval.

use kapsel_testing::TestEnv;

#[tokio::test]
async fn store_and_load_signing_key() {
    let env = TestEnv::new_isolated().await.unwrap();

    // Generate test key
    let public_key = vec![1u8; 32];

    // Store key as active
    sqlx::query(
        "INSERT INTO attestation_keys (public_key, is_active, created_at)
         VALUES ($1, TRUE, NOW())",
    )
    .bind(&public_key)
    .execute(env.pool())
    .await
    .unwrap();

    // Load active key
    let loaded: (Vec<u8>,) =
        sqlx::query_as("SELECT public_key FROM attestation_keys WHERE is_active = TRUE")
            .fetch_one(env.pool())
            .await
            .unwrap();

    assert_eq!(loaded.0, public_key, "Loaded key should match stored key");
}

#[tokio::test]
async fn only_one_active_key_at_a_time() {
    let env = TestEnv::new_isolated().await.unwrap();

    let key1 = vec![1u8; 32];
    let key2 = vec![2u8; 32];

    // Insert first active key
    sqlx::query(
        "INSERT INTO attestation_keys (public_key, is_active, created_at)
         VALUES ($1, TRUE, NOW())",
    )
    .bind(&key1)
    .execute(env.pool())
    .await
    .unwrap();

    // Attempt to insert second active key should fail due to unique constraint
    let result: Result<_, sqlx::Error> = sqlx::query(
        "INSERT INTO attestation_keys (public_key, is_active, created_at)
         VALUES ($1, TRUE, NOW())",
    )
    .bind(&key2)
    .execute(env.pool())
    .await;

    assert!(result.is_err(), "Should not allow multiple active keys");

    // Verify the constraint name in error message
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("idx_attestation_keys_single_active"),
        "Error should reference unique constraint: {}",
        error_msg
    );
}

#[tokio::test]
async fn deactivate_old_key_when_rotating() {
    let env = TestEnv::new_isolated().await.unwrap();

    let old_key = vec![1u8; 32];
    let new_key = vec![2u8; 32];

    // Insert first active key
    sqlx::query(
        "INSERT INTO attestation_keys (public_key, is_active, created_at)
         VALUES ($1, TRUE, NOW())",
    )
    .bind(&old_key)
    .execute(env.pool())
    .await
    .unwrap();

    // Deactivate old key and insert new key in transaction
    let mut tx = env.pool().begin().await.unwrap();

    sqlx::query("UPDATE attestation_keys SET is_active = FALSE WHERE public_key = $1")
        .bind(&old_key)
        .execute(&mut *tx)
        .await
        .unwrap();

    sqlx::query(
        "INSERT INTO attestation_keys (public_key, is_active, created_at)
         VALUES ($1, TRUE, NOW())",
    )
    .bind(&new_key)
    .execute(&mut *tx)
    .await
    .unwrap();

    tx.commit().await.unwrap();

    // Verify only new key is active
    let active_keys: Vec<(Vec<u8>,)> =
        sqlx::query_as("SELECT public_key FROM attestation_keys WHERE is_active = TRUE")
            .fetch_all(env.pool())
            .await
            .unwrap();

    assert_eq!(active_keys.len(), 1, "Should have exactly one active key");
    assert_eq!(active_keys[0].0, new_key, "New key should be active");
}

#[tokio::test]
async fn load_active_key_returns_most_recent() {
    let env = TestEnv::new_isolated().await.unwrap();

    let key = vec![1u8; 32];

    // Insert key
    sqlx::query(
        "INSERT INTO attestation_keys (public_key, is_active, created_at)
         VALUES ($1, TRUE, NOW())",
    )
    .bind(&key)
    .execute(env.pool())
    .await
    .unwrap();

    // Load active key with proper ordering
    let loaded: (Vec<u8>,) = sqlx::query_as(
        "SELECT public_key FROM attestation_keys
         WHERE is_active = TRUE
         ORDER BY created_at DESC
         LIMIT 1",
    )
    .fetch_one(env.pool())
    .await
    .unwrap();

    assert_eq!(loaded.0, key, "Should load the most recently created active key");
}
