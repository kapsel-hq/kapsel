//! Repository for endpoint database operations.
//!
//! Manages webhook endpoint configuration including URLs, authentication,
//! retry policies, and circuit breaker state. Endpoints define where and how
//! webhooks are delivered.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sqlx::{Executor, PgPool, Postgres, Transaction};

use crate::{
    error::Result,
    models::{CircuitState, Endpoint, EndpointId, SignatureConfig, TenantId},
};

/// Parameters for updating circuit breaker state.
#[derive(Debug)]
pub struct CircuitStateUpdate {
    /// New circuit state
    pub state: CircuitState,
    /// Current failure count
    pub failure_count: i32,
    /// Current success count
    pub success_count: i32,
    /// Timestamp of last failure
    pub last_failure_at: Option<DateTime<Utc>>,
    /// Timestamp when half-open state should be attempted
    pub half_open_at: Option<DateTime<Utc>>,
}

/// Repository for endpoint database operations.
///
/// Handles all database interactions for webhook endpoints including
/// configuration management, circuit breaker state tracking, and delivery
/// statistics.
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

    /// Creates a new endpoint.
    ///
    /// # Errors
    ///
    /// Returns error if insert fails or constraints are violated.
    pub async fn create(&self, endpoint: &Endpoint) -> Result<EndpointId> {
        self.create_impl(&*self.pool, endpoint).await
    }

    /// Creates an endpoint within a transaction.
    ///
    /// # Errors
    ///
    /// Returns error if insert fails.
    pub async fn create_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        endpoint: &Endpoint,
    ) -> Result<EndpointId> {
        self.create_impl(&mut **tx, endpoint).await
    }

    /// Private helper for creating endpoints with generic executor.
    async fn create_impl<'e, E>(&self, executor: E, endpoint: &Endpoint) -> Result<EndpointId>
    where
        E: Executor<'e, Database = Postgres>,
    {
        // Serialize signature config for database storage
        let signature_config = match &endpoint.signature_config {
            SignatureConfig::None => "none".to_string(),
            SignatureConfig::HmacSha256 { secret, header } => {
                format!("hmac_sha256:{header}:{secret}")
            },
        };

        let id = sqlx::query_scalar(
            r"
            INSERT INTO endpoints (
                id, tenant_id, name, url, is_active, signature_config,
                max_retries, timeout_seconds, retry_strategy,
                circuit_state, circuit_failure_count, circuit_success_count,
                circuit_last_failure_at, circuit_half_open_at,
                total_events_received, total_events_delivered, total_events_failed
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17
            )
            RETURNING id
            ",
        )
        .bind(endpoint.id.0)
        .bind(endpoint.tenant_id.0)
        .bind(&endpoint.name)
        .bind(&endpoint.url)
        .bind(endpoint.is_active)
        .bind(signature_config)
        .bind(endpoint.max_retries)
        .bind(endpoint.timeout_seconds)
        .bind(endpoint.retry_strategy.to_string())
        .bind(endpoint.circuit_state.to_string())
        .bind(endpoint.circuit_failure_count)
        .bind(endpoint.circuit_success_count)
        .bind(endpoint.circuit_last_failure_at)
        .bind(endpoint.circuit_half_open_at)
        .bind(endpoint.total_events_received)
        .bind(endpoint.total_events_delivered)
        .bind(endpoint.total_events_failed)
        .fetch_one(executor)
        .await?;

        Ok(EndpointId(id))
    }

    /// Finds an endpoint by ID.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_by_id(&self, endpoint_id: EndpointId) -> Result<Option<Endpoint>> {
        self.find_by_id_impl(&*self.pool, endpoint_id).await
    }

    /// Finds an endpoint by ID within a transaction.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_by_id_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        endpoint_id: EndpointId,
    ) -> Result<Option<Endpoint>> {
        self.find_by_id_impl(&mut **tx, endpoint_id).await
    }

    /// Private helper for finding endpoints by ID with generic executor.
    async fn find_by_id_impl<'e, E>(
        &self,
        executor: E,
        endpoint_id: EndpointId,
    ) -> Result<Option<Endpoint>>
    where
        E: Executor<'e, Database = Postgres>,
    {
        let endpoint = sqlx::query_as::<_, Endpoint>(
            r"
            SELECT id, tenant_id, name, url, is_active, signature_config,
                   max_retries, timeout_seconds, retry_strategy,
                   circuit_state, circuit_failure_count, circuit_success_count,
                   circuit_last_failure_at, circuit_half_open_at,
                   created_at, updated_at, deleted_at,
                   total_events_received, total_events_delivered, total_events_failed
            FROM endpoints
            WHERE id = $1
            ",
        )
        .bind(endpoint_id.0)
        .fetch_optional(executor)
        .await?;

        Ok(endpoint)
    }

    /// Finds all endpoints for a tenant.
    ///
    /// Returns endpoints ordered by created_at descending (newest first).
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_by_tenant(&self, tenant_id: TenantId) -> Result<Vec<Endpoint>> {
        let endpoints = sqlx::query_as::<_, Endpoint>(
            r"
            SELECT id, tenant_id, name, url, is_active, signature_config,
                   max_retries, timeout_seconds, retry_strategy,
                   circuit_state, circuit_failure_count, circuit_success_count,
                   circuit_last_failure_at, circuit_half_open_at,
                   created_at, updated_at, deleted_at,
                   total_events_received, total_events_delivered, total_events_failed
            FROM endpoints
            WHERE tenant_id = $1
            ORDER BY created_at DESC
            ",
        )
        .bind(tenant_id.0)
        .fetch_all(&*self.pool)
        .await?;

        Ok(endpoints)
    }

    /// Finds all active endpoints for a tenant.
    ///
    /// Active endpoints are those with `is_active = true` and not soft-deleted.
    /// These are the only endpoints that will receive webhook deliveries.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_active_by_tenant(&self, tenant_id: TenantId) -> Result<Vec<Endpoint>> {
        let endpoints = sqlx::query_as::<_, Endpoint>(
            r"
            SELECT id, tenant_id, name, url, is_active, signature_config,
                   max_retries, timeout_seconds, retry_strategy,
                   circuit_state, circuit_failure_count, circuit_success_count,
                   circuit_last_failure_at, circuit_half_open_at,
                   created_at, updated_at, deleted_at,
                   total_events_received, total_events_delivered, total_events_failed
            FROM endpoints
            WHERE tenant_id = $1 AND is_active = true AND deleted_at IS NULL
            ORDER BY created_at DESC
            ",
        )
        .bind(tenant_id.0)
        .fetch_all(&*self.pool)
        .await?;

        Ok(endpoints)
    }

    /// Updates an endpoint.
    ///
    /// # Errors
    ///
    /// Returns error if update fails.
    pub async fn update(&self, endpoint: &Endpoint) -> Result<()> {
        self.update_impl(&*self.pool, endpoint).await
    }

    /// Updates an endpoint within a transaction.
    ///
    /// # Errors
    ///
    /// Returns error if update fails.
    pub async fn update_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        endpoint: &Endpoint,
    ) -> Result<()> {
        self.update_impl(&mut **tx, endpoint).await
    }

    /// Private helper for updating endpoints with generic executor.
    async fn update_impl<'e, E>(&self, executor: E, endpoint: &Endpoint) -> Result<()>
    where
        E: Executor<'e, Database = Postgres>,
    {
        // Serialize signature config for database storage
        let signature_config = match &endpoint.signature_config {
            SignatureConfig::None => "none".to_string(),
            SignatureConfig::HmacSha256 { secret, header } => {
                format!("hmac_sha256:{header}:{secret}")
            },
        };

        sqlx::query(
            r"
            UPDATE endpoints
            SET name = $2, url = $3, is_active = $4, signature_config = $5,
                max_retries = $6, timeout_seconds = $7,
                retry_strategy = $8, circuit_state = $9, circuit_failure_count = $10,
                circuit_success_count = $11, circuit_last_failure_at = $12,
                circuit_half_open_at = $13, updated_at = NOW()
            WHERE id = $1
            ",
        )
        .bind(endpoint.id.0)
        .bind(&endpoint.name)
        .bind(&endpoint.url)
        .bind(endpoint.is_active)
        .bind(signature_config)
        .bind(endpoint.max_retries)
        .bind(endpoint.timeout_seconds)
        .bind(endpoint.retry_strategy.to_string())
        .bind(endpoint.circuit_state.to_string())
        .bind(endpoint.circuit_failure_count)
        .bind(endpoint.circuit_success_count)
        .bind(endpoint.circuit_last_failure_at)
        .bind(endpoint.circuit_half_open_at)
        .execute(executor)
        .await?;

        Ok(())
    }

    /// Enables or disables an endpoint.
    ///
    /// Disabled endpoints will not receive webhook deliveries but their
    /// configuration is preserved.
    ///
    /// # Errors
    ///
    /// Returns error if update fails.
    pub async fn set_enabled(&self, endpoint_id: EndpointId, enabled: bool) -> Result<()> {
        self.set_enabled_impl(&*self.pool, endpoint_id, enabled).await
    }

    /// Sets endpoint enabled state within a transaction.
    ///
    /// # Errors
    ///
    /// Returns error if update fails.
    pub async fn set_enabled_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        endpoint_id: EndpointId,
        enabled: bool,
    ) -> Result<()> {
        self.set_enabled_impl(&mut **tx, endpoint_id, enabled).await
    }

    /// Private helper for setting endpoint enabled state with generic executor.
    async fn set_enabled_impl<'e, E>(
        &self,
        executor: E,
        endpoint_id: EndpointId,
        enabled: bool,
    ) -> Result<()>
    where
        E: Executor<'e, Database = Postgres>,
    {
        sqlx::query(
            r"
            UPDATE endpoints
            SET is_active = $2, updated_at = NOW()
            WHERE id = $1
            ",
        )
        .bind(endpoint_id.0)
        .bind(enabled)
        .execute(executor)
        .await?;

        Ok(())
    }

    /// Updates the circuit breaker state for an endpoint.
    ///
    /// Circuit breaker prevents cascading failures by temporarily disabling
    /// delivery to endpoints that are consistently failing.
    ///
    /// # Errors
    ///
    /// Returns error if update fails.
    pub async fn update_circuit_state(
        &self,
        endpoint_id: EndpointId,
        state: CircuitState,
        failure_count: i32,
        success_count: i32,
        last_failure_at: Option<DateTime<Utc>>,
        half_open_at: Option<DateTime<Utc>>,
    ) -> Result<()> {
        let update = CircuitStateUpdate {
            state,
            failure_count,
            success_count,
            last_failure_at,
            half_open_at,
        };
        self.update_circuit_state_impl(&*self.pool, endpoint_id, update).await
    }

    /// Updates circuit breaker state within a transaction.
    ///
    /// # Errors
    ///
    /// Returns error if update fails.
    pub async fn update_circuit_state_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        endpoint_id: EndpointId,
        update: CircuitStateUpdate,
    ) -> Result<()> {
        self.update_circuit_state_impl(&mut **tx, endpoint_id, update).await
    }

    /// Private helper for updating circuit state with generic executor.
    async fn update_circuit_state_impl<'e, E>(
        &self,
        executor: E,
        endpoint_id: EndpointId,
        update: CircuitStateUpdate,
    ) -> Result<()>
    where
        E: Executor<'e, Database = Postgres>,
    {
        sqlx::query(
            r"
            UPDATE endpoints
            SET circuit_state = $2,
                circuit_failure_count = $3,
                circuit_success_count = $4,
                circuit_last_failure_at = $5,
                circuit_half_open_at = $6,
                updated_at = NOW()
            WHERE id = $1
            ",
        )
        .bind(endpoint_id.0)
        .bind(update.state.to_string())
        .bind(update.failure_count)
        .bind(update.success_count)
        .bind(update.last_failure_at)
        .bind(update.half_open_at)
        .execute(executor)
        .await?;

        Ok(())
    }

    /// Deletes an endpoint.
    ///
    /// Hard delete - removes the endpoint permanently. Use soft delete
    /// (setting deleted_at) for recoverable deletion.
    ///
    /// # Errors
    ///
    /// Returns error if delete fails or endpoint has associated events.
    pub async fn delete(&self, endpoint_id: EndpointId) -> Result<()> {
        self.delete_impl(&*self.pool, endpoint_id).await
    }

    /// Deletes an endpoint within a transaction.
    ///
    /// # Errors
    ///
    /// Returns error if delete fails.
    pub async fn delete_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        endpoint_id: EndpointId,
    ) -> Result<()> {
        self.delete_impl(&mut **tx, endpoint_id).await
    }

    /// Private helper for deleting endpoints with generic executor.
    async fn delete_impl<'e, E>(&self, executor: E, endpoint_id: EndpointId) -> Result<()>
    where
        E: Executor<'e, Database = Postgres>,
    {
        sqlx::query(
            r"
            DELETE FROM endpoints
            WHERE id = $1
            ",
        )
        .bind(endpoint_id.0)
        .execute(executor)
        .await?;

        Ok(())
    }

    /// Soft deletes an endpoint by setting deleted_at timestamp.
    ///
    /// Soft-deleted endpoints are excluded from active queries but can be
    /// recovered if needed.
    ///
    /// # Errors
    ///
    /// Returns error if update fails.
    pub async fn soft_delete(&self, endpoint_id: EndpointId) -> Result<()> {
        sqlx::query(
            r"
            UPDATE endpoints
            SET deleted_at = NOW(), is_active = false, updated_at = NOW()
            WHERE id = $1 AND deleted_at IS NULL
            ",
        )
        .bind(endpoint_id.0)
        .execute(&*self.pool)
        .await?;

        Ok(())
    }

    /// Recovers a soft-deleted endpoint.
    ///
    /// # Errors
    ///
    /// Returns error if update fails.
    pub async fn recover(&self, endpoint_id: EndpointId) -> Result<()> {
        sqlx::query(
            r"
            UPDATE endpoints
            SET deleted_at = NULL, is_active = true, updated_at = NOW()
            WHERE id = $1 AND deleted_at IS NOT NULL
            ",
        )
        .bind(endpoint_id.0)
        .execute(&*self.pool)
        .await?;

        Ok(())
    }

    /// Counts all endpoints for a tenant.
    ///
    /// Includes both active and inactive endpoints, excluding soft-deleted
    /// ones.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn count_by_tenant(&self, tenant_id: TenantId) -> Result<i64> {
        let count: (i64,) = sqlx::query_as(
            r"
            SELECT COUNT(*) FROM endpoints
            WHERE tenant_id = $1 AND deleted_at IS NULL
            ",
        )
        .bind(tenant_id.0)
        .fetch_one(&*self.pool)
        .await?;

        Ok(count.0)
    }

    /// Counts active endpoints for a tenant.
    ///
    /// Only counts endpoints with `is_active = true` and not soft-deleted.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn count_active_by_tenant(&self, tenant_id: TenantId) -> Result<i64> {
        let count: (i64,) = sqlx::query_as(
            r"
            SELECT COUNT(*) FROM endpoints
            WHERE tenant_id = $1 AND is_active = true AND deleted_at IS NULL
            ",
        )
        .bind(tenant_id.0)
        .fetch_one(&*self.pool)
        .await?;

        Ok(count.0)
    }

    /// Finds an endpoint by name within a tenant.
    ///
    /// Endpoint names are unique within a tenant for easier management.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_by_name(&self, tenant_id: TenantId, name: &str) -> Result<Option<Endpoint>> {
        let endpoint = sqlx::query_as::<_, Endpoint>(
            r"
            SELECT id, tenant_id, name, url, is_active, signature_config,
                   max_retries, timeout_seconds, retry_strategy,
                   circuit_state, circuit_failure_count, circuit_success_count,
                   circuit_last_failure_at, circuit_half_open_at,
                   created_at, updated_at, deleted_at,
                   total_events_received, total_events_delivered, total_events_failed
            FROM endpoints
            WHERE tenant_id = $1 AND name = $2 AND deleted_at IS NULL
            ",
        )
        .bind(tenant_id.0)
        .bind(name)
        .fetch_optional(&*self.pool)
        .await?;

        Ok(endpoint)
    }

    /// Increments delivery statistics for an endpoint.
    ///
    /// Updates the cumulative counters for events received, delivered, and
    /// failed. Used for monitoring and analytics.
    ///
    /// # Errors
    ///
    /// Returns error if update fails.
    pub async fn increment_stats(
        &self,
        endpoint_id: EndpointId,
        events_received: i64,
        events_delivered: i64,
        events_failed: i64,
    ) -> Result<()> {
        sqlx::query(
            r"
            UPDATE endpoints
            SET total_events_received = total_events_received + $2,
                total_events_delivered = total_events_delivered + $3,
                total_events_failed = total_events_failed + $4,
                updated_at = NOW()
            WHERE id = $1
            ",
        )
        .bind(endpoint_id.0)
        .bind(events_received)
        .bind(events_delivered)
        .bind(events_failed)
        .execute(&*self.pool)
        .await?;

        Ok(())
    }

    /// Finds endpoints with open circuit breakers.
    ///
    /// Used for monitoring and alerting when endpoints are experiencing issues.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_with_open_circuits(&self, limit: Option<i64>) -> Result<Vec<Endpoint>> {
        let endpoints = sqlx::query_as::<_, Endpoint>(
            r"
            SELECT id, tenant_id, name, url, is_active, signature_config,
                   max_retries, timeout_seconds, retry_strategy,
                   circuit_state, circuit_failure_count, circuit_success_count,
                   circuit_last_failure_at, circuit_half_open_at,
                   created_at, updated_at, deleted_at,
                   total_events_received, total_events_delivered, total_events_failed
            FROM endpoints
            WHERE circuit_state = 'open' AND deleted_at IS NULL
            ORDER BY circuit_last_failure_at DESC
            LIMIT $1
            ",
        )
        .bind(limit.unwrap_or(100))
        .fetch_all(&*self.pool)
        .await?;

        Ok(endpoints)
    }

    /// Resets circuit breaker state to closed.
    ///
    /// Used when manually recovering an endpoint after resolving issues.
    ///
    /// # Errors
    ///
    /// Returns error if update fails.
    pub async fn reset_circuit_breaker(&self, endpoint_id: EndpointId) -> Result<()> {
        sqlx::query(
            r"
            UPDATE endpoints
            SET circuit_state = 'closed',
                circuit_failure_count = 0,
                circuit_success_count = 0,
                circuit_last_failure_at = NULL,
                circuit_half_open_at = NULL,
                updated_at = NOW()
            WHERE id = $1
            ",
        )
        .bind(endpoint_id.0)
        .execute(&*self.pool)
        .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn repository_can_be_created() {
        let pool = sqlx::PgPool::connect_lazy("postgresql://test").unwrap();
        let _repo = Repository::new(Arc::new(pool));
    }
}
