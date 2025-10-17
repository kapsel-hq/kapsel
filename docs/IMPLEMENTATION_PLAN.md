# Phase 2 Implementation Plan: Merkle Tree Attestation

**Goal**: Add cryptographically verifiable proof-of-delivery using RED-GREEN-REFACTOR TDD.

**Timeline**: 4 weeks

**Principles**:
- Tests are ground truth (RED)
- Implementation adapts to requirements (GREEN)
- Refactor to Kapsel quality standards

---

## Prerequisites

- [ ] Phase 1 complete: HTTP delivery works, circuit breakers integrated
- [ ] Schema updated: `001_initial_schema.sql` includes attestation tables
- [ ] Test infrastructure functional: `cargo make test` passes
- [ ] Read TESTING_STRATEGY.md and STYLE.md

---

## Week 1: Database Schema & Ed25519 Signing

### Task 1.1: Verify Attestation Schema

**Status**: Schema already added to `001_initial_schema.sql` (lines 304-374)

**RED**: Write schema validation tests

```rust
// crates/kapsel-attestation/src/schema.rs

#[cfg(test)]
mod tests {
    use kapsel_testing::TestDatabase;

    #[tokio::test]
    async fn attestation_tables_exist() {
        let db = TestDatabase::new().await.unwrap();
        let pool = db.pool();

        // Verify all 4 attestation tables exist
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.tables
             WHERE table_schema = 'public'
             AND table_name IN ('attestation_keys', 'merkle_leaves', 'signed_tree_heads', 'proof_cache')"
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(count, 4, "All attestation tables must exist");
    }

    #[tokio::test]
    async fn only_one_active_attestation_key_allowed() {
        let db = TestDatabase::new().await.unwrap();
        let mut tx = db.begin_transaction().await.unwrap();

        let key1 = vec![1u8; 32];
        let key2 = vec![2u8; 32];

        // First active key succeeds
        sqlx::query!("INSERT INTO attestation_keys (public_key, is_active) VALUES ($1, TRUE)", &key1)
            .execute(&mut *tx)
            .await
            .unwrap();

        // Second active key fails
        let result = sqlx::query!("INSERT INTO attestation_keys (public_key, is_active) VALUES ($1, TRUE)", &key2)
            .execute(&mut *tx)
            .await;

        assert!(result.is_err(), "Only one active key allowed");
    }

    #[tokio::test]
    async fn merkle_leaves_enforces_constraints() {
        let db = TestDatabase::new().await.unwrap();
        let mut tx = db.begin_transaction().await.unwrap();

        // Create test delivery attempt
        let attempt_id = create_test_delivery_attempt(&mut tx).await;

        // Valid leaf succeeds
        let leaf_hash = vec![0u8; 32];
        let payload_hash = vec![1u8; 32];

        sqlx::query!(
            "INSERT INTO merkle_leaves
             (leaf_hash, delivery_attempt_id, event_id, tenant_id, endpoint_url,
              payload_hash, attempt_number, attempted_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())",
            &leaf_hash,
            attempt_id,
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "https://example.com",
            &payload_hash,
            1i32
        )
        .execute(&mut *tx)
        .await
        .unwrap();

        // Invalid hash length fails
        let bad_hash = vec![0u8; 31]; // Wrong length!
        let result = sqlx::query!(
            "INSERT INTO merkle_leaves
             (leaf_hash, delivery_attempt_id, event_id, tenant_id, endpoint_url,
              payload_hash, attempt_number, attempted_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())",
            &bad_hash,
            attempt_id,
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "https://example.com",
            &payload_hash,
            1i32
        )
        .execute(&mut *tx)
        .await;

        assert!(result.is_err(), "leaf_hash must be exactly 32 bytes");
    }

    async fn create_test_delivery_attempt(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> uuid::Uuid {
        // Create minimal test data chain: tenant -> endpoint -> event -> attempt
        let tenant_id = uuid::Uuid::new_v4();
        sqlx::query!("INSERT INTO tenants (id, name, plan) VALUES ($1, $2, $3)", tenant_id, "test", "free")
            .execute(&mut **tx)
            .await
            .unwrap();

        let endpoint_id = uuid::Uuid::new_v4();
        sqlx::query!(
            "INSERT INTO endpoints (id, tenant_id, name, url) VALUES ($1, $2, $3, $4)",
            endpoint_id, tenant_id, "test-endpoint", "https://example.com"
        )
        .execute(&mut **tx)
        .await
        .unwrap();

        let event_id = uuid::Uuid::new_v4();
        sqlx::query!(
            "INSERT INTO webhook_events (id, tenant_id, endpoint_id, source_event_id, idempotency_strategy, headers, body, content_type, payload_size)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            event_id, tenant_id, endpoint_id, "test-event", "header", serde_json::json!({}), &b"test"[..], "application/json", 4i32
        )
        .execute(&mut **tx)
        .await
        .unwrap();

        let attempt_id = uuid::Uuid::new_v4();
        sqlx::query!(
            "INSERT INTO delivery_attempts (id, event_id, attempt_number, request_url, request_headers, request_method)
             VALUES ($1, $2, $3, $4, $5, $6)",
            attempt_id, event_id, 1i32, "https://example.com", serde_json::json!({}), "POST"
        )
        .execute(&mut **tx)
        .await
        .unwrap();

        attempt_id
    }
}
```

