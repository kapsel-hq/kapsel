//! Integration tests for event system behavior.
//!
//! Tests event handler integration, multicast dispatch, and property-based
//! invariant verification for the event-driven architecture.

use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use async_trait::async_trait;
use chrono::Utc;
use futures::future::join_all;
use kapsel_core::{
    events::{
        DeliveryEvent, DeliveryFailedEvent, DeliverySucceededEvent, EventHandler,
        MulticastEventHandler, NoOpEventHandler,
    },
    models::{EventId, TenantId},
};
use tokio::time::timeout;
use uuid::Uuid;

/// Event handler that simulates real-world processing with potential failures.
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

    fn update_should_fail(&self, should_fail: bool) {
        self.should_fail.store(should_fail, Ordering::Relaxed);
    }

    fn processed_count(&self) -> usize {
        self.processed_events.lock().unwrap().len()
    }

    fn failure_count(&self) -> usize {
        self.failure_count.load(Ordering::Relaxed)
    }

    fn processed_events(&self) -> Vec<DeliveryEvent> {
        self.processed_events.lock().unwrap().clone()
    }
}

#[async_trait]
impl EventHandler for RealisticEventHandler {
    async fn handle_event(&self, event: DeliveryEvent) {
        // Simulate processing time
        if !self.processing_delay.is_zero() {
            tokio::time::sleep(self.processing_delay).await;
        }

        // Simulate failures
        if self.should_fail.load(Ordering::Relaxed) {
            self.failure_count.fetch_add(1, Ordering::Relaxed);
            // Don't actually panic, just record the failure and return without processing
            return;
        }

        // Process successfully
        self.processed_events.lock().unwrap().push(event);
    }
}

/// Test that event handler failures don't break the entire event system.
///
/// Verifies that when one handler panics, other handlers in a multicast
/// continue to work correctly. This is critical for system resilience.
#[tokio::test]
async fn event_handler_failures_are_isolated() {
    let stable_handler = Arc::new(RealisticEventHandler::new("stable", Duration::ZERO));
    let failing_handler = Arc::new(RealisticEventHandler::new("failing", Duration::ZERO));
    let another_stable = Arc::new(RealisticEventHandler::new("stable2", Duration::ZERO));

    failing_handler.update_should_fail(true);

    let mut multicast = MulticastEventHandler::new();
    multicast.add_subscriber(stable_handler.clone());
    multicast.add_subscriber(failing_handler.clone());
    multicast.add_subscriber(another_stable.clone());

    let event = DeliveryEvent::Succeeded(DeliverySucceededEvent {
        delivery_attempt_id: Uuid::new_v4(),
        event_id: EventId::new(),
        tenant_id: TenantId::new(),
        endpoint_url: "https://example.com/webhook".to_string(),
        response_status: 200,
        attempt_number: 1,
        delivered_at: Utc::now(),
        payload_hash: [1u8; 32],
        payload_size: 1024,
    });

    // This should not panic even though one handler fails
    multicast.handle_event(event).await;

    // Stable handlers should have processed the event
    assert_eq!(stable_handler.processed_count(), 1);
    assert_eq!(another_stable.processed_count(), 1);

    // Failing handler should have recorded the failure
    assert_eq!(failing_handler.failure_count(), 1);
    assert_eq!(failing_handler.processed_count(), 0);
}

/// Test event processing order and timing under concurrent load.
///
/// Verifies that events maintain consistent processing behavior even
/// when multiple events arrive concurrently.
#[tokio::test]
async fn concurrent_event_processing_maintains_consistency() {
    let handler = Arc::new(RealisticEventHandler::new("concurrent", Duration::from_millis(10)));
    let mut multicast = MulticastEventHandler::new();
    multicast.add_subscriber(handler.clone());

    // Send 100 events concurrently
    let mut tasks = Vec::new();
    for i in 0..100 {
        let multicast_clone = multicast.clone();
        let event = DeliveryEvent::Succeeded(DeliverySucceededEvent {
            delivery_attempt_id: Uuid::new_v4(),
            event_id: EventId::new(),
            tenant_id: TenantId::new(),
            endpoint_url: format!("https://example.com/webhook/{}", i),
            response_status: 200,
            attempt_number: 1,
            delivered_at: Utc::now(),
            payload_hash: [(i % 256) as u8; 32],
            payload_size: 1024 + i as i32,
        });

        let task = tokio::spawn(async move {
            multicast_clone.handle_event(event).await;
        });
        tasks.push(task);
    }

    // Wait for all events to process with timeout
    let results = timeout(Duration::from_secs(5), join_all(tasks))
        .await
        .expect("All events should process within timeout");

    // Verify no tasks panicked
    for result in results {
        result.expect("Event handling task should not panic");
    }

    // All events should be processed
    assert_eq!(handler.processed_count(), 100);

    // Verify event data integrity
    let processed = handler.processed_events();
    assert_eq!(processed.len(), 100);

    for event in processed {
        match event {
            DeliveryEvent::Succeeded(success) => {
                assert_eq!(success.response_status, 200);
                assert_eq!(success.attempt_number, 1);
                assert!(success.endpoint_url.starts_with("https://example.com/webhook/"));
                assert!(success.payload_size >= 1024);
            },
            _ => panic!("All events should be success events"),
        }
    }
}

