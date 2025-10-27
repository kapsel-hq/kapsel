//! Integration tests for event-driven attestation system.
//!
//! Tests the boundary between delivery events and attestation leaf creation,
//! verifying that the event subscription model correctly triggers cryptographic
//! audit trail generation.

use std::sync::Arc;

use kapsel_attestation::{AttestationEventSubscriber, MerkleService, SigningService};
use kapsel_testing::{
    events::{test_events, EventHandlerTester},
    TestEnv,
};
use tokio::sync::RwLock;

/// Test that successful delivery events create attestation leaves via event
/// subscription.
///
/// Verifies the complete integration: DeliveryEvent ->
/// AttestationEventSubscriber -> MerkleService This validates that the
/// event-driven architecture correctly triggers attestation creation.
#[tokio::test]
async fn successful_delivery_events_create_attestation_leaves() {
    let env = TestEnv::new_isolated().await.expect("failed to create test environment");

    // Create attestation infrastructure
    let signing_service = SigningService::ephemeral();
    let merkle_service = Arc::new(RwLock::new(MerkleService::new(env.storage(), signing_service)));
    let attestation_subscriber = AttestationEventSubscriber::new(merkle_service.clone());

    // Setup event handler tester
    let mut tester = EventHandlerTester::new();
    tester.add_named_subscriber("attestation", Arc::new(attestation_subscriber));

    // Send success event
    let success_event = test_events::create_delivery_succeeded_event();
    tester.handle_event(success_event.clone()).await;

    // Wait for processing to complete
    tester.wait_for_all_completions().await;

    // Verify attestation leaf was created
    let service = merkle_service.read().await;
    assert_eq!(service.pending_count(), 1, "Success event should create one attestation leaf");
    drop(service);
}

/// Test that failed delivery events do not create attestation leaves.
///
/// Failed deliveries should be logged but should not generate attestation
/// entries since there was no successful delivery to attest to.
#[tokio::test]
async fn failed_delivery_events_do_not_create_attestation_leaves() {
    let env = TestEnv::new_isolated().await.expect("failed to create test environment");

    let signing_service = SigningService::ephemeral();
    let merkle_service = Arc::new(RwLock::new(MerkleService::new(env.storage(), signing_service)));
    let attestation_subscriber = AttestationEventSubscriber::new(merkle_service.clone());

    let mut tester = EventHandlerTester::new();
    tester.add_named_subscriber("attestation", Arc::new(attestation_subscriber));

    // Send failure event
    let failure_event = test_events::create_delivery_failed_event();
    tester.handle_event(failure_event).await;

    tester.wait_for_all_completions().await;

    // Verify no attestation leaf was created for failure
    let service = merkle_service.read().await;
    assert_eq!(service.pending_count(), 0, "Failure events should not create attestation leaves");
    drop(service);
}

/// Test handling multiple successful delivery events sequentially.
///
/// Verifies that multiple delivery success events are all processed
/// correctly and create the expected number of attestation leaves.
#[tokio::test]
async fn multiple_successful_delivery_events_create_multiple_leaves() {
    let env = TestEnv::new_isolated().await.expect("failed to create test environment");

    let signing_service = SigningService::ephemeral();
    let merkle_service = Arc::new(RwLock::new(MerkleService::new(env.storage(), signing_service)));
    let attestation_subscriber = AttestationEventSubscriber::new(merkle_service.clone());

    let mut tester = EventHandlerTester::new();
    tester.add_named_subscriber("attestation", Arc::new(attestation_subscriber));

    // Send multiple success events
    for _ in 0..3 {
        let success_event = test_events::create_delivery_succeeded_event();
        tester.handle_event(success_event).await;
    }

    tester.wait_for_all_completions().await;

    // Verify all attestation leaves were created
    let service = merkle_service.read().await;
    assert_eq!(service.pending_count(), 3, "All success events should create attestation leaves");
    drop(service);
}

/// Test multicast dispatch to multiple attestation services.
///
/// Verifies that a single delivery event can be processed by multiple
/// independent attestation services, enabling redundancy and different
/// attestation strategies.
#[tokio::test]
async fn multicast_event_handling_with_multiple_attestation_services() {
    let env = TestEnv::new_isolated().await.expect("failed to create test environment");

    // Create two independent attestation services
    let signing_service1 = SigningService::ephemeral();
    let signing_service2 = SigningService::ephemeral();

    let merkle_service1 =
        Arc::new(RwLock::new(MerkleService::new(env.storage(), signing_service1)));
    let merkle_service2 =
        Arc::new(RwLock::new(MerkleService::new(env.storage(), signing_service2)));

    let subscriber1 = AttestationEventSubscriber::new(merkle_service1.clone());
    let subscriber2 = AttestationEventSubscriber::new(merkle_service2.clone());

    // Setup multicast event testing
    let mut tester = EventHandlerTester::new();
    tester.add_named_subscriber("attestation1", Arc::new(subscriber1));
    tester.add_named_subscriber("attestation2", Arc::new(subscriber2));

    assert_eq!(tester.subscriber_count(), 2);

    // Send success event to both subscribers
    let success_event = test_events::create_delivery_succeeded_event();
    tester.handle_event(success_event).await;

    tester.wait_for_all_completions().await;

    // Verify both services received and processed the event
    {
        let service1 = merkle_service1.read().await;
        let service2 = merkle_service2.read().await;

        assert_eq!(
            service1.pending_count(),
            1,
            "Each service should have its own attestation leaf count"
        );
        assert_eq!(
            service2.pending_count(),
            1,
            "Each service should have its own attestation leaf count"
        );
        drop(service1);
        drop(service2);
    }

    // Verify total completions
    assert_eq!(tester.total_completions(), 2, "Both subscribers should have completed processing");
}

