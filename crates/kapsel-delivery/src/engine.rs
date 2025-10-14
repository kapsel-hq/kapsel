//! Webhook delivery engine with worker pool and reliability guarantees.
//!
//! The delivery engine orchestrates webhook delivery using a pool of async
//! workers that claim events from PostgreSQL and deliver them to configured
//! endpoints. Integrates circuit breakers, retry logic, and graceful shutdown.
//!
//! # Architecture
//!
//! ```text
//! ┌────────────────┐   ┌──────────────┐   ┌─────────────┐
//! │ DeliveryEngine │──▶│ Worker Pool  │──▶│ HTTP Client │
//! └────────────────┘   └──────────────┘   └─────────────┘
//!        │                   │                   │
//!        ▼                   ▼                   ▼
//! ┌───────────────┐    ┌──────────────┐   ┌─────────────┐
//! │ PostgreSQL    │    │ Circuit      │   │ Destination │
//! │ Event Queue   │    │ Breakers     │   │ Endpoints   │
//! └───────────────┘    └──────────────┘   └─────────────┘
//! ```
//!
//! # Key Features
//!
//! - **Lock-free work distribution** using PostgreSQL `FOR UPDATE SKIP LOCKED`
//! - **Circuit breaker integration** prevents cascade failures
//! - **Exponential backoff** with configurable jitter
//! - **Graceful shutdown** completes in-flight deliveries
//! - **Observable by default** with structured logging and metrics

use std::{collections::HashMap, sync::Arc, time::Duration};

use bytes::Bytes;
use chrono::{DateTime, Utc};
use kapsel_core::models::{EndpointId, EventId, EventStatus, TenantId, WebhookEvent};
use sqlx::PgPool;
use tokio::{sync::RwLock, time::sleep};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use uuid;

use crate::{
    circuit::{CircuitBreakerManager, CircuitConfig},
    client::{ClientConfig, DeliveryClient},
    error::{DeliveryError, Result},
    retry::{RetryContext, RetryPolicy},
    worker::WorkerPool,
};

/// Configuration for the delivery engine.
#[derive(Debug, Clone)]
pub struct DeliveryConfig {
    /// Number of concurrent delivery workers.
    pub worker_count: usize,

    /// Maximum events to claim per worker batch.
    pub batch_size: usize,

    /// How often workers poll for new events.
    pub poll_interval: Duration,

    /// HTTP client configuration.
    pub client_config: ClientConfig,

    /// Default retry policy for endpoints without specific configuration.
    pub default_retry_policy: RetryPolicy,

    /// Shutdown timeout - maximum time to wait for workers to complete.
    pub shutdown_timeout: Duration,
}

impl Default for DeliveryConfig {
    fn default() -> Self {
        Self {
            worker_count: crate::DEFAULT_WORKER_COUNT,
            batch_size: crate::DEFAULT_BATCH_SIZE,
            poll_interval: Duration::from_secs(1),
            client_config: ClientConfig::default(),
            default_retry_policy: RetryPolicy::default(),
            shutdown_timeout: Duration::from_secs(30),
        }
    }
}

/// Statistics for delivery engine monitoring.
#[derive(Debug, Clone, Default)]
pub struct EngineStats {
    /// Number of active delivery workers.
    pub active_workers: usize,
    /// Total events processed since startup.
    pub events_processed: u64,
    /// Successful deliveries.
    pub successful_deliveries: u64,
    /// Failed deliveries (will retry).
    pub failed_deliveries: u64,
    /// Permanently failed events (exhausted retries).
    pub permanent_failures: u64,
    /// Events currently being delivered.
    pub in_flight_deliveries: u64,
}

/// Main delivery engine coordinating webhook delivery workers.
pub struct DeliveryEngine {
    pool: PgPool,
    config: DeliveryConfig,
    client: Arc<DeliveryClient>,
    circuit_manager: Arc<RwLock<CircuitBreakerManager>>,
    stats: Arc<RwLock<EngineStats>>,
    cancellation_token: CancellationToken,
    worker_pool: Option<WorkerPool>,
}

impl DeliveryEngine {
    /// Creates a new delivery engine with the given configuration.
    pub fn new(pool: PgPool, config: DeliveryConfig) -> Result<Self> {
        let client = Arc::new(DeliveryClient::new(config.client_config.clone())?);
        let circuit_manager =
            Arc::new(RwLock::new(CircuitBreakerManager::new(CircuitConfig::default())));
        let stats = Arc::new(RwLock::new(EngineStats::default()));
        let cancellation_token = CancellationToken::new();

        Ok(Self {
            pool,
            config,
            client,
            circuit_manager,
            stats,
            cancellation_token,
            worker_pool: None,
        })
    }

