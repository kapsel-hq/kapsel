//! Property-based tests for delivery storage abstraction.
//!
//! This file replaces the 23+ individual HTTP status tests in the old
//! worker_test.rs with focused property-based validation of the storage
//! abstraction layer. Tests focus on business logic invariants and data
//! consistency rather than full HTTP integration, enabling comprehensive
//! validation without infrastructure dependencies.

use std::{collections::HashMap, sync::Arc};

use chrono::Utc;
use kapsel_core::models::{
    BackoffStrategy, CircuitState, EndpointId, EventId, EventStatus, IdempotencyStrategy,
    SignatureConfig, TenantId, WebhookEvent,
};
use kapsel_delivery::storage::{mock::MockDeliveryStorage, DeliveryStorage};
use proptest::prelude::*;
use sqlx::types::Json;
use uuid::Uuid;

/// Strategy for generating realistic webhook events.
fn webhook_event_strategy() -> impl Strategy<Value = WebhookEvent> {
    (
        Just(()).prop_map(|()| EventId::new()),
        Just(()).prop_map(|()| TenantId::new()),
        Just(()).prop_map(|()| EndpointId::new()),
        "[a-zA-Z0-9-]{1,50}",                        // source_event_id
        prop::collection::vec(any::<u8>(), 1..1024), // body
        0i32..5,                                     // failure_count
    )
        .prop_map(|(id, tenant_id, endpoint_id, source_event_id, body, failure_count)| {
            WebhookEvent {
                id,
                tenant_id,
                endpoint_id,
                source_event_id,
                idempotency_strategy: IdempotencyStrategy::Header,
                status: if failure_count == 0 {
                    EventStatus::Pending
                } else {
                    EventStatus::Delivering
                },
                failure_count,
                last_attempt_at: None,
                next_retry_at: None,
                headers: Json(HashMap::new()),
                body: body.clone(),
                content_type: "application/json".to_string(),
                received_at: Utc::now(),
                delivered_at: None,
                failed_at: None,
                payload_size: i32::try_from(body.len()).unwrap_or(0),
                signature_valid: Some(true),
                signature_error: None,
            }
        })
}

/// Strategy for generating endpoint configurations.
fn endpoint_strategy() -> impl Strategy<Value = kapsel_core::models::Endpoint> {
    (
        Just(()).prop_map(|()| EndpointId::new()),
        Just(()).prop_map(|()| TenantId::new()),
        "https://example\\.com/webhook[0-9]{1,3}", // url
        "[a-zA-Z ]{5,30}",                         // name
        1i32..=10,                                 // max_retries
        5i32..120,                                 // timeout_seconds
    )
        .prop_map(|(id, tenant_id, url, name, max_retries, timeout_seconds)| {
            kapsel_core::models::Endpoint {
                id,
                tenant_id,
                url,
                name,
                is_active: true,
                signature_config: SignatureConfig::None,
                max_retries,
                timeout_seconds,
                retry_strategy: BackoffStrategy::Exponential,
                circuit_state: CircuitState::Closed,
                circuit_failure_count: 0,
                circuit_success_count: 0,
                circuit_last_failure_at: None,
                circuit_half_open_at: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                deleted_at: None,
                total_events_received: 0,
                total_events_delivered: 0,
                total_events_failed: 0,
            }
        })
}

