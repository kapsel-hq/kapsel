//! Simple test to validate mock storage abstraction.
//!
//! This test verifies that the DeliveryStorage trait abstraction works
//! correctly with the mock implementation, enabling unit tests without
//! database dependencies.

use std::collections::HashMap;

use chrono::Utc;
use kapsel_core::{
    models::{EndpointId, EventId, EventStatus, IdempotencyStrategy, TenantId, WebhookEvent},
    time::TestClock,
};
use kapsel_delivery::storage::{mock::MockDeliveryStorage, DeliveryStorage};
use sqlx::types::Json;
use uuid::Uuid;

/// Test that mock storage can be created and basic operations work.
#[tokio::test]
async fn mock_storage_basic_operations() {
    let storage = MockDeliveryStorage::new();
    let _clock = TestClock::new();

    // Create a test webhook event with correct field types
    let event = WebhookEvent {
        id: EventId(Uuid::new_v4()),
        tenant_id: TenantId(Uuid::new_v4()),
        endpoint_id: EndpointId(Uuid::new_v4()),
        source_event_id: "test-source-001".to_string(),
        idempotency_strategy: IdempotencyStrategy::Header,
        status: EventStatus::Pending,
        failure_count: 0,
        last_attempt_at: None,
        next_retry_at: None,
        headers: Json(HashMap::new()),
        body: b"test payload".to_vec(),
        content_type: "application/json".to_string(),
        received_at: Utc::now(),
        delivered_at: None,
        failed_at: None,
        payload_size: 12,
        signature_valid: Some(true),
        signature_error: None,
    };

    // Add test data to mock storage
    storage.add_pending_event(event.clone()).await;
    storage.add_endpoint_url(event.endpoint_id, "https://example.com/webhook".to_string()).await;

    // Test claiming events
    let storage_ref: &dyn DeliveryStorage = &storage;
    let claimed: Vec<WebhookEvent> = storage_ref.claim_pending_events(10).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, event.id);

    // Status should be updated to Delivering after claiming
    let status_after_claim: EventStatus = storage_ref.find_event_status(event.id).await.unwrap();
    assert_eq!(status_after_claim, EventStatus::Delivering);

    // Test endpoint URL retrieval
    let url: String = storage_ref.find_endpoint_url(event.endpoint_id).await.unwrap();
    assert_eq!(url, "https://example.com/webhook");

    // Test marking delivered
    let _: () = storage_ref.mark_delivered(event.id).await.unwrap();
    let status: EventStatus = storage_ref.find_event_status(event.id).await.unwrap();
    assert_eq!(status, EventStatus::Delivered);
}

/// Test that mock storage handles retry scheduling correctly.
#[tokio::test]
async fn mock_storage_retry_scheduling() {
    let storage = MockDeliveryStorage::new();

    let event = WebhookEvent {
        id: EventId(Uuid::new_v4()),
        tenant_id: TenantId(Uuid::new_v4()),
        endpoint_id: EndpointId(Uuid::new_v4()),
        source_event_id: "retry-test-001".to_string(),
        idempotency_strategy: IdempotencyStrategy::Header,
        status: EventStatus::Delivering,
        failure_count: 1,
        last_attempt_at: Some(Utc::now()),
        next_retry_at: None,
        headers: Json(HashMap::new()),
        body: b"retry payload".to_vec(),
        content_type: "application/json".to_string(),
        received_at: Utc::now(),
        delivered_at: None,
        failed_at: None,
        payload_size: 13,
        signature_valid: Some(true),
        signature_error: None,
    };

    storage.add_pending_event(event.clone()).await;

    // Test scheduling retry
    let retry_time = Utc::now() + chrono::Duration::seconds(30);
    let storage_ref: &dyn DeliveryStorage = &storage;
    let _: () = storage_ref.schedule_retry(event.id, retry_time, 2).await.unwrap();

    let status: EventStatus = storage_ref.find_event_status(event.id).await.unwrap();
    assert_eq!(status, EventStatus::Pending); // Should be back to pending for
                                              // retry
}

/// Test that mock storage handles failures correctly.
#[tokio::test]
async fn mock_storage_failure_handling() {
    let storage = MockDeliveryStorage::new();

    let event = WebhookEvent {
        id: EventId(Uuid::new_v4()),
        tenant_id: TenantId(Uuid::new_v4()),
        endpoint_id: EndpointId(Uuid::new_v4()),
        source_event_id: "failure-test-001".to_string(),
        idempotency_strategy: IdempotencyStrategy::Header,
        status: EventStatus::Delivering,
        failure_count: 5,
        last_attempt_at: Some(Utc::now()),
        next_retry_at: None,
        headers: Json(HashMap::new()),
        body: b"failure payload".to_vec(),
        content_type: "application/json".to_string(),
        received_at: Utc::now(),
        delivered_at: None,
        failed_at: None,
        payload_size: 15,
        signature_valid: Some(true),
        signature_error: None,
    };

    storage.add_pending_event(event.clone()).await;

    // Test marking as failed
    let storage_ref: &dyn DeliveryStorage = &storage;
    let _: () = storage_ref.mark_failed(event.id).await.unwrap();
    let status: EventStatus = storage_ref.find_event_status(event.id).await.unwrap();
    assert_eq!(status, EventStatus::Failed);
}

/// Test error injection functionality.
#[tokio::test]
async fn mock_storage_error_injection() {
    let storage = MockDeliveryStorage::new();

    // Inject a claim error
    storage.inject_claim_error("simulated database failure".to_string()).await;

    // Should return the injected error
    let storage_ref: &dyn DeliveryStorage = &storage;
    let result: Result<Vec<WebhookEvent>, _> = storage_ref.claim_pending_events(10).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("simulated database failure"));

    // Second call should work (error is consumed)
    let result: Result<Vec<WebhookEvent>, _> = storage_ref.claim_pending_events(10).await;
    assert!(result.is_ok());
}

/// Test that delivery attempts are recorded correctly.
#[tokio::test]
async fn mock_storage_delivery_attempts() {
    use kapsel_core::models::DeliveryAttempt;

    let storage = MockDeliveryStorage::new();

    let event_id = EventId(Uuid::new_v4());
    let endpoint_id = EndpointId(Uuid::new_v4());

    let attempt = DeliveryAttempt {
        id: Uuid::new_v4(),
        event_id,
        attempt_number: 1,
        endpoint_id,
        request_headers: HashMap::new(),
        request_body: b"test payload".to_vec(),
        response_status: Some(200),
        response_headers: None,
        response_body: Some(b"OK".to_vec()),
        attempted_at: Utc::now(),
        succeeded: true,
        error_message: None,
    };

    // Record delivery attempt
    let storage_ref: &dyn DeliveryStorage = &storage;
    let _: () = storage_ref.record_delivery_attempt(attempt.clone()).await.unwrap();

    // Verify it was recorded
    let attempts: Vec<kapsel_core::models::DeliveryAttempt> =
        storage_ref.find_delivery_attempts(event_id).await.unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].id, attempt.id);
    assert!(attempts[0].succeeded);

    // Test recorded_attempts helper method
    let all_attempts: Vec<kapsel_core::models::DeliveryAttempt> = storage.recorded_attempts().await;
    assert_eq!(all_attempts.len(), 1);
    assert_eq!(all_attempts[0].event_id, event_id);
}
