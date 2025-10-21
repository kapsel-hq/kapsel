//! Scenario tests for complete attestation workflows.
//!
//! Tests multi-step workflows combining delivery success events with
//! attestation leaf creation through the event-driven architecture.
//! Validates end-to-end integration scenarios.

use std::sync::Arc;

use kapsel_attestation::{AttestationEventSubscriber, MerkleService, SigningService};
use kapsel_testing::{
    events::{test_events, EventHandlerTester},
    TestEnv,
};
use tokio::sync::RwLock;
use uuid::Uuid;
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Complete scenario: webhook ingestion → delivery → attestation leaf creation.
///
/// Tests the full pipeline where a successful delivery triggers attestation
/// leaf creation via the event system, demonstrating the loose coupling
/// between delivery and attestation systems.
#[tokio::test]
async fn successful_delivery_creates_attestation_leaf_via_events() {
    let mut env = TestEnv::new().await.expect("failed to create test environment");

    // Setup mock webhook destination
    let mock_server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("OK"))
        .expect(1)
        .mount(&mock_server)
        .await;

    // Create tenant and endpoint
    let tenant_id = env.create_tenant("Test Tenant").await.expect("failed to create tenant");
    let endpoint_id = env
        .create_endpoint_with_config(
            tenant_id,
            &mock_server.uri(),
            "Test Endpoint", // name
            3,               // max_retries
            30,              // timeout_seconds
        )
        .await
        .expect("failed to create endpoint");

    // Setup attestation infrastructure
    let signing_service = SigningService::ephemeral();
    let key_id = store_signing_key_in_db(&env, &signing_service).await;
    let signing_service = signing_service.with_key_id(key_id);
    let merkle_service =
        Arc::new(RwLock::new(MerkleService::new(env.pool().clone(), signing_service)));

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
    let status = env.find_webhook_status(event_id).await.expect("failed to find webhook status");
    assert_eq!(status, "delivered");

    // For this test, we need to manually emit the success event since the actual
    // integration between DeliveryEngine and event handlers isn't implemented yet
    let success_event = test_events::create_delivery_succeeded_event_custom(
        &mock_server.uri(),
        200u16,
        1u32,
        [42u8; 32],
        webhook.body.len() as i32,
    );

    event_tester.handle_event(success_event).await;
    event_tester.wait_for_all_completions().await;

    // Verify attestation leaf was created through event integration
    {
        let service = merkle_service.read().await;
        let pending_count =
            service.pending_count().await.expect("failed to get pending attestation count");
        assert_eq!(pending_count, 1, "Successful delivery should create one attestation leaf");
    }
}

