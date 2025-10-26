//! Repository for signed tree head database operations.
//!
//! Provides type-safe access to merkle tree heads for cryptographic
//! verification and tree state management. Used by the MerkleService and
//! verification tests.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sqlx::{Executor, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::Result;

/// Signed tree head data structure for database operations.
#[derive(Debug, Clone)]
pub struct SignedTreeHead {
    /// Unique identifier for this tree head
    pub id: Uuid,
    /// Size of the merkle tree (number of leaves)
    pub tree_size: i64,
    /// Root hash of the merkle tree
    pub root_hash: Vec<u8>,
    /// Ed25519 signature over the tree head
    pub signature: Vec<u8>,
    /// Public key ID used for signing
    pub key_id: Uuid,
    /// Batch ID for grouping related operations
    pub batch_id: Uuid,
    /// When this tree head was created
    pub created_at: DateTime<Utc>,
}

impl sqlx::FromRow<'_, sqlx::postgres::PgRow> for SignedTreeHead {
    fn from_row(row: &sqlx::postgres::PgRow) -> std::result::Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            id: row.try_get("id")?,
            tree_size: row.try_get("tree_size")?,
            root_hash: row.try_get("root_hash")?,
            signature: row.try_get("signature")?,
            key_id: row.try_get("key_id")?,
            batch_id: row.try_get("batch_id")?,
            created_at: row.try_get("created_at")?,
        })
    }
}

/// Tree head information for testing and verification queries.
///
/// Contains specific fields needed for test verification, matching
/// the database schema exactly.
#[derive(Debug, Clone)]
pub struct SignedTreeHeadInfo {
    /// Size of the merkle tree (number of leaves)
    pub tree_size: i64,
    /// Root hash of the merkle tree
    pub root_hash: Vec<u8>,
    /// Unix timestamp in milliseconds when the tree head was signed
    pub timestamp_ms: i64,
    /// Ed25519 signature over the tree head data
    pub signature: Vec<u8>,
    /// Number of leaves included in this batch
    pub batch_size: i32,
}

impl sqlx::FromRow<'_, sqlx::postgres::PgRow> for SignedTreeHeadInfo {
    fn from_row(row: &sqlx::postgres::PgRow) -> std::result::Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            tree_size: row.try_get("tree_size")?,
            root_hash: row.try_get("root_hash")?,
            timestamp_ms: row.try_get("timestamp_ms")?,
            signature: row.try_get("signature")?,
            batch_size: row.try_get("batch_size")?,
        })
    }
}

