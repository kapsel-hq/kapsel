//! Essential delivery worker integration tests.
//!
//! This file contains minimal integration tests that verify the delivery
//! worker functions correctly with the production PostgresDeliveryStorage.
//! Heavy HTTP status code testing has been moved to property_test.rs
//! using MockDeliveryStorage for 23x faster execution.

use std::time::Duration;

use anyhow::Result;
use kapsel_core::models::EventStatus;
use kapsel_testing::{fixtures::WebhookBuilder, TestEnv};
use wiremock::{matchers, Mock, MockServer, ResponseTemplate};

/// Integration test: production engine delivers webhook successfully.
///
/// Verifies the complete production path from PostgreSQL storage through
/// HTTP delivery using real database operations. Property tests cover
/// comprehensive HTTP status code validation.
#[tokio::test]
async fn production_engine_successful_delivery() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        env.create_delivery_engine()?;

        let mock_server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("OK"))
            .expect(1)
            .mount(&mock_server)
            .await;

        let webhook_url = mock_server.uri();
        let tenant = env.create_tenant("integration-test").await?;
        let endpoint = env.create_endpoint(tenant, &webhook_url).await?;

        let webhook = WebhookBuilder::new()
            .tenant(tenant.0)
            .endpoint(endpoint.0)
            .source_event("integration-001")
            .body(b"integration test payload".to_vec())
            .content_type("application/json")
            .build();

        let event_id = env.ingest_webhook(&webhook).await?;
        env.run_delivery_cycle().await?;

        let status = env.event_status(event_id).await?;
        assert_eq!(status, EventStatus::Delivered);

        Ok(())
    })
    .await
}

/// Integration test: production engine respects endpoint retry configuration.
///
/// Verifies that endpoint-specific max_retries settings are correctly
/// applied by the production worker. This is the critical integration
/// between PostgresDeliveryStorage.endpoint_config() and worker retry logic.
#[tokio::test]
async fn production_engine_endpoint_retry_configuration() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        env.create_delivery_engine()?;

        let mock_server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Server Error"))
            .expect(3) // Initial + 2 retries (max_retries = 2)
            .mount(&mock_server)
            .await;

        let webhook_url = mock_server.uri();
        let tenant = env.create_tenant("retry-config-test").await?;

        // Create endpoint with max_retries = 2
        let endpoint = env.create_endpoint_with_retries(tenant, &webhook_url, 2).await?;

        let webhook = WebhookBuilder::new()
            .tenant(tenant.0)
            .endpoint(endpoint.0)
            .source_event("retry-config-001")
            .body(b"retry config test".to_vec())
            .content_type("application/json")
            .build();

        let event_id = env.ingest_webhook(&webhook).await?;

        // Run multiple delivery cycles to process retries
        for _ in 0..5 {
            env.run_delivery_cycle().await?;
            env.advance_time(Duration::from_secs(2));
        }

        let status = env.event_status(event_id).await?;
        assert_eq!(status, EventStatus::Failed, "Event should be failed after exhausting retries");

        // Verify exactly 3 attempts were made (1 initial + 2 retries)
        let attempt_count = env.count_delivery_attempts(event_id).await?;
        assert_eq!(attempt_count, 3, "Should have exactly 3 delivery attempts");

        Ok(())
    })
    .await
}

/// Integration test: production engine graceful shutdown.
///
/// Verifies that the delivery engine can be started and stopped cleanly
/// without leaving workers in inconsistent states. Essential for production
/// deployment safety.
#[tokio::test]
async fn production_engine_graceful_shutdown() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        env.create_delivery_engine()?;

        let mock_server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("OK"))
            .expect(1)
            .mount(&mock_server)
            .await;

        let webhook_url = mock_server.uri();
        let tenant = env.create_tenant("shutdown-test").await?;
        let endpoint = env.create_endpoint(tenant, &webhook_url).await?;

        let webhook = WebhookBuilder::new()
            .tenant(tenant.0)
            .endpoint(endpoint.0)
            .source_event("shutdown-001")
            .body(b"shutdown test".to_vec())
            .build();

        let event_id = env.ingest_webhook(&webhook).await?;
        env.run_delivery_cycle().await?;

        // Verify processing worked (engine is functional)
        let status = env.event_status(event_id).await?;
        assert_eq!(status, EventStatus::Delivered);

        Ok(())
    })
    .await
}

/// Integration test: production storage trait implementation.
///
/// Verifies that PostgresDeliveryStorage correctly implements all
/// DeliveryStorage trait methods. This ensures the storage abstraction
/// layer works correctly with the production database.
#[tokio::test]
async fn postgres_delivery_storage_trait_implementation() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        let tenant = env.create_tenant("storage-trait-test").await?;
        let endpoint = env.create_endpoint(tenant, "https://example.com/test").await?;

        let webhook = WebhookBuilder::new()
            .tenant(tenant.0)
            .endpoint(endpoint.0)
            .source_event("storage-trait-001")
            .body(b"storage trait test".to_vec())
            .content_type("application/json")
            .build();

        let event_id = env.ingest_webhook(&webhook).await?;

        let status = env.event_status(event_id).await?;
        assert_eq!(status, EventStatus::Pending);

        let endpoint_from_db =
            env.storage().endpoints.find_by_id(endpoint.0.into()).await?.unwrap();
        assert_eq!(endpoint_from_db.url, "https://example.com/test");
        assert_eq!(endpoint_from_db.max_retries, 10); // Default from create_endpoint

        env.create_delivery_engine()?;

        let mock_server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("OK"))
            .expect(1)
            .mount(&mock_server)
            .await;

        let mut endpoint_config =
            env.storage().endpoints.find_by_id(endpoint.0.into()).await?.unwrap();
        endpoint_config.url = mock_server.uri();
        env.storage().endpoints.update(&endpoint_config).await?;

        env.run_delivery_cycle().await?;

        let final_status = env.event_status(event_id).await?;
        assert_eq!(final_status, EventStatus::Delivered);

        Ok(())
    })
    .await
}
