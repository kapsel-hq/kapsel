//! Storage abstraction layer for the delivery engine.
//!
//! Provides trait-based abstractions over storage operations to enable
//! testability without database dependencies. Production implementations
//! use the concrete `kapsel_core::storage::Storage` while tests can provide
//! mock implementations for deterministic behavior validation.

use std::{future::Future, pin::Pin, sync::Arc};

use chrono::{DateTime, Utc};
use kapsel_core::{
    error::Result,
    models::{DeliveryAttempt, EndpointId, EventId, EventStatus, WebhookEvent},
};

/// Storage operations required by the delivery engine.
///
/// This trait abstracts all database operations needed for webhook delivery,
/// enabling both production PostgreSQL implementations and lightweight test
/// doubles. The separation allows testing delivery logic, retry policies,
/// and error handling without database overhead.
pub trait DeliveryStorage: Send + Sync + 'static {
    /// Claims pending webhook events for processing.
    ///
    /// Uses FOR UPDATE SKIP LOCKED in production to enable lock-free
    /// concurrent claiming. Returns up to `batch_size` events ordered
    /// by retry time and received time for FIFO processing.
    fn claim_pending_events(
        &self,
        batch_size: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<WebhookEvent>>> + Send + '_>>;

    /// Marks a webhook event as successfully delivered.
    ///
    /// Updates the event status to 'delivered' and records the delivery
    /// timestamp. This is a terminal state - no further processing occurs.
    fn mark_delivered(
        &self,
        event_id: EventId,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;

    /// Schedules a webhook event for retry after a failed delivery.
    ///
    /// Updates the failure count and calculates the next retry time based
    /// on the retry policy. Events exceeding max retries transition to
    /// 'failed' status.
    fn schedule_retry(
        &self,
        event_id: EventId,
        next_retry_at: DateTime<Utc>,
        failure_count: u32,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;

    /// Marks a webhook event as permanently failed.
    ///
    /// Updates the event status to 'failed' after exhausting all retry
    /// attempts. This is a terminal state indicating delivery could not
    /// be completed.
    fn mark_failed(
        &self,
        event_id: EventId,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;

    /// Records a delivery attempt for audit and attestation.
    ///
    /// Stores detailed information about each delivery attempt including
    /// HTTP status, response data, and timing. Used for cryptographic
    /// attestation and debugging.
    fn record_delivery_attempt(
        &self,
        attempt: DeliveryAttempt,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;

    /// Finds the endpoint URL for a given endpoint ID.
    ///
    /// Returns the configured destination URL where webhooks should be
    /// delivered. Used to construct HTTP requests.
    fn find_endpoint_url(
        &self,
        endpoint_id: EndpointId,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + '_>>;

    /// Finds the current status of a webhook event.
    ///
    /// Used for verification in tests and monitoring event lifecycle.
    fn find_event_status(
        &self,
        event_id: EventId,
    ) -> Pin<Box<dyn Future<Output = Result<EventStatus>> + Send + '_>>;

    /// Finds all delivery attempts for a webhook event.
    ///
    /// Returns attempts ordered by timestamp for debugging and verification.
    fn find_delivery_attempts(
        &self,
        event_id: EventId,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<DeliveryAttempt>>> + Send + '_>>;

    /// Finds the endpoint configuration for retry policy decisions.
    ///
    /// Returns the endpoint configuration including max_retries and other
    /// delivery settings. Used by workers to make endpoint-specific retry
    /// decisions instead of relying only on global defaults.
    fn find_endpoint_config(
        &self,
        endpoint_id: EndpointId,
    ) -> Pin<Box<dyn Future<Output = Result<kapsel_core::models::Endpoint>> + Send + '_>>;
}

/// Production storage implementation using PostgreSQL.
///
/// Wraps the concrete `kapsel_core::storage::Storage` to implement the
/// `DeliveryStorage` trait. All database operations go through the
/// repository pattern for consistency and type safety.
pub struct PostgresDeliveryStorage {
    storage: Arc<kapsel_core::storage::Storage>,
}

impl PostgresDeliveryStorage {
    /// Creates a new PostgreSQL storage adapter.
    pub fn new(storage: Arc<kapsel_core::storage::Storage>) -> Self {
        Self { storage }
    }
}

impl DeliveryStorage for PostgresDeliveryStorage {
    fn claim_pending_events(
        &self,
        batch_size: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<WebhookEvent>>> + Send + '_>> {
        let storage = self.storage.clone();
        Box::pin(async move { storage.webhook_events.claim_pending(batch_size).await })
    }

    fn mark_delivered(
        &self,
        event_id: EventId,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        let storage = self.storage.clone();
        Box::pin(async move { storage.webhook_events.mark_delivered(event_id).await })
    }

    fn schedule_retry(
        &self,
        event_id: EventId,
        next_retry_at: DateTime<Utc>,
        failure_count: u32,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        let storage = self.storage.clone();
        Box::pin(async move {
            storage
                .webhook_events
                .mark_failed(
                    event_id,
                    failure_count.try_into().unwrap_or(i32::MAX),
                    Some(next_retry_at),
                )
                .await
        })
    }

    fn mark_failed(
        &self,
        event_id: EventId,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        let storage = self.storage.clone();
        Box::pin(async move { storage.webhook_events.mark_failed(event_id, 0, None).await })
    }

    fn record_delivery_attempt(
        &self,
        attempt: DeliveryAttempt,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        let storage = self.storage.clone();
        Box::pin(async move { storage.delivery_attempts.create(&attempt).await.map(|_| ()) })
    }

    fn find_endpoint_url(
        &self,
        endpoint_id: EndpointId,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + '_>> {
        let storage = self.storage.clone();
        Box::pin(async move {
            storage
                .endpoints
                .find_by_id(endpoint_id)
                .await?
                .ok_or_else(|| {
                    kapsel_core::error::CoreError::Database(format!(
                        "endpoint {endpoint_id} not found"
                    ))
                })
                .map(|endpoint| endpoint.url)
        })
    }

    fn find_event_status(
        &self,
        event_id: EventId,
    ) -> Pin<Box<dyn Future<Output = Result<EventStatus>> + Send + '_>> {
        let storage = self.storage.clone();
        Box::pin(async move {
            storage
                .webhook_events
                .find_by_id(event_id)
                .await
                .map_err(|e| kapsel_core::error::CoreError::Database(e.to_string()))?
                .ok_or_else(|| {
                    kapsel_core::error::CoreError::Database(format!(
                        "webhook event {event_id} not found"
                    ))
                })
                .map(|event| event.status)
        })
    }

    fn find_delivery_attempts(
        &self,
        event_id: EventId,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<DeliveryAttempt>>> + Send + '_>> {
        let storage = self.storage.clone();
        Box::pin(async move { storage.delivery_attempts.find_by_event(event_id).await })
    }

    fn find_endpoint_config(
        &self,
        endpoint_id: EndpointId,
    ) -> Pin<Box<dyn Future<Output = Result<kapsel_core::models::Endpoint>> + Send + '_>> {
        let storage = self.storage.clone();
        Box::pin(async move {
            storage.endpoints.find_by_id(endpoint_id).await?.ok_or_else(|| {
                kapsel_core::error::CoreError::NotFound(format!(
                    "endpoint not found: {}",
                    endpoint_id.0
                ))
            })
        })
    }
}

pub mod mock {
    //! Mock storage implementation for testing.
    //!
    //! Provides deterministic, in-memory storage for testing delivery logic
    //! without database dependencies. Supports configurable behavior for
    //! simulating various storage conditions.

    use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};

    use chrono::{DateTime, Utc};
    use kapsel_core::error::Result;
    use tokio::sync::RwLock;

    use super::{DeliveryAttempt, DeliveryStorage, EndpointId, EventId, EventStatus, WebhookEvent};

    /// Mock storage for testing delivery logic without database.
    ///
    /// Stores data in-memory with configurable behavior. Supports injecting
    /// failures, controlling event sequences, and verifying operations.
    pub struct MockDeliveryStorage {
        events: Arc<RwLock<HashMap<EventId, WebhookEvent>>>,
        attempts: Arc<RwLock<Vec<DeliveryAttempt>>>,
        endpoints: Arc<RwLock<HashMap<EndpointId, kapsel_core::models::Endpoint>>>,
        pending_events: Arc<RwLock<Vec<WebhookEvent>>>,
        claim_error: Arc<RwLock<Option<String>>>,
    }

    impl MockDeliveryStorage {
        /// Creates a new mock storage with empty state.
        pub fn new() -> Self {
            Self {
                events: Arc::new(RwLock::new(HashMap::new())),
                attempts: Arc::new(RwLock::new(Vec::new())),
                endpoints: Arc::new(RwLock::new(HashMap::new())),
                pending_events: Arc::new(RwLock::new(Vec::new())),
                claim_error: Arc::new(RwLock::new(None)),
            }
        }

        /// Adds a webhook event to the pending queue.
        pub async fn add_pending_event(&self, event: WebhookEvent) {
            self.events.write().await.insert(event.id, event.clone());
            self.pending_events.write().await.push(event);
        }

        /// Configures an endpoint for testing.
        pub async fn add_endpoint(&self, endpoint: kapsel_core::models::Endpoint) {
            self.endpoints.write().await.insert(endpoint.id, endpoint);
        }

        /// Configures an endpoint URL for testing (convenience method).
        pub async fn add_endpoint_url(&self, endpoint_id: EndpointId, url: String) {
            use chrono::Utc;
            use kapsel_core::models::{
                BackoffStrategy, CircuitState, Endpoint, SignatureConfig, TenantId,
            };
            let endpoint = Endpoint {
                id: endpoint_id,
                tenant_id: TenantId::new(),
                url,
                name: "Test Endpoint".to_string(),
                is_active: true,
                signature_config: SignatureConfig::None,
                max_retries: 3,
                timeout_seconds: 30,
                retry_strategy: BackoffStrategy::Exponential,
                circuit_state: CircuitState::Closed,
                circuit_failure_count: 0,
                circuit_half_open_at: None,
                circuit_last_failure_at: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                circuit_success_count: 0,
                deleted_at: None,
                total_events_received: 0,
                total_events_delivered: 0,
                total_events_failed: 0,
            };
            self.add_endpoint(endpoint).await;
        }

        /// Injects an error for the next claim operation.
        pub async fn inject_claim_error(&self, error: String) {
            *self.claim_error.write().await = Some(error);
        }

        /// Returns all recorded delivery attempts for verification.
        pub async fn recorded_attempts(&self) -> Vec<DeliveryAttempt> {
            self.attempts.read().await.clone()
        }

        /// Verifies an event reached the expected status.
        pub async fn verify_event_status(&self, event_id: EventId, expected: EventStatus) -> bool {
            self.events.read().await.get(&event_id).is_some_and(|e| e.status == expected)
        }
    }

    impl Default for MockDeliveryStorage {
        fn default() -> Self {
            Self::new()
        }
    }

    impl DeliveryStorage for MockDeliveryStorage {
        fn claim_pending_events(
            &self,
            batch_size: usize,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<WebhookEvent>>> + Send + '_>> {
            let claim_error = self.claim_error.clone();
            let pending_events = self.pending_events.clone();
            let events = self.events.clone();

            Box::pin(async move {
                // Check for injected errors
                let error = claim_error.write().await.take();
                if let Some(error) = error {
                    return Err(kapsel_core::error::CoreError::Database(error));
                }

                // Return up to batch_size events from pending queue
                let mut pending = pending_events.write().await;
                let pending_len = pending.len();
                let mut claimed: Vec<WebhookEvent> =
                    pending.drain(..batch_size.min(pending_len)).collect();
                drop(pending);

                // Update status in main storage and claimed events
                let mut events_map = events.write().await;
                for event in &mut claimed {
                    event.status = EventStatus::Delivering;
                    if let Some(stored) = events_map.get_mut(&event.id) {
                        stored.status = EventStatus::Delivering;
                    }
                }

                Ok(claimed)
            })
        }

        fn mark_delivered(
            &self,
            event_id: EventId,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
            let events = self.events.clone();
            Box::pin(async move {
                if let Some(event) = events.write().await.get_mut(&event_id) {
                    event.status = EventStatus::Delivered;
                    event.delivered_at = Some(Utc::now());
                }
                Ok(())
            })
        }

        fn schedule_retry(
            &self,
            event_id: EventId,
            next_retry_at: DateTime<Utc>,
            failure_count: u32,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
            let events = self.events.clone();
            Box::pin(async move {
                if let Some(event) = events.write().await.get_mut(&event_id) {
                    event.status = EventStatus::Pending;
                    event.next_retry_at = Some(next_retry_at);
                    event.failure_count = failure_count.try_into().unwrap_or(i32::MAX);
                }
                Ok(())
            })
        }

        fn mark_failed(
            &self,
            event_id: EventId,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
            let events = self.events.clone();
            Box::pin(async move {
                if let Some(event) = events.write().await.get_mut(&event_id) {
                    event.status = EventStatus::Failed;
                    event.failed_at = Some(Utc::now());
                }
                Ok(())
            })
        }

        fn record_delivery_attempt(
            &self,
            attempt: DeliveryAttempt,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
            let attempts = self.attempts.clone();
            Box::pin(async move {
                attempts.write().await.push(attempt);
                Ok(())
            })
        }

        fn find_endpoint_url(
            &self,
            endpoint_id: EndpointId,
        ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + '_>> {
            let endpoints = self.endpoints.clone();
            Box::pin(async move {
                endpoints.read().await.get(&endpoint_id).map(|e| e.url.clone()).ok_or_else(|| {
                    kapsel_core::error::CoreError::NotFound(format!(
                        "endpoint {} not found",
                        endpoint_id.0
                    ))
                })
            })
        }

        fn find_event_status(
            &self,
            event_id: EventId,
        ) -> Pin<Box<dyn Future<Output = Result<EventStatus>> + Send + '_>> {
            let events = self.events.clone();
            Box::pin(async move {
                events.read().await.get(&event_id).map(|e| e.status).ok_or_else(|| {
                    kapsel_core::error::CoreError::NotFound(format!(
                        "event {} not found",
                        event_id.0
                    ))
                })
            })
        }

        fn find_delivery_attempts(
            &self,
            event_id: EventId,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<DeliveryAttempt>>> + Send + '_>> {
            let attempts = self.attempts.clone();
            Box::pin(async move {
                let attempts_vec = attempts
                    .read()
                    .await
                    .iter()
                    .filter(|attempt| attempt.event_id == event_id)
                    .cloned()
                    .collect();
                Ok(attempts_vec)
            })
        }

        fn find_endpoint_config(
            &self,
            endpoint_id: EndpointId,
        ) -> Pin<Box<dyn Future<Output = Result<kapsel_core::models::Endpoint>> + Send + '_>>
        {
            let endpoints = self.endpoints.clone();
            Box::pin(async move {
                endpoints.read().await.get(&endpoint_id).cloned().ok_or_else(|| {
                    kapsel_core::error::CoreError::NotFound(format!(
                        "endpoint {} not found",
                        endpoint_id.0
                    ))
                })
            })
        }
    }
}
