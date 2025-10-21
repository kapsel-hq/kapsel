//! Event testing utilities for deterministic async validation.
//!
//! Provides abstractions that eliminate boilerplate when testing event
//! handlers, multicast dispatch, and event-driven workflows. All operations use
//! deterministic timeouts to prevent CI hangs.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use kapsel_core::{DeliveryEvent, EventHandler, MulticastEventHandler};
use tokio::sync::{Notify, RwLock};
use uuid::Uuid;

/// Public test utilities for creating delivery events.
///
/// Provides consistent event creation across all test modules to eliminate
/// duplication and ensure standardized test data patterns.
pub mod test_events {
    use chrono::Utc;
    use kapsel_core::{
        events::{DeliveryEvent, DeliveryFailedEvent, DeliverySucceededEvent},
        models::{EndpointId, EventId, TenantId},
    };
    use uuid::Uuid;

    use crate::{fixtures::TestWebhook, HashMap};

    /// Creates a standard successful delivery event for testing.
    pub fn create_delivery_succeeded_event() -> DeliveryEvent {
        DeliveryEvent::Succeeded(DeliverySucceededEvent {
            delivery_attempt_id: Uuid::new_v4(),
            event_id: EventId::new(),
            tenant_id: TenantId::new(),
            endpoint_url: "https://example.com/webhook".to_string(),
            response_status: 200,
            attempt_number: 1,
            delivered_at: Utc::now(),
            payload_hash: [0u8; 32],
            payload_size: 1024,
        })
    }

    /// Creates a successful delivery event with custom payload hash.
    pub fn create_delivery_succeeded_event_with_hash(payload_hash: [u8; 32]) -> DeliveryEvent {
        DeliveryEvent::Succeeded(DeliverySucceededEvent {
            delivery_attempt_id: Uuid::new_v4(),
            event_id: EventId::new(),
            tenant_id: TenantId::new(),
            endpoint_url: "https://example.com/webhook".to_string(),
            response_status: 200,
            attempt_number: 1,
            delivered_at: Utc::now(),
            payload_hash,
            payload_size: 1024,
        })
    }

    /// Creates a standard failed delivery event for testing.
    pub fn create_delivery_failed_event() -> DeliveryEvent {
        DeliveryEvent::Failed(DeliveryFailedEvent {
            delivery_attempt_id: Uuid::new_v4(),
            event_id: EventId::new(),
            tenant_id: TenantId::new(),
            endpoint_url: "https://example.com/webhook".to_string(),
            response_status: Some(500),
            attempt_number: 3,
            failed_at: Utc::now(),
            error_message: "Connection timeout".to_string(),
            is_retryable: true,
        })
    }

    /// Creates a successful delivery event with custom properties.
    pub fn create_delivery_succeeded_event_custom(
        endpoint_url: &str,
        response_status: u16,
        attempt_number: u32,
        payload_hash: [u8; 32],
        payload_size: i32,
    ) -> DeliveryEvent {
        DeliveryEvent::Succeeded(DeliverySucceededEvent {
            delivery_attempt_id: Uuid::new_v4(),
            event_id: EventId::new(),
            tenant_id: TenantId::new(),
            endpoint_url: endpoint_url.to_string(),
            response_status,
            attempt_number,
            delivered_at: Utc::now(),
            payload_hash,
            payload_size,
        })
    }

    /// Creates a test webhook with default values.
    pub fn create_test_webhook(tenant_id: TenantId, endpoint_id: EndpointId) -> TestWebhook {
        TestWebhook {
            tenant_id: tenant_id.0,
            endpoint_id: endpoint_id.0,
            source_event_id: format!("test-{}", Uuid::new_v4()),
            idempotency_strategy: "content".to_string(),
            headers: HashMap::new(),
            body: r#"{"message": "test"}"#.into(),
            content_type: "application/json".to_string(),
        }
    }

    /// Creates a test webhook with custom payload.
    pub fn create_test_webhook_with_payload(
        tenant_id: TenantId,
        endpoint_id: EndpointId,
        payload: &str,
    ) -> TestWebhook {
        TestWebhook {
            tenant_id: tenant_id.0,
            endpoint_id: endpoint_id.0,
            source_event_id: format!("test-{}", Uuid::new_v4()),
            idempotency_strategy: "content".to_string(),
            headers: HashMap::new(),
            body: payload.to_string().into(),
            content_type: "application/json".to_string(),
        }
    }
}

/// Default timeout for event handler completion in tests.
///
/// This timeout prevents test suite hangs while being generous enough
/// for CI environments with variable timing.
pub const DEFAULT_EVENT_TIMEOUT: Duration = Duration::from_secs(2);

/// Tracks completion of event handler execution for deterministic testing.
///
/// Eliminates the need for manual notification management in tests by
/// automatically tracking when handlers complete their work.
#[derive(Debug, Default)]
pub struct CompletionTracker {
    /// Number of completed handler invocations
    completed_count: AtomicUsize,
    /// Notification for each completion
    notify: Notify,
}