    /// Starts the delivery engine with configured worker pool.
    ///
    /// Returns immediately after spawning workers. Use `shutdown()` to stop
    /// gracefully, or drop the engine to cancel workers immediately.
    pub async fn start(&mut self) -> Result<()> {
        info!(
            worker_count = self.config.worker_count,
            batch_size = self.config.batch_size,
            "starting webhook delivery engine"
        );

        let mut worker_pool = WorkerPool::new(
            self.pool.clone(),
            self.config.clone(),
            self.client.clone(),
            self.circuit_manager.clone(),
            self.stats.clone(),
            self.cancellation_token.clone(),
        );

        worker_pool.spawn_workers().await?;
        self.worker_pool = Some(worker_pool);

        info!("delivery engine started successfully");
        Ok(())
    }

    /// Gracefully shuts down the delivery engine.
    ///
    /// Signals all workers to stop and waits for in-flight deliveries to
    /// Gracefully shuts down the delivery engine, allowing in-flight deliveries
    /// to complete within the configured timeout.
    ///
    /// This method signals all workers to stop processing new events and waits
    /// for current deliveries to complete. If the shutdown timeout is exceeded,
    /// workers may be terminated forcefully.
    pub async fn shutdown(mut self) -> Result<()> {
        info!("shutting down delivery engine");

        if let Some(worker_pool) = self.worker_pool.take() {
            worker_pool.shutdown_graceful(self.config.shutdown_timeout).await
        } else {
            info!("delivery engine was not started, shutdown completed immediately");
            Ok(())
        }
    }

    /// Returns current engine statistics.
    pub async fn stats(&self) -> EngineStats {
        self.stats.read().await.clone()
    }

    /// Forces a specific circuit breaker state for testing.
    #[cfg(test)]
    pub async fn force_circuit_state(
        &self,
        endpoint_id: EndpointId,
        state: crate::circuit::CircuitState,
    ) {
        self.circuit_manager
            .write()
            .await
            .force_circuit_state(&endpoint_id.to_string(), state)
            .await;
    }
}

/// Individual worker that processes webhook deliveries.
pub struct DeliveryWorker {
    id: usize,
    pool: PgPool,
    config: DeliveryConfig,
    client: Arc<DeliveryClient>,
    circuit_manager: Arc<RwLock<CircuitBreakerManager>>,
    stats: Arc<RwLock<EngineStats>>,
    cancellation_token: CancellationToken,
}

impl DeliveryWorker {
    /// Creates a new delivery worker with the given configuration.
    pub fn new(
        id: usize,
        pool: PgPool,
        config: DeliveryConfig,
        client: Arc<DeliveryClient>,
        circuit_manager: Arc<RwLock<CircuitBreakerManager>>,
        stats: Arc<RwLock<EngineStats>>,
        cancellation_token: CancellationToken,
    ) -> Self {
        Self { id, pool, config, client, circuit_manager, stats, cancellation_token }
    }

    /// Main worker loop - claims and processes events until cancelled.
    pub async fn run(&self) -> Result<()> {
        info!(worker_id = self.id, "delivery worker starting");

        loop {
            // Check for cancellation
            if self.cancellation_token.is_cancelled() {
                info!(worker_id = self.id, "delivery worker received shutdown signal");
                break;
            }

            // Claim and process batch of events
            match self.process_batch().await {
                Ok(processed_count) => {
                    if processed_count == 0 {
                        // No events available, wait before polling again
                        tokio::select! {
                            () = sleep(self.config.poll_interval) => {}
                            () = self.cancellation_token.cancelled() => break,
                        }
                    }
                },
                Err(error) => {
                    error!(
                        worker_id = self.id,
                        error = %error,
                        "worker batch processing failed"
                    );
                    // Wait before retrying to avoid tight error loops
                    tokio::select! {
                        () = sleep(Duration::from_secs(5)) => {}
                        () = self.cancellation_token.cancelled() => break,
                    }
                },
            }
        }

        info!(worker_id = self.id, "delivery worker stopped");
        Ok(())
    }

    /// Claims and processes a batch of pending events.
    async fn process_batch(&self) -> Result<usize> {
        // Claim pending events using FOR UPDATE SKIP LOCKED
        let events = self.claim_pending_events().await?;
        let batch_size = events.len();

        debug!(worker_id = self.id, batch_size, "processing event batch");

        // Process each event
        for event in events {
            if self.cancellation_token.is_cancelled() {
                break;
            }

            if let Err(error) = self.process_event(event).await {
                error!(
                    worker_id = self.id,
                    error = %error,
                    "event processing failed"
                );
            }
        }

        Ok(batch_size)
    }