/// Test concurrent processing of multiple delivery events.
///
/// Verifies that the attestation system correctly handles concurrent
/// delivery events without data races or lost attestations.
#[tokio::test]
async fn concurrent_delivery_events_create_correct_attestation_count() {
    let env = TestEnv::new_isolated().await.expect("failed to create test environment");

    let signing_service = SigningService::ephemeral();
    let merkle_service = Arc::new(RwLock::new(MerkleService::new(env.storage(), signing_service)));
    let attestation_subscriber = AttestationEventSubscriber::new(merkle_service.clone());

    let mut tester = EventHandlerTester::new();
    tester.add_named_subscriber("attestation", Arc::new(attestation_subscriber));

    // Send multiple concurrent events
    let event_count = 5;
    for i in 0..event_count {
        let event = test_events::create_delivery_succeeded_event_with_hash(
            [u8::try_from(i).unwrap_or(0); 32],
        );
        tester.handle_event(event).await;
    }

    // Wait for all events to be processed
    tester.wait_for_total_completions(event_count).await;

    // Verify all events created attestation leaves
    let service = merkle_service.read().await;
    assert_eq!(
        service.pending_count(),
        event_count,
        "All concurrent events should create attestation leaves"
    );
    drop(service);
}

/// Test mixed success and failure event processing.
///
/// Verifies that attestation subscriber correctly handles mixed event types,
/// creating attestations only for successful deliveries.
#[tokio::test]
async fn mixed_success_failure_events_create_correct_attestations() {
    let env = TestEnv::new_isolated().await.expect("failed to create test environment");

    let signing_service = SigningService::ephemeral();
    let merkle_service = Arc::new(RwLock::new(MerkleService::new(env.storage(), signing_service)));
    let attestation_subscriber = AttestationEventSubscriber::new(merkle_service.clone());

    let mut tester = EventHandlerTester::new();
    tester.add_named_subscriber("attestation", Arc::new(attestation_subscriber));

    // Send mixed events: 3 successes, 2 failures
    let success_events = [
        test_events::create_delivery_succeeded_event_with_hash([1u8; 32]),
        test_events::create_delivery_succeeded_event_with_hash([2u8; 32]),
        test_events::create_delivery_succeeded_event_with_hash([3u8; 32]),
    ];

    let failure_events =
        [test_events::create_delivery_failed_event(), test_events::create_delivery_failed_event()];

    // Send all events
    for event in success_events {
        tester.handle_event(event).await;
    }
    for event in failure_events {
        tester.handle_event(event).await;
    }

    // Wait for all events to process
    tester.wait_for_total_completions(5).await;

    // Verify only success events created attestation leaves
    let service = merkle_service.read().await;
    assert_eq!(service.pending_count(), 3, "Only success events should create attestation leaves");
    drop(service);

    // Verify event history captured all events
    let history = tester.event_history().await;
    assert_eq!(history.len(), 5, "All events should be recorded in history");
}

/// Test that event processing maintains data integrity.
///
/// Verifies that event data is correctly preserved through the attestation
/// subscription pipeline without corruption or modification.
#[tokio::test]
async fn event_data_integrity_preserved_through_attestation() {
    let env = TestEnv::new_isolated().await.expect("failed to create test environment");

    let signing_service = SigningService::ephemeral();
    let merkle_service = Arc::new(RwLock::new(MerkleService::new(env.storage(), signing_service)));
    let attestation_subscriber = AttestationEventSubscriber::new(merkle_service.clone());

    let mut tester = EventHandlerTester::new();
    tester.add_named_subscriber("attestation", Arc::new(attestation_subscriber));

    // Create event with specific data
    let original_event = test_events::create_delivery_succeeded_event_custom(
        "https://test.example.com/webhook",
        201u16,
        2u32,
        [42u8; 32],
        2048,
    );

    tester.handle_event(original_event.clone()).await;
    tester.wait_for_all_completions().await;

    // Verify data was processed
    let service = merkle_service.read().await;
    assert_eq!(service.pending_count(), 1, "Event should be processed");
    drop(service);

    // Verify event was recorded in history with correct data
    let history = tester.event_history().await;
    assert_eq!(history.len(), 1);

    // Verify the event was recorded correctly
    assert_eq!(history[0], original_event, "Event should be preserved exactly as sent");
}
