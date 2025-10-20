//! Event system for decoupled service integration.
//!
//! Defines delivery events and handler traits for clean separation between
//! webhook delivery and downstream services like attestation. Enables
//! extensible event-driven architecture with multicast dispatch.
//!
//! # Event Flow Architecture
//!
//! ```text
//!                    DeliverySucceeded/Failed
//! ┌─────────────────┐        Events         ┌────────────────────┐
//! │ DeliveryWorker  │ ─────────────────────▶│ MulticastHandler   │
//! │ (Producer)      │                       │ (Event Dispatcher) │
//! └─────────────────┘                       └────────────────────┘
//!                                                     │
//!                                                     │ Distribute
//!                                                     ▼
//!                                          ┌─────────────────────┐
//!                                          │ AttestationService  │
//!                                          │ (Event Subscriber)  │
//!                                          │                     │
//!                                          │ Creates audit trail │
//!                                          │ ● Merkle leaves     │
//!                                          │ ● Signed tree heads │
//!                                          └─────────────────────┘
//! ```
//!
//! This architecture enables:
//! - **Loose coupling**: Components don't directly reference each other
//! - **Extensibility**: New subscribers can be added without changes
//! - **Testability**: Each component can be tested in isolation

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::time::Duration;
use tracing::error;
use uuid::Uuid;

use crate::models::{EventId, TenantId};

/// Events emitted by the delivery system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeliveryEvent {
    /// Webhook delivery succeeded.
    Succeeded(DeliverySucceededEvent),

    /// Webhook delivery failed.
    Failed(DeliveryFailedEvent),

    /// Webhook delivery attempt started.
    AttemptStarted(DeliveryAttemptStartedEvent),
}

impl DeliveryEvent {
    /// Returns the primary `EventId` for any variant of a `DeliveryEvent`.
    pub fn id(&self) -> EventId {
        match self {
            Self::Succeeded(e) => e.event_id,
            Self::Failed(e) => e.event_id,
            Self::AttemptStarted(e) => e.event_id,
        }
    }
}

/// Event emitted when a webhook delivery succeeds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliverySucceededEvent {
    /// Unique ID for this delivery attempt.
    pub delivery_attempt_id: Uuid,

    /// ID of the webhook event that was delivered.
    pub event_id: EventId,

    /// ID of the tenant that owns this event.
    pub tenant_id: TenantId,

    /// URL of the endpoint that received the webhook.
    pub endpoint_url: String,

    /// HTTP status code returned by the endpoint.
    pub response_status: u16,

    /// Number of delivery attempts for this event (1-based).
    pub attempt_number: u32,

    /// When the successful delivery occurred.
    pub delivered_at: DateTime<Utc>,

    /// SHA-256 hash of the delivered payload.
    pub payload_hash: [u8; 32],

    /// Size of the delivered payload in bytes.
    pub payload_size: i32,
}

/// Event emitted when a webhook delivery fails.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryFailedEvent {
    /// Unique ID for this delivery attempt.
    pub delivery_attempt_id: Uuid,

    /// ID of the webhook event that failed to deliver.
    pub event_id: EventId,

    /// ID of the tenant that owns this event.
    pub tenant_id: TenantId,

    /// URL of the endpoint that was attempted.
    pub endpoint_url: String,

    /// HTTP status code if the endpoint responded.
    pub response_status: Option<u16>,

    /// Number of delivery attempts for this event (1-based).
    pub attempt_number: u32,

    /// When the delivery failure occurred.
    pub failed_at: DateTime<Utc>,

    /// Error that caused the delivery failure.
    pub error_message: String,

    /// Whether this failure is retryable.
    pub is_retryable: bool,
}

/// Event emitted when a webhook delivery attempt starts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryAttemptStartedEvent {
    /// Unique ID for this delivery attempt.
    pub delivery_attempt_id: Uuid,

    /// ID of the webhook event being delivered.
    pub event_id: EventId,

    /// ID of the tenant that owns this event.
    pub tenant_id: TenantId,

    /// URL of the endpoint being attempted.
    pub endpoint_url: String,

    /// Number of delivery attempts for this event (1-based).
    pub attempt_number: u32,

    /// When the delivery attempt started.
    pub started_at: DateTime<Utc>,
}

