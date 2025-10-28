//! Repository for tenant database operations.
//!
//! Manages tenant lifecycle including creation, updates, and tier management.
//! All webhook resources are scoped to tenants for complete data isolation
//! between customers.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sqlx::{Executor, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::{
    error::Result,
    models::{Tenant, TenantId},
    Clock,
};

/// Repository for tenant database operations.
///
/// Provides type-safe access to tenant data with support for transactional
/// operations and tier-based feature flags.
pub struct Repository {
    pool: Arc<PgPool>,
    clock: Arc<dyn Clock>,
}

impl Repository {
    /// Creates a new repository instance.
    pub fn new(pool: Arc<PgPool>, clock: Arc<dyn Clock>) -> Self {
        Self { pool, clock }
    }

    /// Returns a reference to the database pool.
    pub fn pool(&self) -> Arc<PgPool> {
        self.pool.clone()
    }

    /// Creates a new tenant.
    ///
    /// # Errors
    ///
    /// Returns error if insert fails or constraints are violated.
    pub async fn create(&self, tenant: &Tenant) -> Result<TenantId> {
        self.create_impl(&*self.pool, tenant).await
    }

    /// Creates a tenant within a transaction.
    ///
    /// # Errors
    ///
    /// Returns error if insert fails.
    pub async fn create_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        tenant: &Tenant,
    ) -> Result<TenantId> {
        self.create_impl(&mut **tx, tenant).await
    }

    /// Private helper for creating tenants with generic executor.
    async fn create_impl<'e, E>(&self, executor: E, tenant: &Tenant) -> Result<TenantId>
    where
        E: Executor<'e, Database = Postgres>,
    {
        let id = sqlx::query_scalar(
            r"
            INSERT INTO tenants (id, name, tier)
            VALUES ($1, $2, $3)
            RETURNING id
            ",
        )
        .bind(tenant.id.0)
        .bind(&tenant.name)
        .bind(&tenant.tier)
        .fetch_one(executor)
        .await?;

        Ok(TenantId(id))
    }

    /// Finds a tenant by ID.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_by_id(&self, tenant_id: TenantId) -> Result<Option<Tenant>> {
        let tenant = sqlx::query_as::<_, Tenant>(
            r"
            SELECT id, name, tier, max_events_per_month, max_endpoints,
                   events_this_month, created_at, updated_at, deleted_at,
                   stripe_customer_id, stripe_subscription_id
            FROM tenants
            WHERE id = $1 AND deleted_at IS NULL
            ",
        )
        .bind(tenant_id.0)
        .fetch_optional(&*self.pool)
        .await?;

        Ok(tenant)
    }

    /// Finds a tenant by its name.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_by_name(&self, name: &str) -> Result<Option<Tenant>> {
        let tenant = sqlx::query_as::<_, Tenant>(
            r"
            SELECT id, name, tier, max_events_per_month, max_endpoints,
                   events_this_month, created_at, updated_at, deleted_at,
                   stripe_customer_id, stripe_subscription_id
            FROM tenants
            WHERE name = $1 AND deleted_at IS NULL
            ",
        )
        .bind(name)
        .fetch_optional(&*self.pool)
        .await?;

        Ok(tenant)
    }

    /// Finds all tenants.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_all(&self, limit: Option<i64>) -> Result<Vec<Tenant>> {
        let tenants = sqlx::query_as::<_, Tenant>(
            r"
            SELECT id, name, tier, max_events_per_month, max_endpoints,
                   events_this_month, created_at, updated_at, deleted_at,
                   stripe_customer_id, stripe_subscription_id
            FROM tenants
            WHERE deleted_at IS NULL
            ORDER BY created_at DESC
            LIMIT $1
            ",
        )
        .bind(limit.unwrap_or(100))
        .fetch_all(&*self.pool)
        .await?;

        Ok(tenants)
    }

    /// Updates a tenant.
    ///
    /// # Errors
    ///
    /// Returns error if update fails.
    pub async fn update(&self, tenant: &Tenant) -> Result<()> {
        let now = DateTime::<Utc>::from(self.clock.now_system());
        sqlx::query(
            r"
            UPDATE tenants
            SET name = $2, tier = $3, updated_at = $4
            WHERE id = $1
            ",
        )
        .bind(tenant.id.0)
        .bind(&tenant.name)
        .bind(&tenant.tier)
        .bind(now)
        .execute(&*self.pool)
        .await?;

        Ok(())
    }

    /// Deletes a tenant.
    ///
    /// # Errors
    ///
    /// Returns error if delete fails or tenant has associated resources.
    pub async fn delete(&self, tenant_id: TenantId) -> Result<()> {
        sqlx::query(
            r"
            DELETE FROM tenants
            WHERE id = $1
            ",
        )
        .bind(tenant_id.0)
        .execute(&*self.pool)
        .await?;

        Ok(())
    }

    /// Counts all tenants.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn count(&self) -> Result<i64> {
        let count: (i64,) = sqlx::query_as(
            r"
            SELECT COUNT(*) FROM tenants
            ",
        )
        .fetch_one(&*self.pool)
        .await?;

        Ok(count.0)
    }

    /// Checks if a tenant exists.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn exists(&self, tenant_id: TenantId) -> Result<bool> {
        let exists: (bool,) = sqlx::query_as(
            r"
            SELECT EXISTS(SELECT 1 FROM tenants WHERE id = $1)
            ",
        )
        .bind(tenant_id.0)
        .fetch_one(&*self.pool)
        .await?;

        Ok(exists.0)
    }

    /// Checks if a tenant name is already in use.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn name_exists(&self, name: &str) -> Result<bool> {
        let exists: (bool,) = sqlx::query_as(
            r"
            SELECT EXISTS(SELECT 1 FROM tenants WHERE name = $1)
            ",
        )
        .bind(name)
        .fetch_one(&*self.pool)
        .await?;

        Ok(exists.0)
    }

    /// Finds tenants by tier.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_by_tier(&self, tier: &str, limit: Option<i64>) -> Result<Vec<Tenant>> {
        let tenants = sqlx::query_as::<_, Tenant>(
            r"
            SELECT id, name, tier, max_events_per_month, max_endpoints,
                   events_this_month, created_at, updated_at, deleted_at,
                   stripe_customer_id, stripe_subscription_id
            FROM tenants
            WHERE tier = $1 AND deleted_at IS NULL
            ORDER BY created_at ASC
            LIMIT $2
            ",
        )
        .bind(tier)
        .bind(limit.unwrap_or(100))
        .fetch_all(&*self.pool)
        .await?;

        Ok(tenants)
    }

    /// Updates the tier for a tenant.
    ///
    /// # Errors
    ///
    /// Returns error if update fails.
    pub async fn update_tier(&self, tenant_id: TenantId, tier: &str) -> Result<()> {
        let now = DateTime::<Utc>::from(self.clock.now_system());
        sqlx::query(
            r"
            UPDATE tenants
            SET tier = $2, updated_at = $3
            WHERE id = $1
            ",
        )
        .bind(tenant_id.0)
        .bind(tier)
        .bind(now)
        .execute(&*self.pool)
        .await?;

        Ok(())
    }

    /// Ensures the system tenant exists, creating it if necessary.
    ///
    /// The system tenant (ID: 00000000-0000-0000-0000-000000000000) is used for
    /// internal operations and monitoring. This method is idempotent.
    ///
    /// # Errors
    ///
    /// Returns error if creation/retrieval fails.
    pub async fn ensure_system_tenant(&self) -> Result<TenantId> {
        // The system tenant has a fixed ID: 00000000-0000-0000-0000-000000000000
        let system_id = Uuid::nil();
        let now = DateTime::<Utc>::from(self.clock.now_system());

        let id = sqlx::query_scalar(
            r"
            INSERT INTO tenants (id, name, tier)
            VALUES ($1, 'system', 'system')
            ON CONFLICT (id) DO UPDATE SET updated_at = $2
            RETURNING id
            ",
        )
        .bind(system_id)
        .bind(now)
        .fetch_one(&*self.pool)
        .await?;

        Ok(TenantId(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn repository_can_be_created() {
        let pool = sqlx::PgPool::connect_lazy("postgresql://test").unwrap();
        let clock = Arc::new(crate::time::TestClock::new());
        let _repo = Repository::new(Arc::new(pool), clock);
    }
}
