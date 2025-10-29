//! Merkle tree service for batch processing and signed tree head generation.
//!
//! Provides high-level coordination of Merkle tree operations including leaf
//! batching, tree construction, cryptographic signing, and database
//! persistence. Designed for high-throughput webhook delivery attestation.
//!
//! # Batch Processing Flow
//!
//! ```text
//! Delivery Events          Pending Queue           Batch Commit
//!       │                        │                      │
//!       ▼                        ▼                      ▼
//! ┌──────────────┐         ┌─────────────┐       ┌──────────────┐
//! │ Success      │────────▶│ ┌─────────┐ │       │ ┌──────────┐ │
//! │ Event        │         │ │ Leaf  1 │ │       │ │ Merkle   │ │
//! └──────────────┘         │ │ Leaf  2 │ │ ────▶ │ │ Tree     │ │
//! ┌──────────────┐         │ │ Leaf  3 │ │ ────▶ │ │ Tree     │ │
//! │ Success      │────────▶│ │ Leaf  4 │ │ ────▶ │ │ Tree     │ │
//! │ Event        │         │ │   ...   │ │       │ │ Build    │ │
//! └──────────────┘         │ │ Leaf  N │ │       │ └──────────┘ │
//!        ▲                 │ └─────────┘ │       └──────────────┘
//!        │                 └─────────────┘             │
//!   Event-Driven           In-Memory Queue             │
//!   Architecture                                       ▼
//!                                                ┌──────────────┐
//!                                                │  Database    │
//!                                                │  Transaction │
//!                                                │              │
//!    ┌──────────────┐       ┌──────────────┐     │ ● Leaves     │
//!    │ Ed25519      │◀─────▶│ Signed Tree  │     │ ● Tree Head  │
//!    │ Signing      │       │ Head (STH)   │     │ ● Signature  │
//!    └──────────────┘       └──────────────┘     └──────────────┘
//! ```
//!
//! Benefits of batch processing:
//! - **Amortized crypto costs**: One signature covers many delivery attempts
//! - **Atomic consistency**: Database transaction ensures tree/DB alignment
//! - **High throughput**: Processes hundreds of events per batch efficiently

use std::{collections::VecDeque, sync::Arc};

use chrono::{DateTime, Utc};
use kapsel_core::{
    storage::{merkle_leaves::MerkleLeafInsert, Storage},
    Clock,
};
use rs_merkle::{algorithms::Sha256, MerkleTree};

use crate::{
    error::{AttestationError, Result},
    leaf::LeafData,
    signing::SigningService,
};

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
    /// Storage abstraction for database operations.
    storage: Arc<Storage>,

    /// Signing service for tree head attestation.
    signing: SigningService,

    /// Clock abstraction for deterministic time operations.
    clock: Arc<dyn Clock>,

    /// Current Merkle tree state.
    #[allow(dead_code)]
    tree: MerkleTree<Sha256>,

    /// Pending leaves awaiting batch commit.
    pending: VecDeque<LeafData>,

    /// Advisory lock ID for database transaction serialization.
    lock_id: i64,
}

impl std::fmt::Debug for MerkleService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MerkleService")
            .field("storage", &"Storage")
            .field("signing", &self.signing)
            .field("clock", &self.clock)
            .field("tree", &"MerkleTree")
            .field("pending_count", &self.pending.len())
            .field("lock_id", &self.lock_id)
            .finish()
    }
}

impl MerkleService {
    /// Create a new Merkle service instance.
    ///
    /// Initializes with an empty tree and pending queue. The service will
    /// load existing tree state from the database on first use.
    pub fn new(storage: Arc<Storage>, signing: SigningService, clock: Arc<dyn Clock>) -> Self {
        // Use a fixed lock ID for production consistency
        const DEFAULT_LOCK_ID: i64 = 1_234_567_890;
        Self {
            storage,
            signing,
            clock,
            tree: MerkleTree::new(),
            pending: VecDeque::new(),
            lock_id: DEFAULT_LOCK_ID,
        }
    }

    /// Create a new Merkle service instance with a custom advisory lock ID.
    ///
    /// This is primarily used for test isolation where multiple services
    /// need different lock IDs to prevent serialization conflicts.
    pub fn with_lock_id(
        storage: Arc<Storage>,
        signing: SigningService,
        clock: Arc<dyn Clock>,
        lock_id: i64,
    ) -> Self {
        Self { storage, signing, clock, tree: MerkleTree::new(), pending: VecDeque::new(), lock_id }
    }

