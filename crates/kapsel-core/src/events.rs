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
        let futures = self.handlers.iter().map(|handler| {
            let event = event.clone();
            async move {
                handler.handle_event(event).await;
            }
        });

        // We don't propagate errors since event handling should not
        // interfere with delivery processing
        futures::future::join_all(futures).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

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

    #[allow(dead_code)]
    fn create_test_delivery_failed_event() -> DeliveryEvent {
        DeliveryEvent::Failed(DeliveryFailedEvent {
            delivery_attempt_id: Uuid::new_v4(),
            event_id: EventId::new(),
            tenant_id: TenantId::new(),
            endpoint_url: "https://example.com/webhook".to_string(),
            response_status: Some(500),
            attempt_number: 2,
            failed_at: chrono::Utc::now(),
            error_message: "Internal server error".to_string(),
            is_retryable: true,
        })
    }
}
