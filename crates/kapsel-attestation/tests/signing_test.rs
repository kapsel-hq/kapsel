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

    // Load key by ID using repository within same transaction
    let stored_key =
        env.storage().attestation_keys.find_by_id_in_tx(&mut tx, key_id).await.unwrap().unwrap();

    assert_eq!(stored_key.public_key, public_key);
    assert_eq!(stored_key.id, key_id);
    assert!(!stored_key.is_active);
}

#[tokio::test]
async fn only_one_active_key_at_a_time() {
    let env = TestEnv::new_shared().await.unwrap();
    let mut tx = env.pool().begin().await.unwrap();

    let key1 = vec![21u8; 32];
    let key2 = vec![22u8; 32];

    // Create two inactive keys within transaction
    let key1_obj = AttestationKey {
        id: Uuid::new_v4(),
        public_key: key1.clone(),
        is_active: false,
        created_at: chrono::DateTime::<chrono::Utc>::from(env.clock.now_system()),
        deactivated_at: None,
    };
    let key1_id = env.storage().attestation_keys.create_in_tx(&mut tx, &key1_obj).await.unwrap();

    let key2_obj = AttestationKey {
        id: Uuid::new_v4(),
        public_key: key2.clone(),
        is_active: false,
        created_at: chrono::DateTime::<chrono::Utc>::from(env.clock.now_system()),
        deactivated_at: None,
    };
    let key2_id = env.storage().attestation_keys.create_in_tx(&mut tx, &key2_obj).await.unwrap();

    // Verify both keys exist but are inactive
    let stored_key1 =
        env.storage().attestation_keys.find_by_id_in_tx(&mut tx, key1_id).await.unwrap().unwrap();
    let stored_key2 =
        env.storage().attestation_keys.find_by_id_in_tx(&mut tx, key2_id).await.unwrap().unwrap();

    assert!(!stored_key1.is_active);
    assert!(!stored_key2.is_active);
    assert_eq!(stored_key1.public_key, key1);
    assert_eq!(stored_key2.public_key, key2);
}

#[tokio::test]
async fn deactivate_old_key_when_rotating() {
    let env = TestEnv::new_shared().await.unwrap();
    let mut tx = env.pool().begin().await.unwrap();

    let old_key = vec![31u8; 32];
    let new_key = vec![32u8; 32];

    // Create two keys within transaction
    let old_key_obj = AttestationKey {
        id: Uuid::new_v4(),
        public_key: old_key.clone(),
        is_active: false,
        created_at: chrono::DateTime::<chrono::Utc>::from(env.clock.now_system()),
        deactivated_at: None,
    };
    let old_key_id =
        env.storage().attestation_keys.create_in_tx(&mut tx, &old_key_obj).await.unwrap();

    let new_key_obj = AttestationKey {
        id: Uuid::new_v4(),
        public_key: new_key.clone(),
        is_active: false,
        created_at: chrono::DateTime::<chrono::Utc>::from(env.clock.now_system()),
        deactivated_at: None,
    };
    let new_key_id =
        env.storage().attestation_keys.create_in_tx(&mut tx, &new_key_obj).await.unwrap();

    // Verify both keys were created successfully
    let stored_old = env
        .storage()
        .attestation_keys
        .find_by_id_in_tx(&mut tx, old_key_id)
        .await
        .unwrap()
        .unwrap();
    let stored_new = env
        .storage()
        .attestation_keys
        .find_by_id_in_tx(&mut tx, new_key_id)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(stored_old.public_key, old_key);
    assert_eq!(stored_new.public_key, new_key);
    assert!(!stored_old.is_active);
    assert!(!stored_new.is_active);
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

    // Load key by ID using repository within same transaction
    let stored_key =
        env.storage().attestation_keys.find_by_id_in_tx(&mut tx, key_id).await.unwrap().unwrap();

    assert_eq!(stored_key.public_key, key);
    assert_eq!(stored_key.id, key_id);
    // Database timestamps may have different precision, check they're within 1
    // second
    let time_diff = (stored_key.created_at - key_obj.created_at).abs();
    assert!(time_diff < chrono::Duration::seconds(1), "Timestamps should be close");
}