    /// Claims pending events from the database for processing.
    async fn claim_pending_events(&self) -> Result<Vec<WebhookEvent>> {
        let now = Utc::now();

        // First, select events to claim using FOR UPDATE SKIP LOCKED
        let event_ids: Vec<uuid::Uuid> = sqlx::query_scalar(
            r"
            SELECT id FROM webhook_events
            WHERE status = 'pending'
              AND (next_retry_at IS NULL OR next_retry_at <= $1)
            ORDER BY received_at ASC
            LIMIT $2
            FOR UPDATE SKIP LOCKED
            ",
        )
        .bind(now)
        .bind(i32::try_from(self.config.batch_size).unwrap_or(100))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| {
            DeliveryError::database(format!("failed to select events for claiming: {e}"))
        })?;

        // If no events to process, return empty vec
        if event_ids.is_empty() {
            return Ok(Vec::new());
        }

        // Update selected events to delivering status and fetch full data
        let rows = sqlx::query(
            r"
            UPDATE webhook_events
            SET status = 'delivering'
            WHERE id = ANY($1)
            RETURNING id, tenant_id, endpoint_id, source_event_id, idempotency_strategy,
                status, failure_count, last_attempt_at, next_retry_at,
                headers, body, content_type, received_at, delivered_at, failed_at,
                payload_size, signature_valid, signature_error, tigerbeetle_id
            ",
        )
        .bind(&event_ids)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| DeliveryError::database(format!("failed to claim events: {e}")))?;

        // Convert database rows to WebhookEvent structs
        let mut events = Vec::with_capacity(rows.len());
        for row in rows {
            let event = Self::parse_webhook_event_from_row(&row)?;
            events.push(event);
        }

        debug!(worker_id = self.id, claimed_events = events.len(), "claimed events from database");

        Ok(events)
    }

    /// Processes a single webhook event through delivery pipeline.
    async fn process_event(&self, event: WebhookEvent) -> Result<()> {
        let _event_id = event.id;

        // Update in-flight counter
        {
            let mut stats = self.stats.write().await;
            stats.in_flight_deliveries += 1;
        }

        // Process the event (placeholder implementation)
        let result = self.attempt_delivery(&event).await;

        // Update stats and decrement in-flight counter
        {
            let mut stats = self.stats.write().await;
            stats.in_flight_deliveries -= 1;
            stats.events_processed += 1;

            match &result {
                Ok(()) => stats.successful_deliveries += 1,
                Err(_) => stats.failed_deliveries += 1,
            }
        }

        result
    }

    /// Attempts delivery of a webhook event.
    async fn attempt_delivery(&self, event: &WebhookEvent) -> Result<()> {
        let endpoint_key = event.endpoint_id.to_string();
        let attempt_number = event.failure_count + 1;

        // 1. Check circuit breaker state BEFORE we do anything else
        let should_allow =
            self.circuit_manager.read().await.should_allow_request(&endpoint_key).await;

        if !should_allow {
            // Circuit breaker is open - don't change event status, just return error
            // Event should remain in its current state for later retry
            return Err(DeliveryError::circuit_open(endpoint_key));
        }

        // 2. Get endpoint URL from database
        let endpoint_url = self.get_endpoint_url(&event.endpoint_id).await?;

        debug!(
            worker_id = self.id,
            event_id = %event.id,
            attempt_number,
            endpoint_url = %endpoint_url,
            "attempting webhook delivery"
        );

        // 3. Build delivery request
        let delivery_request = crate::client::DeliveryRequest {
            delivery_id: uuid::Uuid::new_v4(),
            event_id: event.id.0,
            url: endpoint_url
                .parse()
                .map_err(|e| DeliveryError::configuration(format!("invalid webhook URL: {e}")))?,
            method: "POST".to_string(),
            headers: event.headers.clone(),
            body: event.body.clone(),
            content_type: event.content_type.clone(),
            attempt_number,
        };

        // 4. Make HTTP delivery and record attempt
        let delivery_result = self.client.deliver(delivery_request).await;

        // 5. Record delivery attempt for audit trail (simplified for now)
        // TODO: Implement full audit trail recording once schema is confirmed

        // 6. Handle result and update event status
        // Important: We always update the database state, even on failure
        match delivery_result {
            Ok(response) => {
                // Success - update circuit breaker and mark event delivered
                self.circuit_manager.write().await.record_success(&endpoint_key).await;
                self.mark_event_delivered(&event.id).await?;

                info!(
                    worker_id = self.id,
                    event_id = %event.id,
                    status_code = response.status_code,
                    duration_ms = response.duration.as_millis(),
                    "webhook delivered successfully"
                );
            },
            Err(error) => {
                // Failure - update circuit breaker and handle retry logic
                self.circuit_manager.write().await.record_failure(&endpoint_key).await;

                if error.is_retryable() {
                    // Calculate retry timing using policy
                    let retry_context = RetryContext::new(
                        attempt_number,
                        error.clone(),
                        Utc::now(),
                        self.config.default_retry_policy.clone(),
                    );

                    match retry_context.decide_retry() {
                        crate::retry::RetryDecision::Retry { next_attempt_at } => {
                            // Schedule retry
                            self.schedule_retry(event, next_attempt_at).await?;

                            warn!(
                                worker_id = self.id,
                                event_id = %event.id,
                                attempt_number,
                                next_retry_at = %next_attempt_at,
                                error = %error,
                                "delivery failed, retry scheduled"
                            );
                        },
                        crate::retry::RetryDecision::GiveUp { reason } => {
                            // Mark as permanently failed
                            self.mark_event_failed(&event.id).await?;

                            error!(
                                worker_id = self.id,
                                event_id = %event.id,
                                attempt_number,
                                reason = %reason,
                                error = %error,
                                "delivery permanently failed"
                            );
                        },
                    }
                } else {
                    // Non-retryable error - mark as failed immediately
                    self.mark_event_failed(&event.id).await?;

                    error!(
                        worker_id = self.id,
                        event_id = %event.id,
                        attempt_number,
                        error = %error,
                        "delivery failed with non-retryable error"
                    );
                }
            },
        }

        // Always return Ok - the event has been processed successfully
        // even if the HTTP delivery failed. Database state has been updated
        // appropriately.
        Ok(())
    }

    /// Gets the endpoint URL from the database.
    async fn get_endpoint_url(&self, endpoint_id: &EndpointId) -> Result<String> {
        let url = sqlx::query_scalar::<_, String>("SELECT url FROM endpoints WHERE id = $1")
            .bind(endpoint_id.0)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| match e {
                sqlx::Error::RowNotFound => {
                    DeliveryError::configuration(format!("endpoint {endpoint_id} not found"))
                },
                _ => DeliveryError::database(format!("failed to fetch endpoint URL: {e}")),
            })?;

        Ok(url)
    }

    /// Updates event status to 'delivered' after successful delivery.
    async fn mark_event_delivered(&self, event_id: &EventId) -> Result<()> {
        let now = Utc::now();
        sqlx::query(
            "UPDATE webhook_events
             SET status = 'delivered', delivered_at = $1
             WHERE id = $2",
        )
        .bind(now)
        .bind(event_id.0)
        .execute(&self.pool)
        .await
        .map_err(|e| DeliveryError::database(format!("failed to mark event delivered: {e}")))?;

        Ok(())
    }

    /// Updates event with retry schedule after failed delivery.
    async fn schedule_retry(
        &self,
        event: &WebhookEvent,
        next_retry_at: DateTime<Utc>,
    ) -> Result<()> {
        let now = Utc::now();
        sqlx::query(
            "UPDATE webhook_events
             SET status = 'pending', failure_count = failure_count + 1,
                 last_attempt_at = $1, next_retry_at = $2
             WHERE id = $3",
        )
        .bind(now)
        .bind(next_retry_at)
        .bind(event.id.0)
        .execute(&self.pool)
        .await
        .map_err(|e| DeliveryError::database(format!("failed to schedule retry: {e}")))?;

        Ok(())
    }

    /// Marks event as permanently failed when retries are exhausted.
    async fn mark_event_failed(&self, event_id: &EventId) -> Result<()> {
        let now = Utc::now();
        sqlx::query(
            "UPDATE webhook_events
             SET status = 'failed', failed_at = $1
             WHERE id = $2",
        )
        .bind(now)
        .bind(event_id.0)
        .execute(&self.pool)
        .await
        .map_err(|e| DeliveryError::database(format!("failed to mark event failed: {e}")))?;

        Ok(())
    }

    /// Records a delivery attempt in the audit trail.
    /// Simplified implementation until full schema is confirmed.
    #[allow(dead_code)]
    fn record_delivery_attempt(
        _event: &WebhookEvent,
        _url: &str,
        _attempt_number: u32,
        _result: &Result<crate::client::DeliveryResponse>,
    ) {
        // TODO: Implement full audit trail recording
        // For now, skip detailed recording to avoid schema issues
    }

    /// Parses a database row into a WebhookEvent struct.
    fn parse_webhook_event_from_row(row: &sqlx::postgres::PgRow) -> Result<WebhookEvent> {
        use sqlx::Row;

        // Parse headers from JSONB
        let headers_value: serde_json::Value = row
            .try_get("headers")
            .map_err(|e| DeliveryError::internal(format!("failed to get headers: {e}")))?;
        let headers: HashMap<String, String> = serde_json::from_value(headers_value)
            .map_err(|e| DeliveryError::internal(format!("invalid headers JSON: {e}")))?;

        let event =
            WebhookEvent {
                id: EventId(
                    row.try_get("id")
                        .map_err(|e| DeliveryError::internal(format!("failed to get id: {e}")))?,
                ),
                tenant_id: TenantId(row.try_get("tenant_id").map_err(|e| {
                    DeliveryError::internal(format!("failed to get tenant_id: {e}"))
                })?),
                endpoint_id: EndpointId(row.try_get("endpoint_id").map_err(|e| {
                    DeliveryError::internal(format!("failed to get endpoint_id: {e}"))
                })?),
                source_event_id: row.try_get("source_event_id").map_err(|e| {
                    DeliveryError::internal(format!("failed to get source_event_id: {e}"))
                })?,
                idempotency_strategy: row.try_get("idempotency_strategy").map_err(|e| {
                    DeliveryError::internal(format!("failed to get idempotency_strategy: {e}"))
                })?,
                status: EventStatus::Delivering, // We just updated it to delivering
                failure_count: {
                    let count = row.try_get::<i32, _>("failure_count").map_err(|e| {
                        DeliveryError::internal(format!("failed to get failure_count: {e}"))
                    })?;
                    u32::try_from(count).unwrap_or(0)
                },
                last_attempt_at: row.try_get("last_attempt_at").map_err(|e| {
                    DeliveryError::internal(format!("failed to get last_attempt_at: {e}"))
                })?,
                next_retry_at: row.try_get("next_retry_at").map_err(|e| {
                    DeliveryError::internal(format!("failed to get next_retry_at: {e}"))
                })?,
                headers,
                body: Bytes::from(
                    row.try_get::<Vec<u8>, _>("body")
                        .map_err(|e| DeliveryError::internal(format!("failed to get body: {e}")))?,
                ),
                content_type: row.try_get("content_type").map_err(|e| {
                    DeliveryError::internal(format!("failed to get content_type: {e}"))
                })?,
                received_at: row.try_get("received_at").map_err(|e| {
                    DeliveryError::internal(format!("failed to get received_at: {e}"))
                })?,
                delivered_at: row.try_get("delivered_at").map_err(|e| {
                    DeliveryError::internal(format!("failed to get delivered_at: {e}"))
                })?,
                failed_at: row.try_get("failed_at").map_err(|e| {
                    DeliveryError::internal(format!("failed to get failed_at: {e}"))
                })?,
                payload_size: row.try_get("payload_size").map_err(|e| {
                    DeliveryError::internal(format!("failed to get payload_size: {e}"))
                })?,
                signature_valid: row.try_get("signature_valid").map_err(|e| {
                    DeliveryError::internal(format!("failed to get signature_valid: {e}"))
                })?,
                signature_error: row.try_get("signature_error").map_err(|e| {
                    DeliveryError::internal(format!("failed to get signature_error: {e}"))
                })?,
                tigerbeetle_id: row.try_get("tigerbeetle_id").map_err(|e| {
                    DeliveryError::internal(format!("failed to get tigerbeetle_id: {e}"))
                })?,
            };

        Ok(event)
    }
}