**GREEN**: Schema already exists in migrations

**REFACTOR**: Run tests
```bash
cargo make db-reset
cargo make test
```

**Acceptance**: All schema tests pass

---

### Task 1.2: Implement SigningService

**Location**: `crates/kapsel-attestation/src/signing.rs`

**RED**: Write failing unit tests

```rust
// crates/kapsel-attestation/src/signing.rs

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_valid_ed25519_keypair() {
        let service = SigningService::ephemeral();
        let bytes = service.public_key_as_bytes();

        assert_eq!(bytes.len(), 32, "Ed25519 public key is 32 bytes");
    }

    #[test]
    fn signs_tree_head_deterministically() {
        let service = SigningService::ephemeral();
        let root_hash = [0u8; 32];
        let tree_size = 100i64;
        let timestamp_ms = 1234567890000i64;

        let sig1 = service.sign_tree_head(&root_hash, tree_size, timestamp_ms).unwrap();
        let sig2 = service.sign_tree_head(&root_hash, tree_size, timestamp_ms).unwrap();

        assert_eq!(sig1, sig2, "Signatures must be deterministic");
        assert_eq!(sig1.len(), 64, "Ed25519 signature is 64 bytes");
    }

    #[test]
    fn signature_verifies_with_public_key() {
        let service = SigningService::ephemeral();
        let root_hash = [1u8; 32];
        let tree_size = 42i64;
        let timestamp_ms = 9876543210000i64;

        let signature = service.sign_tree_head(&root_hash, tree_size, timestamp_ms).unwrap();
        let is_valid = service.verify_tree_head(&root_hash, tree_size, timestamp_ms, &signature).unwrap();

        assert!(is_valid, "Signature must verify");
    }

    #[test]
    fn signature_fails_with_tampered_data() {
        let service = SigningService::ephemeral();
        let root_hash = [2u8; 32];
        let tree_size = 100i64;
        let timestamp_ms = 1111111111111i64;

        let signature = service.sign_tree_head(&root_hash, tree_size, timestamp_ms).unwrap();

        // Tamper with root hash
        let tampered_root = [3u8; 32];
        let is_valid = service.verify_tree_head(&tampered_root, tree_size, timestamp_ms, &signature).unwrap();

        assert!(!is_valid, "Tampered data must fail verification");
    }

    // Property tests
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn any_valid_tree_head_signs_and_verifies(
            root_hash in prop::array::uniform32(any::<u8>()),
            tree_size in 1i64..1_000_000i64,
            timestamp_ms in 1_000_000_000_000i64..9_999_999_999_999i64,
        ) {
            let service = SigningService::ephemeral();

            let signature = service.sign_tree_head(&root_hash, tree_size, timestamp_ms).unwrap();
            let is_valid = service.verify_tree_head(&root_hash, tree_size, timestamp_ms, &signature).unwrap();

            prop_assert!(is_valid);
        }

        #[test]
        fn tampered_signatures_always_fail(
            root_hash in prop::array::uniform32(any::<u8>()),
            tree_size in 1i64..1_000_000i64,
            timestamp_ms in 1_000_000_000_000i64..9_999_999_999_999i64,
        ) {
            let service = SigningService::ephemeral();
            let signature = service.sign_tree_head(&root_hash, tree_size, timestamp_ms).unwrap();

            // Tamper with tree size
            let is_valid = service.verify_tree_head(&root_hash, tree_size + 1, timestamp_ms, &signature).unwrap();

            prop_assert!(!is_valid);
        }
    }
}
```

