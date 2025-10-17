//! Integration tests for database-backed signing service.

use kapsel_testing::database::TestDatabase;

#[tokio::test]
async fn store_and_load_signing_key() {
    // RED: Test storing and loading a signing key from database
    let db = TestDatabase::new().await.unwrap();
    let mut tx = db.begin_transaction().await.unwrap();

    // Generate test key
    let public_key = vec![1u8; 32];

    // Store key as active
    sqlx::query(
        "INSERT INTO attestation_keys (public_key, is_active, created_at)
         VALUES ($1, TRUE, NOW())",
    )
    .bind(&public_key)
    .execute(&mut *tx)
    .await
    .unwrap();

    // Load active key
    let loaded: (Vec<u8>,) =
        sqlx::query_as("SELECT public_key FROM attestation_keys WHERE is_active = TRUE")
            .fetch_one(&mut *tx)
            .await
            .unwrap();

    assert_eq!(loaded.0, public_key, "Loaded key should match stored key");
}

#[tokio::test]
async fn only_one_active_key_at_a_time() {
    // RED: Verify constraint that only one key can be active
    let db = TestDatabase::new().await.unwrap();
    let mut tx = db.begin_transaction().await.unwrap();

    let key1 = vec![1u8; 32];
    let key2 = vec![2u8; 32];

    // Insert first active key
    sqlx::query(
        "INSERT INTO attestation_keys (public_key, is_active, created_at)
         VALUES ($1, TRUE, NOW())",
    )
    .bind(&key1)
    .execute(&mut *tx)
    .await
    .unwrap();

    // Attempt to insert second active key should fail
    let result: Result<_, sqlx::Error> = sqlx::query(
        "INSERT INTO attestation_keys (public_key, is_active, created_at)
         VALUES ($1, TRUE, NOW())",
    )
    .bind(&key2)
    .execute(&mut *tx)
    .await;

    assert!(result.is_err(), "Should not allow multiple active keys");
}

#[tokio::test]
async fn deactivate_old_key_when_rotating() {
    // RED: Test key rotation by deactivating old key
    let db = TestDatabase::new().await.unwrap();
    let mut tx = db.begin_transaction().await.unwrap();

    let old_key = vec![1u8; 32];
    let new_key = vec![2u8; 32];

    // Insert first active key
    sqlx::query(
        "INSERT INTO attestation_keys (public_key, is_active, created_at)
         VALUES ($1, TRUE, NOW())",
    )
    .bind(&old_key)
    .execute(&mut *tx)
    .await
    .unwrap();

    // Deactivate old key
    sqlx::query("UPDATE attestation_keys SET is_active = FALSE WHERE public_key = $1")
        .bind(&old_key)
        .execute(&mut *tx)
        .await
        .unwrap();

    // Insert new active key
    sqlx::query(
        "INSERT INTO attestation_keys (public_key, is_active, created_at)
         VALUES ($1, TRUE, NOW())",
    )
    .bind(&new_key)
    .execute(&mut *tx)
    .await
    .unwrap();

    // Verify only new key is active
    let active_keys: Vec<(Vec<u8>,)> =
        sqlx::query_as("SELECT public_key FROM attestation_keys WHERE is_active = TRUE")
            .fetch_all(&mut *tx)
            .await
            .unwrap();

    assert_eq!(active_keys.len(), 1, "Should have exactly one active key");
    assert_eq!(active_keys[0].0, new_key, "New key should be active");
}

#[tokio::test]
async fn load_active_key_returns_most_recent() {
    // RED: Test loading the most recently created active key
    let db = TestDatabase::new().await.unwrap();
    let mut tx = db.begin_transaction().await.unwrap();

    let key = vec![1u8; 32];

    // Insert key
    sqlx::query(
        "INSERT INTO attestation_keys (public_key, is_active, created_at)
         VALUES ($1, TRUE, NOW())",
    )
    .bind(&key)
    .execute(&mut *tx)
    .await
    .unwrap();

    // Load active key
    let loaded: (Vec<u8>,) = sqlx::query_as(
        "SELECT public_key FROM attestation_keys
         WHERE is_active = TRUE
         ORDER BY created_at DESC
         LIMIT 1",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();

    assert_eq!(loaded.0, key);
}