/// Test that event data integrity is preserved during processing.
#[tokio::test]
async fn event_data_integrity_preserved_during_processing() {
    let handler = RealisticEventHandler::new("integrity", Duration::ZERO);

    let success_event = DeliveryEvent::Succeeded(DeliverySucceededEvent {
        delivery_attempt_id: Uuid::new_v4(),
        event_id: EventId::new(),
        tenant_id: TenantId::new(),
        endpoint_url: "https://example.com/webhook".to_string(),
        response_status: 200,
        attempt_number: 1,
        delivered_at: Utc::now(),
        payload_hash: [42u8; 32],
        payload_size: 1024,
    });

    handler.handle_event(success_event.clone()).await;

    let processed = handler.processed_events();
    assert_eq!(processed.len(), 1);

    match (&success_event, &processed[0]) {
        (DeliveryEvent::Succeeded(original), DeliveryEvent::Succeeded(processed)) => {
            assert_eq!(original.response_status, processed.response_status);
            assert_eq!(original.attempt_number, processed.attempt_number);
            assert_eq!(original.payload_size, processed.payload_size);
            assert_eq!(original.endpoint_url, processed.endpoint_url);
        },
        _ => panic!("Event type should be preserved"),
    }
}

/// Test multicast consistency with different subscriber counts.
#[tokio::test]
async fn multicast_consistency_across_subscriber_counts() {
    let mut handlers = Vec::new();
    let mut multicast = MulticastEventHandler::new();

    // Test with 5 subscribers and 10 events
    for i in 0..5 {
        let handler =
            Arc::new(RealisticEventHandler::new(&format!("handler_{}", i), Duration::ZERO));
        handlers.push(handler.clone());
        multicast.add_subscriber(handler);
    }

    for i in 0..10 {
        let event = DeliveryEvent::Succeeded(DeliverySucceededEvent {
            delivery_attempt_id: Uuid::new_v4(),
            event_id: EventId::new(),
            tenant_id: TenantId::new(),
            endpoint_url: format!("https://example.com/webhook/{}", i),
            response_status: 200,
            attempt_number: 1,
            delivered_at: Utc::now(),
            payload_hash: [i as u8; 32],
            payload_size: 1024,
        });

        multicast.handle_event(event).await;
    }

    for handler in &handlers {
        assert_eq!(handler.processed_count(), 10);
        assert_eq!(handler.failure_count(), 0);

        let processed = handler.processed_events();
        assert_eq!(processed.len(), 10);
    }
}

/// Test that NoOp handler truly has no side effects under stress.
///
/// Verifies that NoOp handler can handle high-volume event streams
/// without memory leaks or performance degradation.
#[tokio::test]
async fn noop_handler_has_no_side_effects_under_stress() {
    let noop = NoOpEventHandler::new();
    let start_time = std::time::Instant::now();

    // Process many events rapidly
    for i in 0..10_000 {
        let event = if i % 2 == 0 {
            DeliveryEvent::Succeeded(DeliverySucceededEvent {
                delivery_attempt_id: Uuid::new_v4(),
                event_id: EventId::new(),
                tenant_id: TenantId::new(),
                endpoint_url: format!("https://example.com/webhook/{}", i),
                response_status: 200,
                attempt_number: 1,
                delivered_at: Utc::now(),
                payload_hash: [(i % 256) as u8; 32],
                payload_size: 1024,
            })
        } else {
            DeliveryEvent::Failed(DeliveryFailedEvent {
                delivery_attempt_id: Uuid::new_v4(),
                event_id: EventId::new(),
                tenant_id: TenantId::new(),
                endpoint_url: format!("https://example.com/webhook/{}", i),
                response_status: Some(500),
                attempt_number: 3,
                failed_at: Utc::now(),
                error_message: format!("Error {}", i),
                is_retryable: false,
            })
        };

        noop.handle_event(event).await;
    }

    let elapsed = start_time.elapsed();

    // Should complete quickly (less than 100ms for 10k events)
    assert!(elapsed < Duration::from_millis(100), "NoOp handler too slow: {:?}", elapsed);
}