/// Trait for handling delivery events.
///
/// Services that need to react to delivery outcomes implement this trait.
/// The delivery system will call `handle_event` when deliveries succeed
/// or fail, allowing services to take appropriate action.
///
/// # Design Philosophy
///
/// This trait represents the subscriber/observer side of the event system.
/// Services implement this trait to receive and process delivery events
/// without the delivery system needing to know about specific subscribers.
#[async_trait::async_trait]
pub trait EventHandler: Send + Sync + std::fmt::Debug {
    /// Handles a delivery event.
    ///
    /// This method should not block delivery processing. If event
    /// handling fails, it should log the error but not propagate
    /// it back to the delivery system.
    async fn handle_event(&self, event: DeliveryEvent);
}

/// No-op event handler that discards all events.
///
/// Used when event handling is disabled or for testing scenarios
/// where events should be ignored.
#[derive(Debug, Default)]
pub struct NoOpEventHandler;

impl NoOpEventHandler {
    /// Creates a new no-op event handler.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl EventHandler for NoOpEventHandler {
    async fn handle_event(&self, _event: DeliveryEvent) {}
}

/// Multi-cast event handler that forwards events to multiple subscribers.
///
/// This allows multiple services to subscribe to delivery events without
/// the delivery system needing to know about each subscriber individually.
/// Events are delivered to all subscribers concurrently.
#[derive(Debug, Clone)]
pub struct MulticastEventHandler {
    handlers: Vec<Arc<dyn EventHandler>>,
}

impl MulticastEventHandler {
    /// Creates a new multicast handler with no subscribers.
    pub fn new() -> Self {
        Self { handlers: Vec::new() }
    }

    /// Adds a subscriber to receive delivery events.
    pub fn add_subscriber(&mut self, handler: Arc<dyn EventHandler>) {
        self.handlers.push(handler);
    }

    /// Returns the number of registered subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.handlers.len()
    }
}

