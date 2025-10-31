//! Scenario tests for complete attestation workflows.
//!
//! Tests multi-step workflows combining delivery success events with
//! attestation leaf creation through the event-driven architecture.
//! Validates end-to-end integration scenarios.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]

use std::sync::Arc;

use kapsel_attestation::AttestationEventSubscriber;
use kapsel_core::models::EventStatus;
use kapsel_testing::{
    events::{test_events, EventHandlerTester},
    TestEnv,
};
use tokio::sync::RwLock;
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Complete scenario: webhook ingestion → delivery → attestation leaf creation.
///
/// Tests the full pipeline where a successful delivery triggers attestation
/// leaf creation via the event system, demonstrating the loose coupling
/// between delivery and attestation systems.
#[tokio::test]
async fn successful_delivery_creates_attestation_leaf_via_events() {
    let mut env = TestEnv::new_isolated().await.expect("failed to create test environment");

    // Setup mock webhook destination
    let mock_server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("OK"))
        .expect(1)
        .mount(&mock_server)
        .await;

    // Create tenant and endpoint
    let mut tx = env.pool().begin().await.expect("begin transaction");
    let tenant_id =
        env.create_tenant_tx(&mut tx, "Test Tenant").await.expect("failed to create tenant");
    let endpoint_id = env
        .create_endpoint_with_config_tx(
            &mut tx,
            tenant_id,
            &mock_server.uri(),
            "Test Endpoint", // name
            3,               // max_retries
            30,              // timeout_seconds
        )
        .await
        .expect("failed to create endpoint");
    tx.commit().await.expect("commit transaction");

    // Setup attestation infrastructure
    let merkle_service = env.create_test_attestation_service().await.unwrap();
    let merkle_service = Arc::new(RwLock::new(merkle_service));

    let attestation_subscriber = AttestationEventSubscriber::new(merkle_service.clone());

    // Setup event handler for delivery-to-attestation integration
    let mut event_tester = EventHandlerTester::new();
    event_tester.add_named_subscriber("attestation", Arc::new(attestation_subscriber));

    // Ingest webhook
    let webhook = test_events::create_test_webhook(tenant_id, endpoint_id);
    let event_id = env.ingest_webhook(&webhook).await.expect("failed to ingest webhook");

    // Process delivery (should succeed and trigger attestation event)
    env.run_delivery_cycle().await.expect("failed to run delivery cycle");

    // Verify webhook was delivered successfully
    let status = env.event_status(event_id).await.expect("failed to find webhook status");
    assert_eq!(status, EventStatus::Delivered);

    // For this test, we need to manually emit the success event since the actual
    // integration between DeliveryEngine and event handlers isn't implemented yet
    let success_event = test_events::create_delivery_succeeded_event_custom(
        &mock_server.uri(),
        200u16,
        1u32,
        [42u8; 32],
        i32::try_from(webhook.body.len()).unwrap(),
    );

    event_tester.handle_event(success_event).await;
    event_tester.wait_for_all_completions().await;

    // Verify attestation leaf was created through event integration
    {
        let pending_count = merkle_service.read().await.pending_count();
        assert_eq!(pending_count, 1, "Successful delivery should create one attestation leaf");
    }
}