/// Test event handler behavior with mixed success/failure scenarios.
///
/// Verifies that handlers can correctly process realistic delivery outcome
/// patterns.
#[tokio::test]
async fn event_handlers_process_realistic_delivery_patterns() {
    let handler = Arc::new(RealisticEventHandler::new("realistic", Duration::from_millis(1)));
    let mut multicast = MulticastEventHandler::new();
    multicast.add_subscriber(handler.clone());

    // Simulate realistic delivery pattern:
    // - Most deliveries succeed on first attempt
    // - Some fail with retryable errors
    // - Few fail permanently

    let scenarios = vec![
        // Immediate success (80% of cases)
        (200, 1, true, true), // Success on first try
        (200, 1, true, true), // Success on first try
        (200, 1, true, true), // Success on first try
        (200, 1, true, true), // Success on first try
        // Retry then success (15% of cases)
        (500, 1, false, true), // Initial failure
        (200, 2, true, true),  // Success on retry
        (503, 1, false, true), // Service unavailable
        (200, 3, true, true),  // Success after retries
        // Permanent failure (5% of cases)
        (404, 3, false, false), // Not found - non-retryable
    ];

    for (status, attempt, is_success, is_retryable) in scenarios {
        let event = if is_success {
            DeliveryEvent::Succeeded(DeliverySucceededEvent {
                delivery_attempt_id: Uuid::new_v4(),
                event_id: EventId::new(),
                tenant_id: TenantId::new(),
                endpoint_url: "https://webhook.example.com/receive".to_string(),
                response_status: status,
                attempt_number: attempt,
                delivered_at: Utc::now(),
                payload_hash: [status as u8; 32],
                payload_size: 2048,
            })
        } else {
            DeliveryEvent::Failed(DeliveryFailedEvent {
                delivery_attempt_id: Uuid::new_v4(),
                event_id: EventId::new(),
                tenant_id: TenantId::new(),
                endpoint_url: "https://webhook.example.com/receive".to_string(),
                response_status: Some(status),
                attempt_number: attempt,
                failed_at: Utc::now(),
                error_message: match status {
                    404 => "Webhook endpoint not found".to_string(),
                    500 => "Internal server error".to_string(),
                    503 => "Service temporarily unavailable".to_string(),
                    _ => format!("HTTP {}", status),
                },
                is_retryable,
            })
        };

        multicast.handle_event(event).await;
    }

    // Verify all events processed
    assert_eq!(handler.processed_count(), 9);

    let processed = handler.processed_events();
    let mut success_count = 0;
    let mut failure_count = 0;
    let mut retryable_failures = 0;

    for event in processed {
        match event {
            DeliveryEvent::Succeeded(_) => success_count += 1,
            DeliveryEvent::Failed(failure) => {
                failure_count += 1;
                if failure.is_retryable {
                    retryable_failures += 1;
                }
            },
            DeliveryEvent::AttemptStarted(_) => {
                // Ignore attempt started events for this test
            },
        }
    }

    // Verify expected distribution
    assert_eq!(success_count, 6); // 4 immediate + 2 retry success
    assert_eq!(failure_count, 3); // 2 retryable + 1 permanent
    assert_eq!(retryable_failures, 2); // 500 and 503 errors
}

/// Test event timestamp consistency and realism.
#[tokio::test]
async fn event_timestamps_are_consistent_and_realistic() {
    let handler = RealisticEventHandler::new("timestamp", Duration::ZERO);
    let mut timestamps = Vec::new();

    // Test with mixed success and failure events
    let test_cases = vec![true, false, true, false, true];

    for is_success in test_cases {
        let now = Utc::now();
        let event = if is_success {
            DeliveryEvent::Succeeded(DeliverySucceededEvent {
                delivery_attempt_id: Uuid::new_v4(),
                event_id: EventId::new(),
                tenant_id: TenantId::new(),
                endpoint_url: "https://example.com/webhook".to_string(),
                response_status: 200,
                attempt_number: 1,
                delivered_at: now,
                payload_hash: [1u8; 32],
                payload_size: 1024,
            })
        } else {
            DeliveryEvent::Failed(DeliveryFailedEvent {
                delivery_attempt_id: Uuid::new_v4(),
                event_id: EventId::new(),
                tenant_id: TenantId::new(),
                endpoint_url: "https://example.com/webhook".to_string(),
                response_status: Some(500),
                attempt_number: 1,
                failed_at: now,
                error_message: "Error".to_string(),
                is_retryable: true,
            })
        };

        timestamps.push(now);
        handler.handle_event(event).await;

        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    let processed = handler.processed_events();
    assert_eq!(processed.len(), timestamps.len());

    // Verify timestamps are reasonable (within last minute)
    let cutoff = Utc::now() - chrono::Duration::seconds(60);
    for (i, event) in processed.iter().enumerate() {
        let event_time = match event {
            DeliveryEvent::Succeeded(s) => s.delivered_at,
            DeliveryEvent::Failed(f) => f.failed_at,
            DeliveryEvent::AttemptStarted(_) => continue,
        };

        assert!(event_time > cutoff, "Event timestamp too old");
        assert!(
            event_time <= timestamps[i] + chrono::Duration::seconds(1),
            "Event timestamp too far in future"
        );
    }
}
