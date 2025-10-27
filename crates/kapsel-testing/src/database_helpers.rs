//! Database helper methods for TestEnv

use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use chrono::Utc;
use kapsel_core::{
    models::{
        BackoffStrategy, CircuitState, Endpoint, EventStatus, IdempotencyStrategy, SignatureConfig,
        Tenant, WebhookEvent,
    },
    storage::api_keys::ApiKey,
};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use crate::{EndpointId, EventId, TenantId, TestEnv, TestWebhook, WebhookEventData};

impl TestEnv {
    /// Create tenant within a transaction.
    pub async fn create_tenant_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        name: &str,
    ) -> Result<TenantId> {
        self.create_tenant_with_plan_tx(tx, name, "free").await
    }

    /// Create tenant with specific plan within a transaction.
    ///
    /// # Errors
    ///
    /// Returns error if database insert fails.
    /// Create tenant with plan within a transaction.
    pub async fn create_tenant_with_plan_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        name: &str,
        plan: &str,
    ) -> Result<TenantId> {
        let tenant_id = TenantId::new();
        let unique_name = format!("{}-{}-{}", name, self.test_run_id, tenant_id.0.simple());
        let now = Utc::now();

        let tenant = Tenant {
            id: tenant_id,
            name: unique_name,
            tier: plan.to_string(),
            max_events_per_month: 100_000, // Default for testing
            max_endpoints: 100,            // Default for testing
            events_this_month: 0,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            stripe_customer_id: None,
            stripe_subscription_id: None,
        };

        self.storage()
            .tenants
            .create_in_tx(tx, &tenant)
            .await
            .context("failed to create test tenant")?;

        Ok(tenant_id)
    }

    /// Create endpoint within a transaction.
    pub async fn create_endpoint_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        tenant_id: TenantId,
        url: &str,
    ) -> Result<EndpointId> {
        self.create_endpoint_with_config_tx(tx, tenant_id, url, "test-endpoint", 10, 30).await
    }

    /// Create endpoint with full configuration within a transaction.
    pub async fn create_endpoint_with_config_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        tenant_id: TenantId,
        url: &str,
        name: &str,
        max_retries: i32,
        timeout_seconds: i32,
    ) -> Result<EndpointId> {
        let endpoint_id = EndpointId::new();
        let unique_name = format!("{}-{}-{}", name, self.test_run_id, endpoint_id.0.simple());
        let now = Utc::now();

        let endpoint = Endpoint {
            id: endpoint_id,
            tenant_id,
            name: unique_name,
            url: url.to_string(),
            is_active: true,
            signature_config: SignatureConfig::None,
            max_retries,
            timeout_seconds,
            retry_strategy: BackoffStrategy::Exponential,
            circuit_state: CircuitState::Closed,
            circuit_failure_count: 0,
            circuit_success_count: 0,
            circuit_last_failure_at: None,
            circuit_half_open_at: None,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            total_events_received: 0,
            total_events_delivered: 0,
            total_events_failed: 0,
        };

        self.storage()
            .endpoints
            .create_in_tx(tx, &endpoint)
            .await
            .context("failed to create test endpoint")?;

        Ok(endpoint_id)
    }

    /// Create API key within a transaction.
    ///
    /// Returns both the API key string and its hash.
    pub async fn create_api_key_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        tenant_id: TenantId,
        name: &str,
    ) -> Result<(String, String)> {
        let unique_suffix = Uuid::new_v4().simple();
        let api_key = format!("{}-{}-{}", name, self.test_run_id, unique_suffix);
        let key_hash = sha256::digest(api_key.as_bytes());
        let now = chrono::Utc::now();

        let api_key_record = ApiKey {
            id: Uuid::new_v4(),
            tenant_id,
            key_hash: key_hash.clone(),
            name: api_key.clone(),
            expires_at: None,
            revoked_at: None,
            last_used_at: None,
            created_at: now,
        };

        self.storage()
            .api_keys
            .create_in_tx(tx, &api_key_record)
            .await
            .context("failed to create test API key")?;

        Ok((api_key, key_hash))
    }

    /// Ingests a test webhook, persisting it to the database transaction.
    ///
    /// Handles idempotency by returning existing event ID if duplicate
    /// source_event_id.
    pub async fn ingest_webhook_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        webhook: &TestWebhook,
    ) -> Result<EventId> {
        // Check for existing duplicate first
        if let Some(existing_event) = self
            .storage()
            .webhook_events
            .find_duplicate(webhook.endpoint_id.into(), &webhook.source_event_id)
            .await
            .context("failed to check for duplicate webhook")?
        {
            return Ok(existing_event.id);
        }

        let event_id = EventId::new();
        let body_bytes = webhook.body.to_vec();
        let payload_size = i32::try_from(body_bytes.len()).unwrap_or(i32::MAX).max(1);

        let event = WebhookEvent {
            id: event_id,
            tenant_id: webhook.tenant_id.into(),
            endpoint_id: webhook.endpoint_id.into(),
            source_event_id: webhook.source_event_id.clone(),
            idempotency_strategy: IdempotencyStrategy::SourceId,
            status: EventStatus::Pending,
            failure_count: 0,
            last_attempt_at: None,
            next_retry_at: None,
            headers: sqlx::types::Json(webhook.headers.clone()),
            body: body_bytes,
            content_type: webhook.content_type.clone(),
            received_at: Utc::now(),
            delivered_at: None,
            failed_at: None,
            payload_size,
            signature_valid: None,
            signature_error: None,
        };

        self.storage()
            .webhook_events
            .create_in_tx(tx, &event)
            .await
            .context("failed to create test webhook event")?;

        Ok(event_id)
    }

    /// Ingests a test webhook directly to the database pool.
    pub async fn ingest_webhook(&self, webhook: &TestWebhook) -> Result<EventId> {
        let mut tx = self.pool().begin().await?;
        let event_id = self.ingest_webhook_tx(&mut tx, webhook).await?;
        tx.commit().await?;
        Ok(event_id)
    }

    /// Finds the current status of a webhook event from the database.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails or event not found.
    pub async fn find_webhook_status(&self, event_id: EventId) -> Result<EventStatus> {
        // Add timeout to prevent hanging
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            self.storage().webhook_events.find_by_id(event_id),
        )
        .await
        .context("timeout waiting for webhook status query")?;

        let event = result.with_context(|| {
            format!("failed to find webhook status for event_id: {}", event_id.0)
        })?;

        match event {
            Some(webhook_event) => Ok(webhook_event.status),
            None => Err(anyhow::anyhow!("webhook event {} not found", event_id.0)),
        }
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
        expected_status: EventStatus,
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
                let current =
                    self.find_webhook_status(event_id).await.unwrap_or(EventStatus::Failed); // Default to Failed for error reporting

                anyhow::bail!(
                    "Timeout waiting for event {} to reach status '{:?}'. Current status: '{:?}'",
                    event_id.0,
                    expected_status,
                    current
                );
            }

            // Short sleep to avoid hammering the database
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Create tenant directly (convenience method).
    ///
    /// This is a convenience wrapper around `create_tenant_tx` that handles
    /// the transaction automatically.
    pub async fn create_tenant(&self, name: &str) -> Result<TenantId> {
        let mut tx = self.pool().begin().await?;
        let tenant_id = self.create_tenant_tx(&mut tx, name).await?;
        tx.commit().await?;
        Ok(tenant_id)
    }

    /// Create endpoint directly (convenience method).
    ///
    /// This is a convenience wrapper around `create_endpoint_tx` that handles
    /// the transaction automatically.
    pub async fn create_endpoint(&self, tenant_id: TenantId, url: &str) -> Result<EndpointId> {
        let mut tx = self.pool().begin().await?;
        let endpoint_id = self.create_endpoint_tx(&mut tx, tenant_id, url).await?;
        tx.commit().await?;
        Ok(endpoint_id)
    }

    /// Get the current status of a webhook event (convenience method).
    ///
    /// This is an alias for `find_webhook_status` for better test readability.
    pub async fn event_status(&self, event_id: EventId) -> Result<EventStatus> {
        self.find_webhook_status(event_id).await
    }

    /// Create API key directly (convenience method).
    ///
    /// This is a convenience wrapper around `create_api_key_tx` that handles
    /// the transaction automatically.
    pub async fn create_api_key(
        &self,
        tenant_id: TenantId,
        name: &str,
    ) -> Result<(String, String)> {
        let mut tx = self.pool().begin().await?;
        let result = self.create_api_key_tx(&mut tx, tenant_id, name).await?;
        tx.commit().await?;
        Ok(result)
    }

    /// Ingest webhook with endpoint and payload (convenience method).
    ///
    /// Creates a TestWebhook from the endpoint and payload, then ingests it.
    pub async fn ingest_webhook_simple(
        &self,
        endpoint_id: EndpointId,
        payload: &[u8],
    ) -> Result<EventId> {
        use std::collections::HashMap;

        // Get tenant_id from endpoint
        let endpoint = self
            .storage()
            .endpoints
            .find_by_id(endpoint_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("endpoint not found: {}", endpoint_id.0))?;

        let webhook = TestWebhook {
            tenant_id: endpoint.tenant_id.0,
            endpoint_id: endpoint_id.0,
            source_event_id: uuid::Uuid::new_v4().to_string(),
            idempotency_strategy: "source_id".to_string(),
            headers: HashMap::new(),
            body: Bytes::copy_from_slice(payload),
            content_type: "application/json".to_string(),
        };

        self.ingest_webhook(&webhook).await
    }

    /// Counts the number of delivery attempts for a webhook event.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn count_delivery_attempts(&self, event_id: EventId) -> Result<u32> {
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            self.storage().delivery_attempts.count_by_event(event_id),
        )
        .await
        .context("timeout waiting for delivery attempts count")?;

        let count = result.with_context(|| {
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
        Ok(self.storage().health_check().await.is_ok())
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
        self.storage().webhook_events.count_all().await.map_err(Into::into)
    }

    /// Count events in terminal states (delivered, failed, dead_letter).
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn count_terminal_events(&self) -> Result<i64> {
        self.storage().webhook_events.count_terminal().await.map_err(Into::into)
    }

    /// Count events currently being processed (delivering).
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn count_processing_events(&self) -> Result<i64> {
        self.storage()
            .webhook_events
            .count_by_status(EventStatus::Delivering)
            .await
            .map_err(Into::into)
    }

    /// Count events in pending state (waiting for delivery).
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn count_pending_events(&self) -> Result<i64> {
        self.storage()
            .webhook_events
            .count_by_status(EventStatus::Pending)
            .await
            .map_err(Into::into)
    }

    /// Get all webhook events for invariant checking.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn get_all_events(&self) -> Result<Vec<WebhookEventData>> {
        let events = self.storage().webhook_events.list_all().await?;
        let events: Vec<WebhookEventData> = events
            .into_iter()
            .map(|e| WebhookEventData {
                id: e.id,
                tenant_id: e.tenant_id,
                endpoint_id: e.endpoint_id.0,
                source_event_id: e.source_event_id,
                idempotency_strategy: e.idempotency_strategy.to_string(),
                status: e.status,
                failure_count: e.failure_count,
                last_attempt_at: e.last_attempt_at,
                next_retry_at: e.next_retry_at,
                headers: serde_json::to_value(e.headers.0).unwrap_or_default(),
                body: e.body,
                content_type: e.content_type,
                payload_size: e.payload_size,
                signature_valid: e.signature_valid,
                signature_error: e.signature_error,
                received_at: e.received_at,
                delivered_at: e.delivered_at,
                failed_at: e.failed_at,
            })
            .collect();
        Ok(events)
    }

    /// Create endpoint with custom retry configuration.
    ///
    /// This creates an endpoint with specified max_retries, useful for testing
    /// retry exhaustion scenarios.
    pub async fn create_endpoint_with_retries(
        &self,
        tenant_id: TenantId,
        url: &str,
        max_retries: i32,
    ) -> Result<EndpointId> {
        let mut tx = self.pool().begin().await.context("failed to begin transaction")?;
        let endpoint_id = self
            .create_endpoint_with_config_tx(
                &mut tx,
                tenant_id,
                url,
                "test-endpoint",
                max_retries,
                30,
            )
            .await?;
        tx.commit().await.context("failed to commit endpoint creation")?;
        Ok(endpoint_id)
    }
}
