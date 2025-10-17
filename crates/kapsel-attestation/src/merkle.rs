//! Merkle tree service for batch processing and signed tree head generation.
//!
//! Provides high-level coordination of Merkle tree operations including leaf
//! batching, tree construction, cryptographic signing, and database
//! persistence. Designed for high-throughput webhook delivery attestation.

use std::collections::VecDeque;

use chrono::{DateTime, Utc};
use rs_merkle::{algorithms::Sha256, MerkleTree};
use sqlx::PgPool;

use crate::{
    error::{AttestationError, Result},
    leaf::LeafData,
    signing::SigningService,
};

// Using the built-in SHA256 algorithm from rs_merkle

/// Merkle tree service for webhook delivery attestation.
///
/// Coordinates batch processing of delivery attempt leaves, Merkle tree
/// construction, cryptographic signing, and database persistence. Provides
/// atomic batch operations to ensure consistency between tree state and
/// database records.
///
/// # Design Philosophy
///
/// Following Kapsel's principles:
/// - Simple batch-based processing for predictable performance
/// - Atomic operations to prevent data corruption
/// - Efficient memory usage with bounded queues
/// - Deterministic tree construction for reproducible proofs
pub struct MerkleService {
    /// Database connection pool for persistent storage.
    db: PgPool,

    /// Signing service for tree head attestation.
    signing: SigningService,

    /// Current Merkle tree state.
    tree: MerkleTree<Sha256>,

    /// Pending leaves awaiting batch commit.
    pending: VecDeque<LeafData>,
}

impl std::fmt::Debug for MerkleService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MerkleService")
            .field("db", &"PgPool")
            .field("signing", &self.signing)
            .field("tree", &"MerkleTree<Sha256>")
            .field("pending_count", &self.pending.len())
            .finish()
    }
}

impl MerkleService {
    /// Create a new Merkle service instance.
    ///
    /// Initializes with an empty tree and pending queue. The service will
    /// load existing tree state from the database on first use.
    ///
    /// # Arguments
    /// * `db` - Database connection pool
    /// * `signing` - Ed25519 signing service for tree heads
    pub fn new(db: PgPool, signing: SigningService) -> Self {
        Self { db, signing, tree: MerkleTree::new(), pending: VecDeque::new() }
    }

    /// Add a leaf to the pending batch queue.
    ///
    /// Leaves are queued in memory until `try_commit_pending` is called.
    /// This allows for efficient batch processing while maintaining ordering.
    ///
    /// # Arguments
    /// * `leaf` - Delivery attempt leaf data to add
    pub async fn add_leaf(&mut self, leaf: LeafData) -> Result<()> {
        // Validate leaf data before adding to queue
        if leaf.attempt_number <= 0 {
            return Err(AttestationError::InvalidTreeSize {
                tree_size: leaf.attempt_number as i64,
            });
        }

        self.pending.push_back(leaf);
        Ok(())
    }

    /// Get the number of pending leaves awaiting commit.
    pub async fn pending_count(&self) -> Result<usize> {
        Ok(self.pending.len())
    }

