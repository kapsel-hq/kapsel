//! Event handling tests for attestation system.
//!
//! Tests the event-driven architecture that allows attestation service
//! to subscribe to delivery events without tight coupling. Verifies
//! event propagation, multicast handling, and concurrent processing.

use std::sync::Arc;

use chrono::Utc;
use kapsel_attestation::{AttestationEventSubscriber, MerkleService, SigningService};
use kapsel_core::{
    models::{EventId, TenantId},
    DeliveryEvent, DeliveryFailedEvent, DeliverySucceededEvent, EventHandler,
    MulticastEventHandler,
};
use kapsel_testing::TestEnv;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Test that demonstrates the complete event-driven attestation flow.
///
/// This test shows how:
/// 1. Delivery system emits events without knowing about attestation
/// 2. Attestation service subscribes to events via EventHandler trait
/// 3. Successful deliveries create attestation leaves
/// 4. Failed deliveries are logged but don't create leaves
/// 5. Multiple subscribers can listen to the same events
#[tokio::test]
async fn event_driven_attestation_integration() {
    // Setup test environment
    let env = TestEnv::new().await.expect("failed to create test environment");

    // Create attestation services
    let signing_service = SigningService::ephemeral();
    let merkle_service = MerkleService::new(env.pool().clone(), signing_service);
    let merkle_service = Arc::new(RwLock::new(merkle_service));

    // Create attestation event subscriber
    let attestation_subscriber = AttestationEventSubscriber::new(merkle_service.clone());

    // Simulate delivery success event
    let success_event = create_test_success_event();
    attestation_subscriber.handle_event(DeliveryEvent::Succeeded(success_event.clone())).await;

    // Verify attestation leaf was created
    let service = merkle_service.read().await;
    assert_eq!(service.pending_count().await.expect("failed to get pending count"), 1);

    // Simulate delivery failure event
    let failure_event = create_test_failure_event();
    attestation_subscriber.handle_event(DeliveryEvent::Failed(failure_event)).await;

    // Verify no additional leaf was created for failure
    assert_eq!(service.pending_count().await.expect("failed to get pending count"), 1);
}

/// Test multicast event handling with multiple subscribers.
///
/// This demonstrates how multiple services can subscribe to the same
/// delivery events without coupling to each other or the delivery system.
#[tokio::test]
async fn multicast_event_handling_with_multiple_subscribers() {
    let env = TestEnv::new().await.expect("failed to create test environment");

    // Create first attestation subscriber
    let signing_service1 = SigningService::ephemeral();
    let merkle_service1 =
        Arc::new(RwLock::new(MerkleService::new(env.pool().clone(), signing_service1)));
    let subscriber1 = Arc::new(AttestationEventSubscriber::new(merkle_service1.clone()));

    // Create second attestation subscriber (simulating different service)
    let signing_service2 = SigningService::ephemeral();
    let merkle_service2 =
        Arc::new(RwLock::new(MerkleService::new(env.pool().clone(), signing_service2)));
    let subscriber2 = Arc::new(AttestationEventSubscriber::new(merkle_service2.clone()));

    // Create multicast handler and add both subscribers
    let mut multicast = MulticastEventHandler::new();
    multicast.add_subscriber(subscriber1);
    multicast.add_subscriber(subscriber2);

    assert_eq!(multicast.subscriber_count(), 2);

    // Send success event to multicast handler
    let success_event = create_test_success_event();
    multicast.handle_event(DeliveryEvent::Succeeded(success_event)).await;

    // Verify both services received the event
    let service1 = merkle_service1.read().await;
    let service2 = merkle_service2.read().await;

    assert_eq!(service1.pending_count().await.expect("failed to get pending count"), 1);
    assert_eq!(service2.pending_count().await.expect("failed to get pending count"), 1);
}

/// Test concurrent event handling.
///
/// This verifies that the event system can handle concurrent events
/// without race conditions or data corruption.
#[tokio::test]
async fn concurrent_event_handling() {
    let env = TestEnv::new().await.expect("failed to create test environment");

    let signing_service = SigningService::ephemeral();
    let merkle_service =
        Arc::new(RwLock::new(MerkleService::new(env.pool().clone(), signing_service)));
    let subscriber = Arc::new(AttestationEventSubscriber::new(merkle_service.clone()));

    // Create multiple success events
    let events: Vec<_> =
        (0..10).map(|_| DeliveryEvent::Succeeded(create_test_success_event())).collect();

    // Handle all events concurrently
    let handles: Vec<_> = events
        .into_iter()
        .map(|event| {
            let subscriber = subscriber.clone();
            tokio::spawn(async move {
                subscriber.handle_event(event).await;
            })
        })
        .collect();

    // Wait for all to complete
    for handle in handles {
        handle.await.expect("task should complete");
    }

    // Verify all events were processed
    let service = merkle_service.read().await;
    assert_eq!(service.pending_count().await.expect("failed to get pending count"), 10);
}

// Helper functions for creating test events

fn create_test_success_event() -> DeliverySucceededEvent {
    DeliverySucceededEvent {
        delivery_attempt_id: Uuid::new_v4(),
        event_id: EventId::new(),
        tenant_id: TenantId::new(),
        endpoint_url: "https://example.com/webhook".to_string(),
        response_status: 200,
        attempt_number: 1,
        delivered_at: Utc::now(),
        payload_hash: [1u8; 32], // Use different hash to avoid duplicates
        payload_size: 1024,
    }
}

fn create_test_failure_event() -> DeliveryFailedEvent {
    DeliveryFailedEvent {
        delivery_attempt_id: Uuid::new_v4(),
        event_id: EventId::new(),
        tenant_id: TenantId::new(),
        endpoint_url: "https://example.com/webhook".to_string(),
        response_status: Some(500),
        attempt_number: 1,
        failed_at: Utc::now(),
        error_message: "Internal server error".to_string(),
        is_retryable: true,
    }
}