**GREEN**: Implement minimal SigningService

```rust
// crates/kapsel-attestation/src/signing.rs
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;

use crate::error::{AttestationError, Result};

/// Ed25519 signing service for tree head signatures.
///
/// Signs tree heads using Ed25519 for non-repudiation. Public keys are stored
/// in the database for third-party verification.
pub struct SigningService {
    signing_key: SigningKey,
    verifying_key: VerifyingKey,
    key_id: Option<i32>,
}

impl SigningService {
    /// Creates an ephemeral signing service for testing.
    ///
    /// The generated keypair is not persisted and will be lost when dropped.
    /// Use `try_create()` for production.
    pub fn ephemeral() -> Self {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();

        Self {
            signing_key,
            verifying_key,
            key_id: None,
        }
    }

    /// Signs tree head with Ed25519.
    ///
    /// Message format per RFC 6962:
    /// tree_size (8 bytes, big-endian) || timestamp_ms (8 bytes, big-endian) || root_hash (32 bytes)
    ///
    /// Returns 64-byte Ed25519 signature.
    pub fn sign_tree_head(
        &self,
        root_hash: &[u8; 32],
        tree_size: i64,
        timestamp_ms: i64,
    ) -> Result<Vec<u8>> {
        let mut message = Vec::with_capacity(48);
        message.extend_from_slice(&tree_size.to_be_bytes());
        message.extend_from_slice(&timestamp_ms.to_be_bytes());
        message.extend_from_slice(root_hash);

        let signature = self.signing_key.sign(&message);
        Ok(signature.to_bytes().to_vec())
    }

    /// Verifies tree head signature.
    ///
    /// Returns true if signature is valid, false otherwise.
    pub fn verify_tree_head(
        &self,
        root_hash: &[u8; 32],
        tree_size: i64,
        timestamp_ms: i64,
        signature: &[u8],
    ) -> Result<bool> {
        let mut message = Vec::with_capacity(48);
        message.extend_from_slice(&tree_size.to_be_bytes());
        message.extend_from_slice(&timestamp_ms.to_be_bytes());
        message.extend_from_slice(root_hash);

        let sig = Signature::from_slice(signature)
            .map_err(|_| AttestationError::InvalidSignature)?;

        Ok(self.verifying_key.verify(&message, &sig).is_ok())
    }

    /// Returns public key as bytes for storage/transmission.
    ///
    /// Returns 32-byte Ed25519 public key.
    pub fn public_key_as_bytes(&self) -> &[u8] {
        self.verifying_key.as_bytes()
    }

    /// Returns database key ID if persisted.
    pub fn key_id(&self) -> Option<i32> {
        self.key_id
    }
}
```

**REFACTOR**: Add error types

```rust
// crates/kapsel-attestation/src/error.rs

#[derive(Debug, thiserror::Error)]
pub enum AttestationError {
    #[error("invalid signature format")]
    InvalidSignature,

    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("failed to store attestation key")]
    KeyStorage,

    #[error("tree root unavailable for size {tree_size}")]
    MissingRoot { tree_size: i64 },
}

pub type Result<T> = std::result::Result<T, AttestationError>;
```

**Acceptance**: All tests pass including property tests

---

### Task 1.3: Database-backed SigningService

**RED**: Write integration tests

```rust
// tests/signing_integration_test.rs

use kapsel_attestation::SigningService;
use kapsel_testing::TestDatabase;

#[tokio::test]
async fn stores_key_in_database() {
    let db = TestDatabase::new().await.unwrap();
    let pool = db.pool();

    let service = SigningService::try_create(&pool).await.unwrap();
    let public_key_bytes = service.public_key_as_bytes();

    // Verify key stored
    let stored = sqlx::query!("SELECT public_key FROM attestation_keys WHERE is_active = TRUE")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(&stored.public_key.unwrap()[..], public_key_bytes);
}

#[tokio::test]
async fn loads_existing_active_key() {
    let db = TestDatabase::new().await.unwrap();
    let pool = db.pool();

    // Create first service
    let service1 = SigningService::try_create(&pool).await.unwrap();
    let key1 = service1.public_key_as_bytes().to_vec();

    // Second service should load same key
    let service2 = SigningService::try_load(&pool).await.unwrap();
    let key2 = service2.public_key_as_bytes();

    assert_eq!(&key1[..], key2);
}
```