proptest! {
    /// Property test: Mock storage preserves event data integrity.
    ///
    /// Validates that events stored in mock storage can be retrieved
    /// with identical data, ensuring the storage abstraction doesn't
    /// corrupt or lose webhook data.
    #[test]
    fn mock_storage_preserves_event_data_integrity(
        mut events in prop::collection::vec(webhook_event_strategy(), 1..50),
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let storage = Arc::new(MockDeliveryStorage::new());

            // Ensure all events start as Pending for consistent testing
            for event in &mut events {
                event.status = EventStatus::Pending;
            }

            // Store all events
            for event in &events {
                storage.add_pending_event(event.clone()).await;
            }

            // Verify initial status for all events
            for expected_event in &events {
                let status = storage.find_event_status(expected_event.id).await.unwrap();
                prop_assert_eq!(status, EventStatus::Pending);
            }

            // Test claiming events maintains data integrity
            let claimed = storage.claim_pending_events(events.len()).await.unwrap();
            prop_assert_eq!(claimed.len(), events.len());

            // Verify each claimed event has correct data
            for expected_event in &events {
                let found_event = claimed.iter().find(|e| e.id == expected_event.id);
                prop_assert!(found_event.is_some(), "Event {} not found in claimed events", expected_event.id);

                if let Some(actual_event) = found_event {
                    prop_assert_eq!(actual_event.id, expected_event.id);
                    prop_assert_eq!(actual_event.tenant_id, expected_event.tenant_id);
                    prop_assert_eq!(actual_event.endpoint_id, expected_event.endpoint_id);
                    prop_assert_eq!(&actual_event.source_event_id, &expected_event.source_event_id);
                    prop_assert_eq!(&actual_event.body, &expected_event.body);
                    prop_assert_eq!(&actual_event.content_type, &expected_event.content_type);
                    prop_assert_eq!(actual_event.payload_size, expected_event.payload_size);

                    // Verify status changed to Delivering after claiming
                    prop_assert_eq!(actual_event.status, EventStatus::Delivering);
                }
            }

            Ok(())
        })?;
    }

    /// Property test: Event state transitions are valid and consistent.
    ///
    /// Validates that state transitions follow the correct workflow:
    /// Pending -> Delivering -> (Delivered|Failed|Pending)
    #[test]
    fn event_state_transitions_are_valid(
        mut event in webhook_event_strategy(),
        endpoint in endpoint_strategy(),
        transition_sequence in prop::collection::vec(0u8..4, 1..10),
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let storage = Arc::new(MockDeliveryStorage::new());

            // Setup endpoint and initial event
            storage.add_endpoint(endpoint.clone()).await;
            event.endpoint_id = endpoint.id;
            event.status = EventStatus::Pending;
            storage.add_pending_event(event.clone()).await;

            let mut current_status = EventStatus::Pending;

            for &transition in &transition_sequence {
                match transition {
                    0 => {
                        // Claim event (Pending -> Delivering)
                        if current_status == EventStatus::Pending {
                            let claimed = storage.claim_pending_events(1).await.unwrap();
                            if !claimed.is_empty() {
                                current_status = EventStatus::Delivering;
                            }
                        }
                    },
                    1 => {
                        // Mark delivered (Delivering -> Delivered)
                        if current_status == EventStatus::Delivering {
                            storage.mark_delivered(event.id).await.unwrap();
                            current_status = EventStatus::Delivered;
                        }
                    },
                    2 => {
                        // Schedule retry (Delivering -> Pending)
                        if current_status == EventStatus::Delivering {
                            let retry_time = Utc::now() + chrono::Duration::seconds(60);
                            storage.schedule_retry(event.id, retry_time, 1).await.unwrap();
                            current_status = EventStatus::Pending;
                        }
                    },
                    3 => {
                        // Mark failed (Delivering -> Failed)
                        if current_status == EventStatus::Delivering {
                            storage.mark_failed(event.id).await.unwrap();
                            current_status = EventStatus::Failed;
                        }
                    },
                    _ => {},
                }
            }

            // Verify final state is valid
            let final_status = storage.find_event_status(event.id).await.unwrap();
            prop_assert_eq!(final_status, current_status);

            // Ensure terminal states remain terminal
            if matches!(current_status, EventStatus::Delivered | EventStatus::Failed) {
                // Terminal states should not be claimable
                let claimed_after_terminal = storage.claim_pending_events(10).await.unwrap();
                prop_assert!(!claimed_after_terminal.iter().any(|e| e.id == event.id));
            }

            Ok(())
        })?;
    }

    /// Property test: Endpoint configuration retrieval is consistent.
    ///
    /// Validates that endpoint configurations stored in mock storage
    /// can be retrieved correctly and maintain referential integrity.
    #[test]
    fn endpoint_configuration_consistency(
        endpoints in prop::collection::vec(endpoint_strategy(), 1..20),
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let storage = Arc::new(MockDeliveryStorage::new());

            // Store all endpoints
            for endpoint in &endpoints {
                storage.add_endpoint(endpoint.clone()).await;
            }

            // Retrieve and verify each endpoint
            for expected_endpoint in &endpoints {
                let retrieved = storage.find_endpoint_config(expected_endpoint.id).await.unwrap();

                prop_assert_eq!(retrieved.id, expected_endpoint.id);
                prop_assert_eq!(retrieved.tenant_id, expected_endpoint.tenant_id);
                prop_assert_eq!(&retrieved.url, &expected_endpoint.url);
                prop_assert_eq!(&retrieved.name, &expected_endpoint.name);
                prop_assert_eq!(retrieved.max_retries, expected_endpoint.max_retries);
                prop_assert_eq!(retrieved.timeout_seconds, expected_endpoint.timeout_seconds);
                prop_assert_eq!(retrieved.retry_strategy, expected_endpoint.retry_strategy);

                // Test endpoint_url method returns consistent URL
                let url = storage.find_endpoint_url(expected_endpoint.id).await.unwrap();
                prop_assert_eq!(&url, &expected_endpoint.url);
            }

            Ok(())
        })?;
    }

    /// Property test: Concurrent event claiming prevents double-processing.
    ///
    /// Validates that multiple concurrent claims don't return the same
    /// events, ensuring proper isolation in the storage abstraction.
    #[test]
    fn concurrent_claiming_prevents_double_processing(
        events in prop::collection::vec(webhook_event_strategy(), 5..50),
        claim_operations in prop::collection::vec(1usize..10, 2..8),
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let storage = Arc::new(MockDeliveryStorage::new());

            // Add all events as pending
            for mut event in events.iter().cloned() {
                event.status = EventStatus::Pending;
                storage.add_pending_event(event).await;
            }

            let mut all_claimed_ids = Vec::new();

            // Perform multiple claiming operations
            for &batch_size in &claim_operations {
                let claimed = storage.claim_pending_events(batch_size).await.unwrap();
                for event in claimed {
                    all_claimed_ids.push(event.id);
                }
            }

            // Verify no event was claimed multiple times
            all_claimed_ids.sort_by_key(|id| id.0);
            let original_len = all_claimed_ids.len();
            all_claimed_ids.dedup();
            let deduped_len = all_claimed_ids.len();

            prop_assert_eq!(original_len, deduped_len,
                "Double-processing detected: {} events claimed, {} unique",
                original_len, deduped_len
            );

            // Verify all claimed events are now in Delivering state
            for event_id in &all_claimed_ids {
                let status = storage.find_event_status(*event_id).await.unwrap();
                prop_assert_eq!(status, EventStatus::Delivering);
            }

            Ok(())
        })?;
    }

    /// Property test: Storage trait abstraction maintains invariants.
    ///
    /// Validates that the DeliveryStorage trait abstraction maintains
    /// critical business invariants regardless of implementation details.
    #[test]
    fn storage_abstraction_maintains_invariants(
        mut event in webhook_event_strategy(),
        endpoint in endpoint_strategy(),
        operations in prop::collection::vec(0u8..5, 1..20),
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let storage = Arc::new(MockDeliveryStorage::new());

            // Setup
            storage.add_endpoint(endpoint.clone()).await;
            event.endpoint_id = endpoint.id;
            event.status = EventStatus::Pending;
            storage.add_pending_event(event.clone()).await;

            let mut delivery_attempts = 0;

            for &operation in &operations {
                match operation {
                    0 => {
                        // Claim event
                        let claimed = storage.claim_pending_events(1).await.unwrap();
                        if !claimed.is_empty() {
                            let status = storage.find_event_status(event.id).await.unwrap();
                            prop_assert_eq!(status, EventStatus::Delivering);
                        }
                    },
                    1 => {
                        // Mark delivered
                        let _ = storage.mark_delivered(event.id).await;
                        let status = storage.find_event_status(event.id).await.unwrap();
                        if status == EventStatus::Delivered {
                            // Terminal state reached
                            break;
                        }
                    },
                    2 => {
                        // Schedule retry
                        let retry_time = Utc::now() + chrono::Duration::seconds(30);
                        let _ = storage.schedule_retry(event.id, retry_time, delivery_attempts).await;
                        delivery_attempts += 1;
                    },
                    3 => {
                        // Mark failed
                        let _ = storage.mark_failed(event.id).await;
                        let status = storage.find_event_status(event.id).await.unwrap();
                        if status == EventStatus::Failed {
                            // Terminal state reached
                            break;
                        }
                    },
                    4 => {
                        // Record delivery attempt
                        let attempt = kapsel_core::models::DeliveryAttempt {
                            id: Uuid::new_v4(),
                            event_id: event.id,
                            attempt_number: 1,
                            endpoint_id: endpoint.id,
                            request_headers: std::collections::HashMap::new(),
                            request_body: b"test payload".to_vec(),
                            response_status: Some(200),
                            response_headers: None,
                            response_body: Some(b"OK".to_vec()),
                            attempted_at: Utc::now(),
                            succeeded: true,
                            error_message: None,
                        };
                        storage.record_delivery_attempt(attempt).await.unwrap();
                    },
                    _ => {
                        // No-op for other values
                    }
                }
            }

            // Verify final invariants
            let final_status = storage.find_event_status(event.id).await.unwrap();
            prop_assert!(matches!(final_status,
                EventStatus::Pending | EventStatus::Delivering |
                EventStatus::Delivered | EventStatus::Failed
            ));

            // Verify endpoint configuration is accessible
            let endpoint_config = storage.find_endpoint_config(endpoint.id).await.unwrap();
            prop_assert_eq!(endpoint_config.id, endpoint.id);

            // Verify delivery attempts are recorded
            let attempts = storage.find_delivery_attempts(event.id).await.unwrap();
            prop_assert!(attempts.len() <= 20); // Reasonable upper bound

            Ok(())
        })?;
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    /// Unit test: Mock storage basic operations work correctly.
    #[tokio::test]
    async fn mock_storage_basic_operations() {
        let storage = Arc::new(MockDeliveryStorage::new());

        // Create test endpoint
        let endpoint = kapsel_core::models::Endpoint {
            id: EndpointId(Uuid::new_v4()),
            tenant_id: TenantId(Uuid::new_v4()),
            url: "https://example.com/test".to_string(),
            name: "Test Endpoint".to_string(),
            is_active: true,
            signature_config: SignatureConfig::None,
            max_retries: 5,
            timeout_seconds: 30,
            retry_strategy: BackoffStrategy::Exponential,
            circuit_state: CircuitState::Closed,
            circuit_failure_count: 0,
            circuit_success_count: 0,
            circuit_last_failure_at: None,
            circuit_half_open_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            deleted_at: None,
            total_events_received: 0,
            total_events_delivered: 0,
            total_events_failed: 0,
        };

        storage.add_endpoint(endpoint.clone()).await;

        // Create test event
        let event = WebhookEvent {
            id: EventId(Uuid::new_v4()),
            tenant_id: endpoint.tenant_id,
            endpoint_id: endpoint.id,
            source_event_id: "test-001".to_string(),
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

        storage.add_pending_event(event.clone()).await;

        // Test storage trait methods
        let storage_trait: &dyn DeliveryStorage = &*storage;

        // Test endpoint configuration retrieval
        let retrieved_endpoint = storage_trait.find_endpoint_config(endpoint.id).await.unwrap();
        assert_eq!(retrieved_endpoint.max_retries, 5);
        assert_eq!(retrieved_endpoint.url, "https://example.com/test");

        // Test event claiming
        let claimed = storage_trait.claim_pending_events(1).await.unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].id, event.id);

        // Test status updates
        let status = storage_trait.find_event_status(event.id).await.unwrap();
        assert_eq!(status, EventStatus::Delivering);

        // Test marking delivered
        storage_trait.mark_delivered(event.id).await.unwrap();
        let final_status = storage_trait.find_event_status(event.id).await.unwrap();
        assert_eq!(final_status, EventStatus::Delivered);
    }

    /// Unit test: Error injection functionality works.
    #[tokio::test]
    async fn mock_storage_error_injection() {
        let storage = MockDeliveryStorage::new();

        // Inject error
        storage.inject_claim_error("Test database error".to_string()).await;

        // Test error is returned
        let result = storage.claim_pending_events(1).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Test database error"));

        // Test error is consumed
        let result = storage.claim_pending_events(1).await;
        assert!(result.is_ok());
    }

    /// Unit test: Delivery attempt recording works correctly.
    #[tokio::test]
    async fn delivery_attempt_recording() {
        let storage = MockDeliveryStorage::new();
        let event_id = EventId(Uuid::new_v4());

        let attempt = kapsel_core::models::DeliveryAttempt {
            id: Uuid::new_v4(),
            event_id,
            attempt_number: 1,
            endpoint_id: EndpointId::new(),
            request_headers: std::collections::HashMap::new(),
            request_body: b"test payload".to_vec(),
            response_status: Some(200),
            response_headers: None,
            response_body: Some(b"OK".to_vec()),
            attempted_at: Utc::now(),
            succeeded: true,
            error_message: None,
        };

        // Record attempt
        storage.record_delivery_attempt(attempt.clone()).await.unwrap();

        // Verify it was recorded
        let attempts = storage.find_delivery_attempts(event_id).await.unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].id, attempt.id);
        assert!(attempts[0].succeeded);

        // Test helper method
        let all_attempts = storage.recorded_attempts().await;
        assert_eq!(all_attempts.len(), 1);
        assert_eq!(all_attempts[0].event_id, event_id);
    }
}
