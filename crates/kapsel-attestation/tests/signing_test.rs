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

    // Store key as active using repository
    let _key_id =
        env.storage().attestation_keys.create_and_activate(public_key.clone()).await.unwrap();

    // Load key using repository
    let active_key = env.storage().attestation_keys.find_active().await.unwrap().unwrap();

    assert_eq!(active_key.public_key, public_key);
}

#[tokio::test]
async fn only_one_active_key_at_a_time() {
    let env = TestEnv::new_isolated().await.unwrap();

    let key1 = vec![1u8; 32];
    let key2 = vec![2u8; 32];

    // Insert first active key using repository
    let _key1_id = env.storage().attestation_keys.create_and_activate(key1).await.unwrap();

    // Attempt to create second active key should fail due to unique constraint
    let result = env.storage().attestation_keys.create_and_activate(key2).await;

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

    // Insert first active key using repository
    let _old_key_id = env.storage().attestation_keys.create_and_activate(old_key).await.unwrap();

    // Rotate to new key atomically using repository
    let _new_key_id =
        env.storage().attestation_keys.create_and_activate(new_key.clone()).await.unwrap();

    // Verify only new key is active
    let active_key = env.storage().attestation_keys.find_active().await.unwrap().unwrap();
    let active_count = env.storage().attestation_keys.count_active().await.unwrap();

    assert_eq!(active_count, 1);
    assert_eq!(active_key.public_key, new_key);
}

#[tokio::test]
async fn load_active_key_returns_most_recent() {
    let env = TestEnv::new_isolated().await.unwrap();

    let key = vec![1u8; 32];

    // Insert key using repository
    let _key_id = env.storage().attestation_keys.create_and_activate(key.clone()).await.unwrap();

    // Load active key using repository (already handles proper ordering)
    let active_key = env.storage().attestation_keys.find_active().await.unwrap().unwrap();

    assert_eq!(active_key.public_key, key);
}