impl CompletionTracker {
    /// Create a new completion tracker.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Record that a handler completed execution.
    pub fn record_completion(&self) {
        self.completed_count.fetch_add(1, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    /// Get the current number of completed executions.
    pub fn completed_count(&self) -> usize {
        self.completed_count.load(Ordering::SeqCst)
    }

    /// Wait for at least `count` handlers to complete.
    ///
    /// Returns immediately if the required count is already reached.
    /// Times out after `DEFAULT_EVENT_TIMEOUT` to prevent test hangs.
    pub async fn wait_for_completions(&self, count: usize) {
        self.wait_for_completions_with_timeout(count, DEFAULT_EVENT_TIMEOUT).await
    }

    /// Wait for handler completions with custom timeout.
    pub async fn wait_for_completions_with_timeout(&self, count: usize, timeout: Duration) {
        let result = tokio::time::timeout(timeout, async {
            while self.completed_count() < count {
                self.notify.notified().await;
            }
        })
        .await;

        if result.is_err() {
            panic!(
                "Event handlers did not complete in time. Expected: {}, Actual: {}, Timeout: {:?}",
                count,
                self.completed_count(),
                timeout
            );
        }
    }

    /// Wait for a single handler to complete.
    pub async fn wait_for_completion(&self) {
        self.wait_for_completions(1).await;
    }
}

/// Event handler wrapper that automatically tracks completion.
///
/// Wraps any EventHandler implementation to provide completion notifications
/// without requiring changes to the original handler logic.
#[derive(Debug)]
pub struct TestEventSubscriber {
    inner: Arc<dyn EventHandler>,
    completion_tracker: Arc<CompletionTracker>,
    subscriber_id: String,
}

impl TestEventSubscriber {
    /// Wrap an existing event handler with completion tracking.
    pub fn wrap(handler: Arc<dyn EventHandler>) -> (Self, Arc<CompletionTracker>) {
        let completion_tracker = CompletionTracker::new();
        let subscriber_id = Uuid::new_v4().to_string();

        let wrapper =
            Self { inner: handler, completion_tracker: completion_tracker.clone(), subscriber_id };

        (wrapper, completion_tracker)
    }

    /// Create a wrapper with a named identifier for debugging.
    pub fn wrap_with_id(
        handler: Arc<dyn EventHandler>,
        id: impl Into<String>,
    ) -> (Self, Arc<CompletionTracker>) {
        let completion_tracker = CompletionTracker::new();

        let wrapper = Self {
            inner: handler,
            completion_tracker: completion_tracker.clone(),
            subscriber_id: id.into(),
        };

        (wrapper, completion_tracker)
    }

    /// Get the subscriber identifier.
    pub fn id(&self) -> &str {
        &self.subscriber_id
    }
}

#[async_trait::async_trait]
impl EventHandler for TestEventSubscriber {
    async fn handle_event(&self, event: DeliveryEvent) {
        // Execute the wrapped handler
        self.inner.handle_event(event).await;

        // Record completion for test synchronization
        self.completion_tracker.record_completion();
    }
}

/// High-level abstraction for testing event handler behavior.
///
/// Manages multicast dispatch, completion tracking, and provides a clean
/// API for verifying event processing without boilerplate.
pub struct EventHandlerTester {
    multicast: MulticastEventHandler,
    trackers: HashMap<String, Arc<CompletionTracker>>,
    event_history: Arc<RwLock<Vec<DeliveryEvent>>>,
}

impl EventHandlerTester {
    /// Create a new event handler tester.
    pub fn new() -> Self {
        Self {
            multicast: MulticastEventHandler::new(),
            trackers: HashMap::new(),
            event_history: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Add a subscriber with automatic completion tracking.
    ///
    /// Returns a completion tracker that can be used to wait for this
    /// specific subscriber to complete processing.
    pub fn add_subscriber(&mut self, handler: Arc<dyn EventHandler>) -> Arc<CompletionTracker> {
        let (wrapper, tracker) = TestEventSubscriber::wrap(handler);
        let id = wrapper.id().to_string();

        self.multicast.add_subscriber(Arc::new(wrapper));
        self.trackers.insert(id, tracker.clone());

        tracker
    }

    /// Add a named subscriber for easier debugging.
    pub fn add_named_subscriber(
        &mut self,
        name: impl Into<String>,
        handler: Arc<dyn EventHandler>,
    ) -> Arc<CompletionTracker> {
        let name = name.into();
        let (wrapper, tracker) = TestEventSubscriber::wrap_with_id(handler, name.clone());

        self.multicast.add_subscriber(Arc::new(wrapper));
        self.trackers.insert(name, tracker.clone());

        tracker
    }

    /// Get the number of registered subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.multicast.subscriber_count()
    }

    /// Send an event to all subscribers.
    ///
    /// Records the event in history for later inspection and dispatches
    /// to all registered subscribers via the multicast handler.
    pub async fn handle_event(&self, event: DeliveryEvent) {
        // Record event for history
        self.event_history.write().await.push(event.clone());

        // Dispatch to all subscribers
        self.multicast.handle_event(event).await;
    }

    /// Wait for all subscribers to complete processing the last event.
    pub async fn wait_for_all_completions(&self) {
        for tracker in self.trackers.values() {
            tracker.wait_for_completion().await;
        }
    }

    /// Wait for a specific number of total completions across all subscribers.
    ///
    /// Useful when you want to verify that exactly N handler invocations
    /// occurred without caring which specific subscribers completed.
    pub async fn wait_for_total_completions(&self, expected_total: usize) {
        let result = tokio::time::timeout(DEFAULT_EVENT_TIMEOUT, async {
            loop {
                let total = self.total_completions();
                if total >= expected_total {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;

        if result.is_err() {
            panic!(
                "Expected {} total completions, got {} after timeout",
                expected_total,
                self.total_completions()
            );
        }
    }

    /// Get total completion count across all subscribers.
    pub fn total_completions(&self) -> usize {
        self.trackers.values().map(|t| t.completed_count()).sum()
    }

    /// Get the event processing history.
    pub async fn event_history(&self) -> Vec<DeliveryEvent> {
        self.event_history.read().await.clone()
    }

    /// Clear the event history.
    pub async fn clear_history(&self) {
        self.event_history.write().await.clear();
    }

    /// Get completion tracker for a specific named subscriber.
    pub fn tracker(&self, name: &str) -> Option<Arc<CompletionTracker>> {
        self.trackers.get(name).cloned()
    }
}

impl Default for EventHandlerTester {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use kapsel_core::events::NoOpEventHandler;

    use super::*;

    #[derive(Debug)]
    struct CountingHandler {
        count: Arc<AtomicUsize>,
    }

    impl CountingHandler {
        fn new() -> (Self, Arc<AtomicUsize>) {
            let count = Arc::new(AtomicUsize::new(0));
            (Self { count: count.clone() }, count)
        }
    }

    #[async_trait::async_trait]
    impl EventHandler for CountingHandler {
        async fn handle_event(&self, _event: DeliveryEvent) {
            self.count.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn completion_tracker_records_single_completion() {
        let tracker = CompletionTracker::new();

        assert_eq!(tracker.completed_count(), 0);

        tracker.record_completion();

        assert_eq!(tracker.completed_count(), 1);
    }

    #[tokio::test]
    async fn completion_tracker_waits_for_completion() {
        let tracker = CompletionTracker::new();

        // Spawn task that will complete after delay
        let tracker_clone = tracker.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            tracker_clone.record_completion();
        });

        // Should wait and then complete
        tracker.wait_for_completion().await;
        assert_eq!(tracker.completed_count(), 1);
    }

    #[tokio::test]
    async fn test_event_subscriber_wraps_handler() {
        let (handler, count) = CountingHandler::new();
        let (subscriber, tracker) = TestEventSubscriber::wrap(Arc::new(handler));

        let event = test_events::create_delivery_succeeded_event();
        subscriber.handle_event(event).await;

        // Verify both the original handler and completion tracking worked
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(tracker.completed_count(), 1);
    }

    #[tokio::test]
    async fn event_handler_tester_manages_multiple_subscribers() {
        let mut tester = EventHandlerTester::new();

        let (handler1, count1) = CountingHandler::new();
        let (handler2, count2) = CountingHandler::new();

        let tracker1 = tester.add_named_subscriber("handler1", Arc::new(handler1));
        let tracker2 = tester.add_named_subscriber("handler2", Arc::new(handler2));

        assert_eq!(tester.subscriber_count(), 2);

        let event = test_events::create_delivery_succeeded_event();
        tester.handle_event(event).await;

        tester.wait_for_all_completions().await;

        // Verify all handlers executed
        assert_eq!(count1.load(Ordering::SeqCst), 1);
        assert_eq!(count2.load(Ordering::SeqCst), 1);
        assert_eq!(tracker1.completed_count(), 1);
        assert_eq!(tracker2.completed_count(), 1);
        assert_eq!(tester.total_completions(), 2);
    }

    #[tokio::test]
    async fn event_handler_tester_tracks_history() {
        let mut tester = EventHandlerTester::new();
        tester.add_subscriber(Arc::new(NoOpEventHandler));

        let event1 = test_events::create_delivery_succeeded_event();
        let event2 = test_events::create_delivery_succeeded_event();

        tester.handle_event(event1.clone()).await;
        tester.handle_event(event2.clone()).await;

        tester.wait_for_all_completions().await;

        let history = tester.event_history().await;
        assert_eq!(history.len(), 2);
    }
}