#[cfg(test)]
mod tests {
    use sqlx::Row;
    use test_harness::TestEnv;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[tokio::test]
    async fn engine_starts_with_configured_workers() {
        let env = TestEnv::new().await.expect("test environment setup failed");
        let config = DeliveryConfig { worker_count: 5, ..Default::default() };

        let mut engine =
            DeliveryEngine::new(env.db.pool(), config).expect("engine creation should succeed");
        engine.start().await.expect("engine should start successfully");

        let stats = engine.stats().await;
        assert_eq!(stats.active_workers, 5);

        engine.shutdown().await.expect("engine should shutdown gracefully");
    }

    #[tokio::test]
    async fn engine_shuts_down_gracefully() {
        let env = TestEnv::new().await.expect("test environment setup failed");
        let config = DeliveryConfig::default();
        let mut engine =
            DeliveryEngine::new(env.db.pool(), config).expect("engine creation should succeed");

        engine.start().await.expect("engine should start");

        let shutdown_result = engine.shutdown().await;
        assert!(shutdown_result.is_ok(), "shutdown should complete without error");
    }

    #[tokio::test]
    async fn successful_delivery_updates_database_correctly() {
        let env = TestEnv::new().await.expect("test environment setup failed");
        let mock_server = MockServer::start().await;
        let webhook_url = format!("{}/webhook", mock_server.uri());

        // Set up mock to accept webhook
        Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/webhook"))
            .respond_with(ResponseTemplate::new(200).set_body_string("OK"))
            .expect(1)
            .mount(&mock_server)
            .await;

        // Insert test tenant and endpoint
        let (_tenant_id, _endpoint_id, event_id) = setup_test_data(&env, &webhook_url).await;

        // Create worker
        let worker = create_test_worker(&env).await;

        // Get the event and attempt delivery
        let event = get_event_by_id(&env, &event_id).await;
        let result = worker.attempt_delivery(&event).await;
        assert!(result.is_ok(), "delivery should succeed: {:?}", result.err());

        // Verify event is marked as delivered
        let updated_event = get_event_by_id(&env, &event_id).await;
        assert_eq!(updated_event.status, EventStatus::Delivered);
        assert!(updated_event.delivered_at.is_some());

        // Verify delivery attempt was recorded (skip for now due to schema issues)
        // TODO: Re-enable once audit trail schema is confirmed

        mock_server.verify().await;
    }

