//! Repository for merkle leaf database operations.
//!
//! Provides type-safe access to merkle leaves for cryptographic verification
//! and attestation queries. Used primarily by the MerkleService and tests.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sqlx::{Executor, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::Result;

/// Parameters for inserting a merkle leaf from delivery attempt data.
#[derive(Debug)]
pub struct MerkleLeafInsert<'a> {
    /// Hash of the merkle tree leaf
    pub leaf_hash: &'a [u8],
    /// Unique identifier for the delivery attempt
    pub delivery_attempt_id: Uuid,
    /// URL of the endpoint that was called
    pub endpoint_url: &'a str,
    /// Hash of the payload that was delivered
    pub payload_hash: &'a [u8],
    /// Sequential attempt number for this delivery
    pub attempt_number: i32,
    /// Timestamp when the delivery was attempted
    pub attempted_at: DateTime<Utc>,
    /// Index position in the merkle tree (None for uncommitted leaves)
    pub tree_index: Option<i64>,
    /// Batch identifier for grouping related leaves
    pub batch_id: Option<Uuid>,
}

/// Attestation leaf information for verification queries.
///
/// Contains joined data from merkle_leaves and delivery_attempts tables
/// for comprehensive attestation verification.
#[derive(Debug, Clone)]
pub struct AttestationLeafInfo {
    /// Unique identifier for the delivery attempt
    pub delivery_attempt_id: Uuid,
    /// Target endpoint URL where the webhook was delivered
    pub endpoint_url: String,
    /// Sequential attempt number for this delivery
    pub attempt_number: i32,
    /// HTTP response status code (None if delivery failed before response)
    pub response_status: Option<i32>,
    /// SHA-256 hash of the Merkle tree leaf content
    pub leaf_hash: Vec<u8>,
}

impl sqlx::FromRow<'_, sqlx::postgres::PgRow> for AttestationLeafInfo {
    fn from_row(row: &sqlx::postgres::PgRow) -> std::result::Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            delivery_attempt_id: row.try_get("delivery_attempt_id")?,
            endpoint_url: row.try_get("endpoint_url")?,
            attempt_number: row.try_get("attempt_number")?,
            response_status: row.try_get("response_status")?,
            leaf_hash: row.try_get("leaf_hash")?,
        })
    }
}

/// Merkle leaf data structure for database operations.
#[derive(Debug, Clone)]
pub struct MerkleLeaf {
    /// 32-byte hash of the leaf content
    pub leaf_hash: Vec<u8>,
    /// ID of the delivery attempt this leaf represents
    pub delivery_attempt_id: Uuid,
    /// Event ID associated with this delivery attempt
    pub event_id: Uuid,
    /// Tenant ID for isolation
    pub tenant_id: Uuid,
    /// Target endpoint URL
    pub endpoint_url: String,
    /// Hash of the payload that was delivered
    pub payload_hash: Vec<u8>,
    /// Attempt number for this delivery
    pub attempt_number: i32,
    /// When the delivery attempt was made
    pub attempted_at: DateTime<Utc>,
    /// Position in the merkle tree (None if not yet committed)
    pub tree_index: Option<i64>,
    /// Batch ID for grouping leaves
    pub batch_id: Option<Uuid>,
}

impl sqlx::FromRow<'_, sqlx::postgres::PgRow> for MerkleLeaf {
    fn from_row(row: &sqlx::postgres::PgRow) -> std::result::Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            leaf_hash: row.try_get("leaf_hash")?,
            delivery_attempt_id: row.try_get("delivery_attempt_id")?,
            event_id: row.try_get("event_id")?,
            tenant_id: row.try_get("tenant_id")?,
            endpoint_url: row.try_get("endpoint_url")?,
            payload_hash: row.try_get("payload_hash")?,
            attempt_number: row.try_get("attempt_number")?,
            attempted_at: row.try_get("attempted_at")?,
            tree_index: row.try_get("tree_index")?,
            batch_id: row.try_get("batch_id")?,
        })
    }
}

/// Repository for merkle leaf database operations.
///
/// Handles database interactions for merkle leaves used in cryptographic
/// attestations. Primarily used for verification queries in tests and
/// the MerkleService implementation.
pub struct Repository {
    pool: Arc<PgPool>,
}

impl Repository {
    /// Creates a new repository instance.
    pub fn new(pool: Arc<PgPool>) -> Self {
        Self { pool }
    }

