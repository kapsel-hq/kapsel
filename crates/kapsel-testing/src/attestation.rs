//! Attestation service methods for TestEnv

use std::sync::Arc;

use anyhow::{Context, Result};
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

        // Handle attestation keys using repository pattern
        let key_id = self
            .storage()
            .attestation_keys
            .create_and_activate(public_key)
            .await
            .context("failed to create and activate test attestation key")?;

        tracing::debug!("Created/activated attestation key with id: {}", key_id);

        // Verify the key exists in the database before proceeding
        let key_check = self.storage().attestation_keys.find_by_id(key_id).await;

        match key_check {
            Ok(Some(key)) => {
                tracing::debug!("Confirmed attestation key {} exists in database", key.id);
            },
            Ok(None) => {
                tracing::error!("Attestation key {} not found in database", key_id);
            },
            Err(e) => {
                tracing::error!("Database error checking attestation key {}: {}", key_id, e);

                // Debug: List all attestation keys
                let all_keys: Vec<(Uuid, bool)> = self
                    .storage()
                    .attestation_keys
                    .list_all()
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .take(5)
                    .map(|key| (key.id, key.is_active))
                    .collect();

                tracing::error!("Available attestation keys in database:");
                for (id, active) in all_keys {
                    tracing::error!("  - {}: active={}", id, active);
                }

                return Err(anyhow::anyhow!("Attestation key {key_id} not found in database"));
            },
        }

        // Use the same signing service with the stored key_id
        let signing_service = signing_service.with_key_id(key_id);

        // Generate unique lock ID based on test_run_id to prevent concurrent
        // advisory lock conflicts between isolated tests
        let lock_id = self.generate_unique_lock_id();
        let merkle_service = MerkleService::with_lock_id(
            self.storage(),
            signing_service,
            Arc::new(self.clock.clone()),
            lock_id,
        );

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

        // Use repository to find leaves for the specific event
        let leaves =
            self.storage().merkle_leaves.find_attestation_leaf_info_by_event(event_id.0).await?;
        let count = if leaves.is_some() { 1 } else { 0 };

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
        let result =
            self.storage().merkle_leaves.find_attestation_leaf_info_by_event(event_id.0).await?;

        Ok(result)
    }

    /// Count total attestation leaves across all events.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn count_total_attestation_leaves(&self) -> Result<i64> {
        let count: i64 = self.storage().merkle_leaves.count_all().await?;
        Ok(count)
    }

    /// Get current tree size from database.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn get_current_tree_size(&self) -> Result<i64> {
        let size = self.storage().signed_tree_heads.find_max_tree_size().await?;
        Ok(size)
    }

    /// Count total signed tree heads.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn count_signed_tree_heads(&self) -> Result<i64> {
        let count: i64 = self.storage().signed_tree_heads.count_all().await?;

        Ok(count)
    }

    /// Fetch latest signed tree head from database.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn get_latest_signed_tree_head(&self) -> Result<Option<SignedTreeHeadInfo>> {
        let result = self.storage().signed_tree_heads.find_latest_tree_head_info().await?;
        Ok(result)
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
