//! Repository for attestation key database operations.
//!
//! Provides type-safe access to cryptographic keys used for signing
//! attestations with support for key rotation and activation management.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sqlx::{Executor, PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::error::{CoreError, Result};

/// Attestation key data structure for database operations.
#[derive(Debug, Clone)]
pub struct AttestationKey {
    /// Unique identifier for the attestation key
    pub id: Uuid,
    /// Ed25519 public key bytes (32 bytes)
    pub public_key: Vec<u8>,
    /// Whether this key is currently active for signing
    pub is_active: bool,
    /// Timestamp when the key was created
    pub created_at: DateTime<Utc>,
    /// Optional timestamp when the key was deactivated
    pub deactivated_at: Option<DateTime<Utc>>,
}

impl sqlx::FromRow<'_, sqlx::postgres::PgRow> for AttestationKey {
    fn from_row(row: &sqlx::postgres::PgRow) -> std::result::Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            public_key: row.try_get("public_key")?,
            is_active: row.try_get("is_active")?,
            created_at: row.try_get("created_at")?,
            deactivated_at: row.try_get("deactivated_at")?,
        })
    }
}

/// Repository for attestation key database operations.
///
/// Handles all database interactions for attestation keys including creation,
/// activation management, and key rotation with proper uniqueness constraints.
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

    /// Creates a new attestation key.
    ///
    /// # Errors
    ///
    /// Returns error if insert fails or violates constraints.
    pub async fn create(&self, key: &AttestationKey) -> Result<Uuid> {
        self.create_impl(&*self.pool, key).await
    }

    /// Creates a new attestation key within a transaction.
    ///
    /// # Errors
    ///
    /// Returns error if insert fails or violates constraints.
    pub async fn create_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        key: &AttestationKey,
    ) -> Result<Uuid> {
        self.create_impl(&mut **tx, key).await
    }

    /// Private helper for creating attestation keys with generic executor.
    async fn create_impl<'e, E>(&self, executor: E, key: &AttestationKey) -> Result<Uuid>
    where
        E: Executor<'e, Database = Postgres>,
    {
        sqlx::query(
            r#"
            INSERT INTO attestation_keys (id, public_key, is_active, created_at, deactivated_at)
            VALUES ($1, $2, $3, $4, $5)
            "#,
        )
        .bind(key.id)
        .bind(&key.public_key)
        .bind(key.is_active)
        .bind(key.created_at)
        .bind(key.deactivated_at)
        .execute(executor)
        .await?;

        Ok(key.id)
    }

    /// Finds the currently active attestation key.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_active(&self) -> Result<Option<AttestationKey>> {
        let key = sqlx::query_as::<_, AttestationKey>(
            r#"
            SELECT id, public_key, is_active, created_at, deactivated_at
            FROM attestation_keys
            WHERE is_active = TRUE
            ORDER BY created_at DESC
            LIMIT 1
            "#,
        )
        .fetch_optional(&*self.pool)
        .await?;

        Ok(key)
    }

    /// Finds an attestation key by its ID.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_by_id(&self, key_id: Uuid) -> Result<Option<AttestationKey>> {
        let key = sqlx::query_as::<_, AttestationKey>(
            r#"
            SELECT id, public_key, is_active, created_at, deactivated_at
            FROM attestation_keys
            WHERE id = $1
            "#,
        )
        .bind(key_id)
        .fetch_optional(&*self.pool)
        .await?;

        Ok(key)
    }

    /// Deactivates all currently active keys.
    ///
    /// Used during key rotation to ensure only one active key exists.
    ///
    /// # Errors
    ///
    /// Returns error if update fails.
    pub async fn deactivate_all(&self) -> Result<u64> {
        self.deactivate_all_impl(&*self.pool).await
    }

    /// Deactivates all currently active keys within a transaction.
    ///
    /// # Errors
    ///
    /// Returns error if update fails.
    pub async fn deactivate_all_in_tx(&self, tx: &mut Transaction<'_, Postgres>) -> Result<u64> {
        self.deactivate_all_impl(&mut **tx).await
    }

    /// Private helper for deactivating all keys with generic executor.
    async fn deactivate_all_impl<'e, E>(&self, executor: E) -> Result<u64>
    where
        E: Executor<'e, Database = Postgres>,
    {
        let result = sqlx::query(
            r#"
            UPDATE attestation_keys
            SET is_active = FALSE, deactivated_at = NOW()
            WHERE is_active = TRUE
            "#,
        )
        .execute(executor)
        .await?;

        Ok(result.rows_affected())
    }

    /// Activates a specific attestation key by ID.
    ///
    /// Note: This does not deactivate other keys. Use `rotate_to_key` for
    /// atomic key rotation.
    ///
    /// # Errors
    ///
    /// Returns error if update fails or key not found.
    pub async fn activate(&self, key_id: Uuid) -> Result<()> {
        self.activate_impl(&*self.pool, key_id).await
    }

    /// Activates a specific attestation key within a transaction.
    ///
    /// # Errors
    ///
    /// Returns error if update fails or key not found.
    pub async fn activate_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        key_id: Uuid,
    ) -> Result<()> {
        self.activate_impl(&mut **tx, key_id).await
    }

    /// Private helper for activating keys with generic executor.
    async fn activate_impl<'e, E>(&self, executor: E, key_id: Uuid) -> Result<()>
    where
        E: Executor<'e, Database = Postgres>,
    {
        let result = sqlx::query(
            r#"
            UPDATE attestation_keys
            SET is_active = TRUE, deactivated_at = NULL
            WHERE id = $1
            "#,
        )
        .bind(key_id)
        .execute(executor)
        .await?;

        if result.rows_affected() == 0 {
            return Err(CoreError::NotFound(format!("attestation_key {}", key_id)));
        }

        Ok(())
    }

    /// Atomically rotates to a new attestation key.
    ///
    /// Deactivates all existing keys and activates the specified key
    /// within a single transaction to maintain consistency.
    ///
    /// # Errors
    ///
    /// Returns error if rotation fails or key not found.
    pub async fn rotate_to_key(&self, new_key_id: Uuid) -> Result<()> {
        let mut tx = self.pool.begin().await?;

        // First deactivate all existing keys
        self.deactivate_all_in_tx(&mut tx).await?;

        // Then activate the new key
        self.activate_in_tx(&mut tx, new_key_id).await?;

        tx.commit().await?;
        Ok(())
    }

    /// Creates and activates a new attestation key atomically.
    ///
    /// This is the recommended way to perform key rotation as it
    /// deactivates old keys and creates the new one in a single transaction.
    ///
    /// # Errors
    ///
    /// Returns error if key creation or rotation fails.
    pub async fn create_and_activate(&self, public_key: Vec<u8>) -> Result<Uuid> {
        let mut tx = self.pool.begin().await?;

        // First deactivate all existing keys
        self.deactivate_all_in_tx(&mut tx).await?;

        // Create new key as active
        let key_id = Uuid::new_v4();
        let new_key = AttestationKey {
            id: key_id,
            public_key,
            is_active: true,
            created_at: Utc::now(),
            deactivated_at: None,
        };

        self.create_in_tx(&mut tx, &new_key).await?;

        tx.commit().await?;
        Ok(key_id)
    }

    /// Lists all attestation keys ordered by creation time.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn list_all(&self) -> Result<Vec<AttestationKey>> {
        let keys = sqlx::query_as::<_, AttestationKey>(
            r#"
            SELECT id, public_key, is_active, created_at, deactivated_at
            FROM attestation_keys
            ORDER BY created_at DESC
            "#,
        )
        .fetch_all(&*self.pool)
        .await?;

        Ok(keys)
    }

    /// Counts total number of attestation keys.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn count_all(&self) -> Result<i64> {
        let count: (i64,) = sqlx::query_as(
            r#"
            SELECT COUNT(*) FROM attestation_keys
            "#,
        )
        .fetch_one(&*self.pool)
        .await?;

        Ok(count.0)
    }

    /// Counts currently active attestation keys.
    ///
    /// Should typically be 0 or 1 due to unique constraint.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn count_active(&self) -> Result<i64> {
        let count: (i64,) = sqlx::query_as(
            r#"
            SELECT COUNT(*) FROM attestation_keys WHERE is_active = TRUE
            "#,
        )
        .fetch_one(&*self.pool)
        .await?;

        Ok(count.0)
    }

    /// Permanently deletes an attestation key.
    ///
    /// Warning: This is irreversible and should only be used for cleanup
    /// of test keys or expired keys that are no longer needed.
    ///
    /// # Errors
    ///
    /// Returns error if deletion fails.
    pub async fn delete(&self, key_id: Uuid) -> Result<()> {
        let result = sqlx::query(
            r#"
            DELETE FROM attestation_keys
            WHERE id = $1
            "#,
        )
        .bind(key_id)
        .execute(&*self.pool)
        .await?;

        if result.rows_affected() == 0 {
            return Err(CoreError::NotFound(format!("attestation_key {}", key_id)));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_and_find_active_key() {
        let env = kapsel_testing::TestEnv::new_isolated().await.unwrap();
        let repo = Repository::new(Arc::new(env.pool().clone()));

        let public_key = vec![1u8; 32];
        let key_id = repo.create_and_activate(public_key.clone()).await.unwrap();

        let active_key = repo.find_active().await.unwrap().unwrap();
        assert_eq!(active_key.id, key_id);
        assert_eq!(active_key.public_key, public_key);
        assert!(active_key.is_active);
    }

    #[tokio::test]
    async fn key_rotation_deactivates_old_keys() {
        let env = kapsel_testing::TestEnv::new_isolated().await.unwrap();
        let repo = Repository::new(Arc::new(env.pool().clone()));

        // Create first key
        let key1 = vec![1u8; 32];
        let key1_id = repo.create_and_activate(key1).await.unwrap();

        // Create second key - should deactivate first
        let key2 = vec![2u8; 32];
        let _key2_id = repo.create_and_activate(key2.clone()).await.unwrap();

        // First key should be deactivated
        let key1_after = repo.find_by_id(key1_id).await.unwrap().unwrap();
        assert!(!key1_after.is_active);
        assert!(key1_after.deactivated_at.is_some());

        // Only one active key should exist
        let active_count = repo.count_active().await.unwrap();
        assert_eq!(active_count, 1);

        let active_key = repo.find_active().await.unwrap().unwrap();
        assert_eq!(active_key.public_key, key2);
    }
}
