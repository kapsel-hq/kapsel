//! Repository for delivery attempt database operations.
//!
//! Tracks all webhook delivery attempts for auditing, debugging, and analytics.
//! Each attempt captures the full request/response cycle including headers,
//! status codes, and error details.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sqlx::{Executor, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::{
    error::Result,
    models::{DeliveryAttempt, EndpointId, EventId},
};

/// Repository for delivery attempt database operations.
///
/// Provides immutable audit trail of all delivery attempts with full
/// request/response details for debugging and compliance.
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

    /// Records a new delivery attempt.
    ///
    /// Delivery attempts are immutable once created - we never modify audit
    /// records. Each attempt captures the complete request/response cycle.
    ///
    /// # Errors
    ///
    /// Returns error if insert fails.
    pub async fn create(&self, attempt: &DeliveryAttempt) -> Result<Uuid> {
        self.create_impl(&*self.pool, attempt).await
    }

    /// Records a delivery attempt within a transaction.
    ///
    /// # Errors
    ///
    /// Returns error if insert fails.
    pub async fn create_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        attempt: &DeliveryAttempt,
    ) -> Result<Uuid> {
        self.create_impl(&mut **tx, attempt).await
    }

    /// Private helper for creating delivery attempts with generic executor.
    async fn create_impl<'e, E>(&self, executor: E, attempt: &DeliveryAttempt) -> Result<Uuid>
    where
        E: Executor<'e, Database = Postgres>,
    {
        let id = sqlx::query_scalar(
            r"
            INSERT INTO delivery_attempts (
                id, event_id, attempt_number, endpoint_id,
                request_headers, request_body,
                response_status, response_headers, response_body,
                attempted_at, succeeded, error_message
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12
            )
            RETURNING id
            ",
        )
        .bind(attempt.id)
        .bind(attempt.event_id.0)
        .bind(i32::try_from(attempt.attempt_number).unwrap_or(i32::MAX))
        .bind(attempt.endpoint_id.0)
        .bind(sqlx::types::Json(&attempt.request_headers))
        .bind(&attempt.request_body)
        .bind(attempt.response_status)
        .bind(attempt.response_headers.as_ref().map(sqlx::types::Json))
        .bind(&attempt.response_body)
        .bind(attempt.attempted_at)
        .bind(attempt.succeeded)
        .bind(&attempt.error_message)
        .fetch_one(executor)
        .await?;

        Ok(id)
    }

    /// Finds all delivery attempts for an event.
    ///
    /// Returns attempts ordered by attempt_number ascending to show the
    /// chronological progression of delivery attempts.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_by_event(&self, event_id: EventId) -> Result<Vec<DeliveryAttempt>> {
        self.find_by_event_impl(&*self.pool, event_id).await
    }

    /// Finds delivery attempts for an event within a transaction.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_by_event_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        event_id: EventId,
    ) -> Result<Vec<DeliveryAttempt>> {
        self.find_by_event_impl(&mut **tx, event_id).await
    }

    /// Private helper for finding attempts by event with generic executor.
    async fn find_by_event_impl<'e, E>(
        &self,
        executor: E,
        event_id: EventId,
    ) -> Result<Vec<DeliveryAttempt>>
    where
        E: Executor<'e, Database = Postgres>,
    {
        let attempts = sqlx::query_as::<_, DeliveryAttempt>(
            r"
            SELECT id, event_id, attempt_number, endpoint_id,
                   request_headers, request_body,
                   response_status, response_headers, response_body,
                   error_message, succeeded, attempted_at
            FROM delivery_attempts
            WHERE event_id = $1
            ORDER BY attempt_number ASC
            ",
        )
        .bind(event_id.0)
        .fetch_all(executor)
        .await?;

        Ok(attempts)
    }

    /// Finds the most recent delivery attempt for an event.
    ///
    /// Used to get the current state of delivery without fetching all attempts.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_latest_by_event(&self, event_id: EventId) -> Result<Option<DeliveryAttempt>> {
        let attempt = sqlx::query_as::<_, DeliveryAttempt>(
            r"
            SELECT id, event_id, attempt_number, endpoint_id,
                   request_headers, request_body,
                   response_status, response_headers, response_body,
                   error_message, succeeded, attempted_at
            FROM delivery_attempts
            WHERE event_id = $1
            ORDER BY attempt_number DESC
            LIMIT 1
            ",
        )
        .bind(event_id.0)
        .fetch_optional(&*self.pool)
        .await?;

        Ok(attempt)
    }

    /// Counts total delivery attempts for an event.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn count_by_event(&self, event_id: EventId) -> Result<i64> {
        let count: (i64,) = sqlx::query_as(
            r"
            SELECT COUNT(*) FROM delivery_attempts
            WHERE event_id = $1
            ",
        )
        .bind(event_id.0)
        .fetch_one(&*self.pool)
        .await?;

        Ok(count.0)
    }

    /// Counts successful delivery attempts for an event.
    ///
    /// A successful attempt is one with a 2xx HTTP status code.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn count_successful_by_event(&self, event_id: EventId) -> Result<i64> {
        let count: (i64,) = sqlx::query_as(
            r"
            SELECT COUNT(*) FROM delivery_attempts
            WHERE event_id = $1 AND succeeded = true
            ",
        )
        .bind(event_id.0)
        .fetch_one(&*self.pool)
        .await?;

        Ok(count.0)
    }

    /// Finds all delivery attempts for an endpoint.
    ///
    /// Returns attempts ordered by attempted_at descending (newest first).
    /// Used for endpoint-level debugging and analytics.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_by_endpoint(
        &self,
        endpoint_id: EndpointId,
        limit: Option<i64>,
    ) -> Result<Vec<DeliveryAttempt>> {
        let attempts = sqlx::query_as::<_, DeliveryAttempt>(
            r"
            SELECT da.id, da.event_id, da.attempt_number, da.endpoint_id,
                   da.request_headers, da.request_body,
                   da.response_status, da.response_headers, da.response_body,
                   da.error_message, da.succeeded, da.attempted_at
            FROM delivery_attempts da
            WHERE da.attempted_at >= $1 AND da.attempted_at < $2
            ORDER BY da.attempted_at ASC
            LIMIT $3
            ",
        )
        .bind(endpoint_id.0)
        .bind(limit.unwrap_or(100))
        .fetch_all(&*self.pool)
        .await?;

        Ok(attempts)
    }

    /// Finds recent failed delivery attempts for an endpoint.
    ///
    /// Failed attempts are those with succeeded = false.
    /// Used for circuit breaker decisions and failure analysis.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_recent_failures_by_endpoint(
        &self,
        endpoint_id: EndpointId,
        since: DateTime<Utc>,
        limit: Option<i64>,
    ) -> Result<Vec<DeliveryAttempt>> {
        let attempts = sqlx::query_as::<_, DeliveryAttempt>(
            r"
            SELECT da.id, da.event_id, da.attempt_number, da.endpoint_id,
                   da.request_headers, da.request_body,
                   da.response_status, da.response_headers, da.response_body,
                   da.attempted_at, da.succeeded, da.error_message
            FROM delivery_attempts da
            WHERE da.endpoint_id = $1
              AND da.attempted_at >= $2
            ORDER BY da.attempted_at DESC
            LIMIT $3
            ",
        )
        .bind(endpoint_id.0)
        .bind(since)
        .bind(limit.unwrap_or(100))
        .fetch_all(&*self.pool)
        .await?;

        Ok(attempts)
    }

    /// Counts total delivery attempts for an endpoint.
    ///
    /// Includes all attempts across all events for the endpoint.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn count_by_endpoint(&self, endpoint_id: EndpointId) -> Result<i64> {
        let count: (i64,) = sqlx::query_as(
            r"
            SELECT COUNT(*)
            FROM delivery_attempts
            WHERE endpoint_id = $1
            ",
        )
        .bind(endpoint_id.0)
        .fetch_one(&*self.pool)
        .await?;

        Ok(count.0)
    }

    /// Calculates the success rate for an endpoint.
    ///
    /// Success rate is the percentage of attempts with succeeded = true.
    /// Returns 0.0 if no attempts exist or all failed.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn success_rate_by_endpoint(
        &self,
        endpoint_id: EndpointId,
        since: Option<DateTime<Utc>>,
    ) -> Result<f64> {
        let result: (Option<f64>,) = sqlx::query_as(
            r"
            SELECT
                CASE
                    WHEN COUNT(*) = 0 THEN NULL
                    ELSE CAST(SUM(CASE WHEN succeeded THEN 1 ELSE 0 END) AS FLOAT) / COUNT(*)
                END as success_rate
            FROM delivery_attempts
            WHERE endpoint_id = $1
              AND ($2::TIMESTAMPTZ IS NULL OR attempted_at >= $2)
            ",
        )
        .bind(endpoint_id.0)
        .bind(since)
        .fetch_one(&*self.pool)
        .await?;

        Ok(result.0.unwrap_or(0.0))
    }

    /// Deletes all delivery attempts for an event.
    ///
    /// Used for data cleanup or GDPR compliance. This operation is
    /// irreversible and removes the audit trail.
    ///
    /// # Errors
    ///
    /// Returns error if delete fails.
    pub async fn delete_by_event(&self, event_id: EventId) -> Result<u64> {
        self.delete_by_event_impl(&*self.pool, event_id).await
    }

    /// Deletes delivery attempts for an event within a transaction.
    ///
    /// # Errors
    ///
    /// Returns error if delete fails.
    pub async fn delete_by_event_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        event_id: EventId,
    ) -> Result<u64> {
        self.delete_by_event_impl(&mut **tx, event_id).await
    }

    /// Private helper for deleting attempts by event with generic executor.
    async fn delete_by_event_impl<'e, E>(&self, executor: E, event_id: EventId) -> Result<u64>
    where
        E: Executor<'e, Database = Postgres>,
    {
        let result = sqlx::query(
            r"
            DELETE FROM delivery_attempts
            WHERE event_id = $1
            ",
        )
        .bind(event_id.0)
        .execute(executor)
        .await?;

        Ok(result.rows_affected())
    }

    /// Finds delivery attempts by response status.
    ///
    /// Used for analyzing specific error patterns (e.g., all 429 rate limit
    /// responses).
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_by_response_status(
        &self,
        status_code: i32,
        limit: Option<i64>,
    ) -> Result<Vec<DeliveryAttempt>> {
        let attempts = sqlx::query_as::<_, DeliveryAttempt>(
            r"
            SELECT id, event_id, attempt_number, endpoint_id,
                   request_headers, request_body,
                   response_status, response_headers, response_body,
                   error_message, succeeded, attempted_at
            FROM delivery_attempts
            WHERE attempted_at < $1
            ORDER BY attempted_at ASC
            LIMIT $2
            ",
        )
        .bind(status_code)
        .bind(limit.unwrap_or(100))
        .fetch_all(&*self.pool)
        .await?;

        Ok(attempts)
    }

    /// Finds failed delivery attempts with error messages.
    ///
    /// Used for diagnosing systematic issues by filtering on error patterns.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_failed_with_errors(
        &self,
        endpoint_id: EndpointId,
        limit: Option<i64>,
    ) -> Result<Vec<DeliveryAttempt>> {
        let attempts = sqlx::query_as::<_, DeliveryAttempt>(
            r"
            SELECT id, event_id, attempt_number, endpoint_id,
                   request_headers, request_body,
                   response_status, response_headers, response_body,
                   attempted_at, succeeded, error_message
            FROM delivery_attempts
            WHERE endpoint_id = $1
              AND succeeded = false
              AND error_message IS NOT NULL
            ORDER BY attempted_at DESC
            LIMIT $2
            ",
        )
        .bind(endpoint_id.0)
        .bind(limit.unwrap_or(100))
        .fetch_all(&*self.pool)
        .await?;

        Ok(attempts)
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