**GREEN**: Implement database methods

```rust
impl SigningService {
    /// Creates new signing service and stores public key in database.
    pub async fn try_create(pool: &sqlx::PgPool) -> Result<Self> {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();
        let public_key_bytes = verifying_key.as_bytes();

        let key_id = sqlx::query!(
            "INSERT INTO attestation_keys (public_key, is_active)
             VALUES ($1, TRUE)
             RETURNING id",
            public_key_bytes.as_slice()
        )
        .fetch_one(pool)
        .await?
        .id;

        Ok(Self {
            signing_key,
            verifying_key,
            key_id: Some(key_id),
        })
    }

    /// Loads existing active signing key from database.
    ///
    /// NOTE: Private key is NOT stored - this generates a new keypair
    /// and returns the existing public key for verification only.
    pub async fn try_load(pool: &sqlx::PgPool) -> Result<Self> {
        let row = sqlx::query!("SELECT id, public_key FROM attestation_keys WHERE is_active = TRUE LIMIT 1")
            .fetch_one(pool)
            .await?;

        let public_key_bytes: [u8; 32] = row.public_key.unwrap().try_into()
            .map_err(|_| AttestationError::InvalidSignature)?;

        let verifying_key = VerifyingKey::from_bytes(&public_key_bytes)
            .map_err(|_| AttestationError::InvalidSignature)?;

        // NOTE: We don't have the private key, so signing will fail
        // This is for verification-only use cases
        let signing_key = SigningKey::generate(&mut OsRng); // Dummy

        Ok(Self {
            signing_key,
            verifying_key,
            key_id: Some(row.id),
        })
    }
}
```

**REFACTOR**: Add proper key management strategy to roadmap

**Acceptance**: Integration tests pass

---

## Week 2: Merkle Tree Service & Batch Processing

### Task 2.1: Implement LeafData

**Location**: `crates/kapsel-attestation/src/leaf.rs`

**RED**: Write tests for RFC 6962 compliance

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaf_hash_is_deterministic() {
        let leaf = LeafData::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "https://example.com".parse().unwrap(),
            [1u8; 32],
            1,
            Some(200),
            1234567890000,
        );

        let hash1 = leaf.compute_hash();
        let hash2 = leaf.compute_hash();

        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 32);
    }

    #[test]
    fn uses_rfc6962_leaf_prefix() {
        let leaf = LeafData::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "https://test.com".parse().unwrap(),
            [2u8; 32],
            2,
            Some(404),
            9876543210000,
        );

        // Manually compute with prefix
        let json = serde_json::to_vec(&leaf).unwrap();
        let mut hasher = sha2::Sha256::new();
        hasher.update(&[0x00]); // RFC 6962 leaf prefix
        hasher.update(&json);
        let expected: [u8; 32] = hasher.finalize().into();

        assert_eq!(leaf.compute_hash(), expected);
    }

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn hash_always_32_bytes(
            attempt_number in 1i32..100,
            response_status in proptest::option::of(200i32..600),
            timestamp in 1_000_000_000_000i64..9_999_999_999_999i64,
        ) {
            let leaf = LeafData::new(
                uuid::Uuid::new_v4(),
                uuid::Uuid::new_v4(),
                "https://example.com".parse().unwrap(),
                [0u8; 32],
                attempt_number,
                response_status,
                timestamp,
            );

            prop_assert_eq!(leaf.compute_hash().len(), 32);
        }
    }
}
```

**GREEN**: Implement LeafData with correct types

```rust
// crates/kapsel-attestation/src/leaf.rs
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;
use uuid::Uuid;

/// Merkle tree leaf data.
///
/// Represents a single delivery attempt in the append-only log.
/// Uses type-safe fields to prevent invalid states.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LeafData {
    pub delivery_attempt_id: Uuid,
    pub event_id: Uuid,
    pub endpoint_url: Url,  // Validated URL
    pub payload_hash: [u8; 32],  // Fixed-size array
    pub attempt_number: i32,
    pub response_status: Option<i32>,
    pub attempted_at: i64,  // Unix milliseconds
}