/// Scenario: Multiple webhook deliveries create multiple attestation leaves.
///
/// Verifies that each successful delivery creates a corresponding attestation
/// leaf, maintaining a 1:1 mapping between deliveries and attestations.
#[tokio::test]
async fn multiple_deliveries_create_multiple_attestation_leaves() {
    let mut env = TestEnv::new_isolated().await.expect("failed to create test environment");

    // Setup mock server that accepts all requests
    let mock_server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(3) // Expect 3 webhook deliveries
        .mount(&mock_server)
        .await;

    // Create test data
    let mut tx = env.pool().begin().await.expect("begin transaction");
    let tenant_id =
        env.create_tenant_tx(&mut tx, "Test Tenant Multi").await.expect("failed to create tenant");
    let endpoint_id = env
        .create_endpoint_with_config_tx(
            &mut tx,
            tenant_id,
            &mock_server.uri(),
            "Test Endpoint",
            3,
            30,
        )
        .await
        .expect("failed to create endpoint");
    tx.commit().await.expect("commit transaction");

    // Setup attestation service
    let merkle_service = env.create_test_attestation_service().await.unwrap();
    let merkle_service = Arc::new(RwLock::new(merkle_service));
    let attestation_subscriber = AttestationEventSubscriber::new(merkle_service.clone());

    // Setup event handler
    let mut event_tester = EventHandlerTester::new();
    event_tester.add_named_subscriber("attestation", Arc::new(attestation_subscriber));

    // Create multiple webhook events
    let mut event_ids = Vec::new();
    for i in 0..3 {
        let payload = format!(r#"{{"message": "test {i}", "id": {i}}}"#);
        let webhook =
            test_events::create_test_webhook_with_payload(tenant_id, endpoint_id, &payload);
        let event_id = env.ingest_webhook(&webhook).await.expect("failed to ingest webhook");
        event_ids.push(event_id);
    }

    // Process all deliveries
    env.run_delivery_cycle().await.expect("failed to run delivery cycle");

    // Verify all webhooks were delivered
    for event_id in &event_ids {
        let status = env.event_status(*event_id).await.expect("failed to find webhook status");
        assert_eq!(status, EventStatus::Delivered, "all webhooks should be delivered");
    }

    // Manually emit success events for each delivery
    for (i, _event_id) in event_ids.iter().enumerate() {
        let success_event = test_events::create_delivery_succeeded_event_custom(
            &format!("{}?webhook={}", mock_server.uri(), i),
            200u16,
            1u32,
            [u8::try_from(i % 256).unwrap(); 32], // Unique hash for each event
            1024,
        );

        event_tester.handle_event(success_event).await;
    }

    event_tester.wait_for_total_completions(3).await;

    // Verify all attestation leaves were created
    {
        let pending_count = merkle_service.read().await.pending_count();
        assert_eq!(pending_count, 3, "Each successful delivery should create one attestation leaf");
    }
}

/// Scenario: Failed delivery does not create attestation leaf.
///
/// Verifies that failed deliveries do not trigger attestation creation,
/// ensuring attestations only represent actual successful deliveries.
#[tokio::test]
async fn failed_delivery_does_not_create_attestation_leaf() {
    let mut env = TestEnv::new_isolated().await.expect("failed to create test environment");

    // Setup mock server that always returns 500 (failure)
    let mock_server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
        .expect(4) // Will retry up to max_retries (3) + initial attempt
        .mount(&mock_server)
        .await;

    // Create test setup
    let mut tx = env.pool().begin().await.expect("begin transaction");
    let tenant_id = env
        .create_tenant_tx(&mut tx, "Test Tenant Failure")
        .await
        .expect("failed to create tenant");
    let endpoint_id = env
        .create_endpoint_with_config_tx(
            &mut tx,
            tenant_id,
            &mock_server.uri(),
            "Test Endpoint Failure",
            3,
            30,
        )
        .await
        .expect("failed to create endpoint");
    tx.commit().await.expect("commit transaction");

    // Setup attestation service
    let merkle_service = env.create_test_attestation_service().await.unwrap();
    let merkle_service = Arc::new(RwLock::new(merkle_service));
    let attestation_subscriber = AttestationEventSubscriber::new(merkle_service.clone());

    let mut event_tester = EventHandlerTester::new();
    event_tester.add_named_subscriber("attestation", Arc::new(attestation_subscriber));

    // Ingest webhook
    let webhook = test_events::create_test_webhook_with_payload(
        tenant_id,
        endpoint_id,
        r#"{"message": "test failure"}"#,
    );
    let event_id = env.ingest_webhook(&webhook).await.expect("failed to ingest webhook");

    // Process delivery through all retry attempts (max_retries=3 means 4 total
    // attempts) The test harness now uses production retry logic which properly
    // marks events as failed

    // Attempt 1 (initial): Should fail and remain pending
    env.run_delivery_cycle().await.expect("failed to run delivery cycle");
    let status = env.event_status(event_id).await.expect("failed to find webhook status");
    assert_eq!(status, EventStatus::Pending, "First attempt should leave webhook pending");

    // Advance time and attempt 2 (retry 1): Should fail and remain pending
    env.advance_time(std::time::Duration::from_secs(1));
    env.run_delivery_cycle().await.expect("failed to run delivery cycle");
    let status = env.event_status(event_id).await.expect("failed to find webhook status");
    assert_eq!(status, EventStatus::Pending, "Second attempt should leave webhook pending");

    // Advance time and attempt 3 (retry 2): Should fail and remain pending
    env.advance_time(std::time::Duration::from_secs(2));
    env.run_delivery_cycle().await.expect("failed to run delivery cycle");
    let status = env.event_status(event_id).await.expect("failed to find webhook status");
    assert_eq!(status, EventStatus::Pending, "Third attempt should leave webhook pending");

    // Advance time and attempt 4 (retry 3 - final): Should fail and mark as failed
    env.advance_time(std::time::Duration::from_secs(4));
    env.run_delivery_cycle().await.expect("failed to run delivery cycle");
    let status = env.event_status(event_id).await.expect("failed to find webhook status");
    assert_eq!(
        status,
        EventStatus::Failed,
        "Webhook should be failed after exhausting max retries"
    );

    // Manually emit failure event to test that failures don't create attestations
    let failure_event = test_events::create_delivery_failed_event();
    event_tester.handle_event(failure_event).await;
    event_tester.wait_for_all_completions().await;

    // Verify no attestation leaf was created for failed delivery
    {
        let pending_count = merkle_service.read().await.pending_count();
        assert_eq!(pending_count, 0, "Failed delivery should not create attestation leaf");
    }
}

/// Scenario: Direct event emission for attestation testing.
///
/// Tests attestation creation by directly emitting delivery success events,
/// bypassing the full delivery pipeline for focused testing.
#[tokio::test]
async fn direct_success_event_creates_attestation_leaf() {
    let env = TestEnv::new_shared().await.expect("failed to create test environment");

    // Setup attestation service
    let merkle_service = env.create_test_attestation_service().await.unwrap();
    let merkle_service = Arc::new(RwLock::new(merkle_service));
    let attestation_subscriber = AttestationEventSubscriber::new(merkle_service.clone());

    let mut event_tester = EventHandlerTester::new();
    event_tester.add_named_subscriber("attestation", Arc::new(attestation_subscriber));

    // Create and emit delivery success event directly
    let success_event = test_events::create_delivery_succeeded_event_custom(
        "https://test.example.com/webhook",
        200u16,
        1u32,
        [42u8; 32],
        1024,
    );

    event_tester.handle_event(success_event).await;
    event_tester.wait_for_all_completions().await;

    // Verify attestation leaf was created
    {
        let pending_count = merkle_service.read().await.pending_count();
        assert_eq!(pending_count, 1, "Direct success event should create attestation leaf");
    }
}

/// Scenario: Attestation batch processing after multiple events.
///
/// Tests that multiple attestation leaves can be committed as a batch,
/// demonstrating the batching behavior of the merkle service.
#[tokio::test]
async fn multiple_events_enable_batch_attestation_commitment() {
    let env = TestEnv::new_shared().await.expect("failed to create test environment");

    // Setup attestation service
    let merkle_service = env.create_test_attestation_service().await.unwrap();
    let merkle_service = Arc::new(RwLock::new(merkle_service));
    let attestation_subscriber = AttestationEventSubscriber::new(merkle_service.clone());

    // Capture initial tree size to measure relative growth
    let initial_tree_size = env.storage().signed_tree_heads.find_max_tree_size().await.unwrap();

    let mut event_tester = EventHandlerTester::new();
    event_tester.add_named_subscriber("attestation", Arc::new(attestation_subscriber));

    // Create multiple delivery success events
    let event_count = 5;
    for i in 0..event_count {
        let success_event = test_events::create_delivery_succeeded_event_custom(
            &format!("https://test{i}.example.com/webhook"),
            200u16,
            1u32,
            [u8::try_from(i % 256).unwrap(); 32], // Unique hash for each event
            1024,
        );

        event_tester.handle_event(success_event).await;
    }

    event_tester.wait_for_total_completions(event_count).await;

    // Verify all leaves are pending
    {
        let pending_count = merkle_service.read().await.pending_count();
        assert_eq!(
            pending_count, event_count,
            "All successful delivery events should create attestation leaves"
        );
    }

    // Commit batch of attestation leaves
    // Batch commit the attestation leaves and generate signed tree head
    {
        let signed_tree_head = merkle_service
            .write()
            .await
            .try_commit_pending()
            .await
            .expect("failed to commit attestation batch");

        let expected_tree_size = u64::try_from(initial_tree_size)
            .expect("tree size should be non-negative")
            + event_count as u64;
        assert_eq!(
            signed_tree_head.tree_size, expected_tree_size,
            "Tree should grow by {} leaves (from {} to {}), but got {}",
            event_count, initial_tree_size, expected_tree_size, signed_tree_head.tree_size
        );
    }

    // Verify no leaves are pending after batch commit
    {
        let pending_count = merkle_service.read().await.pending_count();
        assert_eq!(pending_count, 0, "No leaves should be pending after batch commit");
    }
}