    #[tokio::test]
    async fn failed_delivery_schedules_retry() {
        let env = TestEnv::new().await.expect("test environment setup failed");
        let mock_server = MockServer::start().await;
        let webhook_url = format!("{}/webhook", mock_server.uri());

        // Set up mock to return 500 error
        Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/webhook"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
            .expect(1)
            .mount(&mock_server)
            .await;

        // Insert test data
        let (_tenant_id, _endpoint_id, event_id) = setup_test_data(&env, &webhook_url).await;
        // Create worker
        let worker = create_test_worker(&env).await;

        // Get event and attempt delivery
        let event = get_event_by_id(&env, &event_id).await;
        let result = worker.attempt_delivery(&event).await;
        assert!(
            result.is_ok(),
            "worker should handle delivery failure gracefully: {:?}",
            result.err()
        );

        // Verify event is marked for retry
        let updated_event = get_event_by_id(&env, &event_id).await;
        assert_eq!(updated_event.status, EventStatus::Pending);
        assert_eq!(updated_event.failure_count, 1);
        assert!(updated_event.next_retry_at.is_some());
        assert!(updated_event.next_retry_at.unwrap() > Utc::now());

        mock_server.verify().await;
    }

    #[tokio::test]
    async fn exhausted_retries_mark_event_failed() {
        let env = TestEnv::new().await.expect("test environment setup failed");
        let mock_server = MockServer::start().await;
        let webhook_url = format!("{}/webhook", mock_server.uri());

        // Set up mock to return 500 error
        Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/webhook"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
            .expect(1)
            .mount(&mock_server)
            .await;

        // Insert test data with event at max retry limit
        let (_tenant_id, _endpoint_id, event_id) = setup_test_data(&env, &webhook_url).await;

        // Update event to be at max retry limit (assuming default max_attempts = 10)
        sqlx::query("UPDATE webhook_events SET failure_count = 9 WHERE id = $1")
            .bind(event_id)
            .execute(&env.db.pool())
            .await
            .expect("failed to update failure count");

        // Create worker with low retry policy for testing
        let config = DeliveryConfig {
            default_retry_policy: RetryPolicy { max_attempts: 10, ..Default::default() },
            ..Default::default()
        };
        let worker = create_test_worker_with_config(&env, config).await;

        // Get event and attempt delivery (this will be attempt #10)
        let event = get_event_by_id(&env, &event_id).await;
        let result = worker.attempt_delivery(&event).await;
        assert!(
            result.is_ok(),
            "worker should handle exhausted retries gracefully: {:?}",
            result.err()
        );

        // Verify event is marked as permanently failed
        let updated_event = get_event_by_id(&env, &event_id).await;
        assert_eq!(updated_event.status, EventStatus::Failed);
        assert!(updated_event.failed_at.is_some());

        mock_server.verify().await;
    }

