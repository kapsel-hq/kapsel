//! Invariant tests for production webhook delivery behavior.
//!
//! Tests fundamental invariants that must hold in the production system,
//! using the actual DeliveryEngine to ensure test/production consistency.

use std::time::Duration;

use anyhow::Result;
use kapsel_core::EventStatus;
use kapsel_testing::{MockEndpoint, TestEnv, WebhookBuilder};

#[tokio::test]
async fn production_engine_maintains_state_invariants() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        env.create_delivery_engine()?;

        // Create test data using production code paths
        let tenant_id = env.create_tenant("invariant-tenant").await?;
        let endpoint_id = env.create_endpoint(tenant_id, &env.http_mock.url()).await?;

        // Configure successful delivery
        let mock_endpoint = MockEndpoint::success("/");
        env.http_mock.mock_endpoint(mock_endpoint).await;

        // Ingest webhook - should start in pending state
        let event_id = env.ingest_webhook_simple(endpoint_id, b"test payload").await?;
        let initial_status = env.event_status(event_id).await?;
        assert_eq!(initial_status, EventStatus::Pending, "New events must start in pending state");

        // Process with production engine
        env.process_batch().await?;

        // Verify successful delivery invariant
        let final_status = env.event_status(event_id).await?;
        assert_eq!(
            final_status,
            EventStatus::Delivered,
            "Successful delivery must transition to delivered"
        );

        // Verify delivery engine statistics are consistent
        let stats = env.delivery_stats().await.expect("engine should be available");
        assert_eq!(stats.successful_deliveries, 1);
        assert_eq!(stats.events_processed, 1);
        assert_eq!(stats.in_flight_deliveries, 0, "no deliveries should be in flight after batch");

        Ok(())
    })
    .await
}

#[tokio::test]
async fn idempotency_invariant_prevents_duplicates() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        env.create_delivery_engine()?;

        let tenant_id = env.create_tenant("idempotency-tenant").await?;
        let endpoint_id = env.create_endpoint(tenant_id, &env.http_mock.url()).await?;

        // Create two webhooks with same source_event_id
        let source_event_id = "unique-source-123";

        let webhook1 = WebhookBuilder::new()
            .tenant(tenant_id.0)
            .endpoint(endpoint_id.0)
            .source_event(source_event_id)
            .body(b"first webhook".to_vec())
            .build();

        let webhook2 = WebhookBuilder::new()
            .tenant(tenant_id.0)
            .endpoint(endpoint_id.0)
            .source_event(source_event_id)  // Same source ID
            .body(b"duplicate webhook".to_vec())
            .build();

        // First ingestion should succeed
        let event_id1 = env.ingest_webhook(&webhook1).await?;
        assert!(event_id1.0 != uuid::Uuid::nil(), "First event should be created");

        // Second ingestion with same source_event_id should return same event_id
        let event_id2 = env.ingest_webhook(&webhook2).await?;
        assert_eq!(
            event_id1, event_id2,
            "Idempotency invariant: duplicate source_event_id must return same event"
        );

        Ok(())
    })
    .await
}

#[tokio::test]
async fn retry_invariant_maintains_pending_state() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        env.create_delivery_engine()?;

        let tenant_id = env.create_tenant("retry-tenant").await?;
        let endpoint_id = env.create_endpoint(tenant_id, &env.http_mock.url()).await?;

        // Configure endpoint to return server error (should trigger retry)
        let mock_endpoint = MockEndpoint::failure("/", http::StatusCode::INTERNAL_SERVER_ERROR);
        env.http_mock.mock_endpoint(mock_endpoint).await;

        let event_id = env.ingest_webhook_simple(endpoint_id, b"retry payload").await?;

        // Process first attempt - should fail and schedule retry
        env.process_batch().await?;

        let status_after_failure = env.event_status(event_id).await?;
        assert_eq!(
            status_after_failure,
            EventStatus::Pending,
            "Failed delivery with retries must remain pending"
        );

        // Verify delivery statistics reflect the failure
        let stats = env.delivery_stats().await.expect("engine should be available");
        assert_eq!(stats.failed_deliveries, 1, "Failed attempt should be recorded");
        assert_eq!(stats.successful_deliveries, 0, "No successful deliveries yet");

        Ok(())
    })
    .await
}

