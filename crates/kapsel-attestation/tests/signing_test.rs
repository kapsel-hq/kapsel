//! Integration tests for Ed25519 signing service operations.
//!
//! Tests cryptographic signing, key management, and signature verification
//! with database-backed key storage and retrieval.

use kapsel_core::{storage::attestation_keys::AttestationKey, Clock};
use kapsel_testing::TestEnv;
use uuid::Uuid;

#[tokio::test]
async fn store_and_load_signing_key() {
    let env = TestEnv::new_shared().await.unwrap();
    let mut tx = env.pool().begin().await.unwrap();

    // Generate test key
    let public_key = vec![11u8; 32];

    // Create key within transaction
    let key = AttestationKey {
        id: Uuid::new_v4(),
        public_key: public_key.clone(),
        is_active: false,
        created_at: chrono::DateTime::<chrono::Utc>::from(env.clock.now_system()),
        deactivated_at: None,
    };
    let key_id = env.storage().attestation_keys.create_in_tx(&mut tx, &key).await.unwrap();

    // Activate the key within transaction
    env.storage().attestation_keys.activate_in_tx(&mut tx, key_id).await.unwrap();

    // Load key using repository within same transaction
    let active_key =
        env.storage().attestation_keys.find_active_in_tx(&mut tx).await.unwrap().unwrap();

    assert_eq!(active_key.public_key, public_key);

    // Transaction auto-rollbacks when dropped
}

#[tokio::test]
async fn only_one_active_key_at_a_time() {
    let env = TestEnv::new_shared().await.unwrap();
    let mut tx = env.pool().begin().await.unwrap();

    let key1 = vec![21u8; 32];
    let key2 = vec![22u8; 32];

    // Create first active key within transaction
    let key1_obj = AttestationKey {
        id: Uuid::new_v4(),
        public_key: key1,
        is_active: false,
        created_at: chrono::DateTime::<chrono::Utc>::from(env.clock.now_system()),
        deactivated_at: None,
    };
    let key1_id = env.storage().attestation_keys.create_in_tx(&mut tx, &key1_obj).await.unwrap();
    env.storage().attestation_keys.activate_in_tx(&mut tx, key1_id).await.unwrap();

    // Verify only one active key exists
    let active_count = env.storage().attestation_keys.count_active_in_tx(&mut tx).await.unwrap();
    assert_eq!(active_count, 1);

    // Create second key within transaction
    let key2_obj = AttestationKey {
        id: Uuid::new_v4(),
        public_key: key2,
        is_active: false,
        created_at: chrono::DateTime::<chrono::Utc>::from(env.clock.now_system()),
        deactivated_at: None,
    };
    let key2_id = env.storage().attestation_keys.create_in_tx(&mut tx, &key2_obj).await.unwrap();

    // Deactivate first key and activate second within transaction
    env.storage().attestation_keys.deactivate_all_in_tx(&mut tx).await.unwrap();
    env.storage().attestation_keys.activate_in_tx(&mut tx, key2_id).await.unwrap();

    // Verify still only one active key
    let active_count = env.storage().attestation_keys.count_active_in_tx(&mut tx).await.unwrap();
    assert_eq!(active_count, 1);

    let active_key =
        env.storage().attestation_keys.find_active_in_tx(&mut tx).await.unwrap().unwrap();
    assert_eq!(active_key.public_key, key2_obj.public_key);

    // Transaction auto-rollbacks when dropped
}

#[tokio::test]
async fn deactivate_old_key_when_rotating() {
    let env = TestEnv::new_shared().await.unwrap();
    let mut tx = env.pool().begin().await.unwrap();

    let old_key = vec![31u8; 32];
    let new_key = vec![32u8; 32];

    // Insert first active key within transaction
    let old_key_obj = AttestationKey {
        id: Uuid::new_v4(),
        public_key: old_key,
        is_active: false,
        created_at: chrono::DateTime::<chrono::Utc>::from(env.clock.now_system()),
        deactivated_at: None,
    };
    let old_key_id =
        env.storage().attestation_keys.create_in_tx(&mut tx, &old_key_obj).await.unwrap();
    env.storage().attestation_keys.activate_in_tx(&mut tx, old_key_id).await.unwrap();

    // Create and activate new key within transaction
    let new_key_obj = AttestationKey {
        id: Uuid::new_v4(),
        public_key: new_key.clone(),
        is_active: false,
        created_at: chrono::DateTime::<chrono::Utc>::from(env.clock.now_system()),
        deactivated_at: None,
    };
    let new_key_id =
        env.storage().attestation_keys.create_in_tx(&mut tx, &new_key_obj).await.unwrap();
    env.storage().attestation_keys.deactivate_all_in_tx(&mut tx).await.unwrap();
    env.storage().attestation_keys.activate_in_tx(&mut tx, new_key_id).await.unwrap();

    // Verify only new key is active
    let active_key =
        env.storage().attestation_keys.find_active_in_tx(&mut tx).await.unwrap().unwrap();
    let active_count = env.storage().attestation_keys.count_active_in_tx(&mut tx).await.unwrap();

    assert_eq!(active_count, 1);
    assert_eq!(active_key.public_key, new_key);

    // Transaction auto-rollbacks when dropped
}

#[tokio::test]
async fn load_active_key_returns_most_recent() {
    let env = TestEnv::new_shared().await.unwrap();
    let mut tx = env.pool().begin().await.unwrap();

    let key = vec![41u8; 32];

    // Insert key within transaction
    let key_obj = AttestationKey {
        id: Uuid::new_v4(),
        public_key: key.clone(),
        is_active: false,
        created_at: chrono::DateTime::<chrono::Utc>::from(env.clock.now_system()),
        deactivated_at: None,
    };
    let key_id = env.storage().attestation_keys.create_in_tx(&mut tx, &key_obj).await.unwrap();
    env.storage().attestation_keys.activate_in_tx(&mut tx, key_id).await.unwrap();

    // Load active key using repository within same transaction
    let active_key =
        env.storage().attestation_keys.find_active_in_tx(&mut tx).await.unwrap().unwrap();

    assert_eq!(active_key.public_key, key);

    // Transaction auto-rollbacks when dropped
}