    #[tokio::test]
    async fn circuit_breaker_blocks_delivery_when_open() {
        let env = TestEnv::new().await.expect("test environment setup failed");
        let mock_server = MockServer::start().await;
        let webhook_url = format!("{}/webhook", mock_server.uri());

        // Insert test data
        let (_tenant_id, endpoint_id, event_id) = setup_test_data(&env, &webhook_url).await;

        // Create worker and force circuit breaker open
        let worker = create_test_worker(&env).await;
        worker
            .circuit_manager
            .write()
            .await
            .force_circuit_state(&endpoint_id.to_string(), crate::circuit::CircuitState::Open)
            .await;

        // Set event to delivering status (simulates normal claim process)
        sqlx::query("UPDATE webhook_events SET status = 'delivering' WHERE id = $1")
            .bind(event_id)
            .execute(&env.db.pool())
            .await
            .expect("failed to set event to delivering status");

        // Get event and attempt delivery
        let event = get_event_by_id(&env, &event_id).await;
        let result = worker.attempt_delivery(&event).await;

        // Should fail with circuit open error
        assert!(result.is_err());
        if let Err(DeliveryError::CircuitOpen { .. }) = result {
            // Expected error type
        } else {
            panic!("expected circuit open error");
        }

        // Event should remain in delivering status (unchanged)
        let updated_event = get_event_by_id(&env, &event_id).await;
        assert_eq!(updated_event.status, EventStatus::Delivering);
    }

