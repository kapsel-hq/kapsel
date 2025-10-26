//! Database helper methods for TestEnv

use std::time::Duration;

use anyhow::{Context, Result};
use sqlx::Row;
use uuid::Uuid;

use crate::{EndpointId, EventId, TenantId, TestEnv, TestWebhook, WebhookEventData};

impl TestEnv {
    /// Create tenant within a transaction.
    pub async fn create_tenant_tx<'a, E>(&self, executor: E, name: &str) -> Result<TenantId>
    where
        E: sqlx::Executor<'a, Database = sqlx::Postgres>,
    {
        self.create_tenant_with_plan_tx(executor, name, "enterprise").await
    }

    /// Create tenant with specific plan within a transaction.
    ///
    /// # Errors
    ///
    /// Returns error if database insert fails.
    pub async fn create_tenant_with_plan_tx<'a, E>(
        &self,
        executor: E,
        name: &str,
        plan: &str,
    ) -> Result<TenantId>
    where
        E: sqlx::Executor<'a, Database = sqlx::Postgres>,
    {
        let tenant_id = Uuid::new_v4();
        let unique_name = format!("{}-{}-{}", name, self.test_run_id, tenant_id.simple());

        sqlx::query(
            "INSERT INTO tenants (id, name, tier, created_at, updated_at)
             VALUES ($1, $2, $3, NOW(), NOW())",
        )
        .bind(tenant_id)
        .bind(unique_name)
        .bind(plan)
        .execute(executor)
        .await
        .context("failed to create test tenant")?;

        Ok(TenantId(tenant_id))
    }

    /// Create endpoint within a transaction.
    pub async fn create_endpoint_tx<'a, E>(
        &self,
        executor: E,
        tenant_id: TenantId,
        url: &str,
    ) -> Result<EndpointId>
    where
        E: sqlx::Executor<'a, Database = sqlx::Postgres>,
    {
        self.create_endpoint_with_config_tx(executor, tenant_id, url, "test-endpoint", 10, 30).await
    }

    /// Create endpoint with full configuration within a transaction.
    pub async fn create_endpoint_with_config_tx<'a, E>(
        &self,
        executor: E,
        tenant_id: TenantId,
        url: &str,
        name: &str,
        max_retries: i32,
        timeout_seconds: i32,
    ) -> Result<EndpointId>
    where
        E: sqlx::Executor<'a, Database = sqlx::Postgres>,
    {
        let endpoint_id = Uuid::new_v4();
        let unique_name = format!("{}-{}-{}", name, self.test_run_id, endpoint_id.simple());

        sqlx::query(
            "INSERT INTO endpoints (id, tenant_id, name, url, max_retries, timeout_seconds, circuit_state, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, NOW(), NOW())",
        )
        .bind(endpoint_id)
        .bind(tenant_id.0)
        .bind(unique_name)
        .bind(url)
        .bind(max_retries)
        .bind(timeout_seconds)
        .bind("closed")
        .execute(executor)
        .await
        .context("failed to create test endpoint")?;

        Ok(EndpointId(endpoint_id))
    }

    /// Create API key within a transaction.
    ///
    /// Returns both the API key string and its hash.
    pub async fn create_api_key_tx<'a, E>(
        &self,
        executor: E,
        tenant_id: TenantId,
        name: &str,
    ) -> Result<(String, String)>
    where
        E: sqlx::Executor<'a, Database = sqlx::Postgres>,
    {
        let unique_suffix = Uuid::new_v4().simple();
        let api_key = format!("{}-{}-{}", name, self.test_run_id, unique_suffix);
        let key_hash = sha256::digest(api_key.as_bytes());

        sqlx::query(
            "INSERT INTO api_keys (tenant_id, key_hash, name, created_at)
             VALUES ($1, $2, $3, NOW())",
        )
        .bind(tenant_id.0)
        .bind(&key_hash)
        .bind(&api_key)
        .execute(executor)
        .await
        .context("failed to create test API key")?;

        Ok((api_key, key_hash))
    }

    /// Ingests a test webhook, persisting it to the database transaction.
    ///
    /// Handles idempotency by returning existing event ID if duplicate
    /// source_event_id.
    pub async fn ingest_webhook_tx<'a, E>(
        &self,
        executor: E,
        webhook: &TestWebhook,
    ) -> Result<EventId>
    where
        E: sqlx::Executor<'a, Database = sqlx::Postgres>,
    {
        let event_id = Uuid::new_v4();
        let body_bytes = webhook.body.clone();
        let payload_size = i32::try_from(body_bytes.len()).unwrap_or(i32::MAX).max(1);

        let result: (Uuid,) = sqlx::query_as(
            "INSERT INTO webhook_events
             (id, tenant_id, endpoint_id, source_event_id, idempotency_strategy,
              status, failure_count, headers, body, content_type, payload_size, received_at)
             VALUES ($1, $2, $3, $4, $5, 'pending', 0, $6, $7, $8, $9, $10)
             ON CONFLICT (tenant_id, endpoint_id, source_event_id)
             DO UPDATE SET id = webhook_events.id
             RETURNING id",
        )
        .bind(event_id)
        .bind(webhook.tenant_id)
        .bind(webhook.endpoint_id)
        .bind(&webhook.source_event_id)
        .bind(&webhook.idempotency_strategy)
        .bind(serde_json::to_value(&webhook.headers)?)
        .bind(&body_bytes[..])
        .bind(&webhook.content_type)
        .bind(payload_size)
        .bind(chrono::DateTime::<chrono::Utc>::from(self.now_system()))
        .fetch_one(executor)
        .await
        .context("failed to ingest test webhook")?;

        Ok(EventId(result.0))
    }

    /// Ingests a test webhook directly to the database pool.
    pub async fn ingest_webhook(&self, webhook: &TestWebhook) -> Result<EventId> {
        self.ingest_webhook_tx(self.pool(), webhook).await
    }

    /// Finds the current status of a webhook event from the database.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails or event not found.
    pub async fn find_webhook_status(&self, event_id: EventId) -> Result<String> {
        // Add timeout to prevent hanging
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            sqlx::query_as::<_, (String,)>("SELECT status FROM webhook_events WHERE id = $1")
                .bind(event_id.0)
                .fetch_one(self.pool()),
        )
        .await
        .context("timeout waiting for webhook status query")?;

        let status = result.with_context(|| {
            format!("failed to find webhook status for event_id: {}", event_id.0)
        })?;

        Ok(status.0)
    }

    /// Waits for a webhook event to reach the expected status.
    ///
    /// Polls the database until the event reaches the expected status or the
    /// timeout is reached. This is useful for tests that need to wait for
    /// background processing to complete.
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - Database query fails
    /// - Event not found
    /// - Timeout is reached before status matches
    pub async fn wait_for_event_status(
        &self,
        event_id: EventId,
        expected_status: &str,
        timeout: Duration,
    ) -> Result<()> {
        let start = std::time::Instant::now();

        loop {
            match self.find_webhook_status(event_id).await {
                Ok(current_status) => {
                    if current_status == expected_status {
                        return Ok(());
                    }
                },
                Err(e) => {
                    // Event might not exist yet, keep polling
                    tracing::debug!("Status check failed: {}", e);
                },
            }

            if start.elapsed() > timeout {
                // Get current status for better error message
                let current = self
                    .find_webhook_status(event_id)
                    .await
                    .unwrap_or_else(|_| "NOT_FOUND".to_string());

                anyhow::bail!(
                    "Timeout waiting for event {} to reach status '{}'. Current status: '{}'",
                    event_id.0,
                    expected_status,
                    current
                );
            }

            // Short sleep to avoid hammering the database
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Counts the number of delivery attempts for a webhook event.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn count_delivery_attempts(&self, event_id: EventId) -> Result<u32> {
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            sqlx::query_as::<_, (i64,)>(
                "SELECT COUNT(*) FROM delivery_attempts WHERE event_id = $1",
            )
            .bind(event_id.0)
            .fetch_one(self.pool()),
        )
        .await
        .context("timeout waiting for delivery attempts count")?;

        let (count,) = result.with_context(|| {
            format!("failed to count delivery attempts for event_id: {}", event_id.0)
        })?;

        u32::try_from(count).context("delivery attempt count too large")
    }

    /// Count rows in a table by ID.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails or ID not found.
    pub async fn count_by_id(&self, table: &str, column: &str, id: Uuid) -> Result<i64> {
        let query = format!("SELECT COUNT(*) FROM {table} WHERE {column} = $1");

        let row = sqlx::query(&query)
            .bind(id)
            .fetch_one(self.pool())
            .await
            .context("failed to count by id")?;

        let count: i64 = row.try_get(0)?;
        Ok(count)
    }

    /// Check if database connection is healthy.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn database_health_check(&self) -> Result<bool> {
        let result = sqlx::query("SELECT 1 as health").fetch_one(self.pool()).await;
        Ok(result.is_ok())
    }

    /// List all tables in the database schema.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn list_tables(&self) -> Result<Vec<String>> {
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT table_name FROM information_schema.tables
             WHERE table_schema = 'public'
             AND table_type = 'BASE TABLE'
             ORDER BY table_name",
        )
        .fetch_all(self.pool())
        .await
        .context("failed to list tables")?;

        Ok(tables)
    }

    /// Count total number of webhook events.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn count_total_events(&self) -> Result<i64> {
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM webhook_events")
            .fetch_one(self.pool())
            .await
            .context("failed to count total events")?;
        Ok(count.0)
    }

    /// Count events in terminal states (delivered, failed, dead_letter).
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn count_terminal_events(&self) -> Result<i64> {
        let count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM webhook_events WHERE status IN ('delivered', 'failed', 'dead_letter')",
        )
        .fetch_one(self.pool())
        .await
        .context("failed to count terminal events")?;
        Ok(count.0)
    }

    /// Count events currently being processed (delivering).
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn count_processing_events(&self) -> Result<i64> {
        let count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM webhook_events WHERE status = 'delivering'")
                .fetch_one(self.pool())
                .await
                .context("failed to count processing events")?;
        Ok(count.0)
    }

    /// Count events in pending state (waiting for delivery).
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn count_pending_events(&self) -> Result<i64> {
        let count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM webhook_events WHERE status = 'pending'")
                .fetch_one(self.pool())
                .await
                .context("failed to count pending events")?;
        Ok(count.0)
    }

    /// Get all webhook events for invariant checking.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn get_all_events(&self) -> Result<Vec<WebhookEventData>> {
        let events: Vec<WebhookEventData> = sqlx::query_as(
            "SELECT
                id, tenant_id, endpoint_id, source_event_id, idempotency_strategy,
                status, failure_count, last_attempt_at, next_retry_at,
                headers, body, content_type, payload_size,
                signature_valid, signature_error,
                received_at, delivered_at, failed_at
             FROM webhook_events ORDER BY received_at",
        )
        .fetch_all(self.pool())
        .await
        .context("failed to fetch all events")?;
        Ok(events)
    }
}
