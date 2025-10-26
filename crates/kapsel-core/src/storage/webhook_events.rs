//! Repository for webhook event database operations.
//!
//! Provides type-safe access to webhook events with support for concurrent
//! processing, idempotency checking, and transactional operations.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sqlx::{Executor, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::{
    error::Result,
    models::{EndpointId, EventId, EventStatus, TenantId, WebhookEvent},
};

/// Repository for webhook event database operations.
///
/// Handles all database interactions for webhook events including creation,
/// status updates, and lock-free claiming for concurrent processing.
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

    /// Claims pending events for delivery processing.
    ///
    /// Uses `FOR UPDATE SKIP LOCKED` to enable lock-free concurrent
    /// claiming across multiple workers without blocking. This PostgreSQL
    /// feature allows each worker to claim different events simultaneously.
    ///
    /// Events are claimed in FIFO order (oldest first) to maintain
    /// delivery ordering within each endpoint.
    ///
    /// # Errors
    ///
    /// Returns error if database transaction fails.
    pub async fn claim_pending(&self, batch_size: usize) -> Result<Vec<WebhookEvent>> {
        let now = Utc::now();

        // Use transaction for atomic claim operation
        let mut tx = self.pool.begin().await?;

        // Select events using FOR UPDATE SKIP LOCKED for concurrent processing
        let event_ids: Vec<Uuid> = sqlx::query_scalar(
            r#"
            SELECT id FROM webhook_events
            WHERE status = 'pending'
              AND (next_retry_at IS NULL OR next_retry_at <= $1)
            ORDER BY received_at ASC
            LIMIT $2
            FOR UPDATE SKIP LOCKED
            "#,
        )
        .bind(now)
        .bind(batch_size as i32)
        .fetch_all(&mut *tx)
        .await?;

        if event_ids.is_empty() {
            tx.rollback().await?;
            return Ok(Vec::new());
        }

        // Update status and fetch full event data
        let events = sqlx::query_as::<_, WebhookEvent>(
            r#"
            UPDATE webhook_events
            SET status = 'delivering', last_attempt_at = NOW()
            WHERE id = ANY($1)
            RETURNING id, tenant_id, endpoint_id, source_event_id, idempotency_strategy,
                      status, failure_count, last_attempt_at, next_retry_at,
                      headers, body, content_type, received_at, delivered_at, failed_at,
                      payload_size, signature_valid, signature_error
            "#,
        )
        .bind(&event_ids)
        .fetch_all(&mut *tx)
        .await?;

        tx.commit().await?;

        Ok(events)
    }

    /// Creates a new webhook event.
    ///
    /// # Errors
    ///
    /// Returns error if insert fails or constraints are violated.
    pub async fn create(&self, event: &WebhookEvent) -> Result<EventId> {
        self.create_impl(&*self.pool, event).await
    }

    /// Creates a webhook event within a transaction.
    ///
    /// # Errors
    ///
    /// Returns error if insert fails.
    pub async fn create_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        event: &WebhookEvent,
    ) -> Result<EventId> {
        self.create_impl(&mut **tx, event).await
    }

    /// Private helper for creating events with generic executor.
    async fn create_impl<'e, E>(&self, executor: E, event: &WebhookEvent) -> Result<EventId>
    where
        E: Executor<'e, Database = Postgres>,
    {
        let id = sqlx::query_scalar(
            r#"
            INSERT INTO webhook_events (
                id, tenant_id, endpoint_id, source_event_id, idempotency_strategy,
                status, failure_count, headers, body, content_type,
                payload_size, received_at, signature_valid, signature_error
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14
            )
            RETURNING id
            "#,
        )
        .bind(event.id.0)
        .bind(event.tenant_id.0)
        .bind(event.endpoint_id.0)
        .bind(&event.source_event_id)
        .bind(event.idempotency_strategy.to_string())
        .bind(event.status.to_string())
        .bind(event.failure_count)
        .bind(&event.headers)
        .bind(&event.body)
        .bind(&event.content_type)
        .bind(event.payload_size)
        .bind(event.received_at)
        .bind(event.signature_valid)
        .bind(&event.signature_error)
        .fetch_one(executor)
        .await?;

        Ok(EventId(id))
    }

    /// Marks an event as successfully delivered.
    ///
    /// Sets the event status to 'delivered' and records the delivery timestamp.
    /// This is a terminal state - no further processing will occur.
    ///
    /// # Errors
    ///
    /// Returns error if update fails.
    pub async fn mark_delivered(&self, event_id: EventId) -> Result<()> {
        self.mark_delivered_impl(&*self.pool, event_id).await
    }

    /// Marks an event as delivered within a transaction.
    ///
    /// # Errors
    ///
    /// Returns error if update fails.
    pub async fn mark_delivered_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        event_id: EventId,
    ) -> Result<()> {
        self.mark_delivered_impl(&mut **tx, event_id).await
    }

    /// Private helper for marking events as delivered with generic executor.
    async fn mark_delivered_impl<'e, E>(&self, executor: E, event_id: EventId) -> Result<()>
    where
        E: Executor<'e, Database = Postgres>,
    {
        sqlx::query(
            r#"
            UPDATE webhook_events
            SET status = 'delivered', delivered_at = NOW()
            WHERE id = $1
            "#,
        )
        .bind(event_id.0)
        .execute(executor)
        .await?;

        Ok(())
    }

    /// Marks an event as failed and schedules retry.
    ///
    /// If `next_retry_at` is provided, the event returns to 'pending' status
    /// and will be retried after the specified time. If None, the event enters
    /// 'failed' terminal state.
    ///
    /// # Errors
    ///
    /// Returns error if update fails.
    pub async fn mark_failed(
        &self,
        event_id: EventId,
        failure_count: i32,
        next_retry_at: Option<DateTime<Utc>>,
    ) -> Result<()> {
        self.mark_failed_impl(&*self.pool, event_id, failure_count, next_retry_at).await
    }

    /// Marks an event as failed within a transaction.
    ///
    /// # Errors
    ///
    /// Returns error if update fails.
    pub async fn mark_failed_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        event_id: EventId,
        failure_count: i32,
        next_retry_at: Option<DateTime<Utc>>,
    ) -> Result<()> {
        self.mark_failed_impl(&mut **tx, event_id, failure_count, next_retry_at).await
    }

    /// Private helper for marking events as failed with generic executor.
    async fn mark_failed_impl<'e, E>(
        &self,
        executor: E,
        event_id: EventId,
        failure_count: i32,
        next_retry_at: Option<DateTime<Utc>>,
    ) -> Result<()>
    where
        E: Executor<'e, Database = Postgres>,
    {
        let status =
            if next_retry_at.is_some() { EventStatus::Pending } else { EventStatus::Failed };

        sqlx::query(
            r#"
            UPDATE webhook_events
            SET status = $1,
                failure_count = $2,
                next_retry_at = $3,
                failed_at = CASE WHEN $3 IS NULL THEN NOW() ELSE NULL END
            WHERE id = $4
            "#,
        )
        .bind(status.to_string())
        .bind(failure_count)
        .bind(next_retry_at)
        .bind(event_id.0)
        .execute(executor)
        .await?;

        Ok(())
    }

    /// Finds an event by ID.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_by_id(&self, event_id: EventId) -> Result<Option<WebhookEvent>> {
        self.find_by_id_impl(&*self.pool, event_id).await
    }

    /// Finds an event by ID within a transaction.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_by_id_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        event_id: EventId,
    ) -> Result<Option<WebhookEvent>> {
        self.find_by_id_impl(&mut **tx, event_id).await
    }

    /// Private helper for finding events by ID with generic executor.
    async fn find_by_id_impl<'e, E>(
        &self,
        executor: E,
        event_id: EventId,
    ) -> Result<Option<WebhookEvent>>
    where
        E: Executor<'e, Database = Postgres>,
    {
        let event = sqlx::query_as::<_, WebhookEvent>(
            r#"
            SELECT id, tenant_id, endpoint_id, source_event_id, idempotency_strategy,
                   status, failure_count, last_attempt_at, next_retry_at,
                   headers, body, content_type, received_at, delivered_at, failed_at,
                   payload_size, signature_valid, signature_error
            FROM webhook_events
            WHERE id = $1
            "#,
        )
        .bind(event_id.0)
        .fetch_optional(executor)
        .await?;

        Ok(event)
    }

    /// Finds all events for a tenant.
    ///
    /// Returns events ordered by received_at descending (newest first).
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_by_tenant(
        &self,
        tenant_id: TenantId,
        limit: Option<i64>,
    ) -> Result<Vec<WebhookEvent>> {
        let events = sqlx::query_as::<_, WebhookEvent>(
            r#"
            SELECT id, tenant_id, endpoint_id, source_event_id, idempotency_strategy,
                   status, failure_count, last_attempt_at, next_retry_at,
                   headers, body, content_type, received_at, delivered_at, failed_at,
                   payload_size, signature_valid, signature_error
            FROM webhook_events
            WHERE tenant_id = $1
            ORDER BY received_at DESC
            LIMIT $2
            "#,
        )
        .bind(tenant_id.0)
        .bind(limit.unwrap_or(100))
        .fetch_all(&*self.pool)
        .await?;

        Ok(events)
    }

    /// Finds all events for an endpoint.
    ///
    /// Returns events ordered by received_at descending (newest first).
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_by_endpoint(
        &self,
        endpoint_id: EndpointId,
        limit: Option<i64>,
    ) -> Result<Vec<WebhookEvent>> {
        let events = sqlx::query_as::<_, WebhookEvent>(
            r#"
            SELECT id, tenant_id, endpoint_id, source_event_id, idempotency_strategy,
                   status, failure_count, last_attempt_at, next_retry_at,
                   headers, body, content_type, received_at, delivered_at, failed_at,
                   payload_size, signature_valid, signature_error
            FROM webhook_events
            WHERE endpoint_id = $1
            ORDER BY received_at DESC
            LIMIT $2
            "#,
        )
        .bind(endpoint_id.0)
        .bind(limit.unwrap_or(100))
        .fetch_all(&*self.pool)
        .await?;

        Ok(events)
    }

    /// Updates the status of an event.
    ///
    /// # Errors
    ///
    /// Returns error if update fails.
    pub async fn update_status(&self, event_id: EventId, status: EventStatus) -> Result<()> {
        self.update_status_impl(&*self.pool, event_id, status).await
    }

    /// Updates the status of an event within a transaction.
    ///
    /// # Errors
    ///
    /// Returns error if update fails.
    pub async fn update_status_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        event_id: EventId,
        status: EventStatus,
    ) -> Result<()> {
        self.update_status_impl(&mut **tx, event_id, status).await
    }

    /// Private helper for updating event status with generic executor.
    async fn update_status_impl<'e, E>(
        &self,
        executor: E,
        event_id: EventId,
        status: EventStatus,
    ) -> Result<()>
    where
        E: Executor<'e, Database = Postgres>,
    {
        sqlx::query(
            r#"
            UPDATE webhook_events
            SET status = $1
            WHERE id = $2
            "#,
        )
        .bind(status.to_string())
        .bind(event_id.0)
        .execute(executor)
        .await?;

        Ok(())
    }

    /// Counts events by status.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn count_by_status(&self, status: EventStatus) -> Result<i64> {
        let count: (i64,) = sqlx::query_as(
            r#"
            SELECT COUNT(*) FROM webhook_events
            WHERE status = $1
            "#,
        )
        .bind(status.to_string())
        .fetch_one(&*self.pool)
        .await?;

        Ok(count.0)
    }

    /// Counts all events for a tenant.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn count_by_tenant(&self, tenant_id: TenantId) -> Result<i64> {
        let count: (i64,) = sqlx::query_as(
            r#"
            SELECT COUNT(*) FROM webhook_events
            WHERE tenant_id = $1
            "#,
        )
        .bind(tenant_id.0)
        .fetch_one(&*self.pool)
        .await?;

        Ok(count.0)
    }

    /// Deletes all events for a tenant.
    ///
    /// Used for tenant cleanup or GDPR compliance. This operation is
    /// irreversible.
    ///
    /// # Errors
    ///
    /// Returns error if delete fails.
    pub async fn delete_by_tenant(&self, tenant_id: TenantId) -> Result<u64> {
        self.delete_by_tenant_impl(&*self.pool, tenant_id).await
    }

    /// Deletes all events for a tenant within a transaction.
    ///
    /// # Errors
    ///
    /// Returns error if delete fails.
    pub async fn delete_by_tenant_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        tenant_id: TenantId,
    ) -> Result<u64> {
        self.delete_by_tenant_impl(&mut **tx, tenant_id).await
    }

    /// Private helper for deleting events by tenant with generic executor.
    async fn delete_by_tenant_impl<'e, E>(&self, executor: E, tenant_id: TenantId) -> Result<u64>
    where
        E: Executor<'e, Database = Postgres>,
    {
        let result = sqlx::query(
            r#"
            DELETE FROM webhook_events
            WHERE tenant_id = $1
            "#,
        )
        .bind(tenant_id.0)
        .execute(executor)
        .await?;

        Ok(result.rows_affected())
    }

    /// Checks for duplicate events based on idempotency key.
    ///
    /// Used to implement exactly-once processing semantics. Returns the
    /// existing event if a duplicate is found within the deduplication window.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_duplicate(
        &self,
        endpoint_id: EndpointId,
        source_event_id: &str,
    ) -> Result<Option<WebhookEvent>> {
        let event = sqlx::query_as::<_, WebhookEvent>(
            r#"
            SELECT id, tenant_id, endpoint_id, source_event_id, idempotency_strategy,
                   status, failure_count, last_attempt_at, next_retry_at,
                   headers, body, content_type, received_at, delivered_at, failed_at,
                   payload_size, signature_valid, signature_error
            FROM webhook_events
            WHERE endpoint_id = $1
              AND source_event_id = $2
              AND received_at > NOW() - INTERVAL '24 hours'
            "#,
        )
        .bind(endpoint_id.0)
        .bind(source_event_id)
        .fetch_optional(&*self.pool)
        .await?;

        Ok(event)
    }

    /// Moves events to dead letter status after max retries exceeded.
    ///
    /// Terminal state for events that cannot be delivered after exhausting
    /// all retry attempts.
    ///
    /// # Errors
    ///
    /// Returns error if update fails.
    pub async fn move_to_dead_letter(&self, event_id: EventId) -> Result<()> {
        self.move_to_dead_letter_impl(&*self.pool, event_id).await
    }

    /// Moves an event to dead letter status within a transaction.
    ///
    /// # Errors
    ///
    /// Returns error if update fails.
    pub async fn move_to_dead_letter_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        event_id: EventId,
    ) -> Result<()> {
        self.move_to_dead_letter_impl(&mut **tx, event_id).await
    }

    /// Private helper for moving events to dead letter with generic executor.
    async fn move_to_dead_letter_impl<'e, E>(&self, executor: E, event_id: EventId) -> Result<()>
    where
        E: Executor<'e, Database = Postgres>,
    {
        sqlx::query(
            r#"
            UPDATE webhook_events
            SET status = 'dead_letter', failed_at = NOW()
            WHERE id = $1 AND status = 'failed'
            "#,
        )
        .bind(event_id.0)
        .execute(executor)
        .await?;

        Ok(())
    }

    /// Finds events in dead letter status for a tenant.
    ///
    /// Used for monitoring and manual recovery of permanently failed events.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_dead_letter_by_tenant(
        &self,
        tenant_id: TenantId,
        limit: Option<i64>,
    ) -> Result<Vec<WebhookEvent>> {
        let events = sqlx::query_as::<_, WebhookEvent>(
            r#"
            SELECT id, tenant_id, endpoint_id, source_event_id, idempotency_strategy,
                   status, failure_count, last_attempt_at, next_retry_at,
                   headers, body, content_type, received_at, delivered_at, failed_at,
                   payload_size, signature_valid, signature_error
            FROM webhook_events
            WHERE tenant_id = $1 AND status = 'dead_letter'
            ORDER BY failed_at DESC
            LIMIT $2
            "#,
        )
        .bind(tenant_id.0)
        .bind(limit.unwrap_or(100))
        .fetch_all(&*self.pool)
        .await?;

        Ok(events)
    }

    /// Retries a dead letter event by resetting it to pending status.
    ///
    /// Used for manual recovery when the underlying issue has been resolved.
    ///
    /// # Errors
    ///
    /// Returns error if update fails.
    pub async fn retry_dead_letter(&self, event_id: EventId) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE webhook_events
            SET status = 'pending',
                failure_count = 0,
                next_retry_at = NULL,
                failed_at = NULL
            WHERE id = $1 AND status = 'dead_letter'
            "#,
        )
        .bind(event_id.0)
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