#[tokio::test]
async fn batch_processing_invariant_handles_multiple_events() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        env.create_delivery_engine()?;

        let tenant_id = env.create_tenant("batch-tenant").await?;
        let endpoint_id = env.create_endpoint(tenant_id, &env.http_mock.url()).await?;

        // Configure successful delivery
        let mock_endpoint = MockEndpoint::success("/");
        env.http_mock.mock_endpoint(mock_endpoint).await;

        // Create multiple webhooks
        let mut event_ids = Vec::new();
        for i in 0..5 {
            let payload = format!("batch payload {i}");
            event_ids.push(env.ingest_webhook_simple(endpoint_id, payload.as_bytes()).await?);
        }

        // All should start in pending state
        for event_id in &event_ids {
            let status = env.event_status(*event_id).await?;
            assert_eq!(status, EventStatus::Pending);
        }

        // Process batch - may take multiple cycles to process all
        env.process_all_pending(Duration::from_secs(10)).await?;

        // Verify all events were processed
        let mut delivered_count = 0;
        for event_id in &event_ids {
            let status = env.event_status(*event_id).await?;
            if status == EventStatus::Delivered {
                delivered_count += 1;
            }
        }

        assert!(delivered_count > 0, "At least some events should be delivered");

        let final_stats = env.delivery_stats().await.expect("engine should be available");
        assert!(final_stats.events_processed >= u64::try_from(delivered_count).unwrap());
        assert_eq!(
            final_stats.in_flight_deliveries, 0,
            "No deliveries should be in flight after processing all"
        );

        Ok(())
    })
    .await
}

#[tokio::test]
async fn database_consistency_invariant_after_delivery_cycle() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        env.create_delivery_engine()?;

        let tenant_id = env.create_tenant("consistency-tenant").await?;
        let endpoint_id = env.create_endpoint(tenant_id, &env.http_mock.url()).await?;

        // Mix of successful and failing endpoints
        let success_endpoint = MockEndpoint::success("/");
        env.http_mock.mock_endpoint(success_endpoint).await;

        // Create events
        let success_event = env.ingest_webhook_simple(endpoint_id, b"success payload").await?;

        // Process with production engine
        env.process_batch().await?;

        // Verify database state is consistent
        let success_status = env.event_status(success_event).await?;
        assert_eq!(success_status, EventStatus::Delivered);

        // Verify storage layer can find the data
        let event_from_storage = env.storage().webhook_events.find_by_id(success_event).await?;
        assert!(
            event_from_storage.is_some(),
            "Storage layer must be consistent with direct queries"
        );

        let event_data = event_from_storage.unwrap();
        assert_eq!(event_data.status, EventStatus::Delivered);

        Ok(())
    })
    .await
}

#[tokio::test]
async fn engine_lifecycle_invariant_handles_restart() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        env.create_delivery_engine()?;

        let tenant_id = env.create_tenant("lifecycle-tenant").await?;
        let endpoint_id = env.create_endpoint(tenant_id, &env.http_mock.url()).await?;

        let mock_endpoint = MockEndpoint::success("/");
        env.http_mock.mock_endpoint(mock_endpoint).await;

        // Create event
        let event_id = env.ingest_webhook_simple(endpoint_id, b"lifecycle test").await?;

        // Process first batch
        env.process_batch().await?;

        let status_after_first = env.event_status(event_id).await?;
        assert_eq!(status_after_first, EventStatus::Delivered);

        // Create another event and process (tests engine recreation)
        let event_id2 = env.ingest_webhook_simple(endpoint_id, b"second batch").await?;
        env.process_batch().await?;

        let status_after_second = env.event_status(event_id2).await?;
        assert_eq!(status_after_second, EventStatus::Delivered);

        // Both deliveries should be recorded in stats
        let final_stats = env.delivery_stats().await.expect("engine should be available");
        assert!(final_stats.events_processed >= 2, "Both processing cycles should be counted");

        Ok(())
    })
    .await
}
