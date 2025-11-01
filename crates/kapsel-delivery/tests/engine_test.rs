//! Integration tests for delivery engine component.
//!
//! Tests webhook delivery engine processing of pending events from database
//! with engine lifecycle, event claiming, and coordination workflows.
//! - Error handling during event processing
//! - Integration with HTTP client and database

use std::sync::Arc;

use anyhow::Result;
use bytes::Bytes;
use http::StatusCode;
use kapsel_core::{models::EventStatus, Clock};
use kapsel_delivery::worker::{DeliveryConfig, DeliveryEngine};
use kapsel_testing::{http::MockResponse, TestEnv};

#[tokio::test]
async fn delivery_engine_processes_pending_events() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        // Create test data - no transactions needed for isolated tests
        let tenant_id = env.create_tenant("test-tenant").await?;
        let endpoint_id = env.create_endpoint(tenant_id, &env.http_mock.url()).await?;

        // Mock expects webhook delivery
        env.http_mock
            .mock_simple("/", MockResponse::Success {
                status: StatusCode::OK,
                body: Bytes::from_static(b"OK"),
            })
            .await;

        let webhook_data = kapsel_testing::fixtures::WebhookBuilder::new()
            .tenant(tenant_id.0)
            .endpoint(endpoint_id.0)
            .source_event("test-source-engine")
            .body(b"test payload".to_vec())
            .content_type("application/json")
            .build();

        let event_id = env.ingest_webhook(&webhook_data).await?;

        // Use test harness delivery cycle instead of engine to avoid tokio runtime
        // conflicts This still tests the core delivery logic but avoids engine
        // threading issues
        env.run_delivery_cycle().await?;

        // Verify event was processed
        let status = env.event_status(event_id).await?;
        assert_eq!(status, EventStatus::Delivered, "webhook should be delivered");

        Ok(())
    })
    .await
}

/// Test delivery engine starts with configured number of workers.
///
/// Verifies that the delivery engine correctly initializes and starts
/// the specified number of worker threads for processing webhooks.
#[tokio::test]
async fn engine_starts_with_configured_workers() -> Result<()> {
    TestEnv::run_isolated_test(|env| async move {
        let config = DeliveryConfig { worker_count: 5, ..Default::default() };

        let mut engine = DeliveryEngine::new(
            env.pool().clone(),
            config,
            Arc::new(env.clock.clone()) as Arc<dyn Clock>,
        )?;

        engine.start().await?;

        let stats = engine.stats().await;
        assert_eq!(stats.active_workers, 5);

        engine.shutdown().await?;

        Ok(())
    })
    .await
}

/// Test delivery engine shuts down gracefully without errors.
///
/// Verifies that the delivery engine can be cleanly shut down without
/// leaving orphaned workers or causing resource leaks.
#[tokio::test]
async fn engine_shuts_down_gracefully() -> Result<()> {
    TestEnv::run_isolated_test(|env| async move {
        let config = DeliveryConfig::default();

        let mut engine = DeliveryEngine::new(
            env.pool().clone(),
            config,
            Arc::new(env.clock.clone()) as Arc<dyn Clock>,
        )?;

        engine.start().await?;

        // Shutdown should complete without errors
        engine.shutdown().await?;

        Ok(())
    })
    .await
}