/// Scenario: Multiple webhook deliveries create multiple attestation leaves.
///
/// Verifies that each successful delivery creates a corresponding attestation
/// leaf, maintaining a 1:1 mapping between deliveries and attestations.
#[tokio::test]
async fn multiple_deliveries_create_multiple_attestation_leaves() {
    let mut env = TestEnv::new().await.expect("failed to create test environment");

    // Setup mock server that accepts all requests
    let mock_server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(3) // Expect 3 webhook deliveries
        .mount(&mock_server)
        .await;

    // Create test data
    let tenant_id = env.create_tenant("Test Tenant Multi").await.expect("failed to create tenant");
    let endpoint_id = env
        .create_endpoint_with_config(tenant_id, &mock_server.uri(), "Test Endpoint", 3, 30)
        .await
        .expect("failed to create endpoint");

    // Setup attestation service
    let signing_service = SigningService::ephemeral();
    let key_id = store_signing_key_in_db(&env, &signing_service).await;
    let signing_service = signing_service.with_key_id(key_id);
    let merkle_service =
        Arc::new(RwLock::new(MerkleService::new(env.pool().clone(), signing_service)));
    let attestation_subscriber = AttestationEventSubscriber::new(merkle_service.clone());

    // Setup event handler
    let mut event_tester = EventHandlerTester::new();
    event_tester.add_named_subscriber("attestation", Arc::new(attestation_subscriber));

    // Create multiple webhook events
    let mut event_ids = Vec::new();
    for i in 0..3 {
        let payload = format!(r#"{{"message": "test {}", "id": {}}}"#, i, i);
        let webhook =
            test_events::create_test_webhook_with_payload(tenant_id, endpoint_id, &payload);
        let event_id = env.ingest_webhook(&webhook).await.expect("failed to ingest webhook");
        event_ids.push(event_id);
    }

    // Process all deliveries
    env.run_delivery_cycle().await.expect("failed to run delivery cycle");

    // Verify all webhooks were delivered
    for event_id in &event_ids {
        let status =
            env.find_webhook_status(*event_id).await.expect("failed to find webhook status");
        assert_eq!(status, "delivered", "All webhooks should be delivered");
    }

    // Manually emit success events for each delivery
    for (i, _event_id) in event_ids.iter().enumerate() {
        let success_event = test_events::create_delivery_succeeded_event_custom(
            &format!("{}?webhook={}", mock_server.uri(), i),
            200u16,
            1u32,
            [i as u8; 32], // Unique hash for each event
            1024,
        );

        event_tester.handle_event(success_event).await;
    }

    event_tester.wait_for_total_completions(3).await;

    // Verify all attestation leaves were created
    {
        let service = merkle_service.read().await;
        let pending_count =
            service.pending_count().await.expect("failed to get pending attestation count");
        assert_eq!(pending_count, 3, "Each successful delivery should create one attestation leaf");
    }
}

/// Scenario: Failed delivery does not create attestation leaf.
///
/// Verifies that failed deliveries do not trigger attestation creation,
/// ensuring attestations only represent actual successful deliveries.
#[tokio::test]
async fn failed_delivery_does_not_create_attestation_leaf() {
    let mut env = TestEnv::new().await.expect("failed to create test environment");

    // Setup mock server that always returns 500 (failure)
    let mock_server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
        .expect(3) // Will retry up to max_retries
        .mount(&mock_server)
        .await;

    // Create test setup
    let tenant_id =
        env.create_tenant("Test Tenant Failure").await.expect("failed to create tenant");
    let endpoint_id = env
        .create_endpoint_with_config(
            tenant_id,
            &mock_server.uri(),
            "Failing Endpoint",
            2, // max_retries (will attempt 3 times total)
            30,
        )
        .await
        .expect("failed to create endpoint");

    // Setup attestation service
    let signing_service = SigningService::ephemeral();
    let merkle_service =
        Arc::new(RwLock::new(MerkleService::new(env.pool().clone(), signing_service)));
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

    // Process delivery through all retry attempts (max_retries=2 means 3 total
    // attempts + 1 final cycle) Attempt 1: Should fail and remain pending
    env.run_delivery_cycle().await.expect("failed to run delivery cycle");
    let status = env.find_webhook_status(event_id).await.expect("failed to find webhook status");
    assert_eq!(status, "pending", "First attempt should leave webhook pending");

    // Advance time and attempt 2: Should fail and remain pending
    env.clock.advance(std::time::Duration::from_secs(1));
    env.run_delivery_cycle().await.expect("failed to run delivery cycle");
    let status = env.find_webhook_status(event_id).await.expect("failed to find webhook status");
    assert_eq!(status, "pending", "Second attempt should leave webhook pending");

    // Advance time and attempt 3 (final retry): Should fail and remain pending
    env.clock.advance(std::time::Duration::from_secs(2));
    env.run_delivery_cycle().await.expect("failed to run delivery cycle");
    let status = env.find_webhook_status(event_id).await.expect("failed to find webhook status");
    assert_eq!(status, "pending", "Final retry should leave webhook pending");

    // Advance time and final cycle: Should mark as failed
    env.clock.advance(std::time::Duration::from_secs(4));
    env.run_delivery_cycle().await.expect("failed to run delivery cycle");
    let status = env.find_webhook_status(event_id).await.expect("failed to find webhook status");
    assert_eq!(status, "failed", "Webhook should be marked as failed after all retries exhausted");

    // Manually emit failure event to test that failures don't create attestations
    let failure_event = test_events::create_delivery_failed_event();
    event_tester.handle_event(failure_event).await;
    event_tester.wait_for_all_completions().await;

    // Verify no attestation leaf was created for failed delivery
    {
        let service = merkle_service.read().await;
        let pending_count =
            service.pending_count().await.expect("failed to get pending attestation count");
        assert_eq!(pending_count, 0, "Failed delivery should not create attestation leaf");
    }
}

/// Scenario: Direct event emission for attestation testing.
///
/// Tests attestation creation by directly emitting delivery success events,
/// bypassing the full delivery pipeline for focused testing.
#[tokio::test]
async fn direct_success_event_creates_attestation_leaf() {
    let env = TestEnv::new().await.expect("failed to create test environment");

    // Setup attestation service
    let signing_service = SigningService::ephemeral();
    let merkle_service =
        Arc::new(RwLock::new(MerkleService::new(env.pool().clone(), signing_service)));
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
        let service = merkle_service.read().await;
        let pending_count =
            service.pending_count().await.expect("failed to get pending attestation count");
        assert_eq!(pending_count, 1, "Direct success event should create attestation leaf");
    }
}