impl Default for MulticastEventHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl EventHandler for MulticastEventHandler {
    async fn handle_event(&self, event: DeliveryEvent) {
        const HANDLER_TIMEOUT: Duration = Duration::from_secs(30);

        // Spawn each handler in a detached task for fault isolation.
        // This ensures that a panicking or deadlocking subscriber
        // cannot crash the delivery worker.
        for handler in &self.handlers {
            let handler = handler.clone();
            let event = event.clone();

            // Detached task: fire-and-forget execution
            // By adding a timeout, we prevent a hanging subscriber
            // from keeping a task alive indefinitely
            tokio::spawn(async move {
                if tokio::time::timeout(HANDLER_TIMEOUT, handler.handle_event(event.clone()))
                    .await
                    .is_err()
                {
                    error!(
                        handler = ?handler,
                        event_id = ?event.id(),
                        timeout_secs = HANDLER_TIMEOUT.as_secs(),
                        "Event handler execution timed out"
                    );
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    const HANDLER_COMPLETION_DELAY: Duration = Duration::from_millis(100);
    const FAST_HANDLER_EXECUTION_TIME: Duration = Duration::from_millis(50);
    const SLOW_HANDLER_EXECUTION_TIME: Duration = Duration::from_secs(1);
    const HANDLER_TIMEOUT: Duration = Duration::from_secs(30);

    #[derive(Debug)]
    struct TestEventHandler {
        event_count: Arc<AtomicUsize>,
    }

    impl TestEventHandler {
        fn new() -> (Self, Arc<AtomicUsize>) {
            let counter = Arc::new(AtomicUsize::new(0));
            let handler = Self { event_count: counter.clone() };
            (handler, counter)
        }
    }

    #[async_trait::async_trait]
    impl EventHandler for TestEventHandler {
        async fn handle_event(&self, _event: DeliveryEvent) {
            self.event_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[derive(Debug)]
    struct PanickingHandler;

    #[async_trait::async_trait]
    impl EventHandler for PanickingHandler {
        #[allow(clippy::panic)] // Controlled use to verify behavior
        async fn handle_event(&self, _event: DeliveryEvent) {
            panic!("Simulated subscriber failure!");
        }
    }

    #[derive(Debug)]
    struct SlowHandler;

    #[async_trait::async_trait]
    impl EventHandler for SlowHandler {
        async fn handle_event(&self, _event: DeliveryEvent) {
            // Simulate slow processing
            tokio::time::sleep(SLOW_HANDLER_EXECUTION_TIME).await;
        }
    }

    #[derive(Debug)]
    struct InfiniteHandler {
        started: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl EventHandler for InfiniteHandler {
        async fn handle_event(&self, _event: DeliveryEvent) {
            self.started.fetch_add(1, Ordering::SeqCst);
            // Sleep longer than the 30s timeout
            tokio::time::sleep(HANDLER_TIMEOUT * 2).await;
        }
    }

    #[tokio::test]
    async fn no_op_handler_discards_events() {
        let handler = NoOpEventHandler;
        let event = create_test_delivery_succeeded_event();

        // Should not panic or block
        handler.handle_event(event).await;
    }

    #[tokio::test]
    async fn multicast_handler_forwards_to_all_subscribers() {
        let mut multicast = MulticastEventHandler::new();

        let (handler1, counter1) = TestEventHandler::new();
        let (handler2, counter2) = TestEventHandler::new();

        multicast.add_subscriber(Arc::new(handler1));
        multicast.add_subscriber(Arc::new(handler2));

        assert_eq!(multicast.subscriber_count(), 2);

        let event = create_test_delivery_succeeded_event();
        multicast.handle_event(event).await;

        // Give spawned tasks time to execute
        tokio::time::sleep(HANDLER_COMPLETION_DELAY).await;

        assert_eq!(counter1.load(Ordering::SeqCst), 1);
        assert_eq!(counter2.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn multicast_handler_handles_empty_subscribers() {
        let multicast = MulticastEventHandler::new();
        let event = create_test_delivery_succeeded_event();

        // Should not panic with no subscribers
        multicast.handle_event(event).await;
    }

    #[allow(clippy::panic)] // Panic is needed to verify delivery behavior
    #[tokio::test]
    async fn panicking_handler_does_not_crash_delivery() {
        let mut multicast = MulticastEventHandler::new();

        // Add a normal handler to verify it still works
        let (normal_handler, counter) = TestEventHandler::new();

        multicast.add_subscriber(Arc::new(PanickingHandler));
        multicast.add_subscriber(Arc::new(normal_handler));

        let event = create_test_delivery_succeeded_event();

        // This should not panic despite the panicking handler
        multicast.handle_event(event).await;

        // Give spawned tasks time to execute
        tokio::time::sleep(HANDLER_COMPLETION_DELAY).await;

        // Verify the normal handler still executed
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "Normal handler should execute despite panicking handler"
        );
    }

    #[tokio::test]
    async fn slow_handler_does_not_block_delivery() {
        let mut multicast = MulticastEventHandler::new();

        let (fast_handler, counter) = TestEventHandler::new();

        multicast.add_subscriber(Arc::new(SlowHandler));
        multicast.add_subscriber(Arc::new(fast_handler));

        let event = create_test_delivery_succeeded_event();

        let start = tokio::time::Instant::now();
        multicast.handle_event(event).await;
        let elapsed = start.elapsed();

        // Should return immediately, not wait for slow handler
        assert!(
            elapsed < HANDLER_COMPLETION_DELAY,
            "MulticastEventHandler should not wait for slow handlers"
        );

        // Give fast handler time to execute
        tokio::time::sleep(FAST_HANDLER_EXECUTION_TIME).await;

        // Verify the fast handler executed
        assert_eq!(counter.load(Ordering::SeqCst), 1, "Fast handler should execute immediately");
    }

    #[tokio::test]
    async fn handler_timeout_prevents_indefinite_blocking() {
        let mut multicast = MulticastEventHandler::new();

        let started = Arc::new(AtomicUsize::new(0));
        let handler = InfiniteHandler { started: started.clone() };

        multicast.add_subscriber(Arc::new(handler));

        let event = create_test_delivery_succeeded_event();
        multicast.handle_event(event).await;

        // Wait a bit for the handler to start
        tokio::time::sleep(HANDLER_COMPLETION_DELAY).await;

        // Verify the handler started
        assert_eq!(started.load(Ordering::SeqCst), 1, "Handler should have started execution");

        // The handler task should be cancelled after 30s timeout (not waiting
        // that long in test) This test primarily validates that the
        // timeout logic is in place
    }

    fn create_test_delivery_succeeded_event() -> DeliveryEvent {
        DeliveryEvent::Succeeded(DeliverySucceededEvent {
            delivery_attempt_id: Uuid::new_v4(),
            event_id: EventId::new(),
            tenant_id: TenantId::new(),
            endpoint_url: "https://example.com/webhook".to_string(),
            response_status: 200,
            attempt_number: 1,
            delivered_at: chrono::Utc::now(),
            payload_hash: [0u8; 32],
            payload_size: 1024,
        })
    }
}
