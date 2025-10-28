//! Worker pool engine for reliable webhook delivery.
//!
//! Orchestrates async workers that claim events from PostgreSQL using SKIP
//! LOCKED for lock-free distribution. Integrates circuit breakers, exponential
//! backoff, and graceful shutdown with event-driven architecture support.

use std::{collections::HashMap, sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use kapsel_attestation::{AttestationEventSubscriber, MerkleService, SigningService};
use kapsel_core::{
    models::{EndpointId, EventId, WebhookEvent},
    storage::Storage,
    Clock, DeliveryEvent, DeliveryFailedEvent, DeliverySucceededEvent, EventHandler,
    MulticastEventHandler,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::{
    circuit::{CircuitBreakerManager, CircuitConfig},
    client::{extract_retry_after_seconds, ClientConfig, DeliveryClient, DeliveryRequest},
    error::{DeliveryError, Result},
    retry::{RetryContext, RetryPolicy},
    worker_pool::WorkerPool,
};

/// Configuration for the delivery engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    clock: Arc<dyn Clock>,
    event_handler: Arc<dyn EventHandler>,
}

impl DeliveryEngine {
    /// Creates a new delivery engine with the given configuration and event
    /// handler.
    ///
    /// This constructor allows for dependency injection of the event handler,
    /// enabling isolated testing without attestation dependencies.
    ///
    /// # Errors
    ///
    /// Returns error if the delivery client cannot be initialized.
    pub fn with_event_handler(
        pool: PgPool,
        config: DeliveryConfig,
        clock: Arc<dyn Clock>,
        event_handler: Arc<dyn EventHandler>,
    ) -> Result<Self> {
        let client = Arc::new(DeliveryClient::new(config.client_config.clone(), clock.clone())?);
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
            clock,
            event_handler,
        })
    }

    /// Creates a new delivery engine with the given configuration.
    ///
    /// This constructor creates a production engine with attestation support
    /// enabled. For testing without attestation, use `with_event_handler`.
    ///
    /// # Errors
    ///
    /// Returns error if the delivery client cannot be initialized.
    pub fn new(pool: PgPool, config: DeliveryConfig, clock: Arc<dyn Clock>) -> Result<Self> {
        let mut event_handler = MulticastEventHandler::new(clock.clone());
        let signing_service = SigningService::ephemeral();
        let storage = Arc::new(Storage::new(pool.clone(), &clock.clone()));
        let merkle_service = Arc::new(tokio::sync::RwLock::new(MerkleService::new(
            storage,
            signing_service,
            clock.clone(),
        )));
        let attestation_subscriber = Arc::new(AttestationEventSubscriber::new(merkle_service));
        event_handler.add_subscriber(attestation_subscriber);

        Self::with_event_handler(pool, config, clock, Arc::new(event_handler))
    }

    /// Starts the delivery engine with configured worker pool.
    ///
    /// Returns immediately after spawning workers. Use `shutdown()` to stop
    /// gracefully, or drop the engine to cancel workers immediately.
    ///
    /// # Errors
    ///
    /// Returns error if worker pool fails to spawn.
    pub async fn start(&mut self) -> Result<()> {
        info!(
            worker_count = self.config.worker_count,
            batch_size = self.config.batch_size,
            "starting webhook delivery engine"
        );

        let mut worker_pool = WorkerPool::with_event_handler(
            self.pool.clone(),
            self.config.clone(),
            self.client.clone(),
            self.circuit_manager.clone(),
            self.stats.clone(),
            self.cancellation_token.clone(),
            self.clock.clone(),
            self.event_handler.clone(),
        );

        worker_pool.spawn_workers().await?;
        self.worker_pool = Some(worker_pool);

        info!("delivery engine started successfully");
        Ok(())
    }

    /// Gracefully shuts down the delivery engine.
    ///
    /// Signals all workers to stop processing new events and waits for current
    /// deliveries to complete. If the shutdown timeout is exceeded, workers may
    /// be terminated forcefully.
    ///
    /// # Errors
    ///
    /// Returns error if graceful shutdown fails or times out.
    pub async fn shutdown(mut self) -> Result<()> {
        info!("shutting down delivery engine");

        if let Some(worker_pool) = self.worker_pool.take() {
            worker_pool.shutdown_graceful(self.config.shutdown_timeout).await?;
        } else {
            info!("delivery engine was not started, shutdown completed immediately");
        }
        Ok(())
    }

    /// Returns current engine statistics.
    pub async fn stats(&self) -> EngineStats {
        self.stats.read().await.clone()
    }

    /// Processes exactly one batch of pending events synchronously.
    ///
    /// This method is designed for testing and controlled batch processing.
    /// Unlike `start()` which spawns persistent workers, this method:
    /// 1. Claims one batch of pending events
    /// 2. Processes them synchronously
    /// 3. Returns when complete
    /// 4. Does not start persistent background workers
    ///
    /// # Errors
    ///
    /// Returns error if batch processing fails.
    pub async fn process_batch(&self) -> Result<usize> {
        let storage = Arc::new(Storage::new(self.pool.clone(), &self.clock.clone()));

        // Create a temporary worker for this batch
        let worker = DeliveryWorker::new(
            0, // worker_id
            storage,
            self.config.clone(),
            self.client.clone(),
            self.circuit_manager.clone(),
            self.stats.clone(),
            self.cancellation_token.clone(),
            self.event_handler.clone(),
            self.clock.clone(),
        );

        // Process exactly one batch synchronously
        worker.process_batch().await
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
    storage: Arc<Storage>,
    config: DeliveryConfig,
    client: Arc<DeliveryClient>,
    circuit_manager: Arc<RwLock<CircuitBreakerManager>>,
    stats: Arc<RwLock<EngineStats>>,
    cancellation_token: CancellationToken,
    event_handler: Arc<dyn EventHandler>,
    clock: Arc<dyn Clock>,
}