/// Scenario: Attestation batch processing after multiple events.
///
/// Tests that multiple attestation leaves can be committed as a batch,
/// demonstrating the batching behavior of the merkle service.
#[tokio::test]
async fn multiple_events_enable_batch_attestation_commitment() {
    let env = TestEnv::new().await.expect("failed to create test environment");

    // Setup attestation service
    let signing_service = SigningService::ephemeral();
    let key_id = store_signing_key_in_db(&env, &signing_service).await;
    let signing_service = signing_service.with_key_id(key_id);
    let merkle_service =
        Arc::new(RwLock::new(MerkleService::new(env.pool().clone(), signing_service)));
    let attestation_subscriber = AttestationEventSubscriber::new(merkle_service.clone());

    let mut event_tester = EventHandlerTester::new();
    event_tester.add_named_subscriber("attestation", Arc::new(attestation_subscriber));

    // Create multiple delivery success events
    let event_count = 5;
    for i in 0..event_count {
        let success_event = test_events::create_delivery_succeeded_event_custom(
            &format!("https://test{}.example.com/webhook", i),
            200u16,
            1u32,
            [i as u8; 32], // Unique hash for each event
            1024,
        );

        event_tester.handle_event(success_event).await;
    }

    event_tester.wait_for_total_completions(event_count).await;

    // Verify all leaves are pending
    {
        let service = merkle_service.read().await;
        let pending_count =
            service.pending_count().await.expect("failed to get pending attestation count");
        assert_eq!(
            pending_count, event_count,
            "All success events should create attestation leaves"
        );
    }

    // Commit batch of attestation leaves
    {
        let mut service = merkle_service.write().await;
        let signed_tree_head =
            service.try_commit_pending().await.expect("failed to commit attestation batch");

        assert_eq!(
            signed_tree_head.tree_size, event_count as u64,
            "All pending leaves should be committed in batch"
        );
    }

    // Verify no leaves are pending after batch commit
    {
        let service = merkle_service.read().await;
        let pending_count =
            service.pending_count().await.expect("failed to get pending count after commit");
        assert_eq!(pending_count, 0, "No leaves should be pending after batch commit");
    }
}

/// Helper function to store signing key in database for testing.
async fn store_signing_key_in_db(env: &TestEnv, signing_service: &SigningService) -> Uuid {
    let key_id = Uuid::new_v4();
    let public_key_bytes = signing_service.public_key_as_bytes();

    sqlx::query(
        r#"
        INSERT INTO attestation_keys (id, public_key, is_active, created_at)
        VALUES ($1, $2, true, NOW())
        "#,
    )
    .bind(key_id)
    .bind(&public_key_bytes)
    .execute(env.pool())
    .await
    .expect("should store signing key in database");

    key_id
}