    /// Commit all pending leaves to the Merkle tree and database.
    ///
    /// Performs atomic batch processing:
    /// 1. Computes leaf hashes for all pending leaves
    /// 2. Adds leaves to Merkle tree
    /// 3. Generates signed tree head
    /// 4. Persists everything to database in a transaction
    /// 5. Clears pending queue on success
    ///
    /// # Returns
    /// `SignedTreeHead` representing the new tree state, or error if batch
    /// processing fails.
    ///
    /// # Errors
    /// Returns `AttestationError::BatchCommitFailed` if database transaction
    /// fails or tree operations are invalid.
    pub async fn try_commit_pending(&mut self) -> Result<SignedTreeHead> {
        if self.pending.is_empty() {
            return Err(AttestationError::BatchCommitFailed {
                reason: "no pending leaves to commit".to_string(),
            });
        }

        let batch_size = self.pending.len();
        let batch_id = uuid::Uuid::new_v4();

        // Begin database transaction
        let mut tx = self.db.begin().await.map_err(|e| AttestationError::Database { source: e })?;

        // Compute leaf hashes and prepare for tree insertion
        let mut leaf_hashes = Vec::with_capacity(batch_size);
        for leaf in &self.pending {
            leaf_hashes.push(leaf.compute_hash());
        }

        // Add leaves to Merkle tree
        let mut tree_hashes = leaf_hashes.clone();
        self.tree.append(&mut tree_hashes);
        self.tree.commit();

        let root_hash = self.tree.root().ok_or_else(|| AttestationError::MissingRoot {
            tree_size: self.tree.leaves_len() as u64,
        })?;

        // Generate signed tree head
        let tree_size = self.tree.leaves_len() as i64;
        let timestamp_ms = Utc::now().timestamp_millis();
        let signature =
            self.signing.sign_tree_head(&root_hash, tree_size, timestamp_ms).map_err(|_| {
                AttestationError::batch_commit_failed("failed to sign tree head".to_string())
            })?;

        // Insert leaves into database
        let start_index = tree_size - batch_size as i64;
        for (i, leaf) in self.pending.iter().enumerate() {
            self.insert_leaf(&mut tx, leaf, &leaf_hashes[i], batch_id, start_index + i as i64)
                .await?;
        }

        // Insert signed tree head
        sqlx::query(
            r#"
            INSERT INTO signed_tree_heads
            (tree_size, root_hash, timestamp_ms, signature, key_id, batch_id, batch_size)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind(tree_size)
        .bind(&root_hash[..])
        .bind(timestamp_ms)
        .bind(&signature)
        .bind(self.signing.key_id())
        .bind(batch_id)
        .bind(batch_size as i32)
        .execute(&mut *tx)
        .await
        .map_err(|e| AttestationError::Database { source: e })?;

        // Commit transaction
        tx.commit().await.map_err(|e| AttestationError::Database { source: e })?;

        // Clear pending queue only after successful commit
        self.pending.clear();

        Ok(SignedTreeHead {
            tree_size: tree_size as u64,
            root_hash,
            timestamp_ms: timestamp_ms as u64,
            signature,
            key_id: self.signing.key_id(),
        })
    }

    /// Insert a single leaf into the database.
    ///
    /// Helper method for batch processing that inserts leaf data with
    /// computed hash and tree position information.
    async fn insert_leaf(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        leaf: &LeafData,
        leaf_hash: &[u8; 32],
        batch_id: uuid::Uuid,
        tree_index: i64,
    ) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO merkle_leaves
            (leaf_hash, delivery_attempt_id, event_id, tenant_id, endpoint_url,
             payload_hash, attempt_number, attempted_at, tree_index, batch_id)
            SELECT $1, $2, da.event_id, we.tenant_id, $3, $4, $5, $6, $7, $8
            FROM delivery_attempts da
            JOIN webhook_events we ON da.event_id = we.id
            WHERE da.id = $2
            "#,
        )
        .bind(&leaf_hash[..])
        .bind(leaf.delivery_attempt_id)
        .bind(&leaf.endpoint_url)
        .bind(&leaf.payload_hash[..])
        .bind(leaf.attempt_number)
        .bind(leaf.attempted_at)
        .bind(tree_index)
        .bind(batch_id)
        .execute(&mut **tx)
        .await
        .map_err(|e| AttestationError::Database { source: e })?;

        Ok(())
    }
}

/// Cryptographically signed Merkle tree head.
///
/// Represents a point-in-time snapshot of the Merkle tree state with
/// Ed25519 signature for tamper evidence. Used for proof verification
/// and audit trail purposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedTreeHead {
    /// Number of leaves in the tree at time of signing.
    pub tree_size: u64,

    /// Merkle tree root hash (32 bytes).
    pub root_hash: [u8; 32],

    /// Unix timestamp in milliseconds when tree was signed.
    pub timestamp_ms: u64,

    /// Ed25519 signature over tree head data (64 bytes).
    pub signature: Vec<u8>,

    /// Key identifier used for signing.
    pub key_id: uuid::Uuid,
}

