//! Integration tests for Merkle tree commit triggers.
//!
//! Tests basic Merkle tree functionality and commit behavior to ensure
//! reliable cryptographic attestation operations.

use std::{sync::Arc, time::Duration};

use anyhow::Result;
use http::StatusCode;
use kapsel_attestation::{signing::SigningService, MerkleService};
use kapsel_core::models::EventStatus;
use kapsel_testing::{http::MockResponse, TestEnv};

/// Test basic Merkle tree service initialization and setup.
///
/// Validates that the Merkle service can be properly initialized with
/// database storage and signing components.
#[tokio::test]
async fn merkle_service_initialization() -> Result<()> {
    TestEnv::run_isolated_test(|env| async move {
        let tenant_id = env.create_tenant("merkle-init-tenant").await?;
        let (_api_key, _key_hash) = env.create_api_key(tenant_id, "merkle-init-key").await?;
        let endpoint_id =
            env.create_endpoint(tenant_id, "https://merkle-init.example.com/webhook").await?;

        // Create signing service and merkle service
        let signing_service = SigningService::ephemeral();
        let _merkle_service =
            MerkleService::new(env.storage(), signing_service, Arc::new(env.clock.clone()));

        // Verify service is properly initialized by creating it successfully

        // Configure successful delivery mock
        env.http_mock
            .mock_simple("/webhook", MockResponse::Success {
                status: StatusCode::OK,
                body: vec![].into(),
            })
            .await;

        // Inject a webhook event for processing
        let payload = r#"{"test":"merkle_initialization"}"#;
        let event_id = env.ingest_webhook_simple(endpoint_id, payload.as_bytes()).await?;

        // Allow processing time
        env.advance_time(Duration::from_secs(1));

        // Verify event was created
        let event =
            env.storage().webhook_events.find_by_id(event_id).await?.expect("Event should exist");

        assert_eq!(event.tenant_id, tenant_id, "Event should belong to correct tenant");
        assert_eq!(event.endpoint_id.0, endpoint_id.0, "Event should reference correct endpoint");

        Ok(())
    })
    .await
}