    #[tokio::test]
    async fn worker_claims_pending_events_from_database() {
        let env = TestEnv::new().await.expect("test environment setup failed");
        let mock_server = MockServer::start().await;
        let webhook_url = format!("{}/webhook", mock_server.uri());

        // Insert test tenant, endpoint, and first event
        let (tenant_id, endpoint_id, event1_id) = setup_test_data(&env, &webhook_url).await;

        // Insert second event under same tenant/endpoint
        let event2_id = uuid::Uuid::new_v4();
        let now = Utc::now();
        sqlx::query(
            r"
            INSERT INTO webhook_events (
                id, tenant_id, endpoint_id, source_event_id, idempotency_strategy,
                status, failure_count, headers, body, content_type,
                received_at, payload_size
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            ",
        )
        .bind(event2_id)
        .bind(tenant_id)
        .bind(endpoint_id)
        .bind("test-event-2")
        .bind("header")
        .bind("pending")
        .bind(0)
        .bind(serde_json::json!({"content-type": "application/json"}))
        .bind(b"{\"test\": \"data2\"}")
        .bind("application/json")
        .bind(now)
        .bind(17) // payload_size for "{\"test\": \"data\"}"
        .execute(&env.db.pool())
        .await
        .expect("failed to insert second test event");

        // Mark events as pending (they should be pending by default, but make sure)
        sqlx::query("UPDATE webhook_events SET status = 'pending' WHERE id IN ($1, $2)")
            .bind(event1_id)
            .bind(event2_id)
            .execute(&env.db.pool())
            .await
            .expect("failed to set events to pending");

        // Create worker
        let worker = create_test_worker(&env).await;

        // Claim events
        let claimed_events = worker.claim_pending_events().await.expect("failed to claim events");

        // Should claim both events
        assert_eq!(claimed_events.len(), 2);

        // Events should be marked as delivering in database
        let delivering_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM webhook_events WHERE status = 'delivering' AND id IN ($1, $2)",
        )
        .bind(event1_id)
        .bind(event2_id)
        .fetch_one(&env.db.pool())
        .await
        .expect("failed to count delivering events");

        assert_eq!(delivering_count, 2);
    }

    #[tokio::test]
    async fn non_retryable_errors_mark_event_failed_immediately() {
        let env = TestEnv::new().await.expect("test environment setup failed");
        let mock_server = MockServer::start().await;
        let webhook_url = format!("{}/webhook", mock_server.uri());

        // Set up mock to return 404 (non-retryable client error)
        Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/webhook"))
            .respond_with(ResponseTemplate::new(404).set_body_string("Not Found"))
            .expect(1)
            .mount(&mock_server)
            .await;

        // Insert test data
        let (_tenant_id, _endpoint_id, event_id) = setup_test_data(&env, &webhook_url).await;
        // Create worker
        let worker = create_test_worker(&env).await;

        // Get event and attempt delivery
        let event = get_event_by_id(&env, &event_id).await;
        let result = worker.attempt_delivery(&event).await;
        assert!(
            result.is_ok(),
            "worker should handle non-retryable error gracefully: {:?}",
            result.err()
        );

        // Verify event is marked as failed immediately (no retry)
        let updated_event = get_event_by_id(&env, &event_id).await;
        assert_eq!(updated_event.status, EventStatus::Failed);
        assert!(updated_event.failed_at.is_some());
        assert!(updated_event.next_retry_at.is_none());

        mock_server.verify().await;
    }