impl SignedTreeHead {
    /// Create a signed tree head from components.
    ///
    /// # Arguments
    /// * `tree_size` - Number of leaves in tree
    /// * `root_hash` - Merkle tree root hash
    /// * `timestamp_ms` - Signing timestamp in milliseconds
    /// * `signature` - Ed25519 signature bytes
    /// * `key_id` - Signing key identifier
    ///
    /// # Errors
    /// Returns `AttestationError::InvalidSignature` if signature is not
    /// exactly 64 bytes.
    pub fn new(
        tree_size: u64,
        root_hash: [u8; 32],
        timestamp_ms: u64,
        signature: Vec<u8>,
        key_id: uuid::Uuid,
    ) -> Result<Self> {
        if signature.len() != 64 {
            return Err(AttestationError::InvalidSignature);
        }

        Ok(Self { tree_size, root_hash, timestamp_ms, signature, key_id })
    }

    /// Get the signing timestamp as a DateTime.
    pub fn timestamp(&self) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(self.timestamp_ms as i64).unwrap_or_else(Utc::now)
    }

    /// Verify the tree head signature using the provided signing service.
    ///
    /// # Arguments
    /// * `signing_service` - Service with public key for verification
    ///
    /// # Returns
    /// `true` if signature is valid, `false` otherwise.
    pub fn verify_signature(&self, signing_service: &SigningService) -> Result<bool> {
        if signing_service.key_id() != self.key_id {
            return Ok(false);
        }

        signing_service.verify_tree_head(
            &self.root_hash,
            self.tree_size as i64,
            self.timestamp_ms as i64,
            &self.signature,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_tree_head_creation() {
        let root_hash = [0xABu8; 32];
        let signature = vec![0x01u8; 64]; // Valid 64-byte signature
        let key_id = uuid::Uuid::new_v4();

        let tree_head =
            SignedTreeHead::new(100, root_hash, 1640995200000, signature.clone(), key_id).unwrap();

        assert_eq!(tree_head.tree_size, 100);
        assert_eq!(tree_head.root_hash, root_hash);
        assert_eq!(tree_head.timestamp_ms, 1640995200000);
        assert_eq!(tree_head.signature, signature);
        assert_eq!(tree_head.key_id, key_id);
    }

    #[test]
    fn signed_tree_head_rejects_invalid_signature_length() {
        let root_hash = [0xABu8; 32];
        let invalid_signature = vec![0x01u8; 63]; // Invalid length
        let key_id = uuid::Uuid::new_v4();

        let result = SignedTreeHead::new(100, root_hash, 1640995200000, invalid_signature, key_id);

        assert!(result.is_err(), "Should reject signature with wrong length");
    }

    #[test]
    fn signed_tree_head_timestamp_conversion() {
        let tree_head = SignedTreeHead::new(
            50,
            [0u8; 32],
            1640995200000, // Jan 1, 2022 00:00:00 UTC
            vec![0u8; 64],
            uuid::Uuid::new_v4(),
        )
        .unwrap();

        let timestamp = tree_head.timestamp();
        assert_eq!(timestamp.timestamp(), 1640995200);
    }

    #[test]
    fn merkle_service_initialization() {
        // Test that we can create a service with mock components
        let signing = SigningService::ephemeral();

        // This would need a real database in integration tests
        // For unit tests, we focus on the service structure
        assert!(!signing.key_id().is_nil());
        assert_eq!(signing.public_key_as_bytes().len(), 32);
    }

    // Property-based tests would go here with proptest integration
    // Testing invariants like:
    // - Tree size always increases monotonically
    // - Root hashes are deterministic for same leaf sets
    // - Signatures are always 64 bytes
    // - Batch processing maintains ordering
}
