//! Attestation service methods for TestEnv

use anyhow::{Context, Result};
use sqlx::Row;
use uuid::Uuid;

use crate::{AttestationLeafInfo, EventId, SignedTreeHeadInfo, TestEnv};

impl TestEnv {
    /// Create test attestation service using shared active key.
    ///
    /// Ensures all tests use the same active attestation key to avoid
    /// constraint violations. Each test gets its own MerkleService instance,
    /// but they share the same signing key from the database.
    ///
    /// Note: Attestation tests must run sequentially due to global tree
    /// state conflicts in the current MerkleService implementation.
    ///
    /// # Errors
    ///
    /// Returns error if database operations fail.
    pub async fn create_test_attestation_service(
        &self,
    ) -> Result<kapsel_attestation::MerkleService> {
        use kapsel_attestation::{MerkleService, SigningService};

        // Create signing service first to ensure key material consistency
        let signing_service = SigningService::ephemeral();
        let public_key = signing_service.public_key_as_bytes();

        tracing::debug!(
            "Creating test attestation service, is_isolated: {}",
            self.is_isolated_test()
        );

        // Handle attestation keys based on test isolation pattern
        let key_id = if self.is_isolated_test() {
            // Isolated tests: Need to handle existing keys even in isolated schemas
            let mut tx = self.pool().begin().await?;

            // Deactivate any existing keys first to avoid constraint violations
            sqlx::query("UPDATE attestation_keys SET is_active = FALSE WHERE is_active = TRUE")
                .execute(&mut *tx)
                .await
                .context("failed to deactivate existing attestation keys in isolated test")?;

            // Create new active key for this test, or reactivate existing one
            // ON CONFLICT handles the case where the same public_key already exists
            let key_id = sqlx::query_scalar(
                "INSERT INTO attestation_keys (public_key, is_active)
                 VALUES ($1, TRUE)
                 ON CONFLICT (public_key)
                 DO UPDATE SET is_active = TRUE, deactivated_at = NULL
                 RETURNING id",
            )
            .bind(&public_key)
            .fetch_one(&mut *tx)
            .await
            .context("failed to create isolated test attestation key")?;

            tx.commit().await?;
            key_id
        } else {
            let mut tx = self.pool().begin().await?;

            // Deactivate existing keys first to avoid constraint violations
            sqlx::query("UPDATE attestation_keys SET is_active = FALSE WHERE is_active = TRUE")
                .execute(&mut *tx)
                .await
                .context("failed to deactivate existing attestation keys")?;

            // Create new active key for this test, or reactivate existing one
            // ON CONFLICT handles the case where the same public_key already exists
            let key_id = sqlx::query_scalar(
                "INSERT INTO attestation_keys (public_key, is_active)
                 VALUES ($1, TRUE)
                 ON CONFLICT (public_key)
                 DO UPDATE SET is_active = TRUE, deactivated_at = NULL
                 RETURNING id",
            )
            .bind(&public_key)
            .fetch_one(&mut *tx)
            .await
            .context("failed to create test attestation key")?;

            tx.commit().await?;
            key_id
        };

        tracing::debug!("Created/activated attestation key with id: {}", key_id);

        // Verify the key exists in the database before proceeding
        let key_check: Result<(Uuid,), _> =
            sqlx::query_as("SELECT id FROM attestation_keys WHERE id = $1")
                .bind(key_id)
                .fetch_one(self.pool())
                .await;

        match key_check {
            Ok((found_id,)) => {
                tracing::debug!("Verified attestation key {} exists in database", found_id);
            },
            Err(e) => {
                tracing::error!("Attestation key {} not found in database: {}", key_id, e);

                // Debug: List all attestation keys
                let all_keys: Vec<(Uuid, bool)> = sqlx::query_as(
                    "SELECT id, is_active FROM attestation_keys ORDER BY created_at DESC LIMIT 5",
                )
                .fetch_all(self.pool())
                .await
                .unwrap_or_default();

                tracing::error!("Available attestation keys in database:");
                for (id, active) in all_keys {
                    tracing::error!("  - {}: active={}", id, active);
                }

                return Err(anyhow::anyhow!("Attestation key {} not found in database", key_id));
            },
        }

        // Use the same signing service with the stored key_id
        let signing_service = signing_service.with_key_id(key_id);

        // Generate unique lock ID based on test_run_id to prevent concurrent
        // advisory lock conflicts between isolated tests
        let lock_id = self.generate_unique_lock_id();
        let merkle_service =
            MerkleService::with_lock_id(self.pool().clone(), signing_service, lock_id);

        tracing::debug!("MerkleService created with key_id: {} and lock_id: {}", key_id, lock_id);

        Ok(merkle_service)
    }

    /// Count attestation leaves for a specific event.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn count_attestation_leaves_for_event(&self, event_id: EventId) -> Result<i64> {
        tracing::debug!(
            event_id = %event_id.0,
            "querying attestation leaf count for event"
        );