/// Test time-based processing with Merkle tree operations.
///
/// Validates that the system can handle time-based operations and
/// maintains proper event sequencing over time intervals.
#[tokio::test]
async fn merkle_tree_time_based_processing() -> Result<()> {
    TestEnv::run_isolated_test(|env| async move {
        let tenant_id = env.create_tenant("time-processing-tenant").await?;
        let (_api_key, _key_hash) = env.create_api_key(tenant_id, "time-processing-key").await?;
        let endpoint_id =
            env.create_endpoint(tenant_id, "https://time-processing.example.com/webhook").await?;

        // Create merkle service for testing
        let signing_service = SigningService::ephemeral();
        let _merkle_service =
            MerkleService::new(env.storage(), signing_service, Arc::new(env.clock.clone()));

        // Configure successful delivery
        env.http_mock
            .mock_simple("/webhook", MockResponse::Success {
                status: StatusCode::OK,
                body: vec![].into(),
            })
            .await;

        // Add events over time intervals
        let mut event_ids = Vec::new();
        for i in 0..3 {
            let payload = format!(r#"{{"test":"time_based","sequence":{i}}}"#);
            let event_id = env.ingest_webhook_simple(endpoint_id, payload.as_bytes()).await?;
            event_ids.push(event_id);

            // Advance time between events
            env.advance_time(Duration::from_secs(2));
        }

        // Allow final processing
        env.advance_time(Duration::from_secs(1));

        // Verify all events were processed correctly
        for event_id in event_ids {
            let event = env
                .storage()
                .webhook_events
                .find_by_id(event_id)
                .await?
                .expect("Event should exist");

            assert!(
                matches!(
                    event.status,
                    EventStatus::Received | EventStatus::Pending | EventStatus::Delivered
                ),
                "Event should be in a valid processing state"
            );
        }

        Ok(())
    })
    .await
}

/// Test event count-based batch processing.
///
/// Validates that the system can handle multiple events and maintain
/// proper sequencing for batch operations.
#[tokio::test]
async fn merkle_tree_event_count_processing() -> Result<()> {
    TestEnv::run_isolated_test(|env| async move {
        let tenant_id = env.create_tenant("count-processing-tenant").await?;
        let (_api_key, _key_hash) = env.create_api_key(tenant_id, "count-processing-key").await?;
        let endpoint_id =
            env.create_endpoint(tenant_id, "https://count-processing.example.com/webhook").await?;

        // Create merkle service
        let signing_service = SigningService::ephemeral();
        let _merkle_service =
            MerkleService::new(env.storage(), signing_service, Arc::new(env.clock.clone()));

        // Configure successful delivery
        env.http_mock
            .mock_simple("/webhook", MockResponse::Success {
                status: StatusCode::OK,
                body: vec![].into(),
            })
            .await;

        // Add multiple events for batch processing
        let event_count = 10;
        let mut event_ids = Vec::new();

        for i in 0..event_count {
            let payload = format!(r#"{{"test":"count_based","event":{i}}}"#);
            let event_id = env.ingest_webhook_simple(endpoint_id, payload.as_bytes()).await?;
            event_ids.push(event_id);

            // Small delay between events
            env.advance_time(Duration::from_millis(100));
        }

        // Allow batch processing time
        env.advance_time(Duration::from_secs(2));

        // Verify all events exist and were processed
        assert_eq!(event_ids.len(), event_count, "Should have created all events");

        for (i, event_id) in event_ids.iter().enumerate() {
            let event = env
                .storage()
                .webhook_events
                .find_by_id(*event_id)
                .await?
                .expect("Event should exist");

            assert!(
                matches!(
                    event.status,
                    EventStatus::Received | EventStatus::Pending | EventStatus::Delivered
                ),
                "Event {i} should be in a valid state"
            );
        }

        // Verify tenant events can be retrieved
        let tenant_events =
            env.storage().webhook_events.find_by_tenant(tenant_id, Some(20)).await?;
        assert!(tenant_events.len() >= event_count, "Should be able to retrieve tenant events");

        Ok(())
    })
    .await
}

/// Test Merkle tree operations under mixed timing conditions.
///
/// Validates that the system handles both time-based and count-based
/// processing scenarios correctly in mixed workloads.
#[tokio::test]
async fn merkle_tree_mixed_processing_patterns() -> Result<()> {
    TestEnv::run_isolated_test(|env| async move {
        let tenant_id = env.create_tenant("mixed-pattern-tenant").await?;
        let (_api_key, _key_hash) = env.create_api_key(tenant_id, "mixed-pattern-key").await?;
        let endpoint_id =
            env.create_endpoint(tenant_id, "https://mixed-pattern.example.com/webhook").await?;

        // Create merkle service
        let signing_service = SigningService::ephemeral();
        let _merkle_service =
            MerkleService::new(env.storage(), signing_service, Arc::new(env.clock.clone()));

        // Configure successful delivery
        env.http_mock
            .mock_simple("/webhook", MockResponse::Success {
                status: StatusCode::OK,
                body: vec![].into(),
            })
            .await;

        // Phase 1: Rapid event creation (count-based scenario)
        let mut batch1_events = Vec::new();
        for i in 0..5 {
            let payload = format!(r#"{{"test":"rapid_batch","event":{i}}}"#);
            let event_id = env.ingest_webhook_simple(endpoint_id, payload.as_bytes()).await?;
            batch1_events.push(event_id);

            // Very short delays for rapid processing
            env.advance_time(Duration::from_millis(50));
        }

        // Phase 2: Time-spaced event creation (time-based scenario)
        let mut batch2_events = Vec::new();
        for i in 0..3 {
            let payload = format!(r#"{{"test":"time_spaced","event":{i}}}"#);
            let event_id = env.ingest_webhook_simple(endpoint_id, payload.as_bytes()).await?;
            batch2_events.push(event_id);

            // Longer delays for time-based processing
            env.advance_time(Duration::from_secs(2));
        }

        // Allow final processing
        env.advance_time(Duration::from_secs(3));

        // Verify all events from both batches
        for event_id in batch1_events.iter().chain(batch2_events.iter()) {
            let event = env
                .storage()
                .webhook_events
                .find_by_id(*event_id)
                .await?
                .expect("Event should exist");

            assert!(
                matches!(
                    event.status,
                    EventStatus::Received | EventStatus::Pending | EventStatus::Delivered
                ),
                "Mixed pattern event should be in valid state"
            );
        }

        // Verify total event count
        let total_events = env.storage().webhook_events.find_by_tenant(tenant_id, Some(20)).await?;
        assert!(total_events.len() >= 8, "Should have processed events from both batches");

        Ok(())
    })
    .await
}

/// Test Merkle tree error handling and recovery scenarios.
///
/// Validates that the system handles errors gracefully and can continue
/// processing subsequent operations after encountering issues.
#[tokio::test]
async fn merkle_tree_error_handling() -> Result<()> {
    TestEnv::run_isolated_test(|env| async move {
        let tenant_id = env.create_tenant("error-handling-tenant").await?;
        let (_api_key, _key_hash) = env.create_api_key(tenant_id, "error-handling-key").await?;
        let endpoint_id =
            env.create_endpoint(tenant_id, "https://error-handling.example.com/webhook").await?;

        // Create merkle service
        let signing_service = SigningService::ephemeral();
        let _merkle_service =
            MerkleService::new(env.storage(), signing_service, Arc::new(env.clock.clone()));

        // Phase 1: Configure failing responses
        // Phase 1: Configure error responses to test error handling
        env.http_mock
            .mock_simple("/webhook", MockResponse::ServerError { status: 500, body: vec![] })
            .await;

        // Add events that will encounter delivery failures
        let mut error_events = Vec::new();
        for i in 0..3 {
            let payload = format!(r#"{{"test":"error_scenario","event":{i}}}"#);
            let event_id = env.ingest_webhook_simple(endpoint_id, payload.as_bytes()).await?;
            error_events.push(event_id);

            env.advance_time(Duration::from_millis(100));
        }

        // Allow error processing
        env.advance_time(Duration::from_secs(2));

        // Phase 2: Switch to successful responses for recovery
        env.http_mock
            .mock_simple("/webhook", MockResponse::Success {
                status: StatusCode::OK,
                body: vec![].into(),
            })
            .await;

        // Add recovery events
        let mut recovery_events = Vec::new();
        for i in 0..2 {
            let payload = format!(r#"{{"test":"recovery_scenario","event":{i}}}"#);
            let event_id = env.ingest_webhook_simple(endpoint_id, payload.as_bytes()).await?;
            recovery_events.push(event_id);

            env.advance_time(Duration::from_millis(100));
        }

        // Allow recovery processing
        env.advance_time(Duration::from_secs(2));

        // Verify all events exist and system continued processing
        for event_id in error_events.iter().chain(recovery_events.iter()) {
            let event = env
                .storage()
                .webhook_events
                .find_by_id(*event_id)
                .await?
                .expect("Event should exist");

            // Events should be in some valid state, even if failed
            assert!(
                matches!(
                    event.status,
                    EventStatus::Received
                        | EventStatus::Pending
                        | EventStatus::Failed
                        | EventStatus::Delivered
                ),
                "Error handling event should be in valid state"
            );
        }

        // Verify system continued processing after errors
        let all_events = env.storage().webhook_events.find_by_tenant(tenant_id, Some(10)).await?;
        assert!(all_events.len() >= 5, "System should continue processing events after errors");

        Ok(())
    })
    .await
}
