//! Integration tests for event system multicast and handler behavior.
//!
//! Tests component boundaries between event producers, multicast dispatch,
//! and event subscribers using deterministic validation patterns.

use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use async_trait::async_trait;
use kapsel_core::events::{DeliveryEvent, EventHandler};
use kapsel_testing::events::{test_events, EventHandlerTester};
use tokio::sync::Mutex;

/// Simulates realistic event processing with configurable behavior.
#[derive(Debug)]
struct RealisticEventHandler {
    _id: String,
    processed_events: Arc<Mutex<Vec<DeliveryEvent>>>,
    should_fail: Arc<AtomicBool>,
    processing_delay: Duration,
    failure_count: Arc<AtomicUsize>,
}

impl RealisticEventHandler {
    fn new(id: &str, processing_delay: Duration) -> Self {
        Self {
            _id: id.to_string(),
            processed_events: Arc::new(Mutex::new(Vec::new())),
            should_fail: Arc::new(AtomicBool::new(false)),
            processing_delay,
            failure_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn enable_failures(&self) {
        self.should_fail.store(true, Ordering::Relaxed);
    }

    async fn processed_count(&self) -> usize {
        self.processed_events.lock().await.len()
    }

    fn failure_count(&self) -> usize {
        self.failure_count.load(Ordering::Relaxed)
    }

    async fn processed_events(&self) -> Vec<DeliveryEvent> {
        self.processed_events.lock().await.clone()
    }
}

#[async_trait]
impl EventHandler for RealisticEventHandler {
    async fn handle_event(&self, event: DeliveryEvent) {
        // Simulate processing delay
        if !self.processing_delay.is_zero() {
            tokio::time::sleep(self.processing_delay).await;
        }

        // Simulate failures when enabled
        if self.should_fail.load(Ordering::Relaxed) {
            self.failure_count.fetch_add(1, Ordering::Relaxed);
            return;
        }

        // Record successful processing
        self.processed_events.lock().await.push(event);
    }
}

/// Handler that always panics to test failure isolation.
#[derive(Debug)]
struct PanickingHandler;

#[async_trait]
impl EventHandler for PanickingHandler {
    #[allow(clippy::panic)] // Intentional for testing isolation
    async fn handle_event(&self, _event: DeliveryEvent) {
        panic!("Handler panic for isolation testing");
    }
}

/// Handler that tracks processing statistics.
#[derive(Debug)]
struct StatsHandler {
    success_count: Arc<AtomicUsize>,
    failure_count: Arc<AtomicUsize>,
}

impl StatsHandler {
    fn new() -> Self {
        Self {
            success_count: Arc::new(AtomicUsize::new(0)),
            failure_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn success_count(&self) -> usize {
        self.success_count.load(Ordering::Relaxed)
    }

    fn failure_count(&self) -> usize {
        self.failure_count.load(Ordering::Relaxed)
    }

    fn total_count(&self) -> usize {
        self.success_count() + self.failure_count()
    }
}

#[async_trait]
impl EventHandler for StatsHandler {
    async fn handle_event(&self, event: DeliveryEvent) {
        match event {
            DeliveryEvent::Succeeded(_) => {
                self.success_count.fetch_add(1, Ordering::Relaxed);
            },
            DeliveryEvent::Failed(_) => {
                self.failure_count.fetch_add(1, Ordering::Relaxed);
            },
            DeliveryEvent::AttemptStarted(_) => {
                // AttemptStarted events don't affect success/failure counts
            },
        }
    }
}

#[tokio::test]
async fn multicast_isolates_handler_panics() {
    let mut tester = EventHandlerTester::new();

    // Add panicking handler and normal handler
    let normal_handler = Arc::new(RealisticEventHandler::new("normal", Duration::ZERO));
    let normal_count_before = normal_handler.processed_count().await;

    tester.add_named_subscriber("panicking", Arc::new(PanickingHandler));
    tester.add_named_subscriber("normal", normal_handler.clone());

    assert_eq!(tester.subscriber_count(), 2);

    // Send event - should not crash despite panic
    let event = create_test_success_event();
    tester.handle_event(event).await;

    // Wait for normal handler completion
    tester.tracker("normal").unwrap().wait_for_completion().await;

    // Verify normal handler processed event despite other handler panicking
    let normal_count_after = normal_handler.processed_count().await;
    assert_eq!(normal_count_after, normal_count_before + 1);
}

#[tokio::test]
async fn concurrent_multicast_maintains_event_integrity() {
    let mut tester = EventHandlerTester::new();

    // Add multiple handlers with different processing characteristics
    let fast_handler = Arc::new(RealisticEventHandler::new("fast", Duration::ZERO));
    let slow_handler = Arc::new(RealisticEventHandler::new("slow", Duration::from_millis(10)));

    tester.add_named_subscriber("fast", fast_handler.clone());
    tester.add_named_subscriber("slow", slow_handler.clone());

    // Send multiple events concurrently
    let events =
        vec![create_test_success_event(), create_test_failure_event(), create_test_success_event()];

    for event in &events {
        tester.handle_event(event.clone()).await;
    }

    // Wait for all processing to complete
    tester.wait_for_total_completions(6).await; // 3 events × 2 handlers

    // Verify both handlers received all events
    assert_eq!(fast_handler.processed_count().await, 3);
    assert_eq!(slow_handler.processed_count().await, 3);

    // Verify event data integrity
    let fast_events = fast_handler.processed_events().await;
    let slow_events = slow_handler.processed_events().await;

    assert_eq!(fast_events.len(), 3);
    assert_eq!(slow_events.len(), 3);

    // Both should have received the same events (order may vary due to concurrency)
    for event in &events {
        assert!(fast_events.contains(event));
        assert!(slow_events.contains(event));
    }
}

#[tokio::test]
async fn handlers_process_mixed_success_and_failure_events() {
    let mut tester = EventHandlerTester::new();

    let stats_handler = Arc::new(StatsHandler::new());
    tester.add_named_subscriber("stats", stats_handler.clone());

    // Send mixed event types
    tester.handle_event(create_test_success_event()).await;
    tester.handle_event(create_test_failure_event()).await;
    tester.handle_event(create_test_success_event()).await;
    tester.handle_event(create_test_failure_event()).await;

    // Wait for processing
    tester.wait_for_total_completions(4).await;

    // Verify correct event type handling
    assert_eq!(stats_handler.success_count(), 2);
    assert_eq!(stats_handler.failure_count(), 2);
    assert_eq!(stats_handler.total_count(), 4);
}

#[tokio::test]
async fn multicast_scales_with_subscriber_count() {
    // Add varying numbers of subscribers
    let subscriber_counts = [1, 5, 10, 20];

    for &count in &subscriber_counts {
        let mut tester = EventHandlerTester::new(); // Create new tester for each test

        // Add subscribers
        let handlers: Vec<Arc<RealisticEventHandler>> = (0..count)
            .map(|i| {
                Arc::new(RealisticEventHandler::new(&format!("handler-{}", i), Duration::ZERO))
            })
            .collect();

        for (i, handler) in handlers.iter().enumerate() {
            tester.add_named_subscriber(&format!("handler-{}", i), handler.clone());
        }

        assert_eq!(tester.subscriber_count(), count);

        // Send single event
        tester.handle_event(create_test_success_event()).await;

        // Wait for all handlers to complete
        tester.wait_for_total_completions(count).await;

        // Verify all handlers processed the event
        for handler in &handlers {
            assert_eq!(handler.processed_count().await, 1);
        }
    }
}

#[tokio::test]
async fn event_history_tracks_processed_events() {
    let mut tester = EventHandlerTester::new();

    let handler = Arc::new(RealisticEventHandler::new("tracking", Duration::ZERO));
    tester.add_named_subscriber("tracking", handler);

    // Verify empty history initially
    let initial_history = tester.event_history().await;
    assert_eq!(initial_history.len(), 0);

    // Send events
    let events =
        vec![create_test_success_event(), create_test_failure_event(), create_test_success_event()];

    for event in &events {
        tester.handle_event(event.clone()).await;
    }

    tester.wait_for_total_completions(3).await;

    // Verify history captured all events
    let history = tester.event_history().await;
    assert_eq!(history.len(), 3);
    assert_eq!(history, events);

    // Verify history can be cleared
    tester.clear_history().await;
    let cleared_history = tester.event_history().await;
    assert_eq!(cleared_history.len(), 0);
}

#[tokio::test]
async fn handler_failures_do_not_affect_other_handlers() {
    let mut tester = EventHandlerTester::new();

    let reliable_handler = Arc::new(RealisticEventHandler::new("reliable", Duration::ZERO));
    let failing_handler = Arc::new(RealisticEventHandler::new("failing", Duration::ZERO));

    // Configure one handler to fail
    failing_handler.enable_failures();

    tester.add_named_subscriber("reliable", reliable_handler.clone());
    tester.add_named_subscriber("failing", failing_handler.clone());

    // Send events
    for _ in 0..3 {
        tester.handle_event(create_test_success_event()).await;
    }

    tester.wait_for_total_completions(6).await; // Both handlers process all events

    // Verify reliable handler succeeded
    assert_eq!(reliable_handler.processed_count().await, 3);
    assert_eq!(reliable_handler.failure_count(), 0);

    // Verify failing handler failed as expected
    assert_eq!(failing_handler.processed_count().await, 0);
    assert_eq!(failing_handler.failure_count(), 3);
}

// Use consistent test event utilities from kapsel-testing
use test_events::{
    create_delivery_failed_event as create_test_failure_event,
    create_delivery_succeeded_event as create_test_success_event,
};