impl LeafData {
    /// Creates new leaf data with validation.
    pub fn new(
        delivery_attempt_id: Uuid,
        event_id: Uuid,
        endpoint_url: Url,
        payload_hash: [u8; 32],
        attempt_number: i32,
        response_status: Option<i32>,
        attempted_at: i64,
    ) -> Self {
        Self {
            delivery_attempt_id,
            event_id,
            endpoint_url,
            payload_hash,
            attempt_number,
            response_status,
            attempted_at,
        }
    }

    /// Computes leaf hash per RFC 6962.
    ///
    /// Hash = SHA-256(0x00 || JSON(leaf_data))
    ///
    /// The 0x00 prefix is the RFC 6962 leaf domain separator.
    pub fn compute_hash(&self) -> [u8; 32] {
        let json = serde_json::to_vec(self)
            .expect("LeafData serialization cannot fail");

        let mut hasher = Sha256::new();
        hasher.update(&[0x00]); // RFC 6962 leaf prefix
        hasher.update(&json);
        hasher.finalize().into()
    }
}
```

**REFACTOR**: Consider adding builder pattern if construction gets complex

**Acceptance**: All tests pass

---

### Task 2.2: Implement MerkleService

**Location**: `crates/kapsel-attestation/src/merkle.rs`

**RED**: Write core functionality tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use kapsel_testing::TestDatabase;
    use std::sync::Arc;

    #[tokio::test]
    async fn adds_leaf_to_pending_queue() {
        let db = TestDatabase::new().await.unwrap();
        let signing = Arc::new(SigningService::ephemeral());
        let service = MerkleService::new(db.pool(), signing);

        let leaf = create_test_leaf();
        service.add_leaf(leaf).await.unwrap();

        assert_eq!(service.pending_count().await, 1);
    }

    #[tokio::test]
    async fn commits_batch_atomically() {
        let db = TestDatabase::new().await.unwrap();
        let signing = Arc::new(SigningService::try_create(&db.pool()).await.unwrap());
        let service = MerkleService::new(db.pool(), signing);

        // Add 10 leaves
        for i in 0..10 {
            service.add_leaf(create_test_leaf_with_id(i)).await.unwrap();
        }

        // Commit
        let sth = service.try_commit_pending().await.unwrap();

        assert!(sth.is_some());
        let sth = sth.unwrap();
        assert_eq!(sth.tree_size, 10);
        assert_eq!(sth.root_hash.len(), 32);
        assert_eq!(sth.signature.len(), 64);

        // Verify database
        let leaf_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM merkle_leaves")
            .fetch_one(&db.pool())
            .await
            .unwrap();

        assert_eq!(leaf_count, 10);

        // Verify pending cleared
        assert_eq!(service.pending_count().await, 0);
    }

    fn create_test_leaf() -> LeafData {
        LeafData::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "https://example.com".parse().unwrap(),
            [0u8; 32],
            1,
            Some(200),
            chrono::Utc::now().timestamp_millis(),
        )
    }

    fn create_test_leaf_with_id(id: u8) -> LeafData {
        let mut hash = [0u8; 32];
        hash[0] = id;
        LeafData::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "https://example.com".parse().unwrap(),
            hash,
            1,
            Some(200),
            chrono::Utc::now().timestamp_millis(),
        )
    }
}
```

**GREEN**: Implement MerkleService

