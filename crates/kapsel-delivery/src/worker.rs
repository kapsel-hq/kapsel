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
    retry::{RetryContext, RetryDecision, RetryPolicy},
    storage::{DeliveryStorage, PostgresDeliveryStorage},
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
    storage: Arc<dyn DeliveryStorage>,
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
        storage: Arc<dyn DeliveryStorage>,
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
            storage,
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
    pub fn new(pool: &PgPool, config: DeliveryConfig, clock: Arc<dyn Clock>) -> Result<Self> {
        let mut event_handler = MulticastEventHandler::new(clock.clone());
        let signing_service = SigningService::ephemeral();
        let concrete_storage = Arc::new(Storage::new(pool.clone(), &clock.clone()));
        let merkle_service = Arc::new(tokio::sync::RwLock::new(MerkleService::new(
            concrete_storage.clone(),
            signing_service,
            clock.clone(),
        )));
        let attestation_subscriber = Arc::new(AttestationEventSubscriber::new(merkle_service));
        event_handler.add_subscriber(attestation_subscriber);

        let delivery_storage: Arc<dyn DeliveryStorage> =
            Arc::new(PostgresDeliveryStorage::new(concrete_storage));
        Self::with_event_handler(delivery_storage, config, clock, Arc::new(event_handler))
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
            self.storage.clone(),
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
    pub async fn process_batch(&self) -> Result<()> {
        // Create and run a temporary worker for this batch
        let worker = DeliveryWorker::new(
            0, // Single worker ID
            self.storage.clone(),
            self.config.clone(),
            self.client.clone(),
            self.circuit_manager.clone(),
            self.stats.clone(),
            self.cancellation_token.clone(),
            self.event_handler.clone(),
            self.clock.clone(),
        );

        worker.process_batch().await.map(|_| ())
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
    storage: Arc<dyn DeliveryStorage>,
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
        storage: Arc<dyn DeliveryStorage>,
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
            self.storage.claim_pending_events(self.config.batch_size).await.map_err(|e| {
                DeliveryError::database(format!("failed to claim pending events: {e}"))
            })?;

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

        // 1. Get endpoint URL from database first (needed for audit trail)
        let endpoint_url = self.endpoint_url(&event.endpoint_id).await?;

        // 2. Check circuit breaker state
        let should_allow =
            self.circuit_manager.read().await.should_allow_request(&endpoint_key).await;

        if !should_allow {
            // Circuit breaker is open - record attempt for audit trail before failing
            let delivery_attempt_id = Uuid::new_v4();
            let circuit_error = DeliveryError::circuit_open(endpoint_key.clone());

            // Record the blocked attempt
            self.record_delivery_attempt(
                event,
                &endpoint_url,
                attempt_number,
                &Err(circuit_error.clone()),
                start_time.elapsed(),
                delivery_attempt_id,
            )
            .await;

            // Update failure statistics
            {
                let mut stats = self.stats.write().await;
                stats.failed_deliveries += 1;
            }

            // Event should remain in its current state for later retry
            return Err(circuit_error);
        }

        debug!(
            worker_id = self.id,
            event_id = %event.id,
            attempt_number,
            endpoint_url = %endpoint_url,
            "attempting webhook delivery"
        );

        // 3. Build delivery request
        let delivery_attempt_id = Uuid::new_v4();
        debug!(
            worker_id = self.id,
            event_id = %event.id,
            delivery_attempt_id = %delivery_attempt_id,
            "Generated delivery attempt ID"
        );
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
        debug!(
            worker_id = self.id,
            event_id = %event.id,
            delivery_attempt_id = %delivery_attempt_id,
            "Recording delivery attempt to database"
        );
        self.record_delivery_attempt(
            event,
            &endpoint_url,
            u32::try_from(event.failure_count + 1).unwrap_or(u32::MAX),
            &delivery_result,
            start_time.elapsed(),
            delivery_attempt_id,
        )
        .await;

        // 6. Handle result and update event status
        // Important: We always update the database state, even on failure
        match delivery_result {
            Ok(response) => {
                if response.is_success {
                    // Success - update circuit breaker, mark event delivered, and update stats
                    self.circuit_manager.write().await.record_success(&endpoint_key).await;
                    self.mark_event_delivered(&event.id).await?;

                    // Update success statistics
                    {
                        let mut stats = self.stats.write().await;
                        stats.successful_deliveries += 1;
                    }

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
                    debug!(
                        worker_id = self.id,
                        event_id = %event.id,
                        delivery_attempt_id = %delivery_attempt_id,
                        "Publishing delivery success event for attestation"
                    );
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

                    // Update failure statistics
                    {
                        let mut stats = self.stats.write().await;
                        stats.failed_deliveries += 1;
                    }

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
                // Failure - update failure statistics, circuit breaker and handle retry logic
                {
                    let mut stats = self.stats.write().await;
                    stats.failed_deliveries += 1;
                }

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
            self.handle_retryable_failure(event_id, attempt_number, error, response_headers, event)
                .await
        } else {
            self.handle_non_retryable_failure(event_id, attempt_number, error, event).await
        }
    }

    async fn handle_retryable_failure(
        &self,
        event_id: &EventId,
        attempt_number: u32,
        error: crate::error::DeliveryError,
        response_headers: Option<&std::collections::HashMap<String, String>>,
        event: &WebhookEvent,
    ) -> Result<()> {
        // Get endpoint configuration to use endpoint-specific retry limits
        let endpoint_config = match self.storage.find_endpoint_config(event.endpoint_id).await {
            Ok(config) => config,
            Err(e) => {
                return self
                    .handle_fallback_retry_policy(event_id, attempt_number, error, event, e)
                    .await;
            },
        };

        let endpoint_retry_policy = self.build_endpoint_retry_policy(&endpoint_config);
        self.execute_retry_decision(
            event_id,
            attempt_number,
            error,
            response_headers,
            event,
            endpoint_retry_policy,
        )
        .await
    }

    async fn handle_fallback_retry_policy(
        &self,
        event_id: &EventId,
        attempt_number: u32,
        error: crate::error::DeliveryError,
        event: &WebhookEvent,
        config_error: kapsel_core::error::CoreError,
    ) -> Result<()> {
        error!(
            worker_id = self.id,
            event_id = %event_id,
            endpoint_id = %event.endpoint_id,
            error = %config_error,
            "failed to retrieve endpoint config, using defaults"
        );

        let retry_context = RetryContext::new(
            attempt_number,
            error.clone(),
            DateTime::<Utc>::from(self.clock.now_system()),
            self.config.default_retry_policy.clone(),
        );

        match retry_context.decide_retry() {
            RetryDecision::GiveUp { reason } => {
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
            RetryDecision::Retry { next_attempt_at } => {
                self.schedule_retry(event, next_attempt_at).await?;
                warn!(
                    worker_id = self.id,
                    event_id = %event_id,
                    attempt_number,
                    next_retry_at = %next_attempt_at,
                    error = %error,
                    "delivery failed, retry scheduled with default policy"
                );
            },
        }
        Ok(())
    }

    fn build_endpoint_retry_policy(
        &self,
        endpoint_config: &kapsel_core::models::Endpoint,
    ) -> RetryPolicy {
        RetryPolicy {
            max_attempts: u32::try_from(endpoint_config.max_retries).unwrap_or(3) + 1, /* +1 for initial attempt */
            base_delay: self.config.default_retry_policy.base_delay,
            max_delay: self.config.default_retry_policy.max_delay,
            jitter_factor: self.config.default_retry_policy.jitter_factor,
            backoff_strategy: match endpoint_config.retry_strategy {
                kapsel_core::models::BackoffStrategy::Fixed => crate::retry::BackoffStrategy::Fixed,
                kapsel_core::models::BackoffStrategy::Exponential => {
                    crate::retry::BackoffStrategy::Exponential
                },
                kapsel_core::models::BackoffStrategy::Linear => {
                    crate::retry::BackoffStrategy::Linear
                },
            },
        }
    }

    async fn execute_retry_decision(
        &self,
        event_id: &EventId,
        attempt_number: u32,
        error: crate::error::DeliveryError,
        response_headers: Option<&std::collections::HashMap<String, String>>,
        event: &WebhookEvent,
        retry_policy: RetryPolicy,
    ) -> Result<()> {
        let retry_context = RetryContext::new(
            attempt_number,
            error.clone(),
            DateTime::<Utc>::from(self.clock.now_system()),
            retry_policy,
        );

        match retry_context.decide_retry() {
            RetryDecision::Retry { mut next_attempt_at } => {
                // Check for Retry-After header to override calculated delay
                if let Some(headers) = response_headers {
                    if let Some(retry_after_seconds) =
                        extract_retry_after_seconds(headers, self.clock.as_ref())
                    {
                        let retry_after_delay = chrono::Duration::try_seconds(
                            i64::try_from(retry_after_seconds).unwrap_or(i64::MAX),
                        )
                        .unwrap_or(chrono::Duration::zero());
                        next_attempt_at =
                            DateTime::<Utc>::from(self.clock.now_system()) + retry_after_delay;

                        debug!(
                            worker_id = self.id,
                            event_id = %event_id,
                            retry_after_seconds,
                            calculated_retry_at = %next_attempt_at,
                            "using Retry-After header for retry timing"
                        );
                    }
                }

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
            RetryDecision::GiveUp { reason } => {
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
        Ok(())
    }

    async fn handle_non_retryable_failure(
        &self,
        event_id: &EventId,
        attempt_number: u32,
        error: crate::error::DeliveryError,
        event: &WebhookEvent,
    ) -> Result<()> {
        self.mark_event_failed(event).await?;
        error!(
            worker_id = self.id,
            event_id = %event_id,
            attempt_number,
            error = %error,
            "delivery failed with non-retryable error"
        );
        Ok(())
    }

    /// Endpoint URL from the database.
    ///
    /// # Errors
    ///
    /// Returns error if endpoint is not found or database query fails.
    async fn endpoint_url(&self, endpoint_id: &EndpointId) -> Result<String> {
        self.storage
            .find_endpoint_url(*endpoint_id)
            .await
            .map_err(|e| DeliveryError::database(format!("failed to fetch endpoint: {e}")))
    }

    /// Updates event status to 'delivered' after successful delivery.
    ///
    /// # Errors
    ///
    /// Returns error if database update fails.
    async fn mark_event_delivered(&self, event_id: &EventId) -> Result<()> {
        self.storage
            .mark_delivered(*event_id)
            .await
            .map_err(|e| DeliveryError::database(format!("failed to mark event as delivered: {e}")))
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
            .schedule_retry(
                event.id,
                next_retry_at,
                u32::try_from(event.failure_count + 1).unwrap_or(0),
            )
            .await
            .map_err(|e| DeliveryError::database(format!("failed to schedule event retry: {e}")))
    }

    /// Marks event as permanently failed when retries are exhausted.
    ///
    /// # Errors
    ///
    /// Returns error if database update fails.
    async fn mark_event_failed(&self, event: &WebhookEvent) -> Result<()> {
        self.storage
            .mark_failed(event.id)
            .await
            .map_err(|e| DeliveryError::database(format!("failed to mark event as failed: {e}")))
    }

    /// Records a delivery attempt in the audit trail.
    async fn record_delivery_attempt(
        &self,
        event: &WebhookEvent,
        _url: &str,
        attempt_number: u32,
        result: &Result<crate::client::DeliveryResponse>,
        _duration: std::time::Duration,
        delivery_attempt_id: Uuid,
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
            id: delivery_attempt_id,
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

        if let Err(e) = self.storage.record_delivery_attempt(delivery_attempt).await {
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