impl DeliveryWorker {
    /// Creates a new delivery worker with the given configuration.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: usize,
        storage: Arc<Storage>,
        config: DeliveryConfig,
        client: Arc<DeliveryClient>,
        circuit_manager: Arc<RwLock<CircuitBreakerManager>>,
        stats: Arc<RwLock<EngineStats>>,
        cancellation_token: CancellationToken,
        event_handler: Arc<dyn EventHandler>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            id,
            storage,
            config,
            client,
            circuit_manager,
            stats,
            cancellation_token,
            event_handler,
            clock,
        }
    }

    /// Main worker loop - claims and processes events until cancelled.
    ///
    /// # Errors
    ///
    /// Returns error only if worker setup fails. Batch processing errors are
    /// logged and retried.
    pub async fn run(&self) -> Result<()> {
        info!(worker_id = self.id, "delivery worker starting");

        loop {
            // Early cancellation check prevents unnecessary work if shutdown signaled
            if self.cancellation_token.is_cancelled() {
                info!(worker_id = self.id, "delivery worker received shutdown signal");
                break;
            }

            match self.process_batch().await {
                Ok(processed_count) => {
                    if processed_count == 0 {
                        tokio::select! {
                            () = self.clock.sleep(self.config.poll_interval) => {
                                // No events available, wait before polling again
                            }
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
                    tokio::select! {
                        () = self.clock.sleep(Duration::from_secs(5)) => {
                            // Wait before retrying to avoid tight error loops
                        }
                        () = self.cancellation_token.cancelled() => break,
                    }
                },
            }
        }

        info!(worker_id = self.id, "delivery worker stopped");
        Ok(())
    }

    /// Claims and processes a batch of pending events.
    ///
    /// # Errors
    ///
    /// Returns error if database transaction or event claiming fails.
    async fn process_batch(&self) -> Result<usize> {
        // Claim pending events using FOR UPDATE SKIP LOCKED
        let events = self.claim_pending_events().await?;
        let batch_size = events.len();

        debug!(worker_id = self.id, batch_size, "processing event batch");

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
    ///
    /// # Errors
    ///
    /// Returns error if database transaction or query fails.
    pub async fn claim_pending_events(&self) -> Result<Vec<WebhookEvent>> {
        let events =
            self.storage.webhook_events.claim_pending(self.config.batch_size).await.map_err(
                |e| DeliveryError::database(format!("failed to claim pending events: {e}")),
            )?;

        debug!(
            worker_id = self.id,
            claimed_count = events.len(),
            "claimed webhook events for processing"
        );

        Ok(events)
    }

    /// Processes a single webhook event through delivery pipeline.
    ///
    /// # Errors
    ///
    /// Returns error if delivery attempt or database update fails.
    async fn process_event(&self, event: WebhookEvent) -> Result<()> {
        let _ = event.id;

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
    ///
    /// # Errors
    ///
    /// Returns error if circuit breaker is open, endpoint URL is invalid, or
    /// database update fails.
    #[allow(clippy::too_many_lines)]
    async fn attempt_delivery(&self, event: &WebhookEvent) -> Result<()> {
        let start_time = self.clock.now();
        let endpoint_key = event.endpoint_id.to_string();
        let attempt_number = u32::try_from(event.failure_count + 1).unwrap_or(u32::MAX);

        // 1. Check circuit breaker state BEFORE we do anything else
        let should_allow =
            self.circuit_manager.read().await.should_allow_request(&endpoint_key).await;

        if !should_allow {
            // Circuit breaker is open - don't change event status, just return error
            // Event should remain in its current state for later retry
            return Err(DeliveryError::circuit_open(endpoint_key));
        }

        // 2. Get endpoint URL from database
        let endpoint_url = self.endpoint_url(&event.endpoint_id).await?;

        debug!(
            worker_id = self.id,
            event_id = %event.id,
            attempt_number,
            endpoint_url = %endpoint_url,
            "attempting webhook delivery"
        );

        // 3. Build delivery request
        let delivery_attempt_id = Uuid::new_v4();
        let delivery_request = DeliveryRequest {
            delivery_id: delivery_attempt_id,
            event_id: event.id.0,
            url: endpoint_url
                .parse()
                .map_err(|e| DeliveryError::configuration(format!("invalid webhook URL: {e}")))?,
            method: "POST".to_string(),
            headers: event.headers().clone(),
            body: event.body_bytes(),
            content_type: event.content_type.clone(),
            attempt_number,
        };

        // 4. Make HTTP delivery and record attempt
        let delivery_result = self.client.deliver(delivery_request).await;

        // 5. Record delivery attempt for audit trail
        self.record_delivery_attempt(
            event,
            &endpoint_url,
            u32::try_from(event.failure_count + 1).unwrap_or(u32::MAX),
            &delivery_result,
            start_time.elapsed(),
        )
        .await;

        // 6. Handle result and update event status
        // Important: We always update the database state, even on failure
        match delivery_result {
            Ok(response) => {
                if response.is_success {
                    // Success - update circuit breaker and mark event delivered
                    self.circuit_manager.write().await.record_success(&endpoint_key).await;
                    self.mark_event_delivered(&event.id).await?;

                    // Publish delivery success event
                    let success_event = DeliveryEvent::Succeeded(DeliverySucceededEvent {
                        delivery_attempt_id,
                        event_id: event.id,
                        tenant_id: event.tenant_id,
                        endpoint_url: endpoint_url.clone(),
                        response_status: response.status_code,
                        attempt_number,
                        delivered_at: DateTime::<Utc>::from(self.clock.now_system()),
                        payload_hash: Self::compute_payload_hash(&event.body),
                        payload_size: event.payload_size,
                    });
                    self.event_handler.handle_event(success_event).await;

                    info!(
                        worker_id = self.id,
                        event_id = %event.id,
                        status_code = response.status_code,
                        duration_ms = response.duration.as_millis(),
                        "webhook delivered successfully"
                    );
                } else {
                    // HTTP request succeeded but response indicates failure
                    // Create appropriate error based on status code
                    let error = match response.status_code {
                        400..=499 => {
                            DeliveryError::client_error(response.status_code, response.body.clone())
                        },
                        _ => crate::error::DeliveryError::server_error(
                            response.status_code,
                            response.body.clone(),
                        ),
                    };

                    self.handle_failed_delivery(
                        &event.id,
                        &endpoint_key,
                        attempt_number,
                        error.clone(),
                        Some(&response.headers),
                        event,
                    )
                    .await?;

                    // Publish delivery failure event
                    let failure_event = DeliveryEvent::Failed(DeliveryFailedEvent {
                        delivery_attempt_id,
                        event_id: event.id,
                        tenant_id: event.tenant_id,
                        endpoint_url: endpoint_url.clone(),
                        response_status: Some(response.status_code),
                        attempt_number,
                        failed_at: DateTime::<Utc>::from(self.clock.now_system()),
                        error_message: error.to_string(),
                        is_retryable: error.is_retryable(),
                    });
                    self.event_handler.handle_event(failure_event).await;
                }
            },
            Err(error) => {
                // Failure - update circuit breaker and handle retry logic
                self.handle_failed_delivery(
                    &event.id,
                    &endpoint_key,
                    attempt_number,
                    error.clone(),
                    None,
                    event,
                )
                .await?;

                // Publish delivery failure event
                let failure_event = DeliveryEvent::Failed(DeliveryFailedEvent {
                    delivery_attempt_id,
                    event_id: event.id,
                    tenant_id: event.tenant_id,
                    endpoint_url: endpoint_url.clone(),
                    response_status: None,
                    attempt_number,
                    failed_at: DateTime::<Utc>::from(self.clock.now_system()),
                    error_message: error.to_string(),
                    is_retryable: error.is_retryable(),
                });
                self.event_handler.handle_event(failure_event).await;
            },
        }

        // Always return Ok - the event has been processed successfully
        // even if the HTTP delivery failed. Database state has been updated
        // appropriately.
        Ok(())
    }

    /// Handles failed delivery attempts with retry logic.
    ///
    /// # Errors
    ///
    /// Returns error if database update fails while scheduling retry or
    /// marking event as failed.
    async fn handle_failed_delivery(
        &self,
        event_id: &EventId,
        endpoint_key: &str,
        attempt_number: u32,
        error: crate::error::DeliveryError,
        response_headers: Option<&std::collections::HashMap<String, String>>,
        event: &WebhookEvent,
    ) -> Result<()> {
        // Record failure in circuit breaker
        self.circuit_manager.write().await.record_failure(endpoint_key).await;

        if error.is_retryable() {
            // Fetch endpoint to get specific max_retries configuration
            let endpoint = self
                .storage
                .endpoints
                .find_by_id(event.endpoint_id)
                .await
                .map_err(|e| DeliveryError::database(format!("failed to fetch endpoint: {e}")))?
                .ok_or_else(|| {
                    DeliveryError::configuration(format!(
                        "endpoint {} not found",
                        event.endpoint_id
                    ))
                })?;

            // Create retry policy using endpoint-specific max_retries
            // max_retries means total attempts = max_retries + 1 (initial attempt)
            let endpoint_retry_policy = RetryPolicy {
                max_attempts: u32::try_from(endpoint.max_retries + 1).unwrap_or(10),
                ..self.config.default_retry_policy.clone()
            };

            // Calculate retry timing using endpoint-specific policy
            let retry_context = RetryContext::new(
                attempt_number,
                error.clone(),
                DateTime::<Utc>::from(self.clock.now_system()),
                endpoint_retry_policy,
            );

            match retry_context.decide_retry() {
                crate::retry::RetryDecision::Retry { mut next_attempt_at } => {
                    // Check for Retry-After header to override calculated delay
                    if let Some(headers) = response_headers {
                        if let Some(retry_after_seconds) =
                            extract_retry_after_seconds(headers, self.clock.as_ref())
                        {
                            let retry_after_duration = chrono::Duration::seconds(
                                i64::try_from(retry_after_seconds).unwrap_or(i64::MAX),
                            );
                            let retry_after_time = DateTime::<Utc>::from(self.clock.now_system())
                                + retry_after_duration;

                            // Use the later of the two times to respect server's preference
                            if retry_after_time > next_attempt_at {
                                next_attempt_at = retry_after_time;
                            }
                        }
                    }

                    // Schedule retry
                    self.schedule_retry(event, next_attempt_at).await?;

                    warn!(
                        worker_id = self.id,
                        event_id = %event_id,
                        attempt_number,
                        next_retry_at = %next_attempt_at,
                        error = %error,
                        "delivery failed, retry scheduled"
                    );
                },
                crate::retry::RetryDecision::GiveUp { reason } => {
                    // Mark as permanently failed
                    self.mark_event_failed(event).await?;

                    error!(
                        worker_id = self.id,
                        event_id = %event_id,
                        attempt_number,
                        reason = %reason,
                        error = %error,
                        "delivery permanently failed"
                    );
                },
            }
        } else {
            // Non-retryable error - mark as failed immediately
            self.mark_event_failed(event).await?;

            error!(
                worker_id = self.id,
                event_id = %event_id,
                attempt_number,
                error = %error,
                "delivery failed with non-retryable error"
            );
        }

        Ok(())
    }

    /// Endpoint URL from the database.
    ///
    /// # Errors
    ///
    /// Returns error if endpoint is not found or database query fails.
    async fn endpoint_url(&self, endpoint_id: &EndpointId) -> Result<String> {
        let endpoint = self
            .storage
            .endpoints
            .find_by_id(*endpoint_id)
            .await
            .map_err(|e| DeliveryError::database(format!("failed to fetch endpoint: {e}")))?
            .ok_or_else(|| {
                DeliveryError::configuration(format!("endpoint {endpoint_id} not found"))
            })?;

        Ok(endpoint.url)
    }

    /// Updates event status to 'delivered' after successful delivery.
    ///
    /// # Errors
    ///
    /// Returns error if database update fails.
    async fn mark_event_delivered(&self, event_id: &EventId) -> Result<()> {
        self.storage.webhook_events.mark_delivered(*event_id).await.map_err(|e| {
            DeliveryError::database(format!("failed to mark event as delivered: {e}"))
        })?;

        Ok(())
    }

    /// Updates event with retry schedule after failed delivery.
    ///
    /// # Errors
    ///
    /// Returns error if database update fails.
    async fn schedule_retry(
        &self,
        event: &WebhookEvent,
        next_retry_at: DateTime<Utc>,
    ) -> Result<()> {
        self.storage
            .webhook_events
            .mark_failed(event.id, event.failure_count + 1, Some(next_retry_at))
            .await
            .map_err(|e| DeliveryError::database(format!("failed to schedule event retry: {e}")))?;

        Ok(())
    }

    /// Marks event as permanently failed when retries are exhausted.
    ///
    /// # Errors
    ///
    /// Returns error if database update fails.
    async fn mark_event_failed(&self, event: &WebhookEvent) -> Result<()> {
        self.storage
            .webhook_events
            .mark_failed(event.id, event.failure_count + 1, None)
            .await
            .map_err(|e| DeliveryError::database(format!("failed to mark event as failed: {e}")))?;

        Ok(())
    }

    /// Records a delivery attempt in the audit trail.
    async fn record_delivery_attempt(
        &self,
        event: &WebhookEvent,
        _url: &str,
        attempt_number: u32,
        result: &Result<crate::client::DeliveryResponse>,
        _duration: std::time::Duration,
    ) {
        let (response_status, response_body, error_message, succeeded) = match result {
            Ok(response) => (
                Some(i32::from(response.status_code)),
                Some(response.body.as_bytes().to_vec()),
                None,
                (200..300).contains(&response.status_code),
            ),
            Err(e) => (None, None, Some(e.to_string()), false),
        };

        let delivery_attempt = kapsel_core::models::DeliveryAttempt {
            id: uuid::Uuid::new_v4(),
            event_id: event.id,
            attempt_number,
            endpoint_id: event.endpoint_id,
            request_headers: HashMap::new(), // Empty headers for now
            request_body: event.body.clone(),
            response_status,
            response_headers: None, // Empty response headers for now
            response_body,
            attempted_at: DateTime::<Utc>::from(self.clock.now_system()),
            succeeded,
            error_message,
        };

        if let Err(e) = self.storage.delivery_attempts.create(&delivery_attempt).await {
            warn!(
                worker_id = self.id,
                event_id = %event.id,
                attempt = attempt_number,
                error = %e,
                "failed to record delivery attempt"
            );
        }
    }

    /// Computes SHA-256 hash of the payload.
    fn compute_payload_hash(payload: &[u8]) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(payload);
        hasher.finalize().into()
    }
}

#[cfg(test)]
mod tests {

    use kapsel_core::{models::EventStatus, time::TestClock, IdempotencyStrategy};
    use kapsel_testing::TestEnv;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[tokio::test]
    async fn engine_starts_with_configured_workers() {
        let env = TestEnv::new_isolated().await.expect("test environment setup failed");
        let config = DeliveryConfig { worker_count: 5, ..Default::default() };

        let mut engine = DeliveryEngine::new(
            env.create_pool(),
            config,
            Arc::new(env.clock.clone()) as Arc<dyn Clock>,
        )
        .expect("engine creation should succeed");
        engine.start().await.expect("engine should start successfully");

        let stats = engine.stats().await;
        assert_eq!(stats.active_workers, 5);

        engine.shutdown().await.expect("engine should shutdown gracefully");
    }

    #[tokio::test]
    async fn engine_shuts_down_gracefully() {
        let env = TestEnv::new_isolated().await.expect("test environment setup failed");
        let config = DeliveryConfig::default();
        let mut engine = DeliveryEngine::new(
            env.create_pool(),
            config,
            Arc::new(env.clock.clone()) as Arc<dyn Clock>,
        )
        .expect("engine creation should succeed");

        engine.start().await.expect("engine should start");

        let shutdown_result = engine.shutdown().await;
        assert!(shutdown_result.is_ok(), "shutdown should complete without error");
    }

    #[tokio::test]
    async fn successful_delivery_updates_database_correctly() {
        let env = TestEnv::new_isolated().await.expect("test environment setup failed");
        let mock_server = MockServer::start().await;
        let webhook_url = format!("{}/webhook", mock_server.uri());

        // Set up mock to accept webhook
        Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/webhook"))
            .respond_with(ResponseTemplate::new(200).set_body_string("OK"))
            .expect(1)
            .mount(&mock_server)
            .await;

        // Insert test tenant and endpoint using isolated database
        let (_tenant_id, _endpoint_id, event_id) =
            setup_test_data_isolated(&env, &webhook_url).await;

        // Create worker with the pool
        let worker = create_test_worker_with_pool(env.pool());

        // Get the event and attempt delivery
        let event = event_by_id(&env.storage(), &event_id).await;
        let result = worker.attempt_delivery(&event).await;
        assert!(result.is_ok(), "delivery should succeed: {:?}", result.err());

        // Verify event is marked as delivered
        let updated_event = event_by_id(&env.storage(), &event_id).await;
        assert_eq!(updated_event.status, EventStatus::Delivered);
        assert!(updated_event.delivered_at.is_some());

        // Verify delivery attempt was recorded (skip for now due to schema issues)
        // TODO: Re-enable once audit trail schema is confirmed

        mock_server.verify().await;
    }

    #[tokio::test]
    async fn failed_delivery_schedules_retry() {
        let env = TestEnv::new_isolated().await.expect("test environment setup failed");
        let mock_server = MockServer::start().await;
        let webhook_url = format!("{}/webhook", mock_server.uri());

        // Set up mock to return 503 (retryable error)
        Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/webhook"))
            .respond_with(ResponseTemplate::new(503).set_body_string("Service Unavailable"))
            .expect(1)
            .mount(&mock_server)
            .await;

        // Insert test tenant and endpoint using isolated database
        let (_tenant_id, _endpoint_id, event_id) =
            setup_test_data_isolated(&env, &webhook_url).await;

        let worker = create_test_worker_with_pool(env.pool());

        // Get event and attempt delivery
        let event = event_by_id(&env.storage(), &event_id).await;
        let result = worker.attempt_delivery(&event).await;
        assert!(
            result.is_ok(),
            "worker should handle delivery failure gracefully: {:?}",
            result.err()
        );

        // Verify event is marked for retry
        let updated_event = event_by_id(&env.storage(), &event_id).await;
        assert_eq!(updated_event.status, EventStatus::Pending);
        assert_eq!(updated_event.failure_count, 1);
        assert!(updated_event.next_retry_at.is_some());

        mock_server.verify().await;
    }

    #[tokio::test]
    async fn exhausted_retries_mark_event_failed() {
        let env = TestEnv::new_isolated().await.expect("test environment setup failed");
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
        let (_tenant_id, _endpoint_id, event_id) =
            setup_test_data_isolated(&env, &webhook_url).await;

        // Update event to be at max retry limit (endpoint has max_retries = 10, so 11
        // total attempts)
        env.storage()
            .webhook_events
            .mark_failed(event_id.into(), 10, None)
            .await
            .expect("failed to update event failure count");

        // Create worker with reduced retry policy to trigger failure
        let config = DeliveryConfig {
            default_retry_policy: RetryPolicy { max_attempts: 10, ..Default::default() },
            ..Default::default()
        };
        let worker = create_test_worker_with_config_and_pool(env.pool(), config);

        // Get event and attempt delivery (this will be attempt #11, the final one)
        let event = event_by_id(&env.storage(), &event_id).await;
        let result = worker.attempt_delivery(&event).await;
        assert!(
            result.is_ok(),
            "delivery attempt should succeed even when max retries reached: {:?}",
            result.err()
        );

        // Verify event is marked as failed (no more retries)
        let updated_event = event_by_id(&env.storage(), &event_id).await;
        assert_eq!(updated_event.status, EventStatus::Failed);
        assert!(updated_event.failed_at.is_some());

        mock_server.verify().await;
    }

    #[tokio::test]
    async fn circuit_breaker_blocks_delivery_when_open() {
        let env = TestEnv::new_isolated().await.expect("test environment setup failed");
        let mock_server = MockServer::start().await;
        let webhook_url = format!("{}/webhook", mock_server.uri());

        // Insert test data
        let (_tenant_id, endpoint_id, event_id) =
            setup_test_data_isolated(&env, &webhook_url).await;

        // Set event to delivering status (simulates normal claim process)
        env.storage()
            .webhook_events
            .update_status(event_id.into(), kapsel_core::models::EventStatus::Delivering)
            .await
            .expect("failed to update event status");

        // Create worker
        let worker = create_test_worker_with_pool(env.pool());

        // Force circuit breaker open for this endpoint
        worker
            .circuit_manager
            .write()
            .await
            .force_circuit_state(&endpoint_id.to_string(), crate::circuit::CircuitState::Open)
            .await;

        // Get event and attempt delivery
        let event = event_by_id(&env.storage(), &event_id).await;
        let result = worker.attempt_delivery(&event).await;

        // Should fail with circuit open error
        assert!(result.is_err());
        if let Err(DeliveryError::CircuitOpen { .. }) = result {
            // Expected error type
        } else {
            unreachable!("expected circuit open error");
        }

        // Event should remain in delivering status (unchanged)
        let updated_event = event_by_id(&env.storage(), &event_id).await;
        assert_eq!(updated_event.status, EventStatus::Delivering);
    }

    #[tokio::test]
    async fn worker_claims_pending_events_from_database() {
        let env = TestEnv::new_isolated().await.expect("test environment setup failed");
        let mock_server = MockServer::start().await;
        let webhook_url = format!("{}/webhook", mock_server.uri());

        // Insert test tenant, endpoint, and first event
        let (tenant_id, endpoint_id, event1_id) =
            setup_test_data_isolated(&env, &webhook_url).await;

        // Insert second event under same tenant/endpoint
        let event2_id = uuid::Uuid::new_v4();
        let clock = Arc::new(TestClock::new());
        let now = DateTime::<Utc>::from(clock.now_system());
        let webhook_event2 = WebhookEvent {
            id: event2_id.into(),
            tenant_id: tenant_id.into(),
            endpoint_id: endpoint_id.into(),
            source_event_id: format!("source-{event2_id}"),
            idempotency_strategy: IdempotencyStrategy::Header,
            status: kapsel_core::models::EventStatus::Pending,
            failure_count: 0,
            last_attempt_at: None,
            next_retry_at: None,
            headers: sqlx::types::Json(std::collections::HashMap::from([(
                "x-test".to_string(),
                "value".to_string(),
            )])),
            body: b"test payload 2".to_vec(),
            content_type: "application/json".to_string(),
            received_at: now,
            delivered_at: None,
            failed_at: None,
            payload_size: 14,
            signature_valid: None,
            signature_error: None,
        };
        let mut tx = env.pool().begin().await.expect("failed to begin transaction");
        env.storage()
            .webhook_events
            .create_in_tx(&mut tx, &webhook_event2)
            .await
            .expect("failed to create second webhook event");
        tx.commit().await.expect("failed to commit transaction");

        // Create worker with the pool
        let worker = create_test_worker_with_pool(env.pool());

        // Claim events
        let claimed_events = worker.claim_pending_events().await.expect("failed to claim events");

        // Should have claimed both events
        assert_eq!(claimed_events.len(), 2);

        // Events should be marked as delivering in database
        let event1_status = env
            .storage()
            .webhook_events
            .find_by_id(event1_id.into())
            .await
            .expect("failed to find event1")
            .expect("event1 not found");
        let event2_status = env
            .storage()
            .webhook_events
            .find_by_id(event2_id.into())
            .await
            .expect("failed to find event2")
            .expect("event2 not found");

        assert_eq!(event1_status.status, kapsel_core::models::EventStatus::Delivering);
        assert_eq!(event2_status.status, kapsel_core::models::EventStatus::Delivering);
    }

    #[tokio::test]
    async fn non_retryable_errors_mark_event_failed_immediately() {
        let env = TestEnv::new_isolated().await.expect("test environment setup failed");
        let mock_server = MockServer::start().await;
        let webhook_url = format!("{}/webhook", mock_server.uri());

        // Set up mock to return 400 (non-retryable error)
        Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/webhook"))
            .respond_with(ResponseTemplate::new(400).set_body_string("Bad Request"))
            .expect(1)
            .mount(&mock_server)
            .await;

        // Insert test tenant and endpoint using isolated database
        let (_tenant_id, _endpoint_id, event_id) =
            setup_test_data_isolated(&env, &webhook_url).await;

        let worker = create_test_worker_with_pool(env.pool());

        // Get event and attempt delivery
        let event = event_by_id(&env.storage(), &event_id).await;
        let result = worker.attempt_delivery(&event).await;
        assert!(
            result.is_ok(),
            "worker should handle non-retryable error gracefully: {:?}",
            result.err()
        );

        // Verify event is marked as failed immediately (no retry for 4xx)
        let updated_event = event_by_id(&env.storage(), &event_id).await;
        assert_eq!(updated_event.status, EventStatus::Failed);
        assert!(updated_event.failed_at.is_some());
        assert!(updated_event.next_retry_at.is_none());

        mock_server.verify().await;
    }

    async fn setup_test_data_isolated(
        env: &TestEnv,
        webhook_url: &str,
    ) -> (uuid::Uuid, uuid::Uuid, uuid::Uuid) {
        let tenant_id = uuid::Uuid::new_v4();
        let event_id = uuid::Uuid::new_v4();

        // Insert tenant
        let tenant_name = format!("test-tenant-{}", tenant_id.simple());
        let mut tx = env.pool().begin().await.expect("failed to begin transaction");
        let tenant_id =
            env.create_tenant_tx(&mut tx, &tenant_name).await.expect("failed to create tenant");
        tx.commit().await.expect("failed to commit transaction");

        // Insert endpoint
        let mut tx = env.pool().begin().await.expect("failed to begin transaction");
        let endpoint_id = env
            .create_endpoint_tx(&mut tx, tenant_id, webhook_url)
            .await
            .expect("failed to create endpoint");
        tx.commit().await.expect("failed to commit transaction");

        // Insert webhook event using repository
        let clock = Arc::new(TestClock::new());
        let now = DateTime::<Utc>::from(clock.now_system());
        let webhook_event = WebhookEvent {
            id: event_id.into(),
            tenant_id,
            endpoint_id,
            source_event_id: format!("source-{event_id}"),
            idempotency_strategy: IdempotencyStrategy::Header,
            status: kapsel_core::models::EventStatus::Pending,
            failure_count: 0,
            last_attempt_at: None,
            next_retry_at: None,
            headers: sqlx::types::Json(std::collections::HashMap::from([(
                "x-test".to_string(),
                "value".to_string(),
            )])),
            body: b"test payload".to_vec(),
            content_type: "application/json".to_string(),
            received_at: now,
            delivered_at: None,
            failed_at: None,
            payload_size: 12,
            signature_valid: None,
            signature_error: None,
        };
        let mut tx = env.pool().begin().await.expect("failed to begin transaction");
        env.storage()
            .webhook_events
            .create_in_tx(&mut tx, &webhook_event)
            .await
            .expect("failed to create webhook event");
        tx.commit().await.expect("failed to commit transaction");

        (tenant_id.0, endpoint_id.0, event_id)
    }

    fn create_test_worker_with_pool(pool: &PgPool) -> DeliveryWorker {
        let storage = Arc::new(Storage::new(pool.clone(), &{
            let clock: Arc<dyn Clock> = Arc::new(TestClock::new());
            clock
        }));
        DeliveryWorker::new(
            0,
            storage,
            DeliveryConfig::default(),
            Arc::new(
                DeliveryClient::with_defaults(Arc::new(TestClock::new()))
                    .expect("failed to create client"),
            ),
            Arc::new(RwLock::new(CircuitBreakerManager::new(CircuitConfig::default()))),
            Arc::new(RwLock::new(EngineStats::default())),
            CancellationToken::new(),
            Arc::new(MulticastEventHandler::new(Arc::new(TestClock::new()))),
            Arc::new(TestClock::new()),
        )
    }

    fn create_test_worker_with_config_and_pool(
        pool: &PgPool,
        config: DeliveryConfig,
    ) -> DeliveryWorker {
        let storage = Arc::new(Storage::new(pool.clone(), &{
            let clock: Arc<dyn Clock> = Arc::new(TestClock::new());
            clock
        }));
        DeliveryWorker::new(
            0,
            storage,
            config,
            Arc::new(
                DeliveryClient::with_defaults(Arc::new(TestClock::new()))
                    .expect("failed to create client"),
            ),
            Arc::new(RwLock::new(CircuitBreakerManager::new(CircuitConfig::default()))),
            Arc::new(RwLock::new(EngineStats::default())),
            CancellationToken::new(),
            Arc::new(MulticastEventHandler::new(Arc::new(TestClock::new()))),
            Arc::new(TestClock::new()),
        )
    }

    async fn event_by_id(storage: &Storage, event_id: &uuid::Uuid) -> WebhookEvent {
        storage
            .webhook_events
            .find_by_id((*event_id).into())
            .await
            .expect("failed to fetch webhook event")
            .expect("event not found")
    }
}