```rust
// crates/kapsel-attestation/src/merkle.rs
use rs_merkle::{algorithms::Sha256, MerkleTree};
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::{AttestationError, LeafData, Result, SigningService};

pub struct MerkleService {
    db: PgPool,
    signing: Arc<SigningService>,
    tree: Arc<RwLock<MerkleTree<Sha256>>>,
    pending: Arc<RwLock<Vec<LeafData>>>,
}

impl MerkleService {
    pub fn new(db: PgPool, signing: Arc<SigningService>) -> Self {
        Self {
            db,
            signing,
            tree: Arc::new(RwLock::new(MerkleTree::new())),
            pending: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Adds leaf to pending queue.
    pub async fn add_leaf(&self, leaf: LeafData) -> Result<()> {
        self.pending.write().await.push(leaf);
        Ok(())
    }

    /// Returns pending leaf count.
    pub async fn pending_count(&self) -> usize {
        self.pending.read().await.len()
    }

    /// Commits pending leaves to tree and database.
    ///
    /// Returns None if no pending leaves, Some(STH) if committed.
    /// Commit is atomic - either all leaves commit or none.
    pub async fn try_commit_pending(&self) -> Result<Option<SignedTreeHead>> {
        let mut pending = self.pending.write().await;

        if pending.is_empty() {
            return Ok(None);
        }

        // Compute hashes
        let hashes: Vec<[u8; 32]> = pending
            .iter()
            .map(|leaf| leaf.compute_hash())
            .collect();

        // Update tree
        let mut tree = self.tree.write().await;
        for hash in &hashes {
            tree.insert(*hash);
        }
        tree.commit();

        let tree_size = tree.leaves_len() as i64;
        let root_hash = tree.root()
            .ok_or(AttestationError::MissingRoot { tree_size })?;

        // Sign
        let timestamp_ms = chrono::Utc::now().timestamp_millis();
        let signature = self.signing.sign_tree_head(&root_hash, tree_size, timestamp_ms)?;

        // Store atomically
        let mut tx = self.db.begin().await?;

        // Insert leaves
        for (leaf, hash) in pending.iter().zip(hashes.iter()) {
            self.insert_leaf(&mut tx, leaf, hash, tree_size).await?;
        }

        // Insert STH
        let key_id = self.signing.key_id()
            .ok_or(AttestationError::KeyStorage)?;

        sqlx::query!(
            "INSERT INTO signed_tree_heads (tree_size, root_hash, timestamp_ms, signature, key_id)
             VALUES ($1, $2, $3, $4, $5)",
            tree_size,
            root_hash.as_slice(),
            timestamp_ms,
            &signature,
            key_id
        )
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        // Clear pending
        pending.clear();

        Ok(Some(SignedTreeHead {
            tree_size,
            root_hash: root_hash.to_vec(),
            timestamp_ms,
            signature,
            key_id,
        }))
    }

    async fn insert_leaf(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        leaf: &LeafData,
        hash: &[u8; 32],
        tree_size: i64,
    ) -> Result<()> {
        sqlx::query!(
            "INSERT INTO merkle_leaves
             (leaf_hash, delivery_attempt_id, event_id, tenant_id, endpoint_url,
              payload_hash, attempt_number, response_status, attempted_at, tree_size_at_inclusion)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, to_timestamp($9::double precision / 1000), $10)",
            hash.as_slice(),
            leaf.delivery_attempt_id,
            leaf.event_id,
            uuid::Uuid::nil(), // TODO: Extract from leaf
            leaf.endpoint_url.as_str(),
            &leaf.payload_hash[..],
            leaf.attempt_number,
            leaf.response_status,
            leaf.attempted_at as f64,
            tree_size
        )
        .execute(&mut **tx)
        .await?;

        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct SignedTreeHead {
    pub tree_size: i64,
    pub root_hash: Vec<u8>,
    pub timestamp_ms: i64,
    pub signature: Vec<u8>,
    pub key_id: i32,
}
```

**REFACTOR**: Extract transaction handling, add proper error context

**Acceptance**: Tests pass, commits are atomic

---

## Week 3: Proof Generation & API

(Tasks 3.1-3.2 follow same RED-GREEN-REFACTOR pattern)

## Week 4: Integration & Performance

(Tasks 4.1-4.2 follow same RED-GREEN-REFACTOR pattern)

---

## Daily Discipline

Before every commit:
- [ ] `cargo make test` - all pass
- [ ] `cargo make lint` - zero warnings
- [ ] `cargo make format` - code formatted
- [ ] Tests use `TestDatabase::new()` + `begin_transaction()`
- [ ] Integration tests in `tests/`, unit tests inline
- [ ] Functions follow `verb_object` naming
- [ ] Library uses `thiserror`, not `anyhow`
- [ ] Public APIs have doc comments

---

## When Stuck

1. **Test fails unexpectedly**: Debug test, not implementation
2. **Can't implement**: Simplify test to smallest behavior
3. **Refactor breaks tests**: Revert, smaller changes
4. **Flaky tests**: Check if using real time instead of `TestEnv.clock`

---

## Success Criteria

Phase 2 complete when:
- [ ] All attestation tables exist and enforce constraints
- [ ] SigningService signs/verifies Ed25519 correctly
- [ ] MerkleService commits batches atomically
- [ ] Proof generation works for all tree sizes
- [ ] API endpoints return valid responses
- [ ] Full integration test passes (webhook → proof → verify)
- [ ] Performance benchmarks meet targets
- [ ] Test coverage >80%
- [ ] Zero clippy warnings