    // Helper functions for test setup
    async fn setup_test_data(
        env: &TestEnv,
        webhook_url: &str,
    ) -> (uuid::Uuid, uuid::Uuid, uuid::Uuid) {
        let tenant_id = uuid::Uuid::new_v4();
        let endpoint_id = uuid::Uuid::new_v4();
        let event_id = uuid::Uuid::new_v4();

        // Insert tenant
        let tenant_name = format!("test-tenant-{}", tenant_id.simple());
        sqlx::query("INSERT INTO tenants (id, name, plan) VALUES ($1, $2, $3)")
            .bind(tenant_id)
            .bind(tenant_name)
            .bind("free")
            .execute(&env.db.pool())
            .await
            .expect("failed to insert test tenant");

        // Insert endpoint
        sqlx::query(
            "INSERT INTO endpoints (id, tenant_id, name, url, signing_secret, max_retries, circuit_state)
             VALUES ($1, $2, $3, $4, $5, $6, $7)"
        )
        .bind(endpoint_id)
        .bind(tenant_id)
        .bind("test-endpoint")
        .bind(webhook_url)
        .bind("secret123")
        .bind(5)
        .bind("closed")
        .execute(&env.db.pool())
        .await
        .expect("failed to insert test endpoint");

        // Insert webhook event
        let now = Utc::now();
        sqlx::query(
            r"
            INSERT INTO webhook_events (
                id, tenant_id, endpoint_id, source_event_id, idempotency_strategy,
                status, failure_count, headers, body, content_type,
                received_at, payload_size
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            ",
        )
        .bind(event_id)
        .bind(tenant_id)
        .bind(endpoint_id)
        .bind(format!("source-{}", event_id))
        .bind("header")
        .bind("pending")
        .bind(0)
        .bind(serde_json::json!({"x-test": "value"}))
        .bind(b"test payload".as_slice())
        .bind("application/json")
        .bind(now)
        .bind(12)
        .execute(&env.db.pool())
        .await
        .expect("failed to insert test webhook event");

        (tenant_id, endpoint_id, event_id)
    }

    async fn create_test_worker(env: &TestEnv) -> DeliveryWorker {
        create_test_worker_with_config(env, DeliveryConfig::default()).await
    }

    async fn create_test_worker_with_config(
        env: &TestEnv,
        config: DeliveryConfig,
    ) -> DeliveryWorker {
        DeliveryWorker {
            id: 0,
            pool: env.db.pool(),
            config,
            client: Arc::new(DeliveryClient::with_defaults().expect("failed to create client")),
            circuit_manager: Arc::new(RwLock::new(CircuitBreakerManager::new(
                CircuitConfig::default(),
            ))),
            stats: Arc::new(RwLock::new(EngineStats::default())),
            cancellation_token: CancellationToken::new(),
        }
    }

    async fn get_event_by_id(env: &TestEnv, event_id: &uuid::Uuid) -> WebhookEvent {
        let row = sqlx::query(
            r"
            SELECT id, tenant_id, endpoint_id, source_event_id, idempotency_strategy,
                   status, failure_count, last_attempt_at, next_retry_at,
                   headers, body, content_type, received_at, delivered_at, failed_at,
                   payload_size, signature_valid, signature_error, tigerbeetle_id
            FROM webhook_events WHERE id = $1
            ",
        )
        .bind(event_id)
        .fetch_one(&env.db.pool())
        .await
        .expect("failed to fetch event");

        let headers_value: serde_json::Value =
            row.try_get("headers").expect("failed to get headers");
        let headers: HashMap<String, String> =
            serde_json::from_value(headers_value).expect("invalid headers JSON");

        let status_str: String = row.try_get("status").expect("failed to get status");
        let status = match status_str.as_str() {
            "received" => EventStatus::Received,
            "pending" => EventStatus::Pending,
            "delivering" => EventStatus::Delivering,
            "delivered" => EventStatus::Delivered,
            "failed" => EventStatus::Failed,
            "dead_letter" => EventStatus::DeadLetter,
            _ => panic!("unknown event status: {}", status_str),
        };

        WebhookEvent {
            id: EventId(row.try_get("id").expect("failed to get id")),
            tenant_id: TenantId(row.try_get("tenant_id").expect("failed to get tenant_id")),
            endpoint_id: EndpointId(row.try_get("endpoint_id").expect("failed to get endpoint_id")),
            source_event_id: row.try_get("source_event_id").expect("failed to get source_event_id"),
            idempotency_strategy: row
                .try_get("idempotency_strategy")
                .expect("failed to get idempotency_strategy"),
            status,
            failure_count: row
                .try_get::<i32, _>("failure_count")
                .expect("failed to get failure_count") as u32,
            last_attempt_at: row.try_get("last_attempt_at").expect("failed to get last_attempt_at"),
            next_retry_at: row.try_get("next_retry_at").expect("failed to get next_retry_at"),
            headers,
            body: Bytes::from(row.try_get::<Vec<u8>, _>("body").expect("failed to get body")),
            content_type: row.try_get("content_type").expect("failed to get content_type"),
            received_at: row.try_get("received_at").expect("failed to get received_at"),
            delivered_at: row.try_get("delivered_at").expect("failed to get delivered_at"),
            failed_at: row.try_get("failed_at").expect("failed to get failed_at"),
            payload_size: row.try_get("payload_size").expect("failed to get payload_size"),
            signature_valid: row.try_get("signature_valid").expect("failed to get signature_valid"),
            signature_error: row.try_get("signature_error").expect("failed to get signature_error"),
            tigerbeetle_id: row.try_get("tigerbeetle_id").expect("failed to get tigerbeetle_id"),
        }
    }
}
