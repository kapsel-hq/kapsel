//! Webhook delivery simulation methods for TestEnv

use std::time::Duration;

use anyhow::{Context, Result};
use kapsel_delivery::{
    error::DeliveryError,
    retry::{BackoffStrategy, RetryContext, RetryPolicy},
};
use sqlx::Row;
use uuid::Uuid;

use crate::{DeliveryResult, EventId, ReadyWebhook, TestEnv};

impl TestEnv {
    /// Simulates a single run of the delivery worker pool.
    ///
    /// Finds pending webhooks ready for delivery and processes them.
    ///
    /// # Errors
    ///
    /// Returns error if database queries or HTTP mock recording fails.
    pub async fn run_delivery_cycle(&self) -> Result<()> {
        let ready_webhooks = self.fetch_ready_webhooks().await?;
        let webhook_count = ready_webhooks.len();

        for webhook in ready_webhooks {
            let delivery_result = self.attempt_webhook_delivery(&webhook).await?;
            self.process_delivery_result(webhook, delivery_result).await?;
        }

        tracing::debug!("Processed {webhook_count} webhooks in delivery cycle");
        tokio::task::yield_now().await;
        Ok(())
    }

    /// Runs delivery cycle with test isolation - only processes webhooks
    /// belonging to tenants created by this test run.
    ///
    /// This ensures deterministic test execution by preventing cross-test
    /// contamination where one test processes webhooks from another test.
    pub async fn run_test_isolated_delivery_cycle(&self) -> Result<()> {
        let ready_webhooks = self.fetch_test_isolated_ready_webhooks().await?;
        let webhook_count = ready_webhooks.len();

        for webhook in ready_webhooks {
            let delivery_result = self.attempt_webhook_delivery(&webhook).await?;
            self.process_delivery_result(webhook, delivery_result).await?;
        }

        tracing::debug!("Processed {webhook_count} test-isolated webhooks in delivery cycle");
        tokio::task::yield_now().await;

        Ok(())
    }

    /// Fetches webhooks ready for delivery from database.
    async fn fetch_ready_webhooks(&self) -> Result<Vec<ReadyWebhook>> {
        let now = chrono::DateTime::<chrono::Utc>::from(self.now_system());
        tracing::debug!("Fetching ready webhooks at time: {}", now);

        // Add timeout to prevent hanging on database queries
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            sqlx::query(
                "SELECT we.id, e.id as endpoint_id, e.url, we.body, we.failure_count, e.name, e.max_retries
                 FROM webhook_events we
                 JOIN endpoints e ON we.endpoint_id = e.id
                 WHERE we.status = 'pending'
                 AND (
                   (we.next_retry_at IS NULL AND we.failure_count <= e.max_retries) OR
                   (we.next_retry_at IS NOT NULL AND we.next_retry_at <= $1)
                 )
                 ORDER BY we.received_at ASC",
            )
            .bind(now)
            .fetch_all(self.pool())
        )
        .await
        .context("timeout fetching ready webhooks")?;

        let rows = result.context("failed to fetch ready webhooks")?;

        tracing::debug!("Found {} ready webhooks to process", rows.len());

        let webhooks = rows
            .into_iter()
            .map(|row| ReadyWebhook {
                event_id: EventId(row.get("id")),
                _endpoint_id: row.get("endpoint_id"),
                url: row.get("url"),
                body: row.get("body"),
                failure_count: row.get("failure_count"),
                _endpoint_name: row.get("name"),
                max_retries: row.get("max_retries"),
            })
            .collect();