    /// Returns a reference to the database pool.
    pub fn pool(&self) -> Arc<PgPool> {
        self.pool.clone()
    }

    /// Counts merkle leaves for a specific delivery attempt.
    ///
    /// Used to verify that leaves were created for delivery attempts.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn count_by_delivery_attempt(&self, delivery_attempt_id: Uuid) -> Result<i64> {
        let count: (i64,) = sqlx::query_as(
            r"
            SELECT COUNT(*) FROM merkle_leaves
            WHERE delivery_attempt_id = $1
            ",
        )
        .bind(delivery_attempt_id)
        .fetch_one(&*self.pool)
        .await?;

        Ok(count.0)
    }

    /// Finds tree indices for specific delivery attempts.
    ///
    /// Used to verify leaf ordering and tree structure.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_tree_indices_by_attempts(
        &self,
        delivery_attempt_ids: &[Uuid],
    ) -> Result<Vec<i64>> {
        let indices: Vec<(i64,)> = sqlx::query_as(
            r"
            SELECT tree_index FROM merkle_leaves
            WHERE delivery_attempt_id = ANY($1)
            AND tree_index IS NOT NULL
            ORDER BY tree_index ASC
            ",
        )
        .bind(delivery_attempt_ids)
        .fetch_all(&*self.pool)
        .await?;

        Ok(indices.into_iter().map(|(idx,)| idx).collect())
    }

    /// Finds batch ID for a specific delivery attempt.
    ///
    /// Used to verify batch processing and grouping.
    ///
    /// # Errors
    ///
    /// Returns error if query fails or leaf not found.
    pub async fn find_batch_id_by_delivery_attempt(
        &self,
        delivery_attempt_id: Uuid,
    ) -> Result<Option<Uuid>> {
        let batch_id: Option<Uuid> = sqlx::query_scalar(
            r"
            SELECT batch_id FROM merkle_leaves
            WHERE delivery_attempt_id = $1
            ",
        )
        .bind(delivery_attempt_id)
        .fetch_optional(&*self.pool)
        .await?;

        Ok(batch_id)
    }

    /// Finds delivery attempts and their tree indices for verification.
    ///
    /// Returns pairs of (delivery_attempt_id, tree_index) ordered by tree_index
    /// for verifying proper leaf ordering in the merkle tree.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_attempts_with_tree_indices(
        &self,
        delivery_attempt_ids: &[Uuid],
    ) -> Result<Vec<(Uuid, i64)>> {
        let results: Vec<(Uuid, i64)> = sqlx::query_as(
            r"
            SELECT delivery_attempt_id, tree_index FROM merkle_leaves
            WHERE delivery_attempt_id = ANY($1)
            AND tree_index IS NOT NULL
            ORDER BY tree_index ASC
            ",
        )
        .bind(delivery_attempt_ids)
        .fetch_all(&*self.pool)
        .await?;

        Ok(results)
    }

    /// Finds all committed leaves ordered by tree index.
    ///
    /// Used for tree state reconstruction and verification.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_committed_leaf_hashes(&self) -> Result<Vec<Vec<u8>>> {
        let hashes: Vec<Vec<u8>> = sqlx::query_scalar(
            r"
            SELECT leaf_hash FROM merkle_leaves
            WHERE tree_index IS NOT NULL
            ORDER BY tree_index ASC
            ",
        )
        .fetch_all(&*self.pool)
        .await?;

        Ok(hashes)
    }

    /// Finds all committed leaves ordered by tree index within a transaction.
    ///
    /// Used for tree state reconstruction during atomic operations.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_committed_leaf_hashes_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<Vec<Vec<u8>>> {
        let hashes: Vec<Vec<u8>> = sqlx::query_scalar(
            r"
            SELECT leaf_hash FROM merkle_leaves
            WHERE tree_index IS NOT NULL
            ORDER BY tree_index ASC
            ",
        )
        .fetch_all(&mut **tx)
        .await?;

        Ok(hashes)
    }

    /// Counts total number of merkle leaves.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn count_all(&self) -> Result<i64> {
        let count: (i64,) = sqlx::query_as(
            r"
            SELECT COUNT(*) FROM merkle_leaves
            ",
        )
        .fetch_one(&*self.pool)
        .await?;

        Ok(count.0)
    }

    /// Counts committed merkle leaves (those with tree indices).
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn count_committed(&self) -> Result<i64> {
        let count: (i64,) = sqlx::query_as(
            r"
            SELECT COUNT(*) FROM merkle_leaves
            WHERE tree_index IS NOT NULL
            ",
        )
        .fetch_one(&*self.pool)
        .await?;

        Ok(count.0)
    }

    /// Finds attestation leaf info for a specific event.
    ///
    /// Joins with delivery_attempts to provide complete attestation information
    /// including delivery status. Used primarily for test verification and
    /// attestation queries.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_attestation_leaf_info_by_event(
        &self,
        event_id: Uuid,
    ) -> Result<Option<AttestationLeafInfo>> {
        let result = sqlx::query_as::<_, AttestationLeafInfo>(
            r"
            SELECT ml.delivery_attempt_id, ml.endpoint_url, ml.attempt_number,
                   da.response_status, ml.leaf_hash
            FROM merkle_leaves ml
            JOIN delivery_attempts da ON ml.delivery_attempt_id = da.id
            WHERE ml.event_id = $1
            ",
        )
        .bind(event_id)
        .fetch_optional(&*self.pool)
        .await?;

        Ok(result)
    }

    /// Inserts a merkle leaf from delivery attempt data.
    ///
    /// Performs JOIN with delivery_attempts and webhook_events to get
    /// the necessary tenant and event information automatically.
    ///
    /// # Errors
    ///
    /// Returns error if insert fails or delivery attempt not found.
    pub async fn insert_leaf_from_attempt(&self, params: MerkleLeafInsert<'_>) -> Result<()> {
        self.insert_leaf_from_attempt_in_tx(&*self.pool, params).await
    }

    /// Inserts a merkle leaf from delivery attempt data within a transaction.
    ///
    /// # Errors
    ///
    /// Returns error if insert fails or delivery attempt not found.
    pub async fn insert_leaf_from_attempt_in_tx<'e, E>(
        &self,
        executor: E,
        params: MerkleLeafInsert<'_>,
    ) -> Result<()>
    where
        E: Executor<'e, Database = Postgres>,
    {
        tracing::debug!(
            delivery_attempt_id = %params.delivery_attempt_id,
            endpoint_url = %params.endpoint_url,
            attempt_number = params.attempt_number,
            "Inserting merkle leaf for delivery attempt"
        );

        let result = sqlx::query(
            r"
            INSERT INTO merkle_leaves
            (leaf_hash, delivery_attempt_id, event_id, tenant_id, endpoint_url,
             payload_hash, attempt_number, attempted_at, tree_index, batch_id)
            SELECT $1, $2, da.event_id, we.tenant_id, $3, $4, $5, $6, $7, $8
            FROM delivery_attempts da
            JOIN webhook_events we ON da.event_id = we.id
            WHERE da.id = $2
            ",
        )
        .bind(params.leaf_hash)
        .bind(params.delivery_attempt_id)
        .bind(params.endpoint_url)
        .bind(params.payload_hash)
        .bind(params.attempt_number)
        .bind(params.attempted_at)
        .bind(params.tree_index)
        .bind(params.batch_id)
        .execute(executor)
        .await?;

        tracing::debug!(
            delivery_attempt_id = %params.delivery_attempt_id,
            rows_affected = result.rows_affected(),
            "Merkle leaf insert completed"
        );

        if result.rows_affected() == 0 {
            tracing::warn!(
                delivery_attempt_id = %params.delivery_attempt_id,
                "No rows inserted - delivery attempt may not exist in database"
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn count_operations_work() {
        let env = kapsel_testing::TestEnv::new_isolated().await.unwrap();
        let repo = Repository::new(Arc::new(env.pool().clone()));

        // Should start with 0 leaves
        let initial_count = repo.count_all().await.unwrap();
        assert_eq!(initial_count, 0);

        let committed_count = repo.count_committed().await.unwrap();
        assert_eq!(committed_count, 0);
    }

    #[tokio::test]
    async fn find_batch_id_handles_missing_leaves() {
        let env = kapsel_testing::TestEnv::new_isolated().await.unwrap();
        let repo = Repository::new(Arc::new(env.pool().clone()));

        let fake_id = Uuid::new_v4();
        let batch_id = repo.find_batch_id_by_delivery_attempt(fake_id).await.unwrap();
        assert!(batch_id.is_none());
    }
}