        // Debug: Check all leaves in the table
        let all_leaves: Vec<(Uuid, String)> =
            sqlx::query_as("SELECT event_id, endpoint_url FROM merkle_leaves")
                .fetch_all(self.pool())
                .await
                .unwrap_or_default();

        tracing::debug!(
            total_leaves = all_leaves.len(),
            leaves = ?all_leaves,
            "all attestation leaves in database"
        );

        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM merkle_leaves WHERE event_id = $1")
                .bind(event_id.0)
                .fetch_one(self.pool())
                .await?;

        tracing::debug!(
            event_id = %event_id.0,
            count = count,
            "attestation leaf count query result"
        );

        Ok(count)
    }

    /// Fetch attestation leaf for a specific event.
    pub async fn fetch_attestation_leaf_for_event(
        &self,
        event_id: EventId,
    ) -> Result<Option<AttestationLeafInfo>> {
        let result = sqlx::query(
            "SELECT ml.delivery_attempt_id, ml.endpoint_url, ml.attempt_number, da.response_status, ml.leaf_hash
             FROM merkle_leaves ml
             JOIN delivery_attempts da ON ml.delivery_attempt_id = da.id
             WHERE ml.event_id = $1",
        )
        .bind(event_id.0)
        .fetch_optional(self.pool())
        .await?;

        Ok(result.map(|row| AttestationLeafInfo {
            delivery_attempt_id: row.get("delivery_attempt_id"),
            endpoint_url: row.get("endpoint_url"),
            attempt_number: row.get("attempt_number"),
            response_status: row.get("response_status"),
            leaf_hash: row.get("leaf_hash"),
        }))
    }

    /// Count total attestation leaves across all events.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn count_total_attestation_leaves(&self) -> Result<i64> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM merkle_leaves").fetch_one(self.pool()).await?;
        Ok(count)
    }

    /// Get current tree size from database.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn get_current_tree_size(&self) -> Result<i64> {
        let size: Option<Option<i64>> =
            sqlx::query_scalar("SELECT MAX(tree_size) FROM signed_tree_heads")
                .fetch_optional(self.pool())
                .await?;
        Ok(size.flatten().unwrap_or(0))
    }

    /// Count total signed tree heads.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn count_signed_tree_heads(&self) -> Result<i64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM signed_tree_heads")
            .fetch_one(self.pool())
            .await?;

        Ok(count)
    }

    /// Fetch latest signed tree head from database.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn get_latest_signed_tree_head(&self) -> Result<Option<SignedTreeHeadInfo>> {
        let result = sqlx::query(
            "SELECT tree_size, root_hash, timestamp_ms, signature, batch_size
             FROM signed_tree_heads
             ORDER BY timestamp_ms DESC
             LIMIT 1",
        )
        .fetch_optional(self.pool())
        .await?;

        Ok(result.map(|row: sqlx::postgres::PgRow| SignedTreeHeadInfo {
            tree_size: row.get("tree_size"),
            root_hash: row.get("root_hash"),
            timestamp_ms: row.get("timestamp_ms"),
            signature: row.get("signature"),
            batch_size: row.get("batch_size"),
        }))
    }

    /// Trigger attestation batch commitment (simulates background worker).
    pub async fn run_attestation_commitment(&self) -> Result<()> {
        if let Some(ref attestation_service) = self.attestation_service {
            // Check pending count before attempting commit
            let pending_count = attestation_service.read().await.pending_count();
            tracing::debug!(
                pending_count = pending_count,
                "attempting attestation batch commitment"
            );

            if pending_count == 0 {
                tracing::debug!("no pending leaves to commit, skipping");
                return Ok(());
            }

            // Serialize attestation commitment to prevent deadlocks
            // Only one commitment should happen at a time per service instance
            match attestation_service.write().await.try_commit_pending().await {
                Ok(sth) => {
                    tracing::debug!(
                        tree_size = sth.tree_size,
                        root_hash = ?sth.root_hash,
                        "attestation batch committed successfully"
                    );

                    // Verify leaves were actually persisted
                    let total_leaves = self.count_total_attestation_leaves().await?;
                    tracing::debug!(
                        total_leaves = total_leaves,
                        "attestation leaves in database after commitment"
                    );

                    Ok(())
                },
                Err(kapsel_attestation::error::AttestationError::BatchCommitFailed { reason })
                    if reason.contains("no pending leaves") =>
                {
                    tracing::debug!("no pending leaves to commit (caught in error)");
                    Ok(())
                },
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        pending_count = pending_count,
                        "attestation commitment failed"
                    );
                    Err(anyhow::anyhow!("Attestation commitment failed: {e}"))
                },
            }
        } else {
            tracing::debug!("no attestation service configured, skipping commitment");
            Ok(())
        }
    }
}