        Ok(webhooks)
    }

    /// Fetches ready webhooks that belong only to this test run.
    ///
    /// Filters by tenant names that contain this test's run ID, ensuring
    /// test isolation and preventing cross-test contamination.
    async fn fetch_test_isolated_ready_webhooks(&self) -> Result<Vec<ReadyWebhook>> {
        let rows = sqlx::query(
            "SELECT we.id, e.id as endpoint_id, e.url, we.body, we.failure_count, e.name, e.max_retries
             FROM webhook_events we
             JOIN endpoints e ON we.endpoint_id = e.id
             JOIN tenants t ON e.tenant_id = t.id
             WHERE we.status = 'pending'
             AND (we.next_retry_at IS NULL OR we.next_retry_at <= $1)
             AND t.name LIKE '%' || $2 || '%'
             ORDER BY we.received_at ASC",
        )
        .bind(chrono::DateTime::<chrono::Utc>::from(self.now_system()))
        .bind(&self.test_run_id)
        .fetch_all(self.pool())
        .await
        .context("failed to fetch test-isolated ready webhooks")?;

        let webhooks = rows
            .into_iter()
            .map(|row| ReadyWebhook {
                event_id: EventId(row.get("id")),
                _endpoint_id: row.get("endpoint_id"),
                url: row.get("url"),
                body: row.get("body"),
                failure_count: row.get("failure_count"),
                _endpoint_name: row.get("name"),
                max_retries: row.get("max_retries"),
            })
            .collect();

        Ok(webhooks)
    }

    async fn attempt_webhook_delivery(&self, webhook: &ReadyWebhook) -> Result<DeliveryResult> {
        let client = reqwest::Client::new();
        let response_result = client
            .post(&webhook.url)
            .header("Content-Type", "application/json")
            .header("X-Kapsel-Event-Id", webhook.event_id.0.to_string())
            .body(webhook.body.clone())
            .send()
            .await;

        let (status_code, response_body, duration_ms, error_type) = match response_result {
            Ok(response) => {
                let status = i32::from(response.status().as_u16());
                let body = response.text().await.unwrap_or_default();
                let error_type = if status >= 400 { Some("http_error") } else { None };
                (Some(status), Some(body), 75i32, error_type)
            },
            Err(_) => (None, None, 1000i32, Some("network")),
        };

        Ok(DeliveryResult {
            status_code,
            response_body,
            duration_ms,
            error_type: error_type.map(String::from),
        })
    }

    async fn process_delivery_result(
        &self,
        webhook: ReadyWebhook,
        result: DeliveryResult,
    ) -> Result<()> {
        let attempt_number = webhook.failure_count + 1;
        let attempt_id = self.record_delivery_attempt(&webhook, &result, attempt_number).await?;

        if let Some(200..=299) = result.status_code {
            self.handle_successful_delivery(webhook, attempt_id, attempt_number).await?;
        } else {
            self.handle_failed_delivery(webhook, attempt_number, &result).await?;
        }

        Ok(())
    }

    async fn record_delivery_attempt(
        &self,
        webhook: &ReadyWebhook,
        result: &DeliveryResult,
        attempt_number: i32,
    ) -> Result<Uuid> {
        let attempt_id = Uuid::new_v4();
        let attempted_at = chrono::DateTime::<chrono::Utc>::from(self.now_system());

        sqlx::query(
            "INSERT INTO delivery_attempts
             (id, event_id, attempt_number, request_url, request_headers,
              response_status, response_body, attempted_at, duration_ms, error_type)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(attempt_id)
        .bind(webhook.event_id.0)
        .bind(attempt_number)
        .bind(&webhook.url)
        .bind(serde_json::json!({"X-Kapsel-Event-Id": webhook.event_id.0.to_string()}))
        .bind(result.status_code)
        .bind(&result.response_body)
        .bind(attempted_at)
        .bind(result.duration_ms)
        .bind(&result.error_type)
        .execute(self.pool())
        .await
        .context("failed to record delivery attempt")?;

        Ok(attempt_id)
    }

    async fn handle_successful_delivery(
        &self,
        webhook: ReadyWebhook,
        attempt_id: Uuid,
        attempt_number: i32,
    ) -> Result<()> {
        let attempted_at = chrono::DateTime::<chrono::Utc>::from(self.now_system());

        sqlx::query(
            "UPDATE webhook_events
             SET status = $1, delivered_at = $2, failure_count = $3, last_attempt_at = $4
             WHERE id = $5",
        )
        .bind("delivered")
        .bind(attempted_at)
        .bind(webhook.failure_count)
        .bind(attempted_at)
        .bind(webhook.event_id.0)
        .execute(self.pool())
        .await
        .context("failed to update event status after delivery")?;

        // Always emit attestation event if service is configured
        // This ensures proper integration between delivery and attestation systems
        if self.attestation_service.is_some() {
            tracing::debug!(
                event_id = %webhook.event_id.0,
                attempt_id = %attempt_id,
                attempt_number = attempt_number,
                "emitting delivery success event for attestation"
            );
            self.emit_attestation_event(webhook, attempt_id, attempt_number, attempted_at).await?;
        } else {
            tracing::debug!(
                event_id = %webhook.event_id.0,
                "no attestation service configured, skipping event emission"
            );
        }

        Ok(())
    }

    async fn handle_failed_delivery(
        &self,
        webhook: ReadyWebhook,
        attempt_number: i32,
        result: &DeliveryResult,
    ) -> Result<()> {
        let attempted_at = chrono::DateTime::<chrono::Utc>::from(self.now_system());

        // Create DeliveryError from result to match production behavior
        let error = match result.status_code {
            Some(status) if (400..500).contains(&status) => DeliveryError::client_error(
                status.try_into().unwrap_or_default(),
                result.error_type.clone().unwrap_or_else(|| format!("HTTP {status}")),
            ),
            Some(status) if status >= 500 => DeliveryError::server_error(
                status.try_into().unwrap_or_default(),
                result.error_type.clone().unwrap_or_else(|| format!("HTTP {status}")),
            ),
            Some(status) => DeliveryError::server_error(
                status.try_into().unwrap_or_default(),
                result.error_type.clone().unwrap_or_else(|| format!("HTTP {status}")),
            ),
            None => DeliveryError::network(
                result.error_type.clone().unwrap_or_else(|| "network error".to_string()),
            ),
        };

        // Match production worker behavior: check if error is retryable
        if error.is_retryable() {
            // Create endpoint-specific retry policy (matching production worker logic)
            let retry_policy = RetryPolicy {
                max_attempts: u32::try_from(webhook.max_retries.max(0)).unwrap_or(0) + 1, /* max_retries + initial attempt */
                base_delay: std::time::Duration::from_secs(1),
                max_delay: std::time::Duration::from_secs(512),
                jitter_factor: 0.0, // No jitter for deterministic testing
                backoff_strategy: BackoffStrategy::Exponential,
            };

            // Use production retry decision logic
            let retry_context = RetryContext::new(
                u32::try_from(attempt_number.max(0)).unwrap_or(0),
                error.clone(),
                chrono::DateTime::<chrono::Utc>::from(self.now_system()),
                retry_policy.clone(),
            );

            tracing::debug!(
                event_id = %webhook.event_id.0,
                attempt_number = attempt_number,
                max_attempts = retry_policy.max_attempts,
                failure_count = webhook.failure_count,
                max_retries = webhook.max_retries,
                "Making retry decision for failed delivery"
            );

            let retry_decision = retry_context.decide_retry();
            tracing::debug!(
                event_id = %webhook.event_id.0,
                decision = ?retry_decision,
                "Retry decision made"
            );

            match retry_decision {
                kapsel_delivery::retry::RetryDecision::GiveUp { reason } => {
                    // Mark as permanently failed immediately (matching production behavior)
                    tracing::debug!(
                        event_id = %webhook.event_id.0,
                        reason = %reason,
                        "webhook has exhausted retries, marking as failed immediately"
                    );
                    sqlx::query(
                        "UPDATE webhook_events
                         SET status = $1, failure_count = $2, last_attempt_at = $3, failed_at = $4
                         WHERE id = $5",
                    )
                    .bind("failed")
                    .bind(attempt_number)
                    .bind(attempted_at)
                    .bind(attempted_at)
                    .bind(webhook.event_id.0)
                    .execute(self.pool())
                    .await
                    .context("failed to mark webhook as failed after exhausting retries")?;
                },
                kapsel_delivery::retry::RetryDecision::Retry { next_attempt_at } => {
                    // Schedule retry using production-calculated timing
                    tracing::debug!(
                        event_id = %webhook.event_id.0,
                        next_attempt_at = %next_attempt_at,
                        "scheduling webhook retry"
                    );
                    sqlx::query(
                        "UPDATE webhook_events
                         SET status = $1, failure_count = $2, last_attempt_at = $3, next_retry_at = $4
                         WHERE id = $5",
                    )
                    .bind("pending")
                    .bind(attempt_number)
                    .bind(attempted_at)
                    .bind(next_attempt_at)
                    .bind(webhook.event_id.0)
                    .execute(self.pool())
                    .await
                    .context("failed to schedule webhook retry")?;
                },
            }
        } else {
            // Non-retryable error - mark as failed immediately (matching production
            // behavior)
            tracing::debug!(
                event_id = %webhook.event_id.0,
                error = %error,
                "non-retryable error, marking webhook as failed immediately"
            );
            sqlx::query(
                "UPDATE webhook_events
                 SET status = $1, failure_count = $2, last_attempt_at = $3, failed_at = $4
                 WHERE id = $5",
            )
            .bind("failed")
            .bind(attempt_number)
            .bind(attempted_at)
            .bind(attempted_at)
            .bind(webhook.event_id.0)
            .execute(self.pool())
            .await
            .context("failed to mark webhook as failed for non-retryable error")?;
        }

        Ok(())
    }

    async fn emit_attestation_event(
        &self,
        webhook: ReadyWebhook,
        attempt_id: Uuid,
        attempt_number: i32,
        attempted_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<()> {
        // Fetch full webhook event details for the attestation event
        let (tenant_id, payload_size): (uuid::Uuid, i32) =
            sqlx::query_as("SELECT tenant_id, payload_size FROM webhook_events WHERE id = $1")
                .bind(webhook.event_id.0)
                .fetch_one(self.pool())
                .await
                .context("failed to fetch webhook event for attestation")?;

        // Calculate payload hash from the actual body
        let payload_hash = {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(&webhook.body);
            hasher.finalize().into()
        };

        // Create the delivery success event matching production format
        let success_event = kapsel_core::DeliverySucceededEvent {
            delivery_attempt_id: attempt_id,
            event_id: kapsel_core::models::EventId(webhook.event_id.0),
            tenant_id: kapsel_core::models::TenantId(tenant_id),
            endpoint_url: webhook.url.clone(),
            response_status: 200, // We know it's successful here
            attempt_number: u32::try_from(attempt_number).unwrap_or(1),
            delivered_at: attempted_at,
            payload_hash,
            payload_size,
        };

        if let Some(ref merkle_service_wrapped) = self.attestation_service {
            use kapsel_attestation::AttestationEventSubscriber;
            use kapsel_core::EventHandler;

            tracing::info!(
                event_id = %webhook.event_id.0,
                attempt_id = %attempt_id,
                attempt_number = attempt_number,
                "processing delivery success event for attestation"
            );

            // Create subscriber and handle the event
            let attestation_subscriber =
                AttestationEventSubscriber::new(merkle_service_wrapped.clone());

            attestation_subscriber
                .handle_event(kapsel_core::DeliveryEvent::Succeeded(success_event))
                .await;

            // Verify the leaf was actually added
            let pending_count = merkle_service_wrapped.read().await.pending_count();
            tracing::info!(
                event_id = %webhook.event_id.0,
                pending_count = pending_count,
                "attestation event processed, pending leaf count: {}", pending_count
            );
        } else {
            tracing::warn!(
                event_id = %webhook.event_id.0,
                "no attestation service configured, cannot emit attestation event"
            );
        }

        Ok(())
    }
}