/// Repository for signed tree head database operations.
///
/// Handles database interactions for merkle tree metadata including
/// tree size tracking, root hash storage, and batch verification.
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

    /// Gets the current maximum tree size.
    ///
    /// Returns 0 if no tree heads exist yet. Used to determine
    /// the starting point for new tree operations.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn get_max_tree_size(&self) -> Result<i64> {
        let size: i64 = sqlx::query_scalar(
            r#"
            SELECT COALESCE(MAX(tree_size), 0) FROM signed_tree_heads
            "#,
        )
        .fetch_one(&*self.pool)
        .await?;

        Ok(size)
    }

    /// Gets the current maximum tree size within a transaction.
    ///
    /// Used during atomic tree operations to ensure consistency.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn get_max_tree_size_in_tx(&self, tx: &mut Transaction<'_, Postgres>) -> Result<i64> {
        let size: i64 = sqlx::query_scalar(
            r#"
            SELECT COALESCE(MAX(tree_size), 0) FROM signed_tree_heads
            "#,
        )
        .fetch_one(&mut **tx)
        .await?;

        Ok(size)
    }

    /// Checks if any tree heads exist with at least the specified size.
    ///
    /// Used to verify that tree commitment has occurred.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn exists_with_min_size(&self, min_size: i64) -> Result<bool> {
        let exists: bool = sqlx::query_scalar(
            r#"
            SELECT EXISTS(SELECT 1 FROM signed_tree_heads WHERE tree_size >= $1)
            "#,
        )
        .bind(min_size)
        .fetch_one(&*self.pool)
        .await?;

        Ok(exists)
    }

    /// Checks if a tree head exists for the specified batch.
    ///
    /// Used to verify batch commitment and prevent duplicate processing.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn exists_for_batch(&self, batch_id: Uuid) -> Result<bool> {
        let exists: bool = sqlx::query_scalar(
            r#"
            SELECT EXISTS(SELECT 1 FROM signed_tree_heads WHERE batch_id = $1)
            "#,
        )
        .bind(batch_id)
        .fetch_one(&*self.pool)
        .await?;

        Ok(exists)
    }

    /// Creates a new signed tree head.
    ///
    /// # Errors
    ///
    /// Returns error if insert fails.
    pub async fn create(&self, tree_head: &SignedTreeHead) -> Result<Uuid> {
        self.create_impl(&*self.pool, tree_head).await
    }

    /// Creates a new signed tree head within a transaction.
    ///
    /// # Errors
    ///
    /// Returns error if insert fails.
    pub async fn create_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        tree_head: &SignedTreeHead,
    ) -> Result<Uuid> {
        self.create_impl(&mut **tx, tree_head).await
    }

    /// Private helper for creating tree heads with generic executor.
    async fn create_impl<'e, E>(&self, executor: E, tree_head: &SignedTreeHead) -> Result<Uuid>
    where
        E: Executor<'e, Database = Postgres>,
    {
        sqlx::query(
            r#"
            INSERT INTO signed_tree_heads
            (id, tree_size, root_hash, signature, key_id, batch_id, created_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind(tree_head.id)
        .bind(tree_head.tree_size)
        .bind(&tree_head.root_hash)
        .bind(&tree_head.signature)
        .bind(tree_head.key_id)
        .bind(tree_head.batch_id)
        .bind(tree_head.created_at)
        .execute(executor)
        .await?;

        Ok(tree_head.id)
    }

    /// Finds the latest tree head by tree size.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_latest(&self) -> Result<Option<SignedTreeHead>> {
        let tree_head = sqlx::query_as::<_, SignedTreeHead>(
            r#"
            SELECT id, tree_size, root_hash, signature, key_id, batch_id, created_at
            FROM signed_tree_heads
            ORDER BY tree_size DESC, created_at DESC
            LIMIT 1
            "#,
        )
        .fetch_optional(&*self.pool)
        .await?;

        Ok(tree_head)
    }

    /// Finds a tree head by batch ID.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_by_batch(&self, batch_id: Uuid) -> Result<Option<SignedTreeHead>> {
        let tree_head = sqlx::query_as::<_, SignedTreeHead>(
            r#"
            SELECT id, tree_size, root_hash, signature, key_id, batch_id, created_at
            FROM signed_tree_heads
            WHERE batch_id = $1
            "#,
        )
        .bind(batch_id)
        .fetch_optional(&*self.pool)
        .await?;

        Ok(tree_head)
    }

    /// Counts total number of tree heads.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn count_all(&self) -> Result<i64> {
        let count: (i64,) = sqlx::query_as(
            r#"
            SELECT COUNT(*) FROM signed_tree_heads
            "#,
        )
        .fetch_one(&*self.pool)
        .await?;

        Ok(count.0)
    }

    /// Lists all tree heads ordered by tree size.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn list_all(&self) -> Result<Vec<SignedTreeHead>> {
        let tree_heads = sqlx::query_as::<_, SignedTreeHead>(
            r#"
            SELECT id, tree_size, root_hash, signature, key_id, batch_id, created_at
            FROM signed_tree_heads
            ORDER BY tree_size ASC
            "#,
        )
        .fetch_all(&*self.pool)
        .await?;

        Ok(tree_heads)
    }

    /// Finds the latest signed tree head info for testing.
    ///
    /// Returns tree head information with timestamp_ms and batch_size
    /// fields used for test verification.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_latest_tree_head_info(&self) -> Result<Option<SignedTreeHeadInfo>> {
        let tree_head_info = sqlx::query_as::<_, SignedTreeHeadInfo>(
            r#"
            SELECT tree_size, root_hash, timestamp_ms, signature, batch_size
            FROM signed_tree_heads
            ORDER BY timestamp_ms DESC
            LIMIT 1
            "#,
        )
        .fetch_optional(&*self.pool)
        .await?;

        Ok(tree_head_info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn max_tree_size_starts_at_zero() {
        let env = kapsel_testing::TestEnv::new_isolated().await.unwrap();
        let repo = Repository::new(Arc::new(env.pool().clone()));

        let size = repo.get_max_tree_size().await.unwrap();
        assert_eq!(size, 0);
    }

    #[tokio::test]
    async fn exists_checks_work_with_empty_table() {
        let env = kapsel_testing::TestEnv::new_isolated().await.unwrap();
        let repo = Repository::new(Arc::new(env.pool().clone()));

        let exists = repo.exists_with_min_size(1).await.unwrap();
        assert!(!exists);

        let batch_id = Uuid::new_v4();
        let batch_exists = repo.exists_for_batch(batch_id).await.unwrap();
        assert!(!batch_exists);
    }

    #[tokio::test]
    async fn count_operations_work() {
        let env = kapsel_testing::TestEnv::new_isolated().await.unwrap();
        let repo = Repository::new(Arc::new(env.pool().clone()));

        let count = repo.count_all().await.unwrap();
        assert_eq!(count, 0);
    }
}