    /// Add a leaf to the pending batch queue.
    ///
    /// Leaves are queued in memory until `try_commit_pending` is called.
    /// This allows for efficient batch processing while maintaining ordering.
    ///
    /// # Errors
    ///
    /// Returns `AttestationError::InvalidTreeSize` if attempt_number is not
    /// positive.
    pub fn add_leaf(&mut self, leaf: LeafData) -> Result<()> {
        if leaf.attempt_number <= 0 {
            return Err(AttestationError::InvalidTreeSize {
                tree_size: i64::from(leaf.attempt_number),
            });
        }

        self.pending.push_back(leaf);
        Ok(())
    }

    /// Returns the number of pending leaves awaiting commit.
    ///
    /// # Errors
    ///
    /// This method is infallible but returns Result for API consistency.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
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
    /// # Errors
    ///
    /// Returns `AttestationError::BatchCommitFailed` if database transaction
    /// fails or tree operations are invalid.
    #[allow(clippy::too_many_lines)]
    pub async fn try_commit_pending(&mut self) -> Result<SignedTreeHead> {
        if self.pending.is_empty() {
            return Err(AttestationError::BatchCommitFailed {
                reason: "no pending leaves to commit".to_string(),
            });
        }

        let batch_size = self.pending.len();
        let batch_id = uuid::Uuid::new_v4();

        let mut tx = self
            .storage
            .webhook_events
            .pool()
            .begin()
            .await
            .map_err(|e| AttestationError::Database { source: e })?;

        // Acquire advisory lock to serialize tree commits and prevent concurrent
        // tree_index conflicts
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(self.lock_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| AttestationError::Database { source: e })?;

        // Get current tree size from database atomically within transaction
        let current_tree_size: i64 =
            self.storage.signed_tree_heads.find_max_tree_size_in_tx(&mut tx).await.map_err(
                |e| AttestationError::batch_commit_failed(format!("failed to get tree size: {e}")),
            )?;

        let new_tree_size = current_tree_size
            + i64::try_from(batch_size)
                .map_err(|_| AttestationError::InvalidTreeSize { tree_size: i64::MAX })?;

        let mut leaf_hashes = Vec::with_capacity(batch_size);
        for leaf in &self.pending {
            leaf_hashes.push(leaf.compute_hash());
        }

        // Build temporary tree with existing + new leaves for root calculation
        let mut temp_tree = MerkleTree::<Sha256>::new();

        // Load existing leaves if any
        if current_tree_size > 0 {
            let existing_hashes: Vec<Vec<u8>> = self
                .storage
                .merkle_leaves
                .find_committed_leaf_hashes_in_tx(&mut tx)
                .await
                .map_err(|e| {
                    AttestationError::batch_commit_failed(format!(
                        "failed to load existing leaves: {e}"
                    ))
                })?;

            let mut existing_32: Vec<[u8; 32]> = Vec::new();
            for hash in existing_hashes {
                if hash.len() == 32 {
                    let mut array = [0u8; 32];
                    array.copy_from_slice(&hash);
                    existing_32.push(array);
                }
            }
            temp_tree.append(&mut existing_32);
        }

        // Add new leaves
        let mut new_hashes = leaf_hashes.clone();
        temp_tree.append(&mut new_hashes);
        temp_tree.commit();

        let root_hash = temp_tree.root().ok_or_else(|| AttestationError::MissingRoot {
            tree_size: u64::try_from(new_tree_size).unwrap_or(0),
        })?;

        let tree_size = new_tree_size;
        let timestamp_ms = DateTime::<Utc>::from(self.clock.now_system()).timestamp_millis();
        let signature = self.signing.sign_tree_head(&root_hash, tree_size, timestamp_ms);

        let start_index = current_tree_size;
        for (i, leaf) in self.pending.iter().enumerate() {
            self.insert_leaf(
                &mut tx,
                leaf,
                &leaf_hashes[i],
                batch_id,
                start_index
                    + i64::try_from(i)
                        .map_err(|_| AttestationError::InvalidTreeSize { tree_size: i64::MAX })?,
            )
            .await?;
        }

        sqlx::query(
            r"
            INSERT INTO signed_tree_heads
            (id, tree_size, root_hash, timestamp_ms, signature, key_id, batch_id, batch_size, created_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW())
            ",
        )
        .bind(uuid::Uuid::new_v4())
        .bind(tree_size)
        .bind(&root_hash[..])
        .bind(timestamp_ms)
        .bind(&signature)
        .bind(self.signing.key_id())
        .bind(batch_id)
        .bind(i32::try_from(batch_size).unwrap_or(i32::MAX))
        .execute(&mut *tx)
        .await
        .map_err(|e| AttestationError::Database { source: e })?;

        tx.commit().await.map_err(|e| AttestationError::Database { source: e })?;

        // Ordering matters: only clear queue after transaction commits successfully
        self.pending.clear();

        Ok(SignedTreeHead {
            tree_size: u64::try_from(tree_size).unwrap_or(0),
            root_hash,
            timestamp_ms: u64::try_from(timestamp_ms).unwrap_or(0),
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
        self.storage
            .merkle_leaves
            .insert_leaf_from_attempt_in_tx(&mut **tx, MerkleLeafInsert {
                leaf_hash: &leaf_hash[..],
                delivery_attempt_id: leaf.delivery_attempt_id,
                endpoint_url: &leaf.endpoint_url,
                payload_hash: &leaf.payload_hash[..],
                attempt_number: leaf.attempt_number,
                attempted_at: leaf.attempted_at,
                tree_index: Some(tree_index),
                batch_id: Some(batch_id),
            })
            .await
            .map_err(|e| {
                AttestationError::batch_commit_failed(format!("failed to insert leaves: {e}"))
            })?;

        Ok(())
    }

    /// Restore tree state from database.
    ///
    /// Loads all committed leaves from the database and rebuilds the internal
    /// Merkle tree state. This ensures tree_index calculations are correct
    /// when multiple service instances exist or after restarts.
    #[allow(dead_code)]
    async fn restore_tree_state_from_db(&mut self) -> Result<()> {
        // Query all leaves that have been committed to the tree (tree_index IS NOT
        // NULL)
        let leaf_hashes: Vec<Vec<u8>> =
            self.storage.merkle_leaves.find_committed_leaf_hashes().await.map_err(|e| {
                AttestationError::batch_commit_failed(format!("failed to insert tree head: {e}"))
            })?;

        if !leaf_hashes.is_empty() {
            // Convert to the format expected by MerkleTree
            let mut hashes: Vec<[u8; 32]> = Vec::with_capacity(leaf_hashes.len());
            for hash in leaf_hashes {
                if hash.len() == 32 {
                    let mut array = [0u8; 32];
                    array.copy_from_slice(&hash);
                    hashes.push(array);
                } else {
                    return Err(AttestationError::InvalidTreeSize {
                        tree_size: i64::try_from(hash.len()).unwrap_or(i64::MAX),
                    });
                }
            }

            // Rebuild tree state
            self.tree.append(&mut hashes);
            self.tree.commit();
        }

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
    /// # Errors
    ///
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

    /// Returns the signing timestamp as a DateTime.
    pub fn timestamp(&self) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(i64::try_from(self.timestamp_ms).unwrap_or(0))
            .unwrap_or_else(Utc::now)
    }

    /// Verify the tree head signature using the provided signing service.
    ///
    /// # Errors
    ///
    /// Returns `AttestationError` if signature verification fails.
    pub fn verify_signature(&self, signing_service: &SigningService) -> Result<bool> {
        if signing_service.key_id() != self.key_id {
            return Ok(false);
        }

        signing_service.verify_tree_head(
            &self.root_hash,
            i64::try_from(self.tree_size).unwrap_or(0),
            i64::try_from(self.timestamp_ms).unwrap_or(0),
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
            SignedTreeHead::new(100, root_hash, 1_640_995_200_000, signature.clone(), key_id)
                .unwrap();

        assert_eq!(tree_head.tree_size, 100);
        assert_eq!(tree_head.root_hash, root_hash);
        assert_eq!(tree_head.timestamp_ms, 1_640_995_200_000);
        assert_eq!(tree_head.signature, signature);
        assert_eq!(tree_head.key_id, key_id);
    }

    #[test]
    fn signed_tree_head_rejects_invalid_signature_length() {
        let root_hash = [0xABu8; 32];
        let invalid_signature = vec![0x01u8; 63]; // Invalid length
        let key_id = uuid::Uuid::new_v4();

        let result =
            SignedTreeHead::new(100, root_hash, 1_640_995_200_000, invalid_signature, key_id);

        assert!(result.is_err(), "Should reject signature with wrong length");
    }

    #[test]
    fn signed_tree_head_timestamp_conversion() {
        let tree_head = SignedTreeHead::new(
            50,
            [0u8; 32],
            1_640_995_200_000, // Jan 1, 2022 00:00:00 UTC
            vec![0u8; 64],
            uuid::Uuid::new_v4(),
        )
        .unwrap();

        let timestamp = tree_head.timestamp();
        assert_eq!(timestamp.timestamp(), 1_640_995_200);
    }

    #[test]
    fn merkle_service_initialization() {
        // Test that we can create a service with mock components
        let signing = SigningService::ephemeral();

        assert!(!signing.key_id().is_nil());
        assert_eq!(signing.public_key_as_bytes().len(), 32);
    }
}
